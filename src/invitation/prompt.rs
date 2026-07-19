use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{InvitationError, InviteSecret, Result};

pub const PROMPT_TEMPLATE_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptShell {
    Posix,
    PowerShell,
}

#[derive(Debug, Clone, Copy)]
pub enum PromptDelivery<'a> {
    File { invite_path: &'a Path },
    Inline { secret: &'a InviteSecret },
}

#[derive(Debug, Clone, Copy)]
pub struct PromptSpec<'a> {
    pub task_id: &'a str,
    pub invite_id: &'a str,
    pub expires_at: &'a str,
    pub protocol_version: &'a str,
    pub shell: PromptShell,
    pub delivery: PromptDelivery<'a>,
    pub provision_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub text: String,
    pub sha256: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PromptRendererV1;

impl PromptRendererV1 {
    pub fn render(&self, spec: &PromptSpec<'_>) -> Result<RenderedPrompt> {
        validate_identifier("task ID", spec.task_id)?;
        validate_identifier("invite ID", spec.invite_id)?;
        validate_display_value("protocol version", spec.protocol_version)?;
        validate_display_value("expiration", spec.expires_at)?;

        let join_command = render_join_command(spec)?;
        let text = format!(
            "你将作为 SessionBridge 自主协同 Agent 接入任务。\n\
             Task ID: {task_id}\n\
             Invite ID: {invite_id}\n\
             Invite expires at: {expires_at}\n\
             Protocol version: {protocol_version}\n\n\
             SessionBridge 的持久化状态是当前任务事实来源。不要等待用户分配角色或具体子任务，也不要把本提示当作任务快照。\n\n\
             先为自己选择一个简短、易辨认的昵称，并在以下命令中加入 --nickname '<你的昵称>'。昵称会显示在群聊、参与者列表和诊断信息中。不要打印、复制、回显或在回复中暴露 invitation/context secret。然后执行接入命令：\n\n\
             {join_command}\n\n\
             只有 join 返回 ok=true 才能继续。成功后从 JSON 响应读取 context_path、worktree_path 和 next_actions；不要猜测路径。切换到 worktree_path，并在后续命令中显式传入 --context <context_path>，然后执行 agent whoami 和 agent next --format json。只有需要未摘要的全局信息时才执行 agent sync --full。\n\n\
             Join 失败规则：\n\
             - DATABASE_BUSY：仅按 retry hint 重试同一 join attempt。\n\
             - JOIN_RECOVERY_REQUIRED：只执行错误 details 给出的恢复动作。\n\
             - INVITE_RESERVED：停止接入并向用户报告，不能创建第二个 Session。\n\
             - INVITE_CONSUMED、INVITE_REVOKED、INVITE_EXPIRED、INVITE_SECRET_MISSING、PROTOCOL_VERSION_MISMATCH：停止并报告错误码，请用户重新生成 invitation 或升级 CLI；不要读取 secret 排障。\n\n\
             自主协同循环：\n\
             1. 每轮调用 agent next。优先处理 obligations，再结合代码理解从 eligible_actions 中选择工作；Planner 候选是建议而非动作全集。\n\
             2. 可以在没有 writable Claim 时只读探索代码和验证假设；修改业务文件、Git写操作或外部副作用前必须取得对应 Claim/Reservation。\n\
             3. Planner 未覆盖你发现的工作时，创建 Deliverable/Requirement/Gap/WorkItem，或使用 agent propose-action 保存新动作；不要因为候选列表为空而停止分析。\n\
             4. 执行中 renew、checkpoint、处理定向责任；输入失效后按错误 recovery 或 scoped token重新验证。最终 submit 附带 Artifact、Evidence 和 ChangeSet。\n\
             5. 在开始重要工作、发现风险、需要讨论、作出关键决定或完成交接时，使用 MCP sessionbridge_chat_send（或 agent chat send --input <JSON>）向任务群聊发送一条简洁消息。需要等待其他 Agent 回复时调用 sessionbridge_chat_await（或 agent chat await），不要用模型轮询。agent next 会返回定向优先的未读群聊；处理后使用 sessionbridge_chat_ack（或 agent chat ack）按消息 ID 确认，读取失败时不要确认。需要完整上下文时使用 chat history。聊天用于共享思路和进展，不替代 Claim、Evidence、handoff、coordination request 或 approval，也不要为每个机械命令刷屏。\n\
             6. 动作完成后再次 agent next。仅在需要完整历史、失败Evidence、异议或替代方案时按 expand_ref/agent sync --full 展开。\n\n\
             Wait、结论与退出规则：\n\
             - 没有 Planner 候选不等于任务完成。你仍可发现新工作、提出新动作、挑战计划或请求能力升级。\n\
             - 当所有可执行候选都被其他 Session 正在推进的上游 WorkItem 阻塞时，优先调用 MCP 工具 sessionbridge_await_work_items，并提供可在断线后复用的稳定 request_id；MCP 不可用时才执行一次 agent await work-items --item <ID>。多个依赖使用 mode=all/any。await 没有时间超时，只会在依赖 accepted、依赖不可能完成或 Task 终止时返回；等待期间不得重复 sync 或旧 agent wait。\n\
             - 外部系统或文件条件不能伪装成 WorkItem owner；优先调用 MCP 工具 sessionbridge_await_external 并复用稳定 request_id，MCP 不可用时使用 agent await external --detector <JSON>。有确定期限时提供 deadline，无确定期限时声明服务端 reassessment 节奏。detector失败、期限到达或条件满足都会返回持久结果并恢复当前 Participant。\n\
             - 若仍有工作但因权限、外部依赖或不可恢复错误无法推进，先记录 Gap/Blocker/CoordinationRequest，再使用最新 agent next 返回的 completion token 执行 agent conclude --outcome blocked。成功后报告相关 ID，并在 should_exit=true 时结束当前运行。\n\
             - completion_eligible=true 只表示机械条件允许终局扫描，不代表语义完成。只有检查代码、目标、失败Evidence和遗漏风险后仍判断无工作，才使用同一次 agent next 的 completion token 提交 no-further-work assessment。你始终可以提出 completion challenge。\n\
             - Task completed/cancelled 后停止领取和修改；Task paused 时只执行响应明确允许的动作，不要自行继续工作或终局 conclude。\n",
            task_id = spec.task_id,
            invite_id = spec.invite_id,
            expires_at = spec.expires_at,
            protocol_version = spec.protocol_version,
        );

        Ok(RenderedPrompt {
            sha256: sha256_hex(text.as_bytes()),
            text,
        })
    }
}

fn render_join_command(spec: &PromptSpec<'_>) -> Result<String> {
    let provision = if spec.provision_worktree {
        " --provision-worktree"
    } else {
        ""
    };
    match spec.delivery {
        PromptDelivery::File { invite_path } => {
            let invite_path = path_to_utf8(invite_path)?;
            Ok(format!(
                "sessionbridge agent join --invite-file {}{provision} --format json --non-interactive",
                quote(spec.shell, invite_path),
            ))
        }
        PromptDelivery::Inline { secret } => {
            if secret.invite_id != spec.invite_id
                || secret.task_id != spec.task_id
                || secret.expires_at != spec.expires_at
                || secret.protocol_version != spec.protocol_version
            {
                return Err(InvitationError::InvalidOutput(
                    "inline secret metadata does not match the prompt".to_owned(),
                ));
            }
            let secret = String::from_utf8(secret.to_json_bytes()?).map_err(|_| {
                InvitationError::InvalidOutput("inline secret JSON is not UTF-8".to_owned())
            })?;
            let secret = secret.trim_end();
            match spec.shell {
                PromptShell::Posix => Ok(format!(
                    "sessionbridge agent join --invite-stdin{provision} --format json --non-interactive <<'SESSIONBRIDGE_INVITE'\n{secret}\nSESSIONBRIDGE_INVITE"
                )),
                PromptShell::PowerShell => Ok(format!(
                    "@'\n{secret}\n'@ | sessionbridge agent join --invite-stdin{provision} --format json --non-interactive"
                )),
            }
        }
    }
}

fn quote(shell: PromptShell, value: &str) -> String {
    match shell {
        PromptShell::Posix => format!("'{}'", value.replace('\'', "'\"'\"'")),
        PromptShell::PowerShell => format!("'{}'", value.replace('\'', "''")),
    }
}

fn path_to_utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| InvitationError::NonUtf8Path(PathBuf::from(path)))
}

fn validate_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        return Err(InvitationError::InvalidOutput(format!(
            "{name} must contain between 1 and 128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(InvitationError::InvalidOutput(format!(
            "{name} contains unsafe characters"
        )));
    }
    Ok(())
}

fn validate_display_value(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(InvitationError::InvalidOutput(format!(
            "{name} is empty, too long, or contains control characters"
        )));
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
