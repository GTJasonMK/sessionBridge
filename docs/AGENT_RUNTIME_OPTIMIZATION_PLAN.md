# Agent Runtime Optimization Plan

> Implementation status (2026-07-19): Phases 1-4 are implemented. The runtime uses `agent next` for compact reads and explicit `agent sync --full` with signed snapshot baselines for complete state. It includes task/participant/completion/WorkItem/Claim scoped tokens, Planner provenance metadata, capability/tool discovery, completion challenges, capability requests, proposal resolution, facade dry-run planning, explicit WorkItem review policies, server-derived executor identity, transient resource semantics, checkpoint lease renewal, stage handoffs, durable Agent-owned/external waits, MCP crash replay, standalone daemon, Evidence/Deliverable gates, blocking fingerprints, proposal consolidation, speculative work, health signals, and layered GUI status. Production outcome targets are measured from structured command logs during rollout.

> Collaboration visibility update (2026-07-18): Task workgroup chat is implemented for Agent CLI, MCP, and GUI. Chat messages share the immutable Event cursor and are intentionally non-authoritative: they expose intent and progress without replacing Claims, handoffs, approvals, Evidence, or completion gates.

> Reconciliation and contract update (2026-07-19): schema v21 adds persistent target-branch integration integrity and system-managed Invalidations. `TaskReconciler` is the shared reducer for Git drift, Requirement coverage, Deliverable verification, and completion-relevant revision changes. The runtime now includes explicit `work adopt`, batch `claim scope reconcile --from-git`, typed/automatic-scope conclude for CLI and MCP, read-only `agent next --preview`, 10/30 minute liveness thresholds, advisory soft scheduling, exact awaitable dependency ownership, executable Operation Contracts, exact subcommand logging dimensions, and shared CLI/GUI collaboration diagnostics.

## 1. 背景与结论

SessionBridge 当前已经具备严格的协作领域模型，包括 Task、Deliverable、WorkItem、Claim、Attempt、Evidence、Review、ChangeSet、ResourceReservation、Wait 和 Completion。主要问题不是这些机制本身，而是它们几乎被直接暴露为 Agent 的日常 CLI 协议。

运行日志显示，Agent 将大量模型轮次消耗在协议编排和输入修正上，而不是编码任务本身：

- 单日出现 135 次 `agent sync`、45 次 `agent conclude`、40 次 `item claim` 和 33 次 coordination 调用。
- 失败中有 62 次 `INVALID_INPUT`，主要来自缺失字段、JSON 类型错误和手工复制版本参数。
- 一个 WorkItem 的实现、提交、审查和集成通常需要十余次独立 CLI 往返。
- 曾出现所有 Worker waiting、没有 active Claim、仍有 pending review/integration 的全员等待死锁。
- blocked audit 依赖 Agent 连续多个模型回合确认相同阻塞，进一步制造无效 token 消耗。

因此，优化目标不是删除严格领域模型，也不是用更长的 Prompt 约束 Agent，而是在领域层之上增加面向 Agent 的协作运行时。

核心边界：

> SessionBridge 负责压缩状态、计算合法动作、维护责任链、执行机械协议和阻止错误；Agent 负责理解代码、发现工作、选择技术方案并判断语义完成。

## 2. 目标架构

系统划分为四层：

1. **严格领域层**：保留 SQLite、事件审计、Deliverable、WorkItem、Claim fence/lease、Evidence、Review、ChangeSet、Completion epoch/revision 等现有机制。
2. **Action Planner**：根据领域状态计算合同 `obligations`、已知 `eligible_actions` 和硬不变量产生的 `forbidden_actions`，但不替 Agent 选择技术方案，也不宣称候选集完备。
3. **Agent Facade**：提供少量高层复合操作和作用域状态 token，减少 CLI 次数、手写 JSON 和 CAS 参数复制。
4. **Durable Runtime**：负责 deferred wait、持久化 outbox、断线重连、结果重放和幂等回调。

Agent 的推荐快捷路径使用以下主接口：

```text
agent next
work start
work submit
work review
work integrate
agent await
agent conclude
```

现有细粒度命令和领域读取接口必须继续可用，作为高级规划、创新协作、人工运维和复杂恢复的能力逃生口。Facade 是降低机械成本的快捷路径，不是限制 Agent 行为的封闭工作流。

### 2.1 Agent 自治原则

所有优化必须遵守以下原则：

- Planner 输出是建议和已知合法动作，不宣称穷举所有有价值的下一步。
- Agent 可以发现文档之外的问题、提出新 Deliverable/Requirement/WorkItem、拆分或重构计划、挑战已有假设，并请求扩大能力或资源范围。
- 服务端只有在保护安全、数据完整性、资源互斥、权限边界和完成合同等硬不变量时才能拒绝动作。
- 优先级、技术路线、探索顺序、任务拆分、工具选择和语义完成判断不得被静态规则固化。
- 未被 Facade 建模的新动作可以通过通用 proposal/coordination 接口表达；不得因为 Planner 尚不认识某种动作就迫使 Agent 停止。
- 高自治策略下，Agent 可以直接使用细粒度领域接口；低自治策略通过 approval 控制副作用，而不是隐藏信息或削弱分析能力。

## 3. 关键设计

### 3.1 Action Planner，而非中央调度器

新增：

```bash
sessionbridge agent next --context <CONTEXT> --format json
```

返回有序的已知动作集合，而不是唯一强制动作或封闭白名单：

```json
{
  "snapshot_id": "...",
  "next_cursor": 318,
  "tokens": {
    "task_plan": "...",
    "completion": "...",
    "participant": "..."
  },
  "obligations": [],
  "eligible_actions": [
    {
      "action": "claim_review",
      "work_item_id": "...",
      "priority": 90,
      "reason": "submitted item has no reviewer",
      "conflict_free": true
    }
  ],
  "forbidden_actions": [
    {
      "action": "await",
      "reason": "unowned executable work remains"
    }
  ],
  "completion_eligible": false,
  "completion_blockers": []
}
```

Planner 可以可靠判断已知动作是否合法、是否冲突、是否存在无人负责的阶段和是否满足机械完成条件；代码级优先级、任务拆分和技术方案仍由 Agent 决定。

`obligations` 只表示已有合同产生的定向责任，例如必须响应的 handoff 或已持有 Claim 的续租，不能用于向 Agent 下达新的技术方案。`eligible_actions` 必须携带 `advisory=true` 和 `planner_complete=false`，明确允许 Agent 提出列表之外的新工作。

新增通用扩展动作：

```bash
sessionbridge agent propose-action --kind <KIND> --input <FILE>
```

常用的结构化自治入口为：

```bash
sessionbridge agent completion-challenge --input <FILE>
sessionbridge agent request-capability --input <FILE>
sessionbridge agent proposal resolve <PROPOSAL> --decision <accepted|rejected|superseded> --reason <TEXT>
```

它允许 Agent 表达 Planner 尚未建模的任务拆分、调查、验证、协作方式或工具需求。服务端对其应用相同的资源、权限和审计规则，然后将可执行结果纳入共享状态。未知 `kind` 不应直接丢弃，应保存为可审计 proposal，并在需要时进入 capability/approval 流程；未知动作绝不能因为“开放自治”而被自动解释或直接执行外部副作用。

### 3.2 作用域状态 token

隐藏 Agent 当前需要手工复制的版本字段，但不使用一个脆弱的全局 token。token 至少按以下 scope 划分：

```text
task_plan
completion
participant
work_item:<ID>
claim:<ID>
```

服务端只验证操作实际依赖的 scope：

- checkpoint 验证 Claim fence 和对应 WorkItem。
- review 验证目标 WorkItem 和 review policy。
- integrate 验证 ChangeSet、目标分支和 integration lease。
- conclude 验证 completion scope、Participant 和最新全局扫描。

无关 Agent 的事件不能使局部操作失效。错误响应应返回失效 scope 和最新 token，而不是强制完整 sync。

### 3.3 快照与增量同步

`agent next` 默认返回紧凑状态。完整状态改为显式操作：

```bash
sessionbridge agent sync --full
```

增量响应必须携带：

- `snapshot_id`
- `base_cursor` 和 `next_cursor`
- Task phase
- Participant state
- 当前 owned Claim
- 当前 active wait
- directed requests
- completion blockers
- 状态变化和候选动作

快照基线缺失、cursor 过期、进程重启或协议版本变化时返回 `FULL_RESYNC_REQUIRED`。不能在无法证明基线有效时继续提供不完整 delta。

### 3.4 复合操作

新增 Agent Facade：

```text
work start
work submit
work review
work integrate
```

`work submit` 使用一个 bundle 原子记录最终 Artifact、Evidence、ChangeSet、Attempt 和 WorkItem 状态；单独的 `artifact add`、`evidence add` 仅用于长任务 checkpoint。

Facade 不强制 WorkItem 必须遵循单一线性流程。调查、设计、文档、数据分析、非代码验证和多阶段实验可以声明自己的 workflow kind；Agent 也可以选择细粒度接口组合出新的合法流程。复合命令必须支持 dry-run/plan 输出，让 Agent 在执行前检查将发生的领域变化。

所有 Facade 命令通过 `--dry-run` 在 SQLite 在线备份上执行同一服务端校验和领域操作，返回 `would_result`，然后丢弃备份；真实数据库不会产生 Claim、Attempt、Review、handoff 或 integration preparation。

复合 CLI 不等于超大数据库事务。Git、测试和其他外部副作用采用 prepare/execute/confirm：

1. prepare 验证状态并签发短期 lease/fence。
2. Agent 在事务外执行 Git 或测试。
3. confirm 验证 lease、实际结果和 Git ancestry，并原子写入领域状态。

数据库事务不得覆盖长时间测试或不可回滚的 Git 操作。

### 3.5 Review 独立性

复合 review 不得隐式允许实现者自审。Task 或 WorkItem 使用明确策略：

```text
self_review_allowed
independent_review_required
capability_review_required
risk_based_review
```

policy 存储在 WorkItem 上。默认 `risk_based_review`；`capability_review_required` 要求有效的 `review` capability；`independent_review_required` 按服务端签发的 executor identity 判断。仅有一个 distinct executor 时允许带 assurance flag 的自审，避免永久阻塞。

Evidence 记录 producer Session。是否要求独立 reviewer 由 Task policy、风险和当前可用 Participant 共同决定，不能硬编码为所有中高风险工作永远必须存在第二个 Agent。单 Agent任务允许带明确 assurance flag 的自审；Agent可以基于新风险主动升级 review 强度，也可以请求 approval 后调整不适用的 review policy。任何降级都必须显式、可审计，不能伪装成独立审查。

### 3.6 Wait 责任链

Agent-owned wait 必须保持：

```text
waiter -> WorkItem -> active Claim/handoff -> owner Session
```

规则：

- 无有效 owner 的 WorkItem 不允许进入 await。
- Claim 关闭、lease 过期、owner 退出或责任移交失败时，wait 立即返回并恢复 Participant 为 active。
- submit 到 review、review 到 integration 使用短期 handoff reservation，避免阶段交接瞬间造成不必要的等待抖动。
- handoff 超时后释放为全局候选，并唤醒相关等待者。
- 全员 waiting 只有在每条 Agent-owned wait 都有有效责任链时才合法。

外部依赖使用独立 target 类型，不伪装成无人持有的 WorkItem。外部 wait 必须定义 detector、持久化结果和失去可观测性后的恢复策略；能确定 deadline 时应记录 deadline，天然无确定期限的依赖使用 heartbeat/reassessment policy，不能为了满足 schema 编造超时。

### 3.7 Durable deferred callback

持久化 wait 是事实来源，daemon/MCP 只是投递器：

1. wait 写入 SQLite。
2. 状态变化事务中结束 wait，并写 durable outbox。
3. daemon 向仍连接的 MCP deferred request 投递结果。
4. 连接断开时结果保留，重连后按 wait ID 重放。
5. delivery ID 和确认状态保证幂等。

SQLite WAL 监听只用于及时唤醒 daemon，不能作为通知事实来源。

### 3.8 Deliverable 验证强度

Deliverable 不能仅凭任意一条 passed Evidence 进入 verified。Evidence 需要绑定：

```text
subject_type / subject_id
repository_identity
commit
command or verifier
environment
producer_session
observed_at
result
```

代码类 required Deliverable 的 verified 条件至少包括：

- 映射 WorkItem 已 accepted。
- 对应 ChangeSet 已集成到目标分支。
- Evidence 所属 commit 被目标分支包含。
- Evidence 类型满足 Deliverable verification policy。

非代码 Deliverable 不能被强制要求 ChangeSet。文档、研究、设计、运维、外部审批和数据交付按 Deliverable kind 使用不同验证策略；验证策略必须证明用户可观察结果，但不能把“必须有 Git commit”错误泛化到所有工作。

例如 CLI 交付项可要求 `help_smoke`、`functional_smoke` 和 `integration_test`，不能用一个通用 Evidence 批量形式验证全部功能。

Deliverable 来源区分 proposed、required、derived 和 discovered。自动从文档抽取的结果先作为草案，Agent 可以发现并新增交付项，但不能静默删除或 waive required Deliverable。

### 3.9 Blocked 与 conclude

blocked audit 不再要求模型经历三次相同回合。SessionBridge 持久化 blocking fingerprint：

```text
blocker_code
entity_id
dependency_version
first_observed_cursor
last_observed_cursor
unchanged_since
```

服务端根据阻塞持续时间、依赖版本和可执行动作判断 blocked 是否成立：

- 不可恢复错误可以立即 blocked。
- Agent-owned 短期依赖使用 await。
- 外部依赖使用 external wait。
- 只有不确定阻塞才使用观察窗口。

Action Planner 仅在 `completion_eligible=true` 时推荐最终 conclude，但 Agent始终可以提交 completion assessment 或 completion challenge。机械条件不满足时，服务端返回 blockers 和最新 completion token，不把它记为失败的 conclude 尝试，也不改变 Participant 状态。这样既避免盲目 conclude，又保留 Agent主动发现“系统认为未完成但实际已满足”或“系统认为可完成但语义仍缺失”的能力。

最终完成要求双重判断：

```text
机械条件由 SessionBridge 验证
+
语义完成由 Agent 基于代码和目标扫描确认
```

Agent 可以提交 `completion challenge` 指出错误 Deliverable、过期 blocker、无效 Evidence 或遗漏目标。SessionBridge 必须允许这些挑战进入审计和协作流程，不能把当前结构化计划视为不可质疑的真理。

### 3.10 错误恢复

错误返回结构化合法恢复集合，而不是单个武断命令：

```json
{
  "code": "RESOURCE_CONFLICT",
  "recovery_options": [
    { "action": "reduce_scope", "safe": true },
    { "action": "request_handoff", "safe": true },
    { "action": "claim_alternative", "work_item_ids": ["..."] },
    { "action": "await", "allowed": false, "reason": "unowned work remains" }
  ]
}
```

简单输入优先使用类型化 CLI 参数。必须使用 JSON 的复杂输入提供 schema、模板和客户端构造器，避免 Agent 猜测字段。

### 3.11 探索自由、资源边界与能力升级

Agent 在领取工作前必须能够充分理解代码库和共享状态。默认资源规则区分：

```text
read / inspect / search
write / mutate / integrate
```

- 对工作区的只读搜索、类型检查、测试读取和状态查询默认不要求 writable Claim，除非涉及明确的机密或隔离边界。
- 文件修改、生成持久产物、Git写操作和外部副作用必须受 Claim、Reservation 和 capability 控制。
- ResourceReservation 默认保护写冲突，不应把大目录 reservation 解释为禁止其他 Agent只读分析。
- Agent可以在真正修改前提出 tentative scope，并在分析后缩小、扩大或拆分；范围升级应返回冲突信息和协商选项，而不是迫使 Agent重建整个任务。
- 测试和构建产生的临时缓存应有独立 transient resource 语义，避免被误判为业务文件写冲突。

能力系统支持动态发现和升级：

- `agent next` 返回当前能力、缺失能力和可用工具目录，但不隐藏 Agent无权限执行的状态信息。
- Agent可以请求新 capability、外部工具、网络访问、额外资源或更高自治范围，并提供理由、影响面和回滚方案。
- capability 未预注册时允许提出声明式 extension proposal，由用户策略、Task policy 或具备授权的 Agent批准。
- 权限拒绝只阻止具体副作用，不应阻止 Agent继续分析、提出替代方案、创建 Gap 或协调其他 Agent。
- Agent可以把工作委托给具备不同能力的 Participant，而不是被迫自己等待或结束。

开放能力不能绕过安全边界：secret、用户数据、破坏性 Git操作、外部生产系统和不可逆动作仍必须由明确 capability/approval 约束，并保留完整审计。

### 3.12 多 Agent 活性、公平与推测执行

最大化自治不等于让 Planner强制分配任务，也不等于允许多个 Agent无条件写同一资源。系统采用“软协调、硬隔离”的原则：

- Planner提供任务等待时间、被忽略时长、依赖关键路径、已有意图和资源冲突等事实，但优先级只是建议，不强制轮转或平均分配。
- Agent可以选择低优先级但高价值的探索、重构或验证工作，并记录 rationale；系统不能仅因不符合推荐顺序而拒绝。
- 多个 Agent可以并行进行只读分析或 isolated speculative implementation；真正写入共享目标、提交 ChangeSet或占用互斥资源时才需要 Claim/Reservation。
- 推测分支必须标记 base snapshot、假设和预期合并点。若主计划变化，系统返回 rebase/revalidate 候选，而不是直接丢弃推测成果。
- 重复 proposal/WorkItem 使用语义指纹、来源和关系提示 Agent合并、竞争验证或保留替代方案；系统不得仅凭标题相似自动删除其中一个。
- 长期无人领取的 required工作、长期占有但无进展的 Claim、反复被抢占的 WorkItem和集成队列饥饿必须成为 health signal，并进入所有 Agent的候选信息。
- Claim lease用于回收失联责任，不作为限制复杂任务思考时间的硬截止；有可观察进展时允许低成本自动续租。
- Agent离线、模型失败或 worktree损坏时，checkpoint、Artifact和未集成 ChangeSet仍可由其他 Agent接管，不能把知识只保存在会话上下文中。

系统可以提供 fairness score、critical-path score和duplication risk，但这些值必须带计算依据、置信度和 `advisory=true`，不能成为隐藏的硬调度规则。

### 3.13 上下文压缩与按需展开

减少 token不能通过隐藏可能影响判断的信息实现。紧凑响应采用渐进披露：

- 第一层始终返回机械不变量、变化摘要、候选动作、异常和来源引用。
- 每个摘要项提供稳定 entity ID、版本、摘要 digest和按需展开命令。
- Agent可以按实体、关系、事件区间、Deliverable、资源或代码路径请求详细上下文，而不是只能在“极简”与“全量”之间选择。
- Planner不能摘要掉反例、失败 Evidence、未解决异议、替代方案或低置信度信息；这些内容至少以 warning/reference存在。
- 摘要必须区分事实、推断、建议和未知，避免 Agent把 Planner推断当成数据库事实。
- Agent可以声明本轮信息需求和 token预算偏好，但服务端不能因预算偏好截断安全不变量或未明确标注截断。
- 发生上下文压缩、Agent重启或模型切换时，可以用 snapshot ID和 provenance恢复，不依赖模型记住此前对话。

响应中的推荐项应包含：

```text
source_entities
computed_at_cursor
confidence
assumptions
expand_ref
```

这样压缩减少的是重复传输，而不是 Agent可获得的信息上限。

### 3.14 信任、来源与状态治理

开放 Agent创建 proposal、Deliverable和Evidence后，系统需要防止错误信息污染共享状态，但不能依靠封闭能力解决：

- 每个派生事实记录 producer、输入实体、工具/模型声明、时间、代码版本和 provenance。
- Agent生成的判断默认是 assertion，不自动升级为 verified fact；验证规则决定何时提升可信状态。
- Agent可以反驳、替代或使旧 assertion失效，历史保留，不使用静默覆盖。
- proposal洪泛通过语义去重、折叠视图、速率健康信号和可恢复的 soft budget处理；预算达到阈值时仍允许 Agent提交高优先级/安全相关发现，并返回需整理的原因，不能直接让 Agent失语。
- 对长期无效、重复或低质量 proposal的治理应优先触发 consolidation WorkItem，让 Agent自行整理，而不是由系统不可解释地删除。
- capability、approval和自治策略的变更有明确作用域、期限、授予者和撤销路径；撤销只阻止未来副作用，不篡改已产生的审计事实。
- Participant身份、Session和producer provenance不能仅依赖 Agent自报标签，防止同一执行主体伪装成独立 reviewer。
- 外部工具结果、CI、人工审批和MCP回调都必须保留原始来源引用，Agent仍可质疑其有效性。

系统的信任模型用于标注和验证信息，不用于压制不同技术意见。多个互相冲突但有证据的方案可以同时存在，直到 Agent通过实验、review或决策流程收敛。

## 4. 分阶段实施

### Phase 1：压缩读取与机械参数

- 实现 Action Planner 和 `agent next`，先覆盖 claim、review、integration、await、blocked 和 conclude 建议，并声明结果非穷举。
- 实现 `agent propose-action`、capability escalation 和 completion challenge，保证 Planner 未建模的合理动作仍可表达。
- 明确 read/write 资源语义和动态工具目录，避免 Claim阻止只读探索。
- 引入 scoped token，保持旧 CAS 参数兼容但从邀请 Prompt 中移除。
- 增加 compact snapshot 和 `FULL_RESYNC_REQUIRED`。
- 错误响应增加 `recovery_options`。
- Prompt 推荐以 `agent next` 降低机械操作成本，同时明确 Agent可以按需 full sync、展开实体、提出新动作或使用底层接口。

验收：常规 Agent循环不再需要单独调用 `agent inbox`、`item inspect`、`agent list` 或复制 completion 版本字段。

### Phase 2：复合工作流

- 实现 `work start` 和 `work submit bundle`。
- 实现带 review policy 的 `work review`。
- 实现 integration prepare/confirm，并保留 Git ancestry 强验证。
- 内部继续记录细粒度不可变事件。
- 支持 isolated speculative work、语义去重提示和可接管 checkpoint，避免自治并发退化为重复写冲突。

验收：一个 WorkItem 从领取到 accepted 的默认 Agent往返不超过 6 次，且不降低审计信息或独立审查强度。

### Phase 3：责任链与 durable runtime

- 增加 stage handoff reservation 和超时释放。
- 完善 Agent-owned/external wait 类型。
- 实现 durable outbox、daemon 和 MCP deferred tool。
- 支持 daemon 重启、连接断开和结果幂等重放。
- 删除旧轮询式 `agent wait`，统一依赖等待为 durable `agent await` 或 MCP deferred tool。

验收：等待期间不产生模型轮询；owner 消失后等待者能自动恢复；daemon 重启不丢失完成回调。

### Phase 4：完成语义强化与迁移

- Evidence 绑定 commit、subject 和 verification policy。
- Deliverable verified 检查 accepted WorkItem、integrated ChangeSet 和匹配 Evidence。
- blocked fingerprint 替代三轮模型 audit。
- GUI 分开展示 Deliverable、WorkItem、Review、Integration 和 Completion blocker，不使用误导性的单一百分比。
- 统计旧命令使用率，逐步将细粒度接口降级为高级模式。
- 增加渐进披露、provenance、assertion状态、soft budget和状态整理机制。

验收：不能使用通用 Evidence 形式验证全部 Deliverable；不能在 pending integration 或 completion blocker 存在时显示任务即将完成。

## 5. 测试与指标

必须覆盖：

- Planner 对其返回的已知动作保证合法性，不把未返回动作视为非法，也不把技术决策固化为唯一调度结果。
- Planner 候选为空时，Agent仍能提出新工作、新动作和能力升级，不会被迫 conclude 或 waiting。
- Facade 未覆盖的 workflow kind 可以安全退回细粒度领域接口。
- 无关事件不使 WorkItem/Claim scoped token 失效。
- snapshot 基线失效时强制 full resync。
- 复合 submit/review/integrate 的失败不产生部分领域写入。
- Git 外部副作用不在长 SQLite事务中执行。
- independent review policy 阻止实现者自审。
- Claim关闭、lease过期和 handoff 超时会唤醒等待者。
- 合法 external wait 不被当作全员等待死锁。
- daemon 重启、MCP断线和重复投递不会丢失或重复完成 wait。
- Evidence commit 不在目标分支时 Deliverable 不能 verified。
- blocked fingerprint 在状态变化后重置，不依赖模型重复三轮。
- conclude 只在机械条件满足后成为推荐动作，但 completion assessment/challenge 始终可提交且不会被计为失败操作。
- 单 Agent任务不会因 independent review policy 永久阻塞，多 Agent任务也不会静默降级审查强度。
- 非代码 Deliverable 不会被错误要求 Git ChangeSet。
- Agent可以在无 writable Claim 时进行只读代码探索，但不能借此执行未授权副作用。
- 未知 action/capability proposal 会被保留和审计，但不会被自动执行。
- 多 Agent同时提出相似工作时不会被静默删除，且能够安全进行只读或隔离推测执行。
- 长期无人领取、Claim无进展、反复抢占和integration饥饿会产生可见 health signal。
- compact响应不会隐藏失败Evidence、异议、低置信度推断或安全不变量，所有摘要均可按引用展开。
- proposal达到soft budget时仍允许提交安全/高优先级发现，并能通过consolidation恢复正常状态。
- 同一执行主体不能通过创建多个Session伪造独立review。

上线后持续记录：

```text
每个 accepted WorkItem 的 Agent命令数
每个 accepted WorkItem 的模型回合数
sync 完整响应字节数和 compact 响应字节数
INVALID_INPUT 数量
CONCLUSION_STALE / VERSION_CONFLICT 数量
重复 sync 数量
等待期间模型唤醒次数
全员 waiting 且无责任链次数
从 submitted 到 review、从 approved 到 integrated 的空窗时长
重复工作比例与推测成果复用率
长期无人领取和integration饥饿时长
compact响应压缩率与按需展开次数
proposal折叠率、consolidation时长和被错误抑制的高优先级发现数
```

目标值：

- 默认 WorkItem 生命周期 Agent往返减少 60% 以上。
- `INVALID_INPUT` 减少 80% 以上。
- 常规 conclude 一次成功，机械条件不满足时不产生 conclude 请求。
- Agent-owned wait 不出现无 owner 持续等待。
- 等待期间模型唤醒次数为 0。
- 审计事件、并发安全和 Git集成验证不弱于当前版本。
- 状态压缩不能降低任务成功率、缺陷发现率或新工作发现率。
- Agent自治动作中被Planner硬拒绝的比例只包含可解释的安全/一致性不变量违规。

## 6. 非目标与约束

- 不将 SessionBridge 变成替 Agent做技术决策的中央任务调度器。
- 不把 Planner 的已知候选动作当成 Agent 可执行动作的全集。
- 不因 Facade 尚未支持某种工作流而禁止 Agent使用底层能力或提出扩展动作。
- 不限制 Agent发现新问题、修改计划、增加交付项、挑战完成状态或请求能力升级。
- 不用写资源 Reservation 阻止其他 Agent只读理解代码和验证假设。
- 不用公平性分数、优先级或token预算把建议变成隐藏的硬调度规则。
- 不通过截断上下文、丢弃低优先级proposal或静默合并不同方案来换取表面效率。
- 不禁止隔离的推测工作；只在共享写入和不可逆副作用边界执行互斥。
- 不删除现有严格领域对象和 append-only 审计事件。
- 不用一个全局 token 替代局部并发控制。
- 不在 SQLite事务内运行长测试或不可回滚的 Git操作。
- 不为了减少命令次数放宽 review、Evidence、Deliverable 或 completion 规则。
- 不依赖更长 Prompt 修复服务端可以强制的不变量。
