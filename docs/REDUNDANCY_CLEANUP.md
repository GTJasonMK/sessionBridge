# 冗余清理记录

审计日期：2026-09-22（延续 2026-08-29、2026-09-02、2026-09-03、2026-09-20、2026-09-21 的清理记录）

完整入口、模块、协议、宏/配置和资源覆盖表见 [调用与配置拓扑](CALL_TOPOLOGY.md)。历史阶段的未完成列表保留作过程记录，最新状态以本文末尾为准。

## BFS 覆盖范围

从以下生产入口开始逐层追踪显式调用、导入、序列化合同、配置和测试引用：

- CLI：`src/main.rs` -> `src/cli.rs` -> `src/app/handlers.rs` -> 全部 `src/app/*` 服务。
- GUI：`gui/src-tauri/src/main.rs` -> `gui/src-tauri/src/lib.rs` -> `commands.rs` -> `OperatorService`、业务服务和配置/偏好文件。
- MCP：`handlers.rs` 的 `mcp serve` -> `src/app/mcp.rs` 的工具列表、分派和服务调用。
- 持久化：`src/storage/migrations.rs` -> `schema_v1.sql` 至 `schema_v22.sql`，以及 SQLite 读写、恢复和锁路径。
- 外部合同：`docs/protocol.md`、Serde/Clap/Tauri 宏、JSON/TOML 字段、动态 Git/GUI 子进程调用。
- 配置与静态资产：`Cargo.toml`/`Cargo.lock`、`gui/vite.config.ts`、`gui/package.json`、`docs/ui-mockups` 的页面、脚本和样式引用。
- 验证入口：`tests/*.rs`、模块内 `#[cfg(test)]`、`gui/src/api.test.ts`。

隐式入口已单独识别：应用自有 trait 为 `Clock`（`SystemClock`、`FakeClock`、`&T` 实现）；Tauri `.manage(AppState)`/`State` 注入、`generate_handler!`/`generate_context!` 与 mobile cfg；Clap/Serde 派生；SQL migration 的 `include_str!`；文件类型的 Drop。第一方源码和构建清单中未发现自有反射、动态代理、AOP 或 XML 业务装配系统，Android XML 是图标资源。逐项业务判定、反向消费者与验证见下文及完整拓扑。

## 变更台账

| 状态 | 位置 | 处理 | 证据/影响 |
| --- | --- | --- | --- |
| 已完成 | `src/invitation/output.rs`、`src/invitation/mod.rs` | 删除未使用的邀请输出选项抽象及其导出 | 全仓无生产调用；现有输出路径直接使用 CLI 和邀请服务合同 |
| 已完成 | `src/cli.rs`、`src/app/handlers.rs`、`src/domain/model.rs`、`src/app/work.rs`、`src/app/collaboration_service.rs` | 删除 CLI 与领域枚举之间的重复映射层，直接复用领域/服务枚举 | Clap、服务调用和测试均已沿同一枚举类型收敛 |
| 已完成 | `src/app/agent.rs`、`src/app/completion_service.rs`、`src/app/task.rs`、`src/app/mod.rs` | 合并重复数据库 ID/枚举解析逻辑 | 共享解析函数覆盖 agent、completion、task 三条读取路径 |
| 已完成 | `src/app/idempotency.rs` 及其调用方 | 统一 session 幂等 key 构造 | 所有 session 写入服务复用同一构造函数 |
| 已完成 | `src/app/invite.rs`、`src/invitation/files.rs` | 删除只有一个变体的 `PreparedArtifacts` 包装和重复 finalize 实现 | staging 生命周期由 `BatchStaging` 唯一负责；prepared 后的 finalize 错误保留 staging 供同一 request 恢复 |
| 已完成 | `src/app/launch.rs`、`src/invitation/prompt.rs` | 合并重复的 shell 类型和手写十六进制编码 | Launch 与 prompt 使用同一 `PromptShell`；摘要使用标准格式化 |
| 已完成 | `src/domain/completion.rs`、`src/domain/participant.rs`、`src/app/operator.rs` | 删除已由调用图证明无调用的 getter、别名和服务包装方法 | 编译、模块测试、GUI 调用及 integration tests 覆盖剩余 API |
| 已完成 | `src/domain/participant.rs`、`src/domain/completion.rs` | 删除 `ParticipantWorkState::is_empty()` 后将两个残留调用收敛到 `is_clear_for_conclusion()` | BFS 复核确认别名语义完全相同；避免恢复重复 API，并修复清理后的编译回归 |
| 已完成 | `Cargo.toml`、`Cargo.lock` | 删除 root crate 未使用的直接依赖及相应锁记录 | root crate 不再直接引用 `anyhow`、`assert_cmd`、`predicates`、`proptest`；Tauri 仍可能间接引入 `anyhow` |
| 已完成 | `src/app/mod.rs`、`src/app/handlers.rs`、`src/app/invite.rs` | 合并两个相同的相对路径绝对化 helper | 两个调用点共享同一错误映射和路径语义；未改变绝对路径行为 |
| 已完成 | `src/app/invite.rs`、`src/invitation/token.rs` | 删除 marker 专用 `hex_hash` 循环，复用 `InviteTokenHash::to_hex()` | 32 字节摘要编码保持相同的小写十六进制格式；marker 校验和恢复路径测试通过 |
| 已完成 | `gui/src-tauri/src/preferences.rs`、`gui/src-tauri/src/error.rs` | 删除未消费的 GUI 偏好 schema/最近列表和硬编码错误转换；默认值改为 derive | 偏好只保留实际使用的 `last_workspace`；读取失败保留真实文件路径，不再静默重置 |
| 已完成 | `gui/src/api.ts`、`gui/src/App.tsx` | 删除 API 层重复错误 catch，以及前端不可达的 dirty-path 禁用和空 prompt fallback | React Query/交互边界统一负责错误展示；后端仍是 dirty baseline 的唯一门控 |
| 已完成 | `gui/src-tauri/src/commands.rs`、`src/app/launch.rs`、`src/app/settings.rs` | 删除 GUI/Launch/Profile 之间的 shell 转换层，直接复用 `PromptShell` | Serde 值和 CLI 文本值保持 `posix`/`powershell` 合同不变 |
| 已完成 | `src/app/agent.rs`、`src/app/join.rs` | 共享 session token 前缀，并将同文件内三处常量时间比较收敛到一个 helper | token 格式、比较长度校验和错误码未改变 |
| 已完成 | `src/error.rs`、`src/logging.rs`、`src/app/task.rs` | 删除仅用于包装字符串的 `anyhow` 依赖和转换 | `Internal` 保留原错误文本；`cargo machete` 未发现新增未使用依赖 |
| 已完成 | `src/app/operator.rs`、`src/app/task.rs`、`src/git.rs`、`src/output.rs` | 删除无调用的服务 wrapper/getter，并将仅内部序列化 DTO 收窄为私有 | 反向调用图和 workspace/GUI/integration tests 未发现消费者 |

前轮记录的具体删除点补充（按当前 diff 复核）：无消费的 `--no-color`；`app::dispatch` 转发；`TaskQuery`、`InvitationSummary`；`TaskService::verify_workspace`、`OperatorService::events_since`、`GitRepository::root`；`DecisionResolution` 与 `ApprovalDecision` 合并；`bool_i64` 改用 `i64::from`；BatchStaging 的 staging_path/stage_json/cleanup、FinalizedBatch 包装。领域 getter 涉及 CompletionHead、CompletionSignalSnapshot、AgentConclusion、CompletionCohort 和 TaskParticipant，实际状态字段与 Serde 输出仍保留；本轮 R24 继续清掉仅测试消费的残留方法。

## 明确保留

- 所有历史 migration 保留：它们是已部署数据库的升级链，不是死代码。
- `#[serde(default)]`/默认函数须逐项核查：宏引用属于活跃入口，但 `Option<T>` 的重复 default 标注、写入后永不读取的字段和旧格式转换不能凭宏引用一概保留。
- CLI、MCP、Tauri 三套入口保留：它们是不同生产协议，不是同一入口的重复实现。
- 安全校验、原子写入、权限检查、错误映射和恢复 marker 保留：它们防止数据损坏或越权，不能按“低命中率 fallback”删除。
- Tailwind preflight、`@xyflow/react` 以及静态 mockup 资产保留：它们分别属于全局样式初始化、依赖图功能和可交付预览资产。早期保留的 `TAURI_ENV_` 客户端前缀经完整消费者复核后已在 R73 删除，Tauri 构建 hook 的环境变量仍由原生工具消费。
- `LaunchPreflight.dirty_paths` 等序列化字段保留：即使当前 UI 不再自行禁用按钮，它们仍是后端诊断和外部合同的一部分。

## 阶段性结论（2026-09-20）

上一轮已覆盖入口和主要服务，但不足以证明全仓“同一业务，唯一实现”。本次从当前工作树重新复核；尚未完成的项目列在下方，不将历史测试结果当成本次验证。

## 2026-09-20 继续清理

接续判断：上一轮留下实际代码与台账，属于有进展。保留工作树中独立的 `docs/ui-mockups` 改动。开始修改前 `cargo check --workspace --all-targets` 通过。

### BFS：私有文件与恢复产物

| 层次 | 覆盖节点与边 |
| --- | --- |
| L0 外部入口 | CLI `main -> Cli::run -> handlers::dispatch`；桌面 `lib::run -> generate_handler! -> commands`；`AgentRuntime::load`；集成测试直接调用服务 |
| L1 服务 | `WorkspaceService -> workspace::{initialize,link,discover}`；`InvitationService::{create,rotate,recover}`；`JoinService::join`；`LaunchService::{launch,resume}`；`settings::initialize`；`logging::{init,location,tail}`；`GuiPreferences::{load,remember,forget,save}` |
| L2 原文件实现 | `workspace::{atomic_create_private,atomic_write_private,atomic_write_json_private,ensure_private_dir}` 与 `invitation::{atomic_write_private,write_json_private,ensure_private_dir,BatchStaging}`；invite/join/workspace 各自目录 fsync |
| L3 隐式依赖 | JSON/TOML 内容读取；Launch/Join marker 恢复；`.staging` 前后缀搜索；Serde 序列化；`Drop` 清理生命周期；Unix `0600/0700` 与非 Unix 条件编译；Tauri 偏好配置目录 |
| L4 反向验证 | 搜索上述全部符号、`InvitationError` 变体和 `PermissionGuarantee`；`invitation_files`、`invite_service`、`join_service`、`launch_cli`、`workspace_link`、`logging_cli` 与 workspace/runtime 模块测试；已安装 tempfile 3.27.0 的 persist/权限实现 |

| 编号 | 状态 | 优化 | 唯一实现与影响 |
| --- | --- | --- | --- |
| R19 | 已完成 | 合并三套私有文件写入和四处目录 fsync；删除手写临时文件名、文件权限重复设置、Invitation 文件 I/O 错误转换、无生产消费者的 `PermissionGuarantee` | `src/private_files.rs`；`WriteMode::{CreateNew,Replace}` 显式保留两种发布语义；所有消费者直接导入共享实现，不留旧导出或兼容适配器。CreateNew 采用 tempfile 不覆盖发布，消除先检查再 rename 的覆盖竞态；目录元数据异常直接报错。文件相关 Rust API 移到新模块；文件冲突返回保留路径/原因的 `SessionBridgeError::Io`。全 workspace 测试通过。 |
| R20 | 已完成 | 用 `TempDir` 收敛 BatchStaging 生命周期，删除 `finished` 双重状态、自定义随机后缀和 Drop | `src/invitation/files.rs`；仍按原 `.批次.随机.staging` 形式搜索。prepared 后 finalize 先 keep，所有后续失败保留恢复文件；未 finalize 时自动清理。临时目录显式设置 Unix 0700。全 workspace 测试通过。 |
| R21 | 已完成 | 删除仓库身份写入的任意错误读取回退 | `workspace::read_or_create_instance_id`；只对真正的 AlreadyExists 竞争读取获胜者，fsync/权限等其他失败向上传播。全 workspace 测试通过。 |
| R22 | 已完成 | 合并 planning/completion/work/collaboration 的四份同义请求 ID 校验 | `app::idempotency::validate_request_id`；保留 128 字节、非空白、无控制字符约束，以及原样不裁剪的幂等键语义。协作错误文案改为同一详细文案，错误码不变。BFS：CLI JSON/typed flags、MCP conclude、公开服务 -> 四服务全部写入方法 -> 幂等 replay/record；无宏/配置注册；字节边界与控制字符检查通过。 |
| R23 | 已完成 | 删除 `UuidText` 类型包装和 task 的 `parse_database_enum` 转发别名 | BFS：planning 全部 11 个 UUID 调用；TaskService -> task/criteria 两处行解码 -> 6 个枚举解析调用；复用 `Uuid::now_v7` 与同文件 `parse_database_value`，保留诊断字段。两者均无配置/宏注册。 |
| R24 | 已完成 | 删除仅测试使用的 CompletionHead::advance_epoch、配套两个不可达错误分支及 CompletionReduction::is_ready | BFS：CLI/GUI -> TaskService 生命周期 SQL；Agent/MCP -> CompletionService -> CompletionHead/Reducer；全仓仅模块测试调用上述方法，无 Serde/Clap/Tauri 注册。真实 epoch/revision 变更仍在 task 事务中检查溢出；Reducer 测试直接匹配 Ready 结果，保留原验收断言。 |
| R25 | 已完成 | 合并四份 session 事件封装，删除重复传入的完整 AuthenticatedAgent | `events::append_session` -> 唯一 `events::append` INSERT；BFS 覆盖 planning 8、work 12、collaboration 16、completion 2 个调用及 CLI/MCP 路由。认证函数按 context.session_id 查询 Session，并检查 task/token，故事件从同一个已认证 context 取 actor；鉴权路径不变。集成测试已检查真实事务事件的 workspace/task/actor/entity。 |
| R26 | 已完成 | 合并五份 completion revision 更新函数及 Join/过期邀请的同义内联 SQL | `task::advance_completion_revision`；BFS 覆盖 planning/completion 的 CAS、work/collaboration/invite 的事务内更新，以及 join/expire 两种入口，继续要求 `Transaction`。调用者显式提供期望版本或 None；一次 UPDATE RETURNING 返回数据库新值，CAS 失败不转为无条件更新，不再在内存重复计算 expected+1。已有联动 plan_version/version/epoch 更新属于不同状态迁移，保留原条件和原子 SQL。真实 migration 下的 CAS/缺失任务/事务回滚检查通过。 |
| R27 | 已完成 | 合并直接创建关系与批准计划时的两套依赖环查询 | `planning::dependency_path_exists`；BFS：CLI item relate -> PlanningService::relate_work_items；CLI plan/approval -> CollaborationService -> apply_plan_operation 的 work_item.relate 分派 -> 同一递归 CTE。SQL、task 隔离和绑定参数不变；await 的 Session 等待环使用不同图和表，保留。 |
| R28 | 已完成 | 合并 MCP 参数对象入口、类型解析与 ID/枚举/数组解析；删除错误类型回退和 usize 强转 | BFS：`mcp::serve -> tools/call -> call_tool` 的全部 9 个工具；对照 tools/list schema、Serde 领域枚举/ID、Agent/Chat/Await/ExternalWait/Completion 服务及 outbox 确认路径。缺省值只在字段缺失时生效；错误类型、null、越界数字明确报 InvalidInput，deadline 不再被静默删除；Chat 入口同样要求 arguments 对象。进程测试 6 种错误请求和未生成 wait 的断言，以及同进程后续崩溃恢复/单次投递均通过。 |
| R29 | 已完成 | 删除 AgentSessionView 中重复保存的 idempotency_actor_id、SQL 中读取后丢弃的 id 列，以及 WorkService::session_key 包装 | BFS：AgentService::authenticate -> load_session 按同一个 SessionId 读取，重复字段始终等于 session_id.to_string；仅 work 的 13 处、await 与 workflow 各一处消费。改为直接构造已有 IdempotencyKey::session；原字段 serde(skip)，无外部 JSON 合同变化。 |
| R30 | 已完成 | 删除协作结果先转 JSON 再提取游标的 record_result 动态包装 | BFS：16 个协作写入调用及其全部结果 DTO 的 event_sequence:i64 -> idempotency::record -> SQL；改为直接传 Some(result.event_sequence)，只序列化一次，缺字段/错类型由编译器报错，不再静默写入空游标。 |
| R31 | 已完成 | 合并数据库 ID、枚举与微秒时间戳解码，删除 app 中的解析别名和专用转发函数 | 唯一实现 `storage/values.rs`；BFS：CLI/GUI/MCP -> Task/Agent/Work/Planning/Completion/Invitation/Join -> 行解码、marker 恢复 -> FromStr/time，覆盖全部原 parse_id/parse_enum/parse_phase/parse_invitation_id/from_micros 调用及 Serde/Clap 注册。保留 Task 原有 field/value_details 诊断，其他数据库读取路径获得同样诊断；current Task 的损坏字段错误从 Internal 统一为 Database。外部输入的 CLI/GUI/Join ID 校验保留 InvalidInput 分类。i64 微秒转 i128 纳秒不会溢出，删除该不可能分支，保留真实日期范围错误。 |
| R32 | 已完成 | 删除整个自定义 time_serde 模块，直接使用 time::serde::rfc3339；删除输出元数据的 1970 时间戳回退 | BFS：领域模型、participant record、completion 与 TaskDetails 的全部 17 个 Serde with 引用 -> CLI/Tauri/幂等 JSON；output 的成功/错误 envelope -> Meta。已逐函数对比依赖自带 RFC3339/option 实现；time feature 收敛为 serde-well-known（包含原有 serde/formatting/parsing），不新增依赖。保留字符串/null 及必需字段语义；Meta 保存真实 OffsetDateTime，格式错误不再伪造 epoch。增加含微秒时间戳/null 往返和非法时间元数据检查。 |
| R33 | 已完成 | 删除 Unix 微秒的极值饱和回退、Task 重复转换和三份时间字符串格式化 | `clock::unix_micros` 与 `clock::format_timestamp` 为统一实现。BFS：全仓 unix_micros 写入调用、Task 的 8 个转换点、Work lease/Invite manifest/Agent sync 的字符串时间 -> time；已通过 workspace feature tree 核实未启用 large-dates，配置日期范围 ±9999 年可表示为 i64 微秒；保留 checked conversion 的不变量断言，异常不写入伪造极值。新增负时间向零取整与类型最小/最大日期检查；Agent 数据库时间错误携带具体列名。 |
| R34 | 已完成 | 删除 32 个普通 Option 字段上的重复 serde(default) | BFS：Agent 提案、Checkpoint JSON、Claim/Artifact/Evidence、协作请求、Chat、LaunchProfile、Tauri Launch/Chat 输入 -> Deserialize/flatten -> 全部服务消费者；逐字段核实没有 with/deserialize_with。Serde 原生 Option 缺失即 None，序列化字段及验证不变；保留 Vec/String/bool/Value 的业务缺省值和 RFC3339 自定义字段规则。 |
| R35 | 已完成 | 合并 Join/Launch 的 UUID 请求 ID 验证、规范化和缺省生成 | `idempotency::uuid_request_id`；BFS：JoinService::join、LaunchService::{launch,resume} -> marker 路径/锁/恢复载荷；无反射注册。原有 UUID 各文本形式、无 ID 时生成 v7、InvalidInput 分类不变，统一错误文案；路径型非法 ID 仍被拒绝。 |
| R36 | 已完成 | 删除 confirm_integration、validate_active_task_mutation、validate_assessment 的未读参数 | BFS：CLI integrate confirm 与 work_service 两个重放测试 -> confirm_integration 全函数；retire/worker mutation -> task mutation 校验；AgentConclusion::try_new -> assessment 校验。集成确认始终由 preparation ID 决定幂等结果；schema 版本已在 try_new 唯一校验；operation 仍用于真正需要它的权限/状态错误。仅移除未被读取的参数与无效 UUID 生成，Rust confirm_integration 调用方需删去最后参数。保留 WorkerGuard/MigrationLockGuard 的 Drop 生命周期字段和平台 sync 参数。 |
| R37 | 已完成 | 删除前端 command -> invoke 的恒等包装 | BFS：main.tsx -> App 所有 api 属性调用、api.test.ts -> api.ts -> @tauri-apps/api/core::invoke -> generate_handler! 中的 14 个 command。已核实依赖原生 invoke 默认参数就是 {}；命令名、参数名、返回 Promise 与错误传播均保持，前端没有另一条业务实现。 |
| R38 | 已完成 | 删除未知前端错误伪造的 Operation unavailable 编号 | BFS：Tauri/plugin Promise 拒绝 -> App/Query 错误边界 -> normalizeError -> 全部 ErrorCard 消费；后端 GuiError 仍提供真实 operation_id，前端未知异常不再杜撰编号。对实际渲染字段做类型检查，避免伪结构错误让 React 渲染无效对象；错误消息继续显示并通过 alert 宣告。只在存在真实编号时显示该行。 |

### 仍需完成的复核

- 请求 ID：邀请（256 字节）、await（200 字节）、Join/Launch（UUID）和 Chat 等接口的长度/UUID/缺省语义仍需核对，不能静默改变合法输入范围。
- 数据库解析：不同服务仍存在字段诊断、enum/ID 解码和时间转换的重复实现，需继续横向核对。
- 领域层仅测试消费的方法、剩余默认值、生产配置和静态资产引用仍需完成审计。

### 本轮已执行的验证

- 开始前 `cargo check --workspace --all-targets` 通过。
- R19–R21：`cargo test --workspace --no-run`，随后 `XDG_STATE_HOME=/tmp/sessionbridge-redundancy-audit-state timeout 60s cargo test --workspace --quiet`，全部通过。
- R22–R27：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`，测试预编译与上述 60 秒全 workspace 回归，全部通过。
- R28：严格 workspace Clippy 与 `timeout 60s cargo test -p sessionbridge --test mcp_process --quiet` 通过（同样设置可写 XDG_STATE_HOME，执行前单独预编译）。
- R29–R30 后最终复核：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo machete`、`git diff --check` 通过；预编译后 60 秒全 workspace 测试 **174 通过、0 失败、0 忽略**。运行日志：`/tmp/sessionbridge-redundancy-final-tests.log`。
- R33 首次编译发现 `ended_at.map(format_timestamp)` 函数值引用遗漏；已改为具列名的数据库时间转换，并按完整标识符补查所有高阶函数引用，2026-09-21 完成复验。首次失败未作为通过计入。
- 新增检查覆盖并发创建不覆盖、替换失败保留文件、请求 ID 字节边界、真实事件归属、CAS 与事务回滚；旧 staging 冲突恢复、Join/Launch 重放、MCP 崩溃恢复用例继续通过。

已知交付边界：本轮运行验证为 Linux；未重做浏览器、桌面交互和非 Unix 运行验证。私有文件 Rust API 迁至 `private_files` 且新增显式 WriteMode，旧导出已删除；外部 Rust 调用方须更新导入和参数。Join 的 JSON 文件写入统一为 pretty JSON 加换行，恢复按 JSON 内容校验，不依赖该空白格式；邀请 secret 的 to_json_bytes 输出未改动。全仓目标仍进行中，不能用本轮通过的测试推断剩余审计项已完成。

## 上一轮记录的验证（本轮需重新验证）

- `git diff --check`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `XDG_STATE_HOME="$(mktemp -d /tmp/sessionbridge-audit.XXXXXX)" timeout 60s cargo test --workspace`（全部通过；为沙箱提供可写默认日志目录）
- `cargo machete`（未发现未使用依赖）
- `cd gui && npm test -- --run`、`npm run build`（GUI 全部通过）

## 2026-09-21 继续清理

接续判断：上一轮留下 R31–R38 的实际修改和未完成验证，属于有进展。开始时重新执行 `cargo check --workspace --all-targets`、`cargo test --workspace --no-run`、60 秒全 workspace 回归（179 通过）、前端 4 项单测及生产构建，均通过；R31–R38 的编译/测试和 GUI 页面复核现均已完成。保留用户独立的静态 mockup 修改。

### BFS：直接规划与审批执行

| 层次 | 覆盖节点与边 |
| --- | --- |
| L0 | Clap `requirement`/`item`/`plan` 子命令 -> `handlers`；集成测试直接调用 PlanningService/CollaborationService |
| L1 | `supersede_requirement`、`relate_work_items`、`reopen_work_item`、`split_work_item`；`resolve_plan_change` |
| L2 | `apply_mode` 分支 -> 直接 SQL 或 `finalize_plan_change` -> `plan_changes.operations_json` -> 审批分派；全部六个 op 字符串及读写引用 |
| L3 | work_items/work_relations、criterion_coverage、requirement_coverage、work_claims/work_attempts、change_sets/resource_reservations、invalidations；外键、CHECK、唯一索引、事务、幂等重放、Task/Completion reconciliation |
| L4 | `tests/planning_service.rs`、`tests/collaboration_service.rs`、`tests/work_service.rs`、CLI 合同；`docs/protocol.md` 第 11 节；未发现额外 GUI/MCP 规划写入分派或自有反射注册 |

| 编号 | 状态 | 优化 | 证据与影响 |
| --- | --- | --- | --- |
| R39 | 已完成 | 合并直接与审批拆分的父项变更和下游依赖重连 | `planning::{load_splittable_work_item,apply_work_item_split}` 为唯一实现；审批原来仅把父项 superseded，现复用完整重连并重新验证父项无活跃 Claim。新增成对直接/审批回归先复现 Proposal 路径保留旧依赖的失败，再验证两条路径一致；审批 child 更新检查 task/parent/state 和受影响行数，失败回滚。 |
| R40 | 已完成 | 合并直接与审批需求替换的覆盖失效/迁移及版本更新 | `planning::{load_supersedable_requirement,apply_requirement_supersession}`；同一个任务、替代目标有效性、必需需求不能无替代删除等规则共用。成对回归先证明审批路径仍留下 covered，再验证旧覆盖 invalidated、新覆盖 covered；唯一索引冲突仅按已有覆盖处理，其他 SQL 约束错误不再被 INSERT OR IGNORE 吞掉。 |
| R41 | 已完成 | 合并直接与审批工作项重开的变更集、资源预留和下游失效处理 | `planning::{load_reopenable_work_item,apply_work_item_reopen}`；审批现在也验证活跃 Claim、Evidence 与工作项状态，并使用原 PlanChange reason 记录失效原因。回归经真实 claim/submit 路径产生变更集，先复现审批后 pending/未释放/无 invalidation，再验证两种自治策略都得到 superseded/已释放/一条 invalidation。 |
| R42 | 已完成 | 用 `PlanOperation` 收敛六种计划操作的序列化与分派，删除动态字段提取及拒绝流程的静默跳过 | BFS 已覆盖所有 operations_json 写入、读取和宏派生；字符串 op 及 JSON 字段保持。未知 op、缺少字段、混合 child ID 数组会在事务内解析失败；取消/激活目标不存在或状态不符明确失败，不再把零行更新标为成功。没有新增旧格式转换或别名。 |
| R43 | 已完成 | 合并普通工作项关系的四处 INSERT 和直接/审批关系校验 | `planning::{insert_work_relation,validate_work_relation}`；调用覆盖新建工作项依赖、拆分子项继承依赖、直接 relate 与审批 relate。新子项 ID 唯一且依赖集合已去重，原 INSERT OR IGNORE 无可恢复冲突，改为普通 INSERT；拆分时重连已有边属于集合合并，仍只处理明确的唯一键冲突。 |
| R44 | 已完成 | 删除 Candidates 整个无效参数结构、四个从不构造的错误码和只重复验证 derive(Copy) 的测试 | BFS：Clap Candidates -> 唯一 handlers match（原参数整包丢弃）-> sync -> candidates；`--explain` 与该命令的 `--limit` 均未改变过输出，删除后由 Clap 明确拒绝。ErrorCode 全仓构造点、Serde Serialize、CLI/MCP/Tauri 错误边界和文档码表核对后，删除 SessionCapacityReached、CompletionCohortOpen、ParticipantStale、WaitAlreadyTerminal；stale warning 与真实容量/等待校验保留。数据库 busy 分类改由 `code()` 单一判定，retryable 语义不变；事件内容已有真实事务回归覆盖。 |
| R45 | 已完成 | 删除仅测试使用的参与者 new/retire/supersede 实现、NewTaskParticipant、对应两个错误与两个操作变体，以及未调用的 AgentConclusion::basis getter | BFS：CLI participant retire -> CollaborationService 事务（含过期、handoff、最后扫描者、等待收尾）是唯一生产退休实现；task reopen/revise_criteria -> TaskService epoch 迁移是唯一生产换代入口；CompletionService 仅通过 try_from_record 载入。全仓方法引用（包括函数值）、Serde/Clap 注册及所有测试复核无生产调用，原纯内存退休甚至允许生产规则禁止的 Concluded，不能保留第二套语义。Reducer 测试改用校验后的 record 构建 retired/superseded 状态，保留屏障/assurance 断言；真实退休与换代集成检查继续保留。仅剩一个调用的 active-task 校验并入 worker 校验。 |
| R46 | 已完成 | 删除退休查询中读取后丢弃的 label 和紧随其后的重复 Session 查询 | 当前事务已 JOIN agent_sessions，直接读取 last_seen_at；鉴权、stale 门限及错误路径不变。BFS 覆盖全部 retire_participant 调用和 stale/clean/handoff 集成检查。 |
| R47 | 已完成 | 合并 criteria 编辑与 Task reopen 的两套参与者 epoch 迁移 | `task::migrate_completion_participants` 唯一负责选取可迁移 Session、旧记录 supersede、创建下一 epoch 记录及计数；两个调用者及共享函数收窄为 Transaction 参数。BFS：CLI/GUI task lifecycle 与 criterion add/edit -> mutate/mutate_criterion -> 两条事务 -> task_participants 外键/唯一索引 -> Agent authenticate/Completion cohort；原邀请 rebound 与 revoke、plan_version 变化各属不同业务保留，未强行合并。 |
| R48 | 已完成 | 删除 TaskOperation -> 目标 phase/时间戳列 -> TaskOperation 的往返映射及重复参数 | BFS 覆盖 update_task_phase 四个调用；直接用既有 TaskOperation 选择固定 SQL 和错误详情，删除 operation_for_target。SQL、CAS 和期望状态不变，不添加可配置 SQL 列名。 |
| R49 | 已完成 | 删除 Workflow 独立的数据库 ID 错误解析 helper 及已集成结果缺事件时的 0 游标回退 | BFS：work integrate prepare/confirm -> 五处 ID 解码、load_preparation_by_request、load_integrated_result，以及 WorkService integration 的同事务事件 INSERT。复用 storage::parse_value，prepare 的 ChangeSet ID 在写入事务提交前完成解码。真正集成必然存在审计事件，缺事件现在明确 Database 错误；现有完整 Git 集成测试增加损坏状态检查，要求不把 preparation 误标 confirmed，随后恢复并完成真实确认/重放。 |
| R50 | 已完成 | 合并单个工作项提案与拆分子项的重复规格字段、标准化和 SQL 插入 | `WorkItemSpec`、`normalize_work_item_spec`、`insert_work_item` 为唯一实现。BFS：CLI JSON/typed overrides -> propose/propose_and_claim/split -> 工作项字段校验、能力/资源读取、split 继承/approval 及全部 Rust struct literal；连同 `hash_payload` 的隐式字节顺序依赖一并检查。采用 Serde flatten 保持原 JSON 字段和顺序，先对原实现建立字节级序列化检查再合并，避免破坏已存幂等 hash；parent/dedupe/dependencies 等请求上下文仍放在原字段位置。Rust 调用方把规格字段移入 item，未保留别名或转换适配器。 |

本批验证：R39–R43 完成后，60 秒全 workspace 回归 **184 通过、0 失败、0 忽略**，严格 Clippy 通过。R42 的四种损坏载荷均明确失败且不新增 Approval；后一个 child 状态变化时，前一个 child 的激活、父项变更、计划版本和 Approval 一并回滚。三组语义分叉回归均先复现旧路径失败再通过。

R37–R38 页面验证：隔离 Chromium 实际打开 `gui` 页面，测试专用 Tauri bridge 注入覆盖未知错误、带真实 operation_id 的错误，以及预检查 -> 确认发布 -> 后端错误。三场景通过；没有往产品增加 bridge 或 fallback。脚本 `/tmp/sessionbridge-cleanup-browser.mjs`，截图 `/tmp/sessionbridge-cleanup-ui.png`。此检查验证前端渲染与调用合同，不代替桌面 IPC 或非 Linux 运行验证。

保留依据补充：`workspace::resolve_git_instance_id` 的 directory identity 继承是“非 Git 工作区后来执行 git init”的当前活跃生命周期，已有真实状态文件与冲突检查测试；不是已淘汰版本格式转换，不能因变量名 legacy 就删除，否则会把原数据库切换为空状态。

`claim scope reconcile --from-git` 虽不读取 bool 字段，但 `#[arg(required = true)]` 由 Clap 消费，且操作目录把它列为显式来源约束；属于活跃宏校验，不按未读字段删除。

### R50 完成时的复验结果与未完成范围

- R31–R50 均已完成本批复验；最后一次预编译后的 `XDG_STATE_HOME=/tmp/sessionbridge-redundancy-audit-state timeout 60s cargo test --workspace --quiet` 为 **185 通过、0 失败、0 忽略**。
- `cargo check --workspace --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo machete`、`git diff --check` 均通过。前端 4 项单测、生产构建及前述 3 个真实浏览器场景通过；本轮隔离 Vite/Chromium 已停止。
- R50 的原 JSON 序列化字节检查在合并前、合并后均通过；R49 损坏的“已集成但无审计事件”不会再伪造 cursor=0 或改变 preparation 状态，恢复正常输入后真实确认/重放通过。
- 行为变化已记录：审批现在执行完整覆盖迁移、下游重连和变更集清理；损坏计划/状态不再假成功。Rust `ProposeWorkItemRequest` / `SplitChildInput` 的规格字段移到 `item`，JSON 字段及幂等字节合同保持；`agent candidates --explain/--limit` 和未使用的领域 API 已移除，无兼容别名。

**全仓目标仍未完成，不以本批通过代替完整审计。** 以下是 R50 时的待办，后续处理及最新范围见文末：

1. work/planning/collaboration 三处 Worker 写入资格与 Waiting -> Active 迁移：先核对鉴权快照、事务内版本校验和激活时机，再收敛同义规则。
2. await/operator 的行解码与 JSON 解析仍有 `InvalidQuery` 转换；请求 ID 的 128/200/256 字节、裁剪和 UUID 合同还需逐入口横向比对，不能静默缩小合法输入范围。
3. 输出 helper、邀请 prompt 可选字段、数值饱和与省略错误的路径，尚需完成活跃场景/宏入口证据核对；Workflow confirm 的正常/已集成/confirmed 分支也需继续对照上下文鉴权与重放边界。
4. 其余模块的字段读写、生产配置、静态资产选择器/动态加载和桌面 IPC/打包/非 Linux 条件编译尚未完成最终覆盖矩阵。现有测试证明本批修改，不证明全仓“同一业务，唯一实现”。

## 后续复核：写入资格与鉴权

接续判断：上一轮完成 R39–R50 并留下可运行检查和完整台账，属于有进展。当前工作树与该记录一致；保留静态 mockup 的独立改动。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R51 | 已完成 | 合并三套 Worker 写入资格和三套 Waiting 激活；删除规划内部的 activate_participant 控制参数 | L0 CLI/MCP/服务调用 -> L1 Planning 9 个写入口、Work claim/reclaim/artifact/evidence、Collaboration 14 个写入口，以及 Workflow prepare/review 和 propose_and_claim 组合入口 -> L2 authenticate_and_touch 的独立事务、业务事务、幂等 replay、require_planner/require_writable_participant/require_active_worker -> L3 TaskParticipant state/version/kind/epoch 与 Session ended_at、CAS 激活。统一为 AuthenticatedAgent 的 require_writable_worker/activate_waiting，角色/状态/版本规则仅一份；保留各业务的 phase 规则与 replay 先于新写资格校验。Work/Collaboration 现在也复核事务内的真实状态，防止鉴权后结束 Session 或切换 epoch/version 仍新建工作。确定性检查模拟三种鉴权后变化，均拒绝并无 Claim/Attempt；组合提案从 Waiting 激活一次，重放不重复递增版本。Superseded 的规划错误统一为 ParticipantRetired，不再使用兜底 ParticipantVersionChanged。 |
| R52 | 已完成 | 合并 Session 与令牌哈希的重复读取，并补齐唯一鉴权入口的 Workspace 归属约束 | BFS：所有 AgentService::authenticate 调用 -> workspace 身份读取 -> load_session 唯一调用 -> Session/Task 外键 -> token compare -> participant/touch；load_session 现在 JOIN Task 并按 context.workspace_id 限定，同一次读取返回仅内部使用的 hash，不加入公开 DTO。回归先使用合法的第二 Workspace/仓库身份配合同一 Session 复现错误通过 whoami，再验证拒绝且原上下文仍能完整 sync/workflow。初次测试夹具误用了不存在的 workspaces.updated_at，已按真实 schema 修正；该失败未计作漏洞复现。 |
| R53 | 已完成 | 消除集成确认只有正常写入分支执行鉴权的分叉 | BFS：CLI work integrate confirm -> Workflow::confirm_integration 的 prepared/pending、prepared/already-integrated、confirmed、expired 全部分支 -> load_preparation/load_integrated_result、WorkService::integrate_change_set/close 及 preparation 状态更新。入口统一复用 AgentService 鉴权，已确认重放和已集成恢复不再绕过 Token/Workspace 校验；底层 WorkService 仍独立验证其公开写入边界，未复制凭据规则。真实 Git 流程回归先复现伪造 Token 仍取得 confirmed 结果，再要求两个恢复分支拒绝并保持 prepared 状态。 |
| R54 | 已完成 | 统一高层 work review 与底层 review Claim/complete 的评审政策，删除高层重复 Evidence 校验及内部二次幂等记录 | BFS：CLI work review / claim review / review complete -> Workflow::review、WorkService::claim/complete_review -> 私有事务方法 -> work_items.review_policy/risk、execute Attempt、capability_grants、当前 epoch 执行者及 review_results；工具目录和全部结果/幂等 DTO 引用同步核查。唯一规则在 work::review_assurance，领取 Review Claim 和提交 ReviewResult 均按当前政策校验；实现者按 execute Claim 最新 generation 查找，先前 Review/Integrate Attempt 不再误作实现者，缺执行历史明确拒绝。既有 ReviewWorkflowResult 移到 work 作为共用结果，两个公开入口各记录其自己的唯一幂等结果，不再生成 :claim/:complete 子 key、子 hash 或隐藏子记录。回归先复现低层能绕过独立评审领取 Claim，再验证低层 Claim 拒绝、持有 Claim 后政策变更也拒绝 complete 且无半成品，合格 peer 的高层评审和重放继续通过。非法 Evidence 用合格 reviewer 检查，避免同时违反两项约束而依赖错误优先顺序。 |
| R55 | 已完成 | 删除运行时执行者身份退回 Session ID 的三处 COALESCE；身份解析和去重统一为 UUID | BFS：schema_v16 旧库填充 -> 全仓唯一生产 Session INSERT（Join）-> AgentSessionView/评审实现者/当前 epoch 执行者计数 -> 独立评审政策；核对所有测试造数及 source 字段序列化。生产创建显式写邀请 UUID，旧库迁移一次填充邀请或 Session UUID；运行时不应再次推断身份。AgentSessionView 的 Rust 字段改为 Uuid，JSON 仍是原 UUID 字符串；查询严格拒绝 NULL/非法 UUID。评审计数逐个解析到 BTreeSet，不用会静默忽略 NULL 的 COUNT(DISTINCT)。已有 NULL 的当前库明确报 Database，不补旧格式。测试在删除前复现自身身份被替换及其他执行者异常被掩盖；新增 v15 -> 最新的真实数据升级测试覆盖两种历史身份来源且已通过。当前 schema 的测试造数显式提供身份，历史 migration 测试保留旧列合同；未改任何历史 migration。 |
| R56 | 已完成 | 删除时间阈值/同步分页/邀请 epoch 的极值回退，以显式类型表达诊断的不限数量 | BFS：Agent sync/next、diagnostics -> load_participant_liveness/load_runtime_health_actions -> cutoff_micros 仅三个调用；复用已有 checked_duration_micros，溢出返回错误，不饱和到 i64 极值。MAX_SYNC_EVENTS 的唯一常量直接用于切片计数，取消两处不可能失败后的 1000 回退。诊断原来实际用 usize::MAX 触发转换回退，因此改为 Option<u32> 的 None 对应 SQLite LIMIT -1，Agent 传 Some(20)，未误删全量诊断能力。邀请 epoch 转换失败直接不匹配，不伪造 u64::MAX。已有阈值溢出/常规结果检查。 |
| R57 | 已完成 | 删除文本 JSON 渲染的 `<serialization failed>` 假成功路径 | BFS：全部 output::write_value 调用均来自 handlers；沿 CLI 输出格式 -> human_json / 专用文本 formatter -> write_text -> main 错误 envelope 追踪。human_json 和 write_value 的 formatter 回调统一返回 Result，实际序列化错误向上传播；JSON/JSONL 仍只执行各自渲染路径，未新增兼容回调适配器。复用元数据测试验证非法 RFC3339 时间在 Text/Table 中都失败；Rust write_value 调用方需返回 Result<String>。 |
| R58 | 已完成 | 合并邀请可发送性判定，删除 Show 的重复校验、单一调用批次构建的布尔配置、重复过期时间格式化和 prompt_sha256 的不可能 None 状态 | BFS：CLI prompt/invite show、Tauri prompt_show、create/rotate/recover/list/revoke -> manifest_for_row/batch_manifest_from_rows -> marker/secret 文件、版本和 task 状态；反向核对全部 prompt/prompt_sha256 字段消费者。ensure_sendable 的同一结果同时服务布尔元数据和正式发送校验，不再独立维护 is_sendable 谓词或反复读取密钥；audit 仍可读取不可发送历史，版本要求保留。哈希始终有 marker/rendered 来源，改为必填 String；prompt 在 metadata 模式实际缺省，仍保留 Option。CLI 三条取正文路径统一 write_invitation_prompt，缺正文明确报错，不输出空串假成功；新增缺正文检查。 |
| R59 | 已完成 | 删除邀请 marker/hash payload 的 shell 字符串往返适配器，以及创建请求摘要中的有损路径转换 | BFS：create/rotate -> BuildBatchSpec -> RequestMarker/CreatePayload -> Serde、幂等 payload hash、恢复/重渲染的两个 parse_shell 调用；统一直接使用既有 PromptShell，删除 shell_name/parse_shell，无别名或旧格式转换。posix/powershell 的 JSON 值和字段位置保持；CreatePayload 与 RotatePayload 均调用 path_text，不再对非法 UTF-8 路径生成替代摘要。新增 Unix 路径检查，要求在写入 Invitation 行前报 InvalidInput。 |

R51–R55 后的预编译与 60 秒全 workspace 回归为 **187 通过、0 失败、0 忽略**；单独的工作区错绑、评审政策、伪造确认 Token、NULL 执行者和 v15 身份迁移检查均已验证。

### R59 完成时的最终复验

- 预编译后 `XDG_STATE_HOME=/tmp/sessionbridge-redundancy-audit-state timeout 60s cargo test --workspace --quiet`：**190 通过、0 失败、0 忽略**。
- `cargo check --workspace --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo machete`、`git diff --check` 均通过。
- 本批未修改 GUI 页面或静态 mockup；Tauri Rust 命令与 DTO 经全 workspace 编译/链接验证。未把前轮浏览器模拟 IPC 的检查当成本轮桌面端到端验证。
- Rust 接口变化：`AgentSessionView.executor_identity` 改为 `Uuid`；`ReviewWorkflowResult` 移到 `work`；`InvitationManifest.prompt_sha256` 必填；liveness limit 使用 `Option<u32>`，output formatter 返回 `Result<String>`。有效 JSON 数据的已有字段和值保持，未新增兼容别名。

### 仍需完成，目标保持进行中

1. 请求 ID 的所有边界合同与重复校验：Session 写入为 128 字节/拒绝空白与控制字符，Invitation 为 256 字节/仅拒绝空串和控制字符，Await 为 200 字节，另有 Task/Chat 的裁剪和 Workflow/Speculative/ExternalWait 校验；完整比较后才能合并，不能静默缩小合法输入范围。
2. `await_service` / `operator` 的 ID、enum、JSON 行解码仍有 `InvalidQuery` 错误转换，以及其它同目的 JSON 解析 helper，需继续统一并保留字段诊断。
3. 剩余配置默认值、日志输入、仅内部/仅测试消费的 API 与前端错误状态，以及静态资产的选择器/动态加载引用，尚未完成全仓逐项裁决。
4. 最终需补齐覆盖每个模块和生产入口的完整调用/配置拓扑矩阵，完成桌面 IPC/打包及平台差异审查，并按原目标逐条复核；当前 190 项通过不证明全仓已无冗余。

## 请求边界与数据库解码收敛

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R60 | 已完成 | 合并请求 ID 校验，删除 invitation/await 的本地重复实现及 Task 的误命名 normalize helper | CLI flags/JSON、MCP、Tauri -> work/planning/collaboration/completion、invite、await、task、chat、workflow、speculative、external_wait -> 请求摘要与 SQL key 全部横向核对。唯一校验为 `RequestIdPolicy::validate`；SessionWrite 保留 128 字节/非空白/无控制字符，Invitation 保留 256 字节/非空串/无控制字符，Await 保留 200 字节/非空白，NonBlank 保留原无限长合同。Chat 在调用前裁剪；Task 仅检空白、保留原 key 与 None。Join/Launch 的 UUID 规范化是不同合同，仍用 `uuid_request_id`。没有给原先仅依赖 SQL 的入口新增范围限制。既有单测扩展为空白、首尾空格、控制字符、四组长度边界及多字节矩阵。 |
| R61 | 已完成 | 合并数据库 JSON/SQL 行解码，删除 InvalidQuery 吞错、Agent 字符串数组 parser 和 Work 的专用错误包装 | BFS 从 Tauri task_list/dashboard、CLI inspect/diagnostics/chat/await、Agent sync/next、幂等 replay、Planning/Completion/Invitation/ExternalWait/Speculative 读取，追到每个 SELECT 的物理字段或 JSON 投影；再反查所有 serde_json 解码与 SQL 写入。统一 `storage::{parse_value,parse_json,row_value,row_json}`；SQL 回调要求 rusqlite::Result，通过 FromSqlConversionFailure 装箱，公共错误 details 解包原字段/原因。CLI/文件/MCP/令牌输入不改为数据库错误。删除全仓生产 InvalidQuery 转换；Work 的 change_sets 投影复用既有 query_json_rows。Deliverable 原先 filter_map 丢弃坏 missing-change-set 项，现复用 MissingIntegratedChangeSet，损坏记录明确失败。真实 SQLite 单测验证行解码诊断，交付物回归验证 `[{}]` 不会跳过检查，四种坏 plan operation 检查明确 Database 且事务无副作用。 |

R60–R61 后预编译和 60 秒全 workspace 回归：**191 通过、0 失败、0 忽略**。数据库保存的 JSON 结构错误现在统一返回 Database，并携带 field/source；有效数据、真实可空字段与外部输入错误类别保持。投影使用投影名称而非虚构物理列名。新 helper 的装箱/downcast 是 SQLite 回调接口的隐式依赖，已由真实 query_row 经过公开 SessionBridgeError 的检查覆盖。

接续复核仍包括：日志输入/默认值、持久状态枚举、剩余 API 与配置/静态资产引用、桌面 IPC/打包/平台分支，以及最终完整拓扑矩阵。历史“仍需完成”列表按上述记录逐项推进，不将已完成项重复执行。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R62 | 已完成 | 删除日志环境变量错误时的降级与坏 JSON 的 unparsed 成功占位，合并剩余路径绝对化实现 | L0 main/Tauri run -> logging::init，CLI logs path/tail、Tauri logs_tail -> 日志目录、过滤器、JSONL -> README/protocol 与前端错误显示；保留文档明确的 unset 时 RUST_LOG 优先级和默认 info，但空值、非法过滤器、非 UTF-8 明确 InvalidInput。tail 解析失败返回带真实文件路径的 Io，不把坏行包装成功。logging::absolutize 与 app::absolute_path 同义，统一移到 private_files，避免基础设施反向依赖 app；原样拼接路径、不 canonicalize，保留请求摘要的路径字节。真实 CLI 子进程三项检查通过，覆盖两种环境变量的错误/合法选择、损坏日志、正常关联日志及不输出密钥；protocol 同步说明错误行为。 |

R61 严格 workspace Clippy 已通过。R62 的目标测试使用最新 `--no-run` 输出的 `logging_cli-7dddda5978820e07`，**3 通过**；较早同名旧二进制的单项通过不计作本次验证。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R63 | 已完成 | 删除两处未知 Await mode 自动变成 Any 的分支，统一持久枚举解析 | BFS：CLI value_enum、MCP schema/default -> register -> SQL mode CHECK -> reconcile_wait、finish_wait、load_wait、Agent active-wait summary 与 Operator dashboard；所有 mode 读取均检查。AwaitMode 移至 domain/model 并复用既有 string_enum，删除本地类型与手写 as_str，未留旧路径别名；JSON 仍是 all/any。真实 SQLite 回归注入未知 mode，读取/协调/完成三条路径均返回 Database/agent_waits.mode，恢复 all 后原原子唤醒流程通过。workspace 全部测试已预编译，目标等待检查通过；完整复验在后续汇总。 |
| R64 | 已完成 | 删除试验性工作的无摘要重放和伪造 checkpoint 成功，去掉缺少摘要时生成的通用 rationale | BFS：CLI work speculative 四入口 -> SpeculativeService -> 共享 Agent 鉴权、Git 解析、speculative_work 唯一键、checkpoint 覆盖 provenance、action proposal 与事件；反查创建事件与历史 schema，确认无法从旧行还原初始 provenance。start 复用公共 hash/replay/record，记录不可变创建请求摘要与资源 ID，重放仍读取最新 checkpoint。SQL 行、事件、幂等记录同事务提交。checkpoint 的所有权/活跃状态和受影响行数由一次 UPDATE 判定，不再为零行更新追加成功事件或复活 abandoned 记录；load 始终限定 Task。ready 状态缺 summary/head 明确 Database；只为可执行状态提供恢复动作。真实 Git 回归先复现改变 provenance 仍返回成功，再通过正常重放、他人 checkpoint 无事件、缺 summary、计划变更、放弃后不可复活及旧行无摘要的显式失败。 |

R64 历史数据限制：已有 speculative_work 行的 provenance 可能被 checkpoint 改写，不能安全补算初始摘要；未添加迁移占位或兼容分支。没有公共幂等记录的旧 start 请求再次提交时由已有 UNIQUE(session_id,request_id) 返回 Database，保留原行。新请求完整校验 payload；查询、checkpoint 与历史记录不需要伪造摘要。目标 Git 检查已通过。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R65 | 已完成 | 删除完成阻塞的强类型 -> JSON -> 动态字段/default 往返、枚举 JSON 往返和重复令牌解码；文件系统检查不再吞错 | BFS：TaskHealth::completion_warnings/push_signal（已保证 count>0）-> Agent next -> fingerprint SQL/classify -> Serde；直接复用 CompletionSignalWarning，删除重复筛选和 COMPLETION_BLOCKED 兜底、永远取不到的 entity_id 动态读取，投影在构造时填写真实分类结果。规范化 fingerprint 仍通过同一个 json! 路径生成，字段值保持；真实 SQLite 检查稳定窗口、cursor 和解除状态。TaskPhase/ParticipantState 用现有 as_str，删除 enum_text。两种 state token 复用同一个 signed_token 和常量时间整串比较，completion payload 只解码一次，删除必含分隔符后仍退空签名的分支。whoami/sync 对注册路径的读取错误保留路径返回 Io；无注册路径仍明确 false。软推荐模运算结果数学上必小于 usize 长度，删除不可能失败的转换/Option 降级。同步、伪造令牌、坏工作树和两项 fingerprint 检查共 4 项通过；workspace 预编译通过。 |
| R66 | 已完成 | 删除行动提案的重复请求写入和伪造 dedupe 结果，收紧全仓 INSERT OR IGNORE | BFS：CLI propose-action/completion-challenge/request-capability、Speculative adoption -> AgentService::propose_action -> proposals UNIQUE 与事件/公共幂等记录。请求重放现在使用同一 hash/replay/record，改参数明确拒绝，重放不再次写去重事件；语义 dedupe 使用原生指定唯一键并返回数据库的真实 kind/status，不把已接受提案描述为 proposed。所有剩余 INSERT OR IGNORE 逐表核对：Deliverable 关联、资源使用、Chat receipts 只处理其指定集合键；拆分产生的新子项覆盖与终态投递在同事务只创建一次，去掉不可恢复的忽略。后者重复调用在父状态检查处结束，不能吞掉异常 outbox 行。新增提案检查区分原请求重放与新请求的语义去重。 |

R60–R66 预编译后的 60 秒全 workspace 回归：**195 通过、0 失败、0 忽略**。全仓生产 Rust 已无 INSERT OR IGNORE。行动提案的新请求 ID 现在要求非空白；历史仅记录在事件中的请求没有完整载荷摘要，不作推测性回填。已有语义去重键仍按其原有实体唯一性生效。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R67 | 已完成 | 删除两种等待投递队列的未消费 JSON 副本，合并外部等待三处入队 SQL，消除 schema 版本与迁移列表的双重维护 | BFS：await/external 注册与终态事务 -> wait.result_json / delivery outbox -> MCP register/replay/poll -> write response -> acknowledge_delivery；全仓仅两个测试读取 outbox.payload_json，生产回复始终读取原等待记录，队列只管理 status/attempts/delivered_at。schema_v22 用原生 DROP COLUMN 删除两列，v1–v21 原文件保留；等待结果与投递状态不复制、不填默认。External enqueue_delivery 唯一负责从父记录取得 Task/Session 并插入 marker。LATEST_SCHEMA_VERSION 从唯一迁移脚本列表长度得到。v21 -> 最新真实升级测试保留原始结果、pending/delivered 状态、投递次数和外键；MCP 杀进程后重试、单次事件和已交付状态检查通过。 |

R67 需要既有工作区执行 `sessionbridge migrate` 升至 schema 22；现有迁移入口会先备份旧库，普通 open 不偷偷升级。此轮仅迁移测试临时库，未打开或升级用户工作区数据库。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R68 | 已完成 | 消除等待 poll 的免凭据分支和后台扫描的空 SessionContext，复用 Worker 写资格并显式拒绝超范围期限 | BFS：CLI await work-items/resume/external、MCP deferred calls -> 两个公开 poll；daemon scan_due -> 私有 poll_wait；register -> 同一 AuthenticatedAgent::require_writable_worker。公开 poll 复用唯一 authenticate 的既有不更新时间模式，仍保留循环心跳；系统扫描从 SQL 读取三个强类型身份，直接调用内部观测逻辑，不再伪造 token/path/protocol 字段。注册拒绝 Observer，复核 epoch/version/ended；原 Active 限制保留。用户传入的 i64 极大期限通过 time::checked_add 报 InvalidInput，库内坏复查间隔报带字段的 Database，不 panic、不饱和。真实状态检查覆盖活跃及已完成等待的伪造 Token、Await 入口鉴权、Observer、输入溢出、损坏库值及系统扫描正常唤醒。 |

R67–R68 后预编译和 60 秒全 workspace 回归：**196 通过、0 失败、0 忽略**；包括 v21 数据升级和 MCP 进程恢复。后端代码复验完成，后续继续收拢前端错误/状态与配置拓扑。

## 前端、桌面与最终入口复核

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R69 | 已完成 | 删除前端把错误当空数据的显示、重复异步状态、跨 Task 数据引用和写死的默认 Profile | API invoke -> QueryClient -> tasks/dashboard/diagnostics/prompts/transition 全部消费者逐项核对。失败有 ErrorPanel/ErrorCard，不显示“还没有任务”或绿色同步状态；目录选择器拒绝被同一错误边界接住，取消仍是正常 null。Inspector 直接读 Dashboard 的 Task；Chat/Inspector 以 Task key 隔离状态。Chat、Launch 和 Prompt 复用 Query/Mutation 数据与错误，删除本地副本；预检关闭后迟到结果不能恢复旧状态，邀请 A 的慢响应不能覆盖 B。预检期间输入固定，Profile 未填写时由项目配置选择，0 不再显示成空值。全部请求为本地 IPC，因此 QueryClient 不受浏览器联网状态的额外门控。12 个 Chromium 场景通过，1380/1024/390 页面级溢出均为 0；原生发布版证明自定义默认 profile 生效。 |
| R70 | 已完成 | 统一 Operator 与服务的 JSON DTO 类型 | dashboard speculative assumptions 改为 Vec<String>，external detector 复用 ExternalDetector；row_json 因此同时验证结构，不再让坏 JSON 对象到前端 .join/path 消费时崩溃。所有 Operator 字段消费、service DTO 和 SQL 读写已对照；有效 JSON 字段/值保持。workspace Clippy、生产构建及真实 task_snapshot IPC 已通过。 |
| R71 | 已完成 | 删除仅单测使用的 TaskParticipant::wait、WaitReadiness、专用错误和操作变体 | 反查所有 .wait 的接收者；生产是 AwaitService/ExternalWaitService，其他同名调用是 Child/Barrier，不是领域方法。删除完成服务中的不可达错误映射；is_clear_for_conclusion 直接表达五个当前条件。测试改为从真实等待记录载入后验证 resume，Observer 测试保留 conclude，活跃工作禁止变更用生产 leave 操作检查；durable 等待/Observer/Token 的事务检查仍保留。最终 196 项 workspace 回归和严格 Clippy 通过。 |
| R72 | 已完成 | 统一两种弹窗的焦点恢复 | 真实 WebKit 发布版复现 Escape 可关闭但焦点未返回按钮；Radix 的默认恢复依赖 Trigger，而现有入口由外部按钮控制。第一版只在 open autofocus 记录，Prompt 已通过原生复验，但补充 Chromium 键盘检查复现 Launch 的 input autoFocus 提前移动焦点，导致该事件不触发。共用 ModalContent 改在子控件挂载前保存触发元素，close autofocus 时恢复，保留标题输入的初始焦点以及 Content 原生 props/ref、焦点约束和关闭行为；不新增第二套对话框库。最终 deb 包中两种弹窗的原生焦点检查、14 IPC 和 12 个 Chromium 场景均通过。 |
| R73 | 已完成 | 删除未被消费的 nextest 配置及 Vite 环境变量前缀 | BFS：核对 CI、npm/Rust 脚本、SDK 与前端环境变量引用。删除根 nextest.toml 和 TAURI_ENV_ 的 envPrefix，不影响 Tauri 构建 hook。当前 CI 使用预编译后的 60 秒 cargo test；PROJECT_PLAN 明确区分历史选型与当前配置。构建及 4 项前端单测通过。 |
| R74 | 已完成 | 删除 Git common-dir 的无用重试及等待建议中的不可能空引用 | Workspace 已识别 Git worktree；GitRepository 也使用 --path-format=absolute，改参数重试不是有效兼容路径，失败现在直接保持明确错误。Agent 已知依赖集合非空时，用一次 Some 匹配取得首项，删除重复 first 查询和 expand_ref 空串替代；有效建议保持。最终 196 项 workspace 回归和严格 Clippy 通过。 |
| R75 | 已完成 | 明确 Linux 打包图标的配置消费边 | BFS：tauri.conf.json -> Tauri bundler -> AppImage 图标选择与 deb desktop/icon 安装路径；codegen 的默认窗口图标不等于 bundle 图标。原配置未绑定 bundle.icon，AppImage 因无方形图标失败，旧 deb 也缺 launcher icon。直接引用现有 512×512 icons/icon.png，不新增图片或图标解析分支。两种包已构建，包内 desktop/icon 匹配，AppImage 真实运行通过；后续打包工具问题见 R81。 |

桌面第一轮：本地 release binary + WebKitWebDriver 完成全部 **14 个已注册 IPC**；隔离临时目录验证了空工作区、初始化、持久偏好、只有 audit profile 的项目默认、发布/两份真实邀请、prompt、群聊、快照、暂停/恢复、两种诊断和日志。临时把 schema 标为旧版时显示真实 SCHEMA_UPGRADE_REQUIRED，未出现空任务成功态；恢复版本后恢复同步。报告 `/tmp/sessionbridge-native-audit-nc4qimyt/report.json`。该轮也明确记录焦点恢复失败，随后按 R72 修复，不把 IPC 成功等同于键盘行为通过。

Chromium 验证脚本 `/tmp/sessionbridge-cleanup-browser.mjs`；独立只读审查覆盖 7 张截图。审查发现 fixture 漏了 required_deliverables_open，已补全测试数据并重跑 12 场景，未给产品增加 NaN 默认值。此处的桥接注入只存在于测试浏览器，真实桌面检查直接使用原生 IPC。

构建过程：首次 deb 发布包已构建；后续 AppImage 用户缓存写入被沙箱只读策略阻止，已按权限流程重试。旧 linuxdeploy 自带 strip 不支持本机库的 .relr.dyn；核对同版本上游源码后，仅在打包环境设置 NO_STRIP=1，Rust release 自身仍按 Cargo profile 剥离符号。随后 GTK 插件遇到本机 GdkPixbuf 2.44.7 已移除的旧 loader 目录；未改系统库或添加运行时兼容分支，使用隔离 Debian 12 环境继续验证。未修改用户工作区数据库，也未安装或对外发布包。

## 最终验证（2026-09-22）

| 范围 | 当前证据 |
| --- | --- |
| Rust | 预编译后的 60 秒全 workspace 回归正常退出，196 通过、0 失败、0 忽略，含 doc tests 收尾；日志 /tmp/sessionbridge-final-tests-verified.log。前一次在全部业务测试通过后超时，未计为最终通过。 |
| 静态检查 | cargo fmt --all -- --check、cargo clippy --workspace --all-targets -- -D warnings、git diff --check 通过；依赖未变，既有 cargo machete 检查仍适用。 |
| 前端 | 4 项单测与 tsc -b / Vite 生产构建通过；最终脚本为 assets/index-JwzCSN7x.js，CSS 保持 index-BTiycvb1.css。 |
| Chromium | 最新 12 场景通过，新增 Launch 初始标题焦点及 Escape 后恢复焦点检查；失败、竞态、离线本地 IPC、图节点/边和页面溢出均已复验。 |
| 真实 deb | 从最终 ar 包提取执行文件，用 WebKitWebDriver 验证全部 14 IPC、schema 错误及恢复、两种弹窗焦点、1024 最小宽度；报告 /tmp/sessionbridge-native-audit-bujbs1eu/report.json。包内脚本路径与最终 dist 一致；desktop 的 Icon=sessionbridge-gui 对应包内 512×512 PNG。 |
| 拓扑清单 | 当前文件核对覆盖 30 个 app 模块；14 个 IPC 在 Rust 注册、前端 invoke 和文档逐项一致；9 个 MCP 工具及 22 个 include_str 迁移脚本无遗漏。测试清单不代替表内逐层语义追踪。 |
| 源码规模 | 当前 src、gui/src、gui/src-tauri/src 的 53 个已跟踪变更文件，加 3 个新增共享/迁移文件，净减少 1119 行；此统计含模块内单测，不含独立集成测试、文档、锁文件或用户的 ui-mockups 改动。行数仅描述 diff，不作为正确性证明。 |

本机 deb：`target/release/bundle/deb/SessionBridge_0.1.0_amd64.deb`，SHA-256 `a1746cdace45baeeb262a3c0cd83fff8442da521c960b675353c80a915e20052`。一次临时副本的 WebDriver 会话因连接断开中止，未产出验收结果；最终包独立会话完整通过，验证器没有添加自动重试来掩盖此失败。

## 原始要求的逐项验收依据

| 要求 | 对应证据 |
| --- | --- |
| 删除前广度优先追踪全部入口与隐式依赖 | CALL_TOPOLOGY 的 CLI、app 模块、14 IPC、9 MCP、trait/宏、SQL、文件生命周期和资源矩阵；各 R 项记录修改前的调用层次、反向消费者和边界裁决。 |
| 优先删除大块死代码与旧实现 | invitation/output、domain/time_serde、无消费 nextest 配置，以及领域旧生命周期、CLI 映射、批次包装等已删除；所有剩余入口通过当前源码编译和真实调用。 |
| 去掉伪安全回退，不增加旧格式兼容或成功占位 | R21、R28、R32–R33、R38、R42、R49、R55–R59、R61–R69、R74 的损坏输入、凭据、数据库、文件、输出和 UI 错误检查；保留的缺省业务输入、故障恢复和历史迁移均有明确的当前消费者。 |
| 同一业务收敛到唯一实现 | 共享私有文件写入、storage 解码、请求 ID 政策、事件写入、completion revision/epoch 迁移、Worker 资格与激活、直接/审批规划变更、评审政策、请求幂等和邀请可发送性；原转发别名与分叉策略已移除。 |
| 每项优化可追溯，避免重复劳动 | 早期变更表与 R19–R75 均记录位置、覆盖范围、行为影响和验证；阶段性待办按日期保留，当前结果以本节前的最终验证为准。 |

交付边界：现有工作区必须显式执行 `sessionbridge migrate` 到 schema 22，迁移入口先备份；本次仅使用临时数据库。缺失初始摘要的历史 speculative start 请求不能安全重放，未推测回填。已移除的 Rust 导出、结构体字段/参数及无效 CLI 参数须按对应记录更新调用方，未保留兼容别名。运行实测限 Linux；Windows、macOS、mobile 和 PowerShell 的条件源码/字符串合同已经核对，未宣称相应平台运行通过。未提交、安装或对外发布。

## 接续复核（2026-09-22）

当前工作树保留上述变更；接续环境中此前 `/tmp` 测试报告与隔离打包环境已经不存在，历史记录不能充当当前可复查报告。AppImage 目录只有未完成的 AppDir，没有最终包；R75 仍需验证。上一阶段有实际代码、拓扑及台账，属于有进展，以下从现状继续而不重复已有清理。

| 编号 | 状态 | 优化 | BFS 证据与验证 |
| --- | --- | --- | --- |
| R76 | 已完成 | 删除邀请摘要的两套手写十六进制循环，统一使用已安装 `sha2` 的 `LowerHex` | L0 CLI/GUI launch、invite create/rotate/recover/show、agent join -> L1 PromptRenderer 与 InvitationService marker/产物校验、InviteTokenHash 的 DB 哈希/Debug -> L2 `sha256_hex`、`to_hex`、全部 SHA256/`:02x` 调用 -> L3 marker JSON、token BLOB、prompt 字符串与邀请恢复测试。复用仓库其他摘要已采用的标准格式化，不引入新工具层或依赖；保留小写、64 字符和前导零合同，新增 SHA256 固定向量及原始摘要编码检查。预编译后的 60 秒全 workspace 回归 197 通过、0 失败。 |
| R77 | 已完成 | 合并 CLI 两套参与者列表，并统一 heartbeat stale 的领域状态集合与判定 | BFS：Clap `agent list`/`participant list` -> 同一列表 handler/SQL；CLI/MCP Agent sync/next、Task health/Operator、CompletionService -> participant state、session last_seen/ended -> CompletionCohort 验证；diagnostics/Agent runtime advice -> liveness SQL；所有 Serde 输出和 schema 约束；PROJECT_PLAN 的 heartbeat 规则。`ParticipantState::HEARTBEAT_STATES/is_stale` 是唯一状态政策，SQL 绑定同一枚举集合。失联仅适用于未结束 Session 的 active/waiting/blocked；CLI 不再把 superseded 等历史状态误标为失联，同步与完成投影不再把 ended Session 误标。departed 的 30 分钟退休建议保留，10–30 分钟不再冒充 heartbeat stale。ParticipantRecordView.state 直接解码为领域类型，JSON 字符串不变；没有旧类型别名。修改前已复现 CLI 返回 2 个失联参与者而预期 1 个；夹具的签名、唯一 token、版本和 epoch 问题均已修正，不计为业务复现。当前跨入口回归和状态/时间边界检查通过。 |
| R78 | 已完成 | 删除 CLI 的 ReviseContractPayload 与重复 compatibility 默认规则 | BFS：Clap contract revise -> 文件/stdin JSON、位置参数 contract ID、全局 request ID -> ReviseContractRequest -> service 规范化/hash/replay/写入；全部 6 个旧 read_json_with_request_id 消费者和 Serde 字段合同。既有边界解析器扩展为 read_json_with_fields，真实使用者均直接构造各自服务请求，删除中间 DTO 和旧函数名。CLI 参数仍覆盖文件中的身份字段；缺省 compatibility 只由服务 DTO 决定，缺失 content 或非法类型仍失败。对象边界、CLI 优先级、服务默认值与坏字段检查通过。 |
| R79 | 已完成 | 合并 Planning/Collaboration 的重复文本校验和两种有序字符串集合实现 | BFS：两服务全部 required_text/canonical_enum/normalize_string_set/normalize_strings 调用 -> 请求规范化 -> JSON hash、幂等与数据库写入；前端/宏没有绕过这些函数的注册。共同约束统一到私有 app::validation，集合采用 Vec 原地排序/去重，删除 Planning 的额外 BTreeSet 构造与字段重复校验。排序、trim、大小写保留和有效请求 hash 不变；错误统一携带字段名及明确原因，仍为 InvalidInput。Planning 的 Unicode 小写（也用于自由 dedupe key）与 Collaboration 的 ASCII 小写是不同现存合同，保留原生字符串操作，不强行收窄/放宽输入；Task/CLI 的保序列表和允许段落控制字符规则也不合并为集合。空白、控制字符、Unicode、去重与顺序检查通过。 |
| R80 | 已完成 | 删除 inline prompt 的 JSON -> UTF-8 字节 -> 字符串往返与不可能的 UTF-8 失败分支 | BFS：唯一生产 Inline 构造点 handlers::emit_inline_invitation -> PromptRenderer::render_join_command -> InviteSecret::to_json_bytes/Serde -> POSIX/PowerShell stdin 片段；File 模式和持久化 secret 文件输出独立核对。Serde 直接 to_string，保持原紧凑 JSON 内容和 shell 分隔换行，不增加新格式。现有 inline 回归补充从真实 heredoc 解码并还原全部 secret 字段；邀请提示词 7 项检查与随后 201 项 workspace 回归通过。 |
| R81 | 已完成 | 更新过期打包工具，移除 AppImage 中与宿主 Mesa 冲突的 Wayland client 副本 | BFS：Tauri bundle -> linuxdeploy/GTK plugin -> AppDir/ELF 搜索路径 -> 宿主 EGL/Mesa/Wayland 的动态符号。旧缓存 linuxdeploy 659c9db（2024-07-26）仍打包旧 libwayland-client.so.0；ldd -r 显示宿主 Mesa 缺少 wl_fixes_interface 等三个符号，eglinfo 对照为宿主成功、使用 AppDir 库失败。官方当前 excludelist 已明确排除此库；更新到官方 07333c6（2026-09-01）后默认打包自动移除副本，EGL 和最终 AppImage 全部 14 IPC/六页/焦点/错误恢复通过。没有修改渲染模式、添加运行时探测或 fallback。已更新本机 Tauri 的 linuxdeploy 缓存，原文件备份于 target/cleanup-verification/linuxdeploy-2024.AppImage；新工具 SHA-256 为 36a2d7e274d12e1050d0e9ecfe11d339ed54720b2bec464c286d53f8b07f5c62。 |
| R82 | 已完成 | 删除 Agent 集成预检把数据库错误伪装为资源冲突建议的回退，合并变更集的两次独立选择 | BFS：CLI/MCP agent next/preview -> next_internal -> load_stage_actions -> integration_resource_conflicts -> change_sets.changed_resources_json / resource_reservations；反查 submit/reopen/integrate 和 schema NOT NULL。使用单次关联查询同时读取同一 ChangeSet ID 与资源字段；同时间戳以 ID 稳定排序，不再各自选择一行。持久 JSON/SQL 错误沿 Result 返回，仍保留真正资源冲突的建议。真实 claim/submit/peer review 流程回归先复现坏 JSON 得到成功 next；修复后返回 Database 与原字段诊断，恢复数据后正常建议与完整集成通过。 |
| R83 | 已完成 | 删除 completion 重复的最新事件 SQL、重复游标选择及 MCP 缺失方法的空字符串占位 | BFS：Completion conclude 的已有结论分支 -> reconcile（最后事件为 task.completed，后续仅 SQL 写入且 schema 无事件 INSERT trigger）-> latest_cursor；完成记录 basis cursor 使用同一 events::latest_cursor。已有结论的响应始终使用实际最新游标，不再再次从 reconcile/同一游标二选一。Agent 事件分页的 nullable MAX 用于识别空流，合同不同而保留。MCP method 直接匹配 Option，保留未知/缺失方法的既有失败语义，删除空字符串中间值；9 个工具和通知路由均核对。Completion、Work 和 MCP 进程共 21 项目标回归通过。 |

## 接续后的最终验收（2026-09-22）

本节是当前工作树的验收结果，取代前文阶段性待办和旧包哈希。源码与配置已停止修改后再执行下列检查；打包快照的 157 个输入文件与当前工作树逐文件 SHA-256 一致。

| 范围 | 结果与可复查证据 |
| --- | --- |
| Rust 全量 | 单独预编译后，`XDG_STATE_HOME=/tmp/sessionbridge-cleanup-state timeout 60s cargo test --workspace --quiet` 正常退出；**201 通过、0 失败、0 忽略**，含 29 组单测、集成与 doc-test 结果。`target/cleanup-verification/rust-compile.log`、`rust-tests.log` |
| 静态检查 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo machete`、`git diff --check` 全部通过。Clippy 日志为 `target/cleanup-verification/clippy.log` |
| 生产前端 | 4 项 Vitest 单测通过；`npm run build` 的 TypeScript/Vite 检查通过。当前资源仍为 `index-JwzCSN7x.js`、`index-BTiycvb1.css`，接续阶段未修改前端代码或依赖 |
| Chromium | 11 组场景通过：空/错误状态、目录选择失败、带 operation_id 的发布错误、迟到预检、提示词乱序响应/复制、离线本地 IPC、跨 Task 草稿隔离、筛选后的图节点/边，以及主题/减少动画。六页在 1380、1024、390 的 18 个页面级横向溢出检查全部为 0。`target/cleanup-verification/browser/report.json`；此处 IPC 为测试注入，基础数据来自真实桌面返回 |
| 最终 AppImage | 使用默认 strip 和默认 FUSE 启动方式，无软件渲染或 GDK 降级环境；实际包内执行全部 **14 个 IPC**、六页 1380/1024、两个弹窗初始/返回焦点、旧 schema 明确报错及恢复。`target/cleanup-verification/native-appimage-delivery/report.json` |
| 最终 deb | 从 deb 的 data.tar.gz 提取执行文件，独立临时配置/数据库重跑同一完整原生流程，全部通过。最终重新打包后的 3 个运行文件内容及权限与已验证提取件逐一相等；见 `target/cleanup-verification/native-deb-verified/report.json` 和 `deb-content-identity.json` |
| 图标与工具 | 两种包的 desktop Icon 都对应 512×512 PNG，包内 PNG 与源图哈希一致；AppDir 不再含 libwayland-client.so.0。旧/新打包环境的 EGL 对照和真实启动证明 R81 修复，不依赖渲染回退 |
| 独立渲染审查 | 独立只读审查检查了 17 张真实 WebKit 截图，未发现本次清理造成的显示矛盾或遮挡。截图不替代行为测试；1024 下 Inspector 隐藏是原有 CSS 行为，不宣称该宽度所有按钮都已验收 |
| 入口与资产 | 62 个生产 Rust 文件；30 个 app 模块与实际文件/注册相等；14 个 IPC、9 个 MCP 工具、22 个迁移脚本匹配。静态预览 12 个 HTML、117 条本地引用均无缺失。`registration-coverage.json`、`symbol-candidates.json`；词法候选检查只作 BFS 的补充 |
| 源码规模 | `src`、`gui/src`、`gui/src-tauri/src` 的 54 个已跟踪变更文件和 4 个新增共享/迁移文件，净减少 **1098 行**。包含模块内单测，不包含独立集成测试、文档、锁文件、构建产物或用户独立的 ui-mockups 修改 |

最终可执行包已放在标准输出目录：

| 包 | SHA-256 |
| --- | --- |
| `target/release/bundle/appimage/SessionBridge_0.1.0_amd64.AppImage` | `09f42a14647346621aa432ab32ba2a421fcce650657e77f840f85fe2b1c975cf` |
| `target/release/bundle/deb/SessionBridge_0.1.0_amd64.deb` | `3e98a6083c008039d18b4cba308d8e2167eff1e730401cca513e5c40f45591f2` |

原始报告、截图、包和验证脚本保存在 `target/cleanup-verification/`；`verification-summary.json` 汇总当前哈希与统计。复验命令为 `python3 target/cleanup-verification/browser_check.py`，以及 `python3 target/cleanup-verification/native_check.py --binary <包或提取后的执行文件> --output <新的报告目录>`。本地前端脚本需要现有 Python Playwright/Chromium，原生脚本使用现有 tauri-driver/WebKitWebDriver，未向产品增加验证依赖。

隔离 Debian 打包使用既有 `sessionbridge-packaging:bookworm` 镜像、Rust 1.95、锁定的离线 Cargo 依赖及当前已构建前端。更新工具后确认不再需要早期的 NO_STRIP 设置，最终通过 `env -u NO_STRIP` 恢复打包工具默认 strip，构建和原生复验均通过；没有写入产品配置。无网络容器所需的官方 AppImage runtime 显式预下载并经 LDAI_RUNTIME_FILE 提供，SHA-256 为 `1cc49bcf1e2ccd593c379adb17c9f85a36d619088296504de95b1d06215aebbf`。源码构建日志为 `bookworm-verified-build.log`，最终默认打包日志为 `bookworm-standard-bundle.log`，输入清单为 `bookworm-source-sha256.json`。

失败记录保留：旧 linuxdeploy 的 Wayland/EGL 失败见 `appimage-egl.log`、`egl-loader.log` 和 `native-appimage/report.json`；第一轮原生验证器误把应用 error 对象当成 WebDriver 协议错误，修正后完成了真实 schema 错误检查。启动阶段只轮询驱动的 ready 状态，业务 IPC 不自动重试。上述失败均未计作通过。

原始要求已按当前证据闭环：BFS 拓扑覆盖生产入口与实际隐式依赖；已确认死模块、旧映射/适配层、错误回退和业务分叉均已删除或收敛；R19–R83 与前置清单逐项留有覆盖、变更和验证记录。保留的多协议输入/投影、历史迁移、私有文件恢复、测试时钟和缺省业务值均有明确消费者与不同合同，未为凑删除量去掉有效路径。

交付注意：现有库必须显式迁移至 schema 22；已删除的 Rust API/无效 CLI 参数，以及没有初始摘要的旧 speculative start 重放限制，按各记录处理，未增加兼容别名。运行实测为 Linux；其他平台只核对条件源码与合同。没有提交、安装、对外发布，也未迁移用户的真实工作区数据库。所有本轮测试进程和隔离容器均已结束。
