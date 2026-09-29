# SessionBridge 协议 v2

当前协议以工作区和工作群为核心，不兼容旧版 task/invitation CLI。旧版状态目录保留但不读取；v2 状态目录中的 user_version=1 过渡 schema 会原子迁移到当前 schema。

## 信任与入口

GUI、管理 CLI、Agent MCP 通过用户私有 Unix socket 调用同一 Rust 服务。请求为：

```json
{"workspace":"/absolute/project","operation":"room.get","args":{"room":"UUID"},"token":"private credential"}
```

操作人由凭证确定，不能从 args 指定。老板凭证只在本机后端使用；成员凭证仅在受保护的 context 文件与 MCP 进程中使用。数据库保存成员凭证的哈希。

JSONL 帧上限 2 MiB。响应为 ok/data 或 ok=false/error，error 含 code、message。调用方不得将 IO 错误解释为业务成功。

## 对象和地址

Workspace 由规范化路径识别，数据库独立。Room 是协作任务及其工作群。Member 身份稳定，generation 表示当前连接代次；session 是原生 CLI 会话 ID。

每群一个老板、一个协调者、多个执行者。所有交流公开可见；recipients 决定投递对象，不是隐私范围。禁止跨群引用和寻址。广播收件人于发送时固定，排除发送者和已移除成员。

## 操作

| 操作 | 参数和权限 |
| --- | --- |
| workspace.get / room.list | 老板；打开工作区、列群 |
| room.create | 老板；title，默认三位成员 |
| room.get | room；群成员可读本群快照 |
| room.transition | 老板；room、state，暂停/取消前先停止成员 |
| member.add | 老板；room、name，添加执行者 |
| member.configure | 老板；member、cli、provider、model；仅配置中或离线成员 |
| member.initialize / member.stop | 老板；member |
| member.remove | 老板；member；先停止，保留历史和产出 |
| providers | 老板；只读 CC Switch 非敏感列表（id、name、cli、默认模型；不含凭证） |
| inspect / skill.read | 成员；获取自身上下文和协作 Skill |
| ready | 成员；protocol 字符串 "2"、generation 整数、challenge |
| history | room；after 游标、limit 1–200；读取不等于确认 |
| message.send | room、kind、body、recipients 或 broadcast；可选 reply_to |
| message.ack | 成员；messages 为明确已交付消息 ID 列表 |
| work.create | 协调者；room、title、owner、dependencies |
| work.update | 负责人或协调者；room、work、version、state、evidence、commit；重新分配时由协调者指定 owner |
| check.run | 协调者；room、argv；在群整合目录执行，记录实际退出状态 |
| room.integrate | 协调者；room；校验工作、提交、检查并本地合入 |
| service.shutdown | 老板；停止所有成员、暂停活跃群并退出服务 |

数据库内的消息、工作项和成员管理写操作使用 UUID request_id。相同 ID 和相同输入返回原结果；复用 ID 改变输入返回 REQUEST_CONFLICT。CLI 会为未提供 ID 的直接调用生成新 ID；需要安全重试时请显式复用 ID。外部进程和 Git 副作用须按真实状态恢复。

## 入群

1. 服务准备 worktree、成员运行配置、Skill 和 MCP，递增 generation，发送一次性 JOIN challenge。
2. 成员实际调用 skill.read、inspect，再调用 ready。服务校验版本、身份、代次、挑战和读取记录。

普通文本 READY 无效。默认超时 120 秒；旧凭证和旧挑战不能建立新连接。原生 CLI/MCP 自身初始化仍须完整执行。

一位协调者和至少一位执行者就绪才开始调度。额外执行者初始化失败不阻塞其他成员。停止后恢复优先使用原生会话 ID；Codex 线程失效时清除旧 ID 并只回退一次新建线程。保存模型配置会明确清除旧 session，下次建立新会话。

## 消息和调度

类型为 request、reply、notice；运行服务另外记录 system、output、tool、approval。

单个成员最多一轮执行中。普通消息排队，控制通道处理停止。默认一批最多 20 条；空闲时没有模型轮询。消息落库后才交付，ACK 只接受当前成员已交付的明确 ID。

投递状态为 queued、delivered、processed、cancelled。至少一次投递和幂等写入不等于模型或 shell 的恰好一次执行。未确认投递出现在 inspect.unconfirmed 中，恢复时先核实。

同一因果链默认限制为 16 层自动交流，越界显式返回 CHAIN_LIMIT，并停止触发上限的成员，等待老板检查后恢复。回复和通知无需礼貌性回执，状态事件和 ACK 不产生自动回复。

history 的 after 用于普通正向历史查询。UI 首次以 tail 获取最近页，通过 before 读取更早消息；后续以 change_after 获取消息和投递变更。change_cursor 与消息 cursor 分离，流式输出和 ACK 不会因消息序号不变而丢失。

## 工作与整合

工作项状态：todo → running → review → done；另有 blocked、cancelled。每项单一负责人，每位执行者最多一个 running 项。依赖只能引用已有同群工作且不可变，因此不能构造循环。

review 要求明确证据和真实完整提交 SHA；done 仅协调者可操作，且提交必须已存在于群整合分支。重新打开上游工作会使下游结果失效。version 提供 CAS，过期提交被拒绝。

check.run 记录整合 HEAD、命令 argv、实际退出码和工作版本。验证在 Bubblewrap 沙箱中运行，仅整合目录可写，网络隔离；命令默认 60 秒超时，输出有大小上限。缺少 bwrap 时明确失败，不使用无隔离 fallback。所有必要工作验收后，当前 HEAD 与当前工作版本必须具有通过的检查，且没有未解决的失败检查。

最终合入锁位于 Git common directory，不同工作群共享。仅在项目保持目标分支、目录洁净、整合分支包含最新目标、检查有效时执行 ff-only。目标推进会先更新群整合分支，再要求重新验证。冲突在整合目录解决，不自动 stash/reset 用户改动。

进程注册记录 PID 和启动指纹；服务重启不会向复用的 PID 发送信号。旧运行凭证被撤销，成员离线并需要显式恢复。取消保留代码和历史，不回滚已合入提交；归档后只读。

## 限制

Linux 本地文件系统、单系统用户。没有远程多用户、跨群通信、自动推送或部署。会话权限请求沿用原生 CLI。供应商和模型必须真实可用；本程序不推断服务端模型映射或静默回退。
