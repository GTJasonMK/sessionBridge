import { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { open } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Activity, AlertTriangle, Bot, Check, ChevronRight, CircleDot,
  Clipboard, Command, Copy, FolderGit2, GitBranch, LayoutList,
  LoaderCircle, MessageCircle, Moon, Network, Pause, Play, Plus, RefreshCw, Reply,
  Search, Send, Settings, ShieldCheck, Sun, TerminalSquare, Users, X,
} from "lucide-react";
import { Background, BackgroundVariant, Controls, ReactFlow, type Edge, type Node } from "@xyflow/react";
import { api, normalizeError } from "./api";
import type { ChatMessage, ChatMessageKind, GuiError, InvitationManifest, LaunchInput, LaunchPreflight, TaskDashboard, TaskSummary, WorkspaceView } from "./types";

type Page = "overview" | "work" | "agents" | "chat" | "activity" | "diagnostics";
type Theme = "system" | "light" | "dark";

const pageItems: Array<{ id: Page; label: string; icon: typeof Activity }> = [
  { id: "overview", label: "概览", icon: CircleDot },
  { id: "work", label: "工作项", icon: LayoutList },
  { id: "agents", label: "Agent", icon: Users },
  { id: "chat", label: "群聊", icon: MessageCircle },
  { id: "activity", label: "活动", icon: Activity },
  { id: "diagnostics", label: "诊断", icon: TerminalSquare },
];

export default function App() {
  const queryClient = useQueryClient();
  const [workspace, setWorkspace] = useState<WorkspaceView | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string>();
  const [page, setPage] = useState<Page>("overview");
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem("sessionbridge-theme") as Theme) || "system");
  const [launchOpen, setLaunchOpen] = useState(false);
  const [promptTaskId, setPromptTaskId] = useState<string>();
  const [bootError, setBootError] = useState<GuiError>();
  const [pendingWorkspacePath, setPendingWorkspacePath] = useState<string>();
  const [workspaceInitializing, setWorkspaceInitializing] = useState(false);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("sessionbridge-theme", theme);
  }, [theme]);

  useEffect(() => {
    api.restoreWorkspace().then(setWorkspace).catch((error) => setBootError(normalizeError(error)));
  }, []);

  const tasks = useQuery({
    queryKey: ["tasks", workspace?.root],
    queryFn: () => api.listTasks(workspace!.root),
    enabled: Boolean(workspace),
    refetchInterval: 2500,
  });

  useEffect(() => {
    if (!selectedTaskId && tasks.data?.length) setSelectedTaskId(tasks.data[0].id);
    if (selectedTaskId && tasks.data && !tasks.data.some((task) => task.id === selectedTaskId)) {
      setSelectedTaskId(tasks.data[0]?.id);
    }
  }, [tasks.data, selectedTaskId]);

  const dashboard = useQuery({
    queryKey: ["dashboard", workspace?.root, selectedTaskId],
    queryFn: () => api.snapshot(workspace!.root, selectedTaskId!),
    enabled: Boolean(workspace && selectedTaskId),
    refetchInterval: 1500,
  });

  async function chooseWorkspace() {
    const path = await open({ directory: true, multiple: false, title: "选择 SessionBridge Workspace" });
    if (!path || Array.isArray(path)) return;
    try {
      const next = await api.openWorkspace(path);
      setWorkspace(next);
      setSelectedTaskId(undefined);
      setBootError(undefined);
      setPendingWorkspacePath(undefined);
    } catch (error) {
      const normalized = normalizeError(error);
      setBootError(normalized);
      setPendingWorkspacePath(normalized.code === "WORKSPACE_NOT_FOUND" ? path : undefined);
    }
  }

  async function initializeWorkspace() {
    if (!pendingWorkspacePath) return;
    setWorkspaceInitializing(true);
    try {
      const next = await api.initializeWorkspace(pendingWorkspacePath);
      setWorkspace(next);
      setSelectedTaskId(undefined);
      setBootError(undefined);
      setPendingWorkspacePath(undefined);
    } catch (error) {
      setBootError(normalizeError(error));
    } finally {
      setWorkspaceInitializing(false);
    }
  }

  if (!workspace) {
    return <WorkspaceChooser onOpen={chooseWorkspace} onInitialize={initializeWorkspace} error={bootError} pendingPath={pendingWorkspacePath} initializing={workspaceInitializing} />;
  }

  const activeTask = tasks.data?.find((task) => task.id === selectedTaskId);
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-row">
          <div className="brand-mark"><Command size={17} /></div>
          <div><strong>SessionBridge</strong><span>Operator Console</span></div>
        </div>
        <button className="workspace-switcher" onClick={chooseWorkspace} title="切换 Workspace">
          <FolderGit2 size={16} />
          <span><strong>{workspace.name}</strong><small>{workspace.root}</small></span>
          <ChevronRight size={15} />
        </button>
        <div className="sidebar-section-heading"><span>任务</span><button className="icon-button" onClick={() => setLaunchOpen(true)} title="发布任务"><Plus size={15} /></button></div>
        <nav className="task-nav">
          {tasks.isLoading && <div className="sidebar-loading"><LoaderCircle className="spin" size={15} /> 读取任务</div>}
          {tasks.data?.map((task) => (
            <button key={task.id} className={task.id === selectedTaskId ? "task-link active" : "task-link"} onClick={() => setSelectedTaskId(task.id)} aria-current={task.id === selectedTaskId ? "true" : undefined} title={task.title}>
              <StatusDot phase={task.phase} /><span><strong>{task.title}</strong><small>{task.phase} · v{task.plan_version}</small></span>
            </button>
          ))}
          {!tasks.isLoading && !tasks.data?.length && <div className="sidebar-empty">还没有任务</div>}
        </nav>
        <nav className="page-nav">
          {pageItems.map((item) => <button key={item.id} className={page === item.id ? "page-link active" : "page-link"} onClick={() => setPage(item.id)} aria-current={page === item.id ? "page" : undefined}><item.icon size={16} />{item.label}</button>)}
        </nav>
        <div className="sidebar-footer">
          <button className="page-link" onClick={() => setTheme(theme === "dark" ? "light" : theme === "light" ? "system" : "dark")} title="切换主题">
            {theme === "dark" ? <Moon size={16} /> : <Sun size={16} />} 主题：{theme === "system" ? "系统" : theme === "dark" ? "深色" : "浅色"}
          </button>
          <div className="connection-state"><span className="live-dot" /> SQLite 实时同步</div>
        </div>
      </aside>

      <main className="main-panel">
        <header className="topbar">
          <div className="topbar-title">
            <span className="eyebrow">{pageItems.find((item) => item.id === page)?.label}</span>
            <h1>{activeTask?.title || "Workspace"}</h1>
          </div>
          <div className="toolbar">
            {activeTask && <PhaseBadge phase={activeTask.phase} />}
            <button className="icon-button" onClick={() => { tasks.refetch(); dashboard.refetch(); }} title="刷新" aria-label="刷新任务数据"><RefreshCw size={16} /></button>
            {activeTask && <button className="secondary-button" onClick={() => setPromptTaskId(activeTask.id)}><Bot size={16} /> Agent 提示词</button>}
            <button className="primary-button" onClick={() => setLaunchOpen(true)}><Plus size={16} /> 发布任务</button>
          </div>
        </header>

        <section className="content">
          {bootError && <WorkspaceError error={bootError} pendingPath={pendingWorkspacePath} initializing={workspaceInitializing} onInitialize={initializeWorkspace} onDismiss={() => { setBootError(undefined); setPendingWorkspacePath(undefined); }} />}
          {!activeTask && <EmptyTask onLaunch={() => setLaunchOpen(true)} />}
          {activeTask && dashboard.isLoading && <LoadingPanel />}
          {activeTask && dashboard.error && <ErrorPanel error={normalizeError(dashboard.error)} onRetry={() => dashboard.refetch()} />}
          {dashboard.data && page === "overview" && <Overview dashboard={dashboard.data} />}
          {dashboard.data && page === "work" && <WorkView dashboard={dashboard.data} />}
          {dashboard.data && page === "agents" && <AgentsView dashboard={dashboard.data} />}
          {dashboard.data && page === "chat" && <ChatView dashboard={dashboard.data} workspace={workspace} taskId={activeTask!.id} onSent={() => queryClient.invalidateQueries({ queryKey: ["dashboard"] })} />}
          {dashboard.data && page === "activity" && <ActivityView dashboard={dashboard.data} />}
          {page === "diagnostics" && <DiagnosticsView workspace={workspace} taskId={activeTask?.id} />}
        </section>
      </main>

      {dashboard.data && <Inspector dashboard={dashboard.data} task={activeTask} workspace={workspace} onChanged={() => { queryClient.invalidateQueries({ queryKey: ["dashboard"] }); queryClient.invalidateQueries({ queryKey: ["tasks"] }); }} />}
      <LaunchDialog open={launchOpen} onOpenChange={setLaunchOpen} workspace={workspace} onPublished={(taskId) => { setSelectedTaskId(taskId); queryClient.invalidateQueries({ queryKey: ["tasks"] }); }} />
      {promptTaskId && <PromptDialog workspace={workspace} taskId={promptTaskId} onClose={() => setPromptTaskId(undefined)} />}
    </div>
  );
}

function WorkspaceChooser({ onOpen, onInitialize, error, pendingPath, initializing }: { onOpen: () => void; onInitialize: () => void; error?: GuiError; pendingPath?: string; initializing: boolean }) {
  return <div className="workspace-gate"><div className="gate-panel"><div className="gate-icon"><GitBranch size={28} /></div><h1>SessionBridge</h1><p>选择项目目录；尚未初始化时可以直接创建 Workspace。</p><button className="primary-button large" onClick={onOpen}><FolderGit2 size={17} /> 打开 Workspace</button>{error && <WorkspaceError error={error} pendingPath={pendingPath} initializing={initializing} onInitialize={onInitialize} />}</div></div>;
}

function WorkspaceError({ error, pendingPath, initializing, onInitialize, onDismiss }: { error: GuiError; pendingPath?: string; initializing: boolean; onInitialize: () => void; onDismiss?: () => void }) {
  return <div className="workspace-error"><ErrorCard error={error} />{pendingPath && <div className="workspace-error-actions"><span title={pendingPath}>{pendingPath}</span><button className="primary-button" disabled={initializing} onClick={onInitialize}>{initializing ? <LoaderCircle className="spin" size={15} /> : <Plus size={15} />} 初始化并打开</button></div>}{onDismiss && <button className="icon-button workspace-error-close" onClick={onDismiss} title="关闭错误" aria-label="关闭 Workspace 错误"><X size={14} /></button>}</div>;
}

function Overview({ dashboard }: { dashboard: TaskDashboard }) {
  const { signals } = dashboard.health;
  const attention = signals.open_blockers + signals.open_required_gaps + signals.open_invalidations + signals.required_deliverables_open;
  const pipeline = dashboard.pipeline;
  return <div className="stack">
    <div className="metric-strip">
      <Metric label="活跃 Agent" value={signals.active_participants} detail={`${signals.waiting_participants} 等待 · ${signals.concluded_participants} 已停止`} tone="green" />
      <Metric label="需要关注" value={attention} detail={`${signals.open_blockers} 阻塞 · ${signals.open_required_gaps} 缺口`} tone={attention ? "amber" : "neutral"} />
      <Metric label="事件游标" value={dashboard.health.event_cursor} detail={`计划版本 ${dashboard.health.task.plan_version}`} tone="neutral" />
    </div>
    <section className="section-block"><SectionHeader title="交付流水线" subtitle="各阶段独立计算，不以单一百分比推断任务完成" /><div className="pipeline-grid">
      <PipelineStage label="Deliverable" complete={pipeline.required_deliverables_verified} total={pipeline.required_deliverables_total} />
      <PipelineStage label="WorkItem" complete={pipeline.work_items_accepted} total={pipeline.work_items_total} />
      <PipelineStage label="Review" complete={pipeline.reviews_approved} total={pipeline.reviews_required} />
      <PipelineStage label="Integration" complete={pipeline.integrations_completed} total={pipeline.integrations_required} />
      <PipelineStage label="Completion" complete={pipeline.completion_blocker_count === 0 ? 1 : 0} total={1} detail={pipeline.completion_blocker_count ? `${pipeline.completion_blocker_count} 个机械阻塞` : "机械条件已清空"} />
    </div>{dashboard.health.completion_signal_warnings.length > 0 && <div className="completion-blockers"><strong>Completion blockers</strong><div>{dashboard.health.completion_signal_warnings.filter((item) => item.count > 0).map((item) => <span key={item.code}><StateBadge value={item.severity === "critical" ? "blocked" : "pending"} />{item.code}<b>{item.count}</b></span>)}</div></div>}</section>
    <section className="section-block"><SectionHeader title="交付清单" subtitle={`${dashboard.deliverables.filter((item) => item.status === "verified" || item.status === "waived").length}/${dashboard.deliverables.length} 已满足`} /><div className="data-table compact deliverable-table"><div className="table-row header"><span>交付项</span><span>类型</span><span>状态</span><span>工作项</span><span>Evidence</span></div>{dashboard.deliverables.map((item) => <div className="table-row" key={item.id}><span className="item-title" title={item.description}><strong>{item.title}</strong><small>{item.key}{item.required ? " · required" : ""}</small></span><span>{item.kind}</span><span><StateBadge value={item.status} /></span><span>{item.work_item_ids.length}</span><span>{item.evidence_ids.length}</span></div>)}{dashboard.deliverables.length === 0 && <div className="table-empty"><Check size={18} />尚未定义交付项</div>}</div></section>
    <section className="section-block"><SectionHeader title="任务健康" subtitle="由 Rust 领域层实时计算" /><div className="health-grid">
      <HealthRow label="规划" value={dashboard.health.planning_required ? "需要继续规划" : "已覆盖"} warning={dashboard.health.planning_required} />
      <HealthRow label="完成队列" value={dashboard.health.completion_cohort_open ? "仍有邀请席位" : "已关闭"} warning={dashboard.health.completion_cohort_open} />
      <HealthRow label="推进能力" value={dashboard.health.no_progress_possible ? "无法推进" : "可继续推进"} warning={dashboard.health.no_progress_possible} />
      <HealthRow label="最终状态" value={dashboard.health.task.phase} warning={dashboard.health.completed_with_warnings} />
    </div></section>
    <section className="section-block"><SectionHeader title="最近活动" subtitle={`${dashboard.recent_events.events.length} 条事件`} /><EventList events={dashboard.recent_events.events.slice(-8).reverse()} /></section>
  </div>;
}

function WorkView({ dashboard }: { dashboard: TaskDashboard }) {
  const [mode, setMode] = useState<"list" | "graph">("list");
  const [query, setQuery] = useState("");
  const items = dashboard.work_items.filter((item) => item.title.toLowerCase().includes(query.toLowerCase()) || item.kind.includes(query.toLowerCase()));
  const visibleItemIds = new Set(items.map((item) => item.id));
  const nodes: Node[] = items.map((item, index) => ({ id: item.id, position: { x: (index % 3) * 280, y: Math.floor(index / 3) * 140 }, data: { label: item.title }, className: `flow-node state-${item.result_state}` }));
  const edges: Edge[] = dashboard.relations.filter((relation) => relation.relation_type === "depends_on" && visibleItemIds.has(relation.from_id) && visibleItemIds.has(relation.to_id)).map((relation) => ({ id: `${relation.from_id}-${relation.to_id}`, source: relation.to_id, target: relation.from_id, animated: true }));
  return <div className="stack work-view"><div className="view-controls"><div className="search-box"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="筛选工作项" aria-label="筛选工作项" /></div><div className="segmented" aria-label="工作项视图"><button className={mode === "list" ? "active" : ""} onClick={() => setMode("list")} aria-pressed={mode === "list"}><LayoutList size={14} />列表</button><button className={mode === "graph" ? "active" : ""} onClick={() => setMode("graph")} aria-pressed={mode === "graph"}><Network size={14} />依赖图</button></div></div>
    {mode === "list" ? <div className="data-table"><div className="table-row header"><span>工作项</span><span>类型</span><span>状态</span><span>执行者</span><span>风险</span></div>{items.map((item) => <div className="table-row" key={item.id}><span className="item-title" title={`${item.title}\n${item.description || "无附加描述"}`}><strong>{item.title}</strong><small>{item.description || "无附加描述"}</small></span><span>{item.kind}</span><span><StateBadge value={item.result_state} /></span><span title={item.active_claim?.session_id}>{item.active_claim ? shortId(item.active_claim.session_id) : "未认领"}</span><span><RiskBadge value={item.risk} /></span></div>)}{items.length === 0 && <div className="table-empty"><Search size={18} />没有匹配的工作项</div>}</div> : items.length ? <div className="graph-canvas"><ReactFlow nodes={nodes} edges={edges} fitView minZoom={0.35} maxZoom={1.6}><Background variant={BackgroundVariant.Dots} gap={20} size={1} /><Controls /></ReactFlow></div> : <div className="graph-empty"><Search size={20} /><span>没有匹配的工作项</span></div>}
    <section className="section-block"><SectionHeader title="隔离探索" subtitle={`${dashboard.speculative_work.length} 个 speculative 结果，不占用共享 Claim`} /><div className="data-table compact"><div className="table-row header"><span>探索</span><span>分支</span><span>状态</span><span>基线</span><span>Plan</span></div>{dashboard.speculative_work.map((item) => <div className="table-row" key={item.id}><span className="item-title" title={item.assumptions.join("\n")}><strong>{item.title}</strong><small>{item.summary || `${item.assumptions.length} 个假设`}</small></span><span title={item.branch}>{item.branch}</span><span><StateBadge value={item.status} /></span><span title={item.base_commit}>{shortId(item.base_commit, 10)}</span><span>{item.base_plan_version}</span></div>)}{dashboard.speculative_work.length === 0 && <div className="table-empty"><CircleDot size={18} />尚无隔离探索成果</div>}</div></section>
  </div>;
}

function AgentsView({ dashboard }: { dashboard: TaskDashboard }) {
  const activeWaits = dashboard.waits.filter((wait) => wait.status === "active");
  const externalWaits = dashboard.external_waits.filter((wait) => wait.status === "active");
  return <div className="stack"><section className="section-block"><SectionHeader title="参与者" subtitle={`${dashboard.participants.length} 个已接入会话`} /><div className="agent-grid">{dashboard.participants.map((agent) => { const wait = activeWaits.find((item) => item.session_id === agent.session_id); const external = externalWaits.find((item) => item.session_id === agent.session_id); return <div className="agent-card" key={agent.id}><div className="agent-avatar"><Bot size={18} /></div><div className="agent-main"><div><strong>{agent.label}</strong><StateBadge value={agent.state} /></div><p>{agent.model || "未声明模型"}</p><dl><dt>分支</dt><dd>{agent.branch || "-"}</dd><dt>最后活动</dt><dd>{relativeMicros(agent.last_seen_at_micros)}</dd>{wait && <><dt>WorkItem等待</dt><dd>{wait.mode} · {wait.targets.map((target) => shortId(target)).join(", ")}</dd></>}{external && <><dt>外部等待</dt><dd title={external.detector.path}>{external.detector_type} · {external.detector.path}</dd><dt>下次复查</dt><dd>{relativeFutureMicros(external.next_reassessment_at_micros)}</dd><dt>观测次数</dt><dd>{external.observation_count}</dd>{external.deadline_at_micros && <><dt>Deadline</dt><dd>{relativeFutureMicros(external.deadline_at_micros)}</dd></>}</>}</dl></div></div>; })}</div></section>
    <section className="section-block"><SectionHeader title="邀请席位" subtitle={`${dashboard.invitations.filter((item) => item.state === "pending").length} 个等待接入`} /><div className="data-table compact"><div className="table-row header"><span>批次 / 席位</span><span>类型</span><span>状态</span><span>到期</span><span>阻塞完成</span></div>{dashboard.invitations.map((invite) => <div className="table-row" key={invite.id}><span>{shortId(invite.batch_id)} / {shortId(invite.id)}</span><span>{invite.participant_kind}</span><span><StateBadge value={invite.state} /></span><span>{relativeMicros(invite.expires_at_micros)}</span><span>{invite.blocks_completion ? "是" : "否"}</span></div>)}</div></section></div>;
}

const chatKindLabels: Record<ChatMessageKind, string> = {
  progress: "进展", intent: "意图", question: "问题", decision: "决定",
  warning: "风险", handoff: "交接", note: "消息",
};

function ChatView({ dashboard, workspace, taskId, onSent }: { dashboard: TaskDashboard; workspace: WorkspaceView; taskId: string; onSent: () => void }) {
  const [body, setBody] = useState("");
  const [kind, setKind] = useState<ChatMessageKind>("note");
  const [replyTo, setReplyTo] = useState<ChatMessage>();
  const [error, setError] = useState<GuiError>();
  const send = useMutation({
    mutationFn: () => api.sendChat(workspace.root, taskId, { kind, body: body.trim(), replyToMessageId: replyTo?.id }),
    onSuccess: () => { setBody(""); setReplyTo(undefined); setError(undefined); onSent(); },
    onError: (value) => setError(normalizeError(value)),
  });
  const byId = new Map(dashboard.chat_messages.map((message) => [message.id, message]));
  const submit = () => { if (body.trim() && !send.isPending) send.mutate(); };
  return <section className="chat-shell">
    <div className="chat-heading"><div><h2>任务群聊</h2><p>{dashboard.chat_messages.length} 条消息 · Event 游标 {dashboard.health.event_cursor}</p></div><MessageCircle size={18} /></div>
    <div className="chat-feed">
      {dashboard.chat_messages.map((message) => {
        const replied = message.reply_to_message_id ? byId.get(message.reply_to_message_id) : undefined;
        return <div className={`chat-message sender-${message.sender_type}`} key={message.id}>
          <div className="chat-avatar">{message.sender_type === "agent" ? <Bot size={16} /> : <Users size={16} />}</div>
          <div className="chat-message-main">
            <div className="chat-meta"><strong>{message.sender_label}</strong><span className={`chat-kind kind-${message.kind}`}>{chatKindLabels[message.kind]}</span><time>{relativeMicros(message.created_at_micros)}</time></div>
            {replied && <div className="chat-reply-preview"><Reply size={12} /><span><strong>{replied.sender_label}</strong> {replied.body}</span></div>}
            <p>{message.body}</p>
            {message.related_entities.length > 0 && <div className="chat-entities">{message.related_entities.map((entity) => <code key={`${entity.entity_type}:${entity.entity_id}`}>{entity.entity_type}:{shortId(entity.entity_id)}</code>)}</div>}
          </div>
          <button className="icon-button chat-reply" onClick={() => setReplyTo(message)} title="回复" aria-label={`回复 ${message.sender_label}`}><Reply size={14} /></button>
        </div>;
      })}
      {dashboard.chat_messages.length === 0 && <div className="chat-empty"><MessageCircle size={22} /><span>还没有群聊消息</span></div>}
    </div>
    <div className="chat-composer">
      {replyTo && <div className="composer-reply"><Reply size={12} /><span>回复 <strong>{replyTo.sender_label}</strong>：{replyTo.body}</span><button className="icon-button" onClick={() => setReplyTo(undefined)} title="取消回复"><X size={13} /></button></div>}
      {error && <ErrorCard error={error} />}
      <div className="composer-row">
        <select value={kind} onChange={(event) => setKind(event.target.value as ChatMessageKind)} aria-label="消息类型">{Object.entries(chatKindLabels).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select>
        <textarea value={body} onChange={(event) => setBody(event.target.value)} onKeyDown={(event) => { if ((event.ctrlKey || event.metaKey) && event.key === "Enter") submit(); }} rows={2} maxLength={8000} placeholder="发送消息到任务群聊" aria-label="群聊消息" />
        <button className="primary-button chat-send" disabled={!body.trim() || send.isPending} onClick={submit} title="发送"><Send size={16} /></button>
      </div>
    </div>
  </section>;
}

function ActivityView({ dashboard }: { dashboard: TaskDashboard }) { return <section className="section-block full"><SectionHeader title="事件流" subtitle={`游标 ${dashboard.health.event_cursor}`} /><EventList events={[...dashboard.recent_events.events].reverse()} expanded /></section>; }

function DiagnosticsView({ workspace, taskId }: { workspace: WorkspaceView; taskId?: string }) {
  const doctor = useQuery({ queryKey: ["doctor", workspace.root], queryFn: () => api.doctor(workspace.root) });
  const collaboration = useQuery({ queryKey: ["collaboration-diagnostics", workspace.root, taskId], queryFn: () => api.collaborationDiagnostics(workspace.root, taskId!), enabled: Boolean(taskId), refetchInterval: 5000 });
  const logs = useQuery({ queryKey: ["logs"], queryFn: () => api.logs(100), refetchInterval: 5000 });
  return <div className="stack">{taskId && <section className="section-block"><SectionHeader title="协作诊断" subtitle={collaboration.data ? `${collaboration.data.critical_count} 个严重问题 · ${collaboration.data.warning_count} 个警告` : "分析责任链、阶段交接与 Git 完整性"} action={<button className="icon-button" onClick={() => collaboration.refetch()} title="重新运行协作诊断" aria-label="重新运行协作诊断"><RefreshCw size={15} /></button>} />{collaboration.isLoading ? <LoadingPanel /> : <div className="diagnostic-list">{collaboration.data?.findings?.map((item: any, index: number) => <div className="diagnostic-row" key={`${item.code}:${item.entity_id || index}`}><span className={`diagnostic-icon ${item.status}`}>{item.status === "pass" ? <Check size={14} /> : <AlertTriangle size={14} />}</span><span><strong>{item.code}</strong><small>{item.message}</small></span></div>)}</div>}</section>}<section className="section-block"><SectionHeader title="环境诊断" subtitle={workspace.database_path} action={<button className="icon-button" onClick={() => doctor.refetch()} title="重新运行诊断" aria-label="重新运行诊断"><RefreshCw size={15} /></button>} />{doctor.isLoading ? <LoadingPanel /> : <div className="diagnostic-list">{doctor.data?.diagnostics?.map((item: any, index: number) => <div className="diagnostic-row" key={`${item.code}:${item.path || index}`}><span className={`diagnostic-icon ${item.status}`}>{item.status === "pass" ? <Check size={14} /> : <AlertTriangle size={14} />}</span><span><strong>{item.code}</strong><small>{item.message}</small></span></div>)}</div>}</section><section className="section-block"><SectionHeader title="运行日志" subtitle="最近 100 条 JSONL 记录" /><div className="log-view">{logs.data?.entries?.slice(-100).reverse().map((entry: any, index: number) => <div key={index}><time>{entry.record.timestamp || ""}</time><code>{entry.record.event || entry.record.message || JSON.stringify(entry.record)}</code></div>)}</div></section></div>;
}

function Inspector({ dashboard, task, workspace, onChanged }: { dashboard: TaskDashboard; task?: TaskSummary; workspace: WorkspaceView; onChanged: () => void }) {
  const transition = useMutation({ mutationFn: (action: string) => api.transition(workspace.root, task!.id, action), onSuccess: onChanged });
  return <aside className="inspector"><div className="inspector-heading"><div><span className="eyebrow">Task Inspector</span><h2>运行状态</h2></div><Settings size={17} /></div><div className="inspector-section"><KeyValue label="Task ID" value={shortId(dashboard.details.task.id)} mono /><KeyValue label="目标分支" value={dashboard.details.task.target_branch || "当前分支"} /><KeyValue label="自治策略" value={dashboard.details.task.autonomy_policy} /><KeyValue label="完成 Epoch" value={String(dashboard.details.task.completion_epoch)} /><KeyValue label="Plan Version" value={String(dashboard.details.task.plan_version)} /></div><div className="inspector-section"><h3>完成条件</h3>{dashboard.details.criteria.map((criterion) => <div className="criterion" key={criterion.id}><span>{criterion.kind}</span><p>{criterion.content}</p></div>)}</div><div className="inspector-actions">{task?.phase === "active" && <button className="secondary-button full" disabled={transition.isPending} onClick={() => transition.mutate("pause")}><Pause size={15} /> 暂停任务</button>}{task?.phase === "paused" && <button className="primary-button full" disabled={transition.isPending} onClick={() => transition.mutate("resume")}><Play size={15} /> 恢复任务</button>}</div></aside>;
}

function LaunchDialog({ open: isOpen, onOpenChange, workspace, onPublished }: { open: boolean; onOpenChange: (open: boolean) => void; workspace: WorkspaceView; onPublished: (taskId: string) => void }) {
  const [input, setInput] = useState<LaunchInput>({ title: "", objective: "", profile: "normal" });
  const [preflight, setPreflight] = useState<LaunchPreflight>();
  const [error, setError] = useState<GuiError>();
  const preflightMutation = useMutation({ mutationFn: () => api.preflight(workspace.root, input), onSuccess: setPreflight, onError: (value) => setError(normalizeError(value)) });
  const launchMutation = useMutation({ mutationFn: () => api.launch(workspace.root, input), onSuccess: (result) => { onPublished(result.task.task.id); onOpenChange(false); setPreflight(undefined); setInput({ title: "", objective: "", profile: "normal" }); }, onError: (value) => setError(normalizeError(value)) });
  return <Dialog.Root open={isOpen} onOpenChange={(value) => { onOpenChange(value); if (!value) { setError(undefined); setPreflight(undefined); } }}><Dialog.Portal><Dialog.Overlay className="dialog-overlay" /><Dialog.Content className="dialog-content launch-sheet"><div className="dialog-heading"><div><Dialog.Title>发布新任务</Dialog.Title><Dialog.Description>使用项目 Profile 创建任务和 Agent 邀请。</Dialog.Description></div><Dialog.Close className="icon-button" title="关闭" aria-label="关闭发布任务面板"><X size={17} /></Dialog.Close></div>{!preflight ? <div className="form-stack"><label>任务标题<input value={input.title} onChange={(event) => setInput({ ...input, title: event.target.value })} placeholder="例如：实现用户认证" autoFocus /></label><label>任务目标<textarea value={input.objective} onChange={(event) => setInput({ ...input, objective: event.target.value })} placeholder="描述 Agent 需要共同交付的结果、边界和验收要求" rows={8} /></label><div className="form-grid"><label>Profile<input value={input.profile} onChange={(event) => setInput({ ...input, profile: event.target.value })} /></label><label>Agent 数量<input type="number" min={1} max={256} value={input.agents || ""} placeholder="默认" onChange={(event) => setInput({ ...input, agents: event.target.value ? Number(event.target.value) : undefined })} /></label></div>{error && <ErrorCard error={error} />}<button className="primary-button full large" disabled={!input.title.trim() || !input.objective.trim() || preflightMutation.isPending} onClick={() => { setError(undefined); preflightMutation.mutate(); }}>{preflightMutation.isPending ? <LoaderCircle className="spin" size={16} /> : <ShieldCheck size={16} />} 发布前检查</button></div> : <div className="preflight"><div className="preflight-status"><Check size={18} /><span><strong>可以发布</strong><small>Baseline 与 Workspace 已验证</small></span></div><KeyValue label="目标分支" value={preflight.target_branch} /><KeyValue label="Baseline" value={shortId(preflight.baseline_commit, 12)} mono /><KeyValue label="Agent" value={String(preflight.agents)} /><KeyValue label="有效期" value={formatDuration(preflight.expires_in_seconds)} />{preflight.dirty_paths.length > 0 && <div className="warning-box"><AlertTriangle size={16} />存在 {preflight.dirty_paths.length} 个未提交路径，默认不会发布。</div>}{error && <ErrorCard error={error} />}<div className="button-row"><button className="secondary-button" onClick={() => setPreflight(undefined)}>返回编辑</button><button className="primary-button" disabled={launchMutation.isPending || preflight.dirty_paths.length > 0} onClick={() => launchMutation.mutate()}>{launchMutation.isPending ? <LoaderCircle className="spin" size={16} /> : <Plus size={16} />} 确认发布</button></div></div>}</Dialog.Content></Dialog.Portal></Dialog.Root>;
}

function PromptDialog({ workspace, taskId, onClose }: { workspace: WorkspaceView; taskId: string; onClose: () => void }) {
  const prompts = useQuery({ queryKey: ["prompts", workspace.root, taskId], queryFn: () => api.prompts(workspace.root, taskId) });
  const [selected, setSelected] = useState<InvitationManifest>();
  const [error, setError] = useState<GuiError>();
  async function loadPrompt(inviteId: string) {
    setError(undefined);
    try { setSelected(await api.prompt(workspace.root, inviteId)); }
    catch (value) { setError(normalizeError(value)); }
  }
  async function copyPrompt(item: InvitationManifest) {
    setError(undefined);
    try {
      const loaded = item.prompt ? item : await api.prompt(workspace.root, item.invite_id);
      await writeText(loaded.prompt || "");
      setSelected(loaded);
    } catch (value) { setError(normalizeError(value)); }
  }
  return <Dialog.Root open onOpenChange={(value) => !value && onClose()}><Dialog.Portal><Dialog.Overlay className="dialog-overlay" /><Dialog.Content className="dialog-content prompt-dialog"><div className="dialog-heading"><div><Dialog.Title>Agent 提示词</Dialog.Title><Dialog.Description>每个席位使用独立 Invitation，提示词按需读取。</Dialog.Description></div><Dialog.Close className="icon-button" title="关闭" aria-label="关闭 Agent 提示词"><X size={17} /></Dialog.Close></div><div className="prompt-layout"><div className="prompt-list">{prompts.data?.map((prompt) => <button key={prompt.invite_id} className={selected?.invite_id === prompt.invite_id ? "prompt-item active" : "prompt-item"} onClick={() => loadPrompt(prompt.invite_id)} aria-pressed={selected?.invite_id === prompt.invite_id}><span><Bot size={16} /><strong>Agent {prompt.batch_index}</strong></span><StateBadge value={prompt.status} /></button>)}</div><div className="prompt-preview">{error ? <ErrorCard error={error} /> : selected ? <><div className="prompt-toolbar"><span>{shortId(selected.invite_id)}</span><button className="primary-button" onClick={() => copyPrompt(selected)}><Copy size={15} />复制提示词</button></div><pre>{selected.prompt}</pre></> : <div className="empty-inline"><Clipboard size={24} /><p>选择一个 Agent 席位查看提示词</p></div>}</div></div></Dialog.Content></Dialog.Portal></Dialog.Root>;
}

function Metric({ label, value, detail, tone }: { label: string; value: string | number; detail: string; tone: string }) { return <div className={`metric ${tone}`}><span>{label}</span><strong>{value}</strong><small>{detail}</small></div>; }
function PipelineStage({ label, complete, total, detail }: { label: string; complete: number; total: number; detail?: string }) { const pending = Math.max(0, total - complete); const ready = total === 0 || pending === 0; return <div className="pipeline-stage"><span className={ready ? "pipeline-icon ready" : "pipeline-icon"}>{ready ? <Check size={15} /> : <CircleDot size={15} />}</span><div><small>{label}</small><strong>{complete} / {total}</strong><p>{detail || (ready ? "当前阶段已满足" : `${pending} 项待处理`)}</p></div></div>; }
function HealthRow({ label, value, warning }: { label: string; value: string; warning?: boolean }) { return <div className="health-row"><span className={warning ? "health-icon warning" : "health-icon"}>{warning ? <AlertTriangle size={15} /> : <Check size={15} />}</span><span><small>{label}</small><strong>{value}</strong></span></div>; }
function SectionHeader({ title, subtitle, action }: { title: string; subtitle?: string; action?: React.ReactNode }) { return <div className="section-header"><div><h2>{title}</h2>{subtitle && <p>{subtitle}</p>}</div>{action}</div>; }
function EventList({ events, expanded = false }: { events: TaskDashboard["recent_events"]["events"]; expanded?: boolean }) { return <div className={expanded ? "event-list expanded" : "event-list"}>{events.length ? events.map((event) => <div className="event-row" key={event.sequence}><span className="event-icon"><Activity size={14} /></span><span><strong>{event.event_type}</strong><small>{event.actor_type}{event.actor_id ? ` · ${shortId(event.actor_id)}` : ""}</small></span><time>{relativeMicros(event.created_at_micros)}</time>{expanded && <code>{event.entity_type}{event.entity_id ? `:${shortId(event.entity_id)}` : ""}</code>}</div>) : <div className="empty-inline">尚无事件</div>}</div>; }
function StatusDot({ phase }: { phase: string }) { return <span className={`status-dot ${phase}`} />; }
function PhaseBadge({ phase }: { phase: string }) { return <span className={`phase-badge ${phase}`}><StatusDot phase={phase} />{phase}</span>; }
function StateBadge({ value }: { value: string }) { return <span className={`state-badge state-${value}`}>{value.replaceAll("_", " ")}</span>; }
function RiskBadge({ value }: { value: string }) { return <span className={`risk risk-${value}`}>{value}</span>; }
function KeyValue({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) { return <div className="key-value"><span>{label}</span><strong className={mono ? "mono" : ""} title={value}>{value}</strong></div>; }
function EmptyTask({ onLaunch }: { onLaunch: () => void }) { return <div className="empty-state"><div><Plus size={24} /></div><h2>发布第一个任务</h2><p>创建任务后，SessionBridge 会生成可发送给 Agent 的独立提示词。</p><button className="primary-button" onClick={onLaunch}><Plus size={16} />发布任务</button></div>; }
function LoadingPanel() { return <div className="loading-panel"><LoaderCircle className="spin" size={20} />同步 Workspace 状态</div>; }
function ErrorPanel({ error, onRetry }: { error: GuiError; onRetry: () => void }) { return <div className="error-panel"><ErrorCard error={error} /><button className="secondary-button" onClick={onRetry}><RefreshCw size={15} />重试</button></div>; }
function ErrorCard({ error }: { error: GuiError }) { return <div className="error-card"><AlertTriangle size={16} /><span><strong>{error.code}</strong><p>{error.message}</p><small>Operation {error.operation_id}</small></span></div>; }
function shortId(value: string, size = 8) { return value.length > size ? value.slice(0, size) : value; }
function relativeMicros(value: number) { const delta = Date.now() - value / 1000; if (delta < 0) return "未来"; if (delta < 60_000) return `${Math.max(1, Math.floor(delta / 1000))} 秒前`; if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`; if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`; return new Date(value / 1000).toLocaleDateString(); }
function relativeFutureMicros(value: number) { const delta = value / 1000 - Date.now(); if (delta <= 0) return "等待复查"; if (delta < 60_000) return `${Math.ceil(delta / 1000)} 秒后`; if (delta < 3_600_000) return `${Math.ceil(delta / 60_000)} 分钟后`; return `${Math.ceil(delta / 3_600_000)} 小时后`; }
function formatDuration(seconds: number) { return seconds % 3600 === 0 ? `${seconds / 3600} 小时` : `${Math.round(seconds / 60)} 分钟`; }
