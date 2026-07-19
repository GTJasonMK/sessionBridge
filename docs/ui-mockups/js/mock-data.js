/**
 * SessionBridge UI Mockups — 模拟数据
 * 字段对齐 gui/src/types.ts 与 task_snapshot 结构
 */
window.SB_MOCK = {
  workspace: {
    project_id: "proj_8f2a1c",
    name: "SessionBridge",
    root: "/home/fufu/Code/SessionBridge",
    repository_kind: "git",
    repository_instance_id: "repo_a91e",
    database_path: "~/.local/share/sessionbridge/sessionbridge.db",
  },

  tasks: [
    {
      id: "task_auth_impl",
      title: "实现用户认证与会话",
      phase: "active",
      autonomy_policy: "bounded",
      target_branch: "main",
      plan_version: 4,
      completion_epoch: 1,
      completion_revision: 2,
    },
    {
      id: "task_mcp_watch",
      title: "MCP 等待回调优化",
      phase: "paused",
      autonomy_policy: "bounded",
      target_branch: "main",
      plan_version: 2,
      completion_epoch: 1,
      completion_revision: 1,
    },
    {
      id: "task_docs_sync",
      title: "协议文档与 CLI 对齐",
      phase: "completed",
      autonomy_policy: "bounded",
      target_branch: "main",
      plan_version: 3,
      completion_epoch: 1,
      completion_revision: 3,
    },
  ],

  dashboard: {
    pipeline: {
      required_deliverables_total: 4,
      required_deliverables_verified: 2,
      work_items_total: 9,
      work_items_accepted: 5,
      reviews_required: 2,
      reviews_approved: 1,
      integrations_required: 1,
      integrations_completed: 0,
      completion_blocker_count: 2,
    },
    deliverables: [
      {
        id: "d1", key: "auth-api", title: "认证 API", description: "注册 / 登录 / 登出 HTTP 接口",
        kind: "api", required: true, status: "verified", work_item_ids: ["w1", "w2"], evidence_ids: ["e1"],
      },
      {
        id: "d2", key: "session-store", title: "会话存储", description: "SQLite 会话与刷新令牌",
        kind: "storage", required: true, status: "submitted", work_item_ids: ["w3"], evidence_ids: [],
      },
      {
        id: "d3", key: "auth-tests", title: "认证测试", description: "单元与集成测试覆盖",
        kind: "tests", required: true, status: "open", work_item_ids: ["w4"], evidence_ids: [],
      },
      {
        id: "d4", key: "docs", title: "操作文档", description: "README 与协议补充",
        kind: "docs", required: false, status: "waived", work_item_ids: [], evidence_ids: [],
      },
    ],
    details: {
      task: {
        id: "task_auth_impl",
        title: "实现用户认证与会话",
        phase: "active",
        autonomy_policy: "bounded",
        target_branch: "main",
        plan_version: 4,
        completion_epoch: 1,
        completion_policy: "collective_stillness",
        workspace_id: "ws_1",
        version: 12,
      },
      criteria: [
        { id: "c1", kind: "objective", content: "用户可注册、登录、登出，会话在重启后可恢复。", required: true },
        { id: "c2", kind: "constraint", content: "不引入外部托管鉴权服务；状态落在本地 SQLite。", required: true },
        { id: "c3", kind: "done", content: "关键路径有测试；无未解决的 required gap / blocker。", required: true },
      ],
      event_cursor: 148,
    },
    health: {
      task: { id: "task_auth_impl", title: "实现用户认证与会话", phase: "active", plan_version: 4 },
      signals: {
        active_participants: 3,
        waiting_participants: 1,
        concluded_participants: 0,
        open_blockers: 1,
        open_required_gaps: 1,
        open_invalidations: 0,
        required_deliverables_open: 2,
      },
      planning_required: false,
      completion_cohort_open: true,
      completed_with_warnings: false,
      no_progress_possible: false,
      completion_signal_warnings: [
        { code: "OPEN_REQUIRED_DELIVERABLE", severity: "critical", count: 2 },
        { code: "OPEN_BLOCKER", severity: "warning", count: 1 },
      ],
      event_cursor: 148,
    },
    participants: [
      {
        id: "p1", session_id: "sess_build", label: "Build Agent", kind: "worker", state: "active",
        completion_epoch: 1, last_seen_at_micros: Date.now() * 1000 - 12_000_000,
        worktree_path: ".sessionbridge/worktrees/build", branch: "sb/task-auth/build", model: "claude-opus",
      },
      {
        id: "p2", session_id: "sess_review", label: "Review Agent", kind: "worker", state: "waiting",
        completion_epoch: 1, last_seen_at_micros: Date.now() * 1000 - 45_000_000,
        worktree_path: ".sessionbridge/worktrees/review", branch: "sb/task-auth/review", model: "gpt-5",
      },
      {
        id: "p3", session_id: "sess_test", label: "Test Agent", kind: "worker", state: "active",
        completion_epoch: 1, last_seen_at_micros: Date.now() * 1000 - 8_000_000,
        worktree_path: ".sessionbridge/worktrees/test", branch: "sb/task-auth/test", model: "claude-sonnet",
      },
    ],
    invitations: [
      {
        id: "inv_01", batch_id: "batch_a", state: "consumed", participant_kind: "worker",
        blocks_completion: true, completion_epoch: 1, expires_at_micros: Date.now() * 1000 + 3_600_000_000,
        consumed_by_session_id: "sess_build",
      },
      {
        id: "inv_02", batch_id: "batch_a", state: "consumed", participant_kind: "worker",
        blocks_completion: true, completion_epoch: 1, expires_at_micros: Date.now() * 1000 + 3_600_000_000,
        consumed_by_session_id: "sess_review",
      },
      {
        id: "inv_03", batch_id: "batch_a", state: "pending", participant_kind: "worker",
        blocks_completion: true, completion_epoch: 1, expires_at_micros: Date.now() * 1000 + 3_600_000_000,
      },
      {
        id: "inv_04", batch_id: "batch_a", state: "pending", participant_kind: "worker",
        blocks_completion: true, completion_epoch: 1, expires_at_micros: Date.now() * 1000 + 3_600_000_000,
      },
    ],
    work_items: [
      {
        id: "w1", kind: "implementation", title: "注册接口", description: "POST /auth/register 与校验",
        required: true, priority: 10, risk: "medium", plan_state: "ready", result_state: "accepted",
        active_claim: null,
      },
      {
        id: "w2", kind: "implementation", title: "登录与会话", description: "登录、刷新、登出路径",
        required: true, priority: 10, risk: "high", plan_state: "ready", result_state: "submitted",
        active_claim: { id: "cl1", session_id: "sess_build", mode: "exclusive", generation: 1, lease_expires_at_micros: Date.now() * 1000 + 600_000_000 },
      },
      {
        id: "w3", kind: "implementation", title: "会话表迁移", description: "schema vN 会话表与索引",
        required: true, priority: 8, risk: "medium", plan_state: "ready", result_state: "accepted",
        active_claim: null,
      },
      {
        id: "w4", kind: "test", title: "认证集成测试", description: "happy path + 边界",
        required: true, priority: 7, risk: "low", plan_state: "ready", result_state: "open",
        active_claim: { id: "cl2", session_id: "sess_test", mode: "exclusive", generation: 1, lease_expires_at_micros: Date.now() * 1000 + 900_000_000 },
      },
      {
        id: "w5", kind: "review", title: "API 契约审查", description: "与协议文档对齐",
        required: true, priority: 6, risk: "low", plan_state: "blocked", result_state: "open",
        active_claim: null,
      },
      {
        id: "w6", kind: "integration", title: "合并到目标分支", description: "worktree 结果集成",
        required: true, priority: 5, risk: "high", plan_state: "pending", result_state: "open",
        active_claim: null,
      },
    ],
    relations: [
      { from_id: "w2", to_id: "w1", relation_type: "depends_on" },
      { from_id: "w4", to_id: "w2", relation_type: "depends_on" },
      { from_id: "w5", to_id: "w2", relation_type: "depends_on" },
      { from_id: "w6", to_id: "w4", relation_type: "depends_on" },
      { from_id: "w6", to_id: "w5", relation_type: "depends_on" },
    ],
    gaps: [
      {
        id: "g1", kind: "requirement_uncovered", severity: "high", required: true, status: "open",
        title: "密码重置未覆盖", description: "Objective 提到重置流程，尚无 WorkItem。",
      },
    ],
    blockers: [
      {
        id: "b1", work_item_id: "w5", waits_for_type: "work_item", waits_for_id: "w2",
        status: "open", reason: "等待登录接口提交证据后再审查",
      },
    ],
    waits: [
      {
        id: "wait1", participant_id: "p2", session_id: "sess_review", mode: "any",
        status: "active", created_at_micros: Date.now() * 1000 - 40_000_000, targets: ["w2"],
      },
    ],
    external_waits: [
      {
        id: "ew1", participant_id: "p2", session_id: "sess_review", detector_type: "file_exists",
        detector: { type: "file_exists", path: ".sessionbridge/evidence/login-submitted.flag" },
        status: "active", next_reassessment_at_micros: Date.now() * 1000 + 30_000_000,
        observation_count: 4,
      },
    ],
    speculative_work: [
      {
        id: "sp1", session_id: "sess_build", title: "JWT 备选方案", branch: "sb/spec/jwt-alt",
        base_commit: "a1b2c3d4e5f6", base_plan_version: 3, merge_ref: "refs/sb/spec/jwt-alt",
        expected_merge_point: "main", head_commit: "f6e5d4c3b2a1", status: "ready_for_adoption",
        summary: "若会话表方案被否决，可切换到本地 JWT",
        assumptions: ["不引入网络依赖", "密钥仅存本地"],
        updated_at_micros: Date.now() * 1000 - 120_000_000,
      },
    ],
    recent_events: {
      previous_cursor: 140,
      next_cursor: 148,
      has_more: true,
      events: [
        { sequence: 148, event_type: "chat.message_sent", actor_type: "operator", actor_id: "op", entity_type: "chat", entity_id: "m7", payload: {}, created_at_micros: Date.now() * 1000 - 5_000_000 },
        { sequence: 147, event_type: "work.claim_renewed", actor_type: "agent", actor_id: "sess_build", entity_type: "work_item", entity_id: "w2", payload: {}, created_at_micros: Date.now() * 1000 - 12_000_000 },
        { sequence: 146, event_type: "deliverable.submitted", actor_type: "agent", actor_id: "sess_build", entity_type: "deliverable", entity_id: "d2", payload: {}, created_at_micros: Date.now() * 1000 - 30_000_000 },
        { sequence: 145, event_type: "work.item_accepted", actor_type: "agent", actor_id: "sess_build", entity_type: "work_item", entity_id: "w1", payload: {}, created_at_micros: Date.now() * 1000 - 90_000_000 },
        { sequence: 144, event_type: "participant.waiting", actor_type: "agent", actor_id: "sess_review", entity_type: "participant", entity_id: "p2", payload: {}, created_at_micros: Date.now() * 1000 - 120_000_000 },
        { sequence: 143, event_type: "blocker.opened", actor_type: "agent", actor_id: "sess_review", entity_type: "blocker", entity_id: "b1", payload: {}, created_at_micros: Date.now() * 1000 - 150_000_000 },
        { sequence: 142, event_type: "invitation.consumed", actor_type: "agent", actor_id: "sess_test", entity_type: "invitation", entity_id: "inv_03", payload: {}, created_at_micros: Date.now() * 1000 - 300_000_000 },
        { sequence: 141, event_type: "task.launched", actor_type: "operator", actor_id: "op", entity_type: "task", entity_id: "task_auth_impl", payload: {}, created_at_micros: Date.now() * 1000 - 3_600_000_000 },
      ],
    },
    chat_messages: [
      {
        id: "m1", task_id: "task_auth_impl", sender_type: "system", sender_label: "System",
        kind: "note", body: "任务已发布，4 个 worker 席位待接入。", mentions: [], related_entities: [],
        event_sequence: 141, created_at_micros: Date.now() * 1000 - 3_500_000_000,
      },
      {
        id: "m2", task_id: "task_auth_impl", sender_type: "agent", sender_id: "sess_build", sender_label: "Build Agent",
        kind: "intent", body: "我会先落地注册接口与会话表迁移，再实现登录路径。", mentions: [],
        related_entities: [{ entity_type: "work_item", entity_id: "w1" }],
        event_sequence: 142, created_at_micros: Date.now() * 1000 - 3_000_000_000,
      },
      {
        id: "m3", task_id: "task_auth_impl", sender_type: "agent", sender_id: "sess_review", sender_label: "Review Agent",
        kind: "question", body: "登录响应是否需要包含 refresh token，还是仅 session id？", mentions: ["operator"],
        related_entities: [{ entity_type: "deliverable", entity_id: "d1" }],
        event_sequence: 143, created_at_micros: Date.now() * 1000 - 2_000_000_000,
      },
      {
        id: "m4", task_id: "task_auth_impl", sender_type: "operator", sender_id: "op", sender_label: "Operator",
        kind: "decision", body: "仅 session id 即可；刷新走本地续期接口。", mentions: ["sess_review"],
        related_entities: [], reply_to_message_id: "m3",
        event_sequence: 144, created_at_micros: Date.now() * 1000 - 1_800_000_000,
      },
      {
        id: "m5", task_id: "task_auth_impl", sender_type: "agent", sender_id: "sess_build", sender_label: "Build Agent",
        kind: "progress", body: "注册接口已 accepted；登录接口提交中，证据路径已写入。", mentions: [],
        related_entities: [
          { entity_type: "work_item", entity_id: "w1" },
          { entity_type: "work_item", entity_id: "w2" },
        ],
        event_sequence: 146, created_at_micros: Date.now() * 1000 - 30_000_000,
      },
      {
        id: "m6", task_id: "task_auth_impl", sender_type: "agent", sender_id: "sess_test", sender_label: "Test Agent",
        kind: "warning", body: "集成测试尚缺密码错误边界用例，我会在 w4 补齐。", mentions: [],
        related_entities: [{ entity_type: "work_item", entity_id: "w4" }],
        event_sequence: 147, created_at_micros: Date.now() * 1000 - 15_000_000,
      },
      {
        id: "m7", task_id: "task_auth_impl", sender_type: "operator", sender_id: "op", sender_label: "Operator",
        kind: "note", body: "优先保证登录 happy path，边界用例可以并行补。", mentions: [],
        related_entities: [],
        event_sequence: 148, created_at_micros: Date.now() * 1000 - 5_000_000,
      },
    ],
  },

  prompts: [
    {
      invite_id: "inv_01", task_id: "task_auth_impl", batch_id: "batch_a", batch_index: 1,
      batch_count: 4, status: "consumed", participant_kind: "worker", expires_at: "2h",
      sendable: false,
      prompt: `# SessionBridge Invitation · Agent 1\n\nYou are joining task: 实现用户认证与会话\n\n1. Run: sessionbridge agent join --invite-file <PRIVATE>\n2. Switch to the returned worktree\n3. Use agent next / agent wait for coordination\n\nDo not invent roles. Discover gaps, claim work, submit evidence.`,
    },
    {
      invite_id: "inv_02", task_id: "task_auth_impl", batch_id: "batch_a", batch_index: 2,
      batch_count: 4, status: "consumed", participant_kind: "worker", expires_at: "2h",
      sendable: false,
      prompt: `# SessionBridge Invitation · Agent 2\n\n...`,
    },
    {
      invite_id: "inv_03", task_id: "task_auth_impl", batch_id: "batch_a", batch_index: 3,
      batch_count: 4, status: "pending", participant_kind: "worker", expires_at: "2h",
      sendable: true,
      prompt: `# SessionBridge Invitation · Agent 3\n\nYou are joining task: 实现用户认证与会话\n\nPrivate invite file path is injected at join time.\nRun agent join, then follow the protocol loop.`,
    },
    {
      invite_id: "inv_04", task_id: "task_auth_impl", batch_id: "batch_a", batch_index: 4,
      batch_count: 4, status: "pending", participant_kind: "worker", expires_at: "2h",
      sendable: true,
      prompt: `# SessionBridge Invitation · Agent 4\n\n...`,
    },
  ],

  doctor: {
    diagnostics: [
      { code: "WORKSPACE_OK", status: "pass", message: "Workspace 根目录可读写" },
      { code: "SQLITE_WAL", status: "pass", message: "SQLite 处于 WAL 模式" },
      { code: "GIT_BASELINE", status: "pass", message: "Baseline commit 可解析" },
      { code: "DIRTY_TREE", status: "warn", message: "工作区存在 2 个未提交路径（不阻塞观察）" },
      { code: "DAEMON_REACHABLE", status: "pass", message: "本机 daemon 可连接" },
    ],
  },

  collaboration: {
    critical_count: 0,
    warning_count: 2,
    findings: [
      { code: "CLAIM_LEASE_HEALTHY", status: "pass", message: "活跃 Claim 均在租约内" },
      { code: "WAIT_CHAIN_OK", status: "pass", message: "等待链无环" },
      { code: "GAP_REQUIRED_OPEN", status: "warn", message: "仍有 1 个 required gap：密码重置未覆盖", entity_id: "g1" },
      { code: "INTEGRATION_PENDING", status: "warn", message: "集成阶段尚未开始", entity_id: "w6" },
      { code: "GIT_WORKTREE_OK", status: "pass", message: "各 Agent worktree 完整" },
    ],
  },

  logs: {
    entries: [
      { record: { timestamp: "12:41:08", event: "chat.message_sent", message: "operator note" } },
      { record: { timestamp: "12:40:55", event: "work.claim_renewed", message: "sess_build renewed w2" } },
      { record: { timestamp: "12:40:12", event: "deliverable.submitted", message: "d2 session-store" } },
      { record: { timestamp: "12:38:01", event: "agent.heartbeat", message: "sess_test" } },
      { record: { timestamp: "12:35:44", event: "external_wait.poll", message: "file_exists observation=4" } },
    ],
  },

  preflight: {
    title: "实现用户认证与会话",
    objective_sha256: "9f86d081884c7d659a2feaa0c55ad015",
    agents: 4,
    expires_in_seconds: 7200,
    autonomy_policy: "bounded",
    target_branch: "main",
    baseline_commit: "a1b2c3d4e5f67890",
    dirty_paths: [],
    ready: true,
    warnings: [],
  },
};
