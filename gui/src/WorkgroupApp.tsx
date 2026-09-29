import {
    useEffect,
    useRef,
    useState,
    type FormEvent,
    type ReactNode,
} from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Dialog from "@radix-ui/react-dialog";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
    ArrowDown,
    ArrowLeft,
    ArrowUp,
    Check,
    ChevronDown,
    FolderOpen,
    ListTodo,
    LoaderCircle,
    MoreHorizontal,
    Pause,
    Play,
    Plus,
    Reply,
    RotateCw,
    Settings2,
    Square,
    Trash2,
    Users,
    X,
} from "lucide-react";
import {
    call,
    errorText,
    mergeHistory,
    labels,
    type History,
    type HistoryCache,
    type Member,
    type Provider,
    type Room,
    type Snapshot,
    type Workspace,
} from "./bridge";

function IconButton({
    label,
    children,
    ...props
}: {
    label: string;
    children: ReactNode;
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
    return (
        <button
            type="button"
            className="icon"
            aria-label={label}
            title={label}
            {...props}
        >
            {children}
        </button>
    );
}
function Sheet({
    title,
    open,
    onClose,
    children,
}: {
    title: string;
    open: boolean;
    onClose: () => void;
    children: ReactNode;
}) {
    return (
        <Dialog.Root
            open={open}
            onOpenChange={(value) => {
                if (!value) onClose();
            }}
        >
            <Dialog.Portal>
                <Dialog.Overlay className="sheet-overlay" />
                <Dialog.Content className="sheet" aria-describedby={undefined}>
                    <header>
                        <Dialog.Title>{title}</Dialog.Title>
                        <Dialog.Close asChild>
                            <IconButton label="关闭">
                                <X size={19} />
                            </IconButton>
                        </Dialog.Close>
                    </header>
                    {children}
                </Dialog.Content>
            </Dialog.Portal>
        </Dialog.Root>
    );
}
function readRecent(): string[] {
    try {
        const value: unknown = JSON.parse(
            localStorage.getItem("bridge-workspaces") ?? "[]",
        );
        return Array.isArray(value)
            ? value.filter((v): v is string => typeof v === "string")
            : [];
    } catch {
        return [];
    }
}

export default function WorkgroupApp() {
    const query = useQueryClient();
    const [recent, setRecent] = useState(readRecent);
    const [workspace, setWorkspace] = useState<Workspace>();
    const [room, setRoom] = useState<string>();
    const [sidebar, setSidebar] = useState(false);
    const [panel, setPanel] = useState<
        "members" | "work" | "workspace" | "new" | "actions" | null
    >(null);
    const [error, setError] = useState("");
    const [pending, setPending] = useState(false);
    const closing = useRef(false);
    const [newTitle, setNewTitle] = useState("");
    const root = workspace?.root ?? "";
    const rooms = useQuery({
        queryKey: ["bridge", root, "rooms"],
        queryFn: () => call<Room[]>(root, "room.list"),
        enabled: !!root,
    });
    const snapshot = useQuery({
        queryKey: ["bridge", root, room, "snapshot"],
        queryFn: () => call<Snapshot>(root, "room.get", { room }),
        enabled: !!root && !!room,
    });

    async function perform(action: () => Promise<unknown>) {
        setPending(true);
        setError("");
        try {
            await action();
            await query.invalidateQueries({ queryKey: ["bridge", root] });
        } catch (e) {
            setError(errorText(e));
        } finally {
            setPending(false);
        }
    }
    async function openWorkspace(path: string) {
        const next = await call<Workspace>(path, "workspace.get");
        setWorkspace(next);
        setRoom(undefined);
        setPanel(null);
        setSidebar(false);
        setRecent((previous) => {
            const paths = [
                next.root,
                ...previous.filter((p) => p !== next.root),
            ].slice(0, 12);
            localStorage.setItem("bridge-workspaces", JSON.stringify(paths));
            return paths;
        });
    }
    async function choose() {
        const path = await open({
            directory: true,
            multiple: false,
            title: "打开项目",
        });
        if (path) await openWorkspace(path);
    }
    useEffect(() => {
        if (recent[0]) void perform(() => openWorkspace(recent[0]));
    }, []);
    useEffect(() => {
        if (!room && rooms.data?.length) setRoom(rooms.data[0].id);
    }, [room, rooms.data]);
    useEffect(() => {
        if (!root) return;
        let cancelled = false;
        let timer: ReturnType<typeof setTimeout> | undefined;
        async function watch(revision = 0) {
            try {
                const next = await call<{
                    revision: number;
                    stopping: boolean;
                }>(root, "watch", {
                    after: revision,
                });
                if (cancelled || closing.current) return;
                if (next.stopping) {
                    setError("后台已停止，重新打开项目后可恢复。");
                    return;
                }
                await query.invalidateQueries({ queryKey: ["bridge", root] });
                timer = setTimeout(() => void watch(next.revision), 150);
            } catch (e) {
                if (cancelled || closing.current) return;
                setError(errorText(e));
                timer = setTimeout(() => void watch(revision), 3000);
            }
        }
        void watch();
        return () => {
            cancelled = true;
            clearTimeout(timer);
        };
    }, [root, query]);

    const data = snapshot.data;
    async function initialize() {
        if (!data) return;
        const targets = data.members.filter(
            (m) =>
                m.role !== "boss" &&
                ["configured", "offline"].includes(m.state),
        );
        const results = await Promise.allSettled(
            targets.map((m) =>
                call(root, "member.initialize", { member: m.id }),
            ),
        );
        const errors = results.flatMap((r, i) =>
            r.status === "rejected"
                ? [`${targets[i].name}：${errorText(r.reason)}`]
                : [],
        );
        if (errors.length) throw new Error(errors.join("\n"));
    }
    async function transition(state: string) {
        if (!data) return;
        await call(root, "room.transition", { room, state });
        setPanel(null);
    }
    if (!workspace)
        return (
            <main className="welcome">
                <div className="welcome-mark">
                    <FolderOpen size={34} />
                </div>
                <h1>SessionBridge</h1>
                <button
                    className="primary"
                    disabled={pending}
                    onClick={() => void perform(choose)}
                >
                    <FolderOpen size={18} />
                    打开项目
                </button>
                {recent.length > 0 && (
                    <div className="recent">
                        {recent.map((path) => (
                            <button
                                key={path}
                                onClick={() =>
                                    void perform(() => openWorkspace(path))
                                }
                            >
                                <span>{path.split("/").pop()}</span>
                                <small>{path}</small>
                            </button>
                        ))}
                    </div>
                )}
                {error && (
                    <p role="alert" className="error">
                        {error}
                    </p>
                )}
            </main>
        );
    return (
        <div className={`workbench ${sidebar ? "show-sidebar" : ""}`}>
            <aside className="workspace-sidebar">
                <button
                    className="workspace-picker"
                    onClick={() => setPanel("workspace")}
                >
                    <FolderOpen size={20} />
                    <strong>{workspace.name}</strong>
                    <ChevronDown size={15} />
                </button>
                <div className="sidebar-heading">
                    <span>工作群</span>
                    <IconButton
                        label="新建工作群"
                        onClick={() => setPanel("new")}
                    >
                        <Plus size={19} />
                    </IconButton>
                </div>
                <nav aria-label="工作群">
                    {rooms.data?.map((r) => (
                        <button
                            key={r.id}
                            className={`room-link ${room === r.id ? "selected" : ""}`}
                            aria-current={room === r.id ? "page" : undefined}
                            onClick={() => {
                                setRoom(r.id);
                                setSidebar(false);
                            }}
                        >
                            <span className={`room-dot ${r.state}`} />
                            <span>
                                <strong>{r.title}</strong>
                                <small>{labels[r.state]}</small>
                            </span>
                            {r.state === "completed" && <Check size={15} />}
                        </button>
                    ))}
                </nav>
                {!rooms.data?.length && (
                    <button
                        className="empty-create"
                        onClick={() => setPanel("new")}
                    >
                        <Plus size={20} />
                        创建第一个工作群
                    </button>
                )}
                <footer>
                    <small>{rooms.data?.length ?? 0} 个工作群</small>
                    <IconButton
                        label="工作区设置"
                        onClick={() => setPanel("workspace")}
                    >
                        <Settings2 size={18} />
                    </IconButton>
                </footer>
            </aside>
            <main className="conversation">
                <header className="conversation-header">
                    <IconButton
                        label="工作群列表"
                        className="icon mobile-nav"
                        onClick={() => setSidebar(true)}
                    >
                        <ArrowLeft size={20} />
                    </IconButton>
                    <div className="conversation-title">
                        <h1>{data?.room.title ?? "工作群"}</h1>
                        <small>
                            {data
                                ? `${labels[data.room.state]} · ${data.members.filter((m) => m.state !== "removed").length} 位成员`
                                : workspace.name}
                        </small>
                    </div>
                    <div className="toolbar">
                        <IconButton
                            label="工作项"
                            onClick={() => setPanel("work")}
                            disabled={!data}
                        >
                            <ListTodo size={20} />
                        </IconButton>
                        <IconButton
                            label="成员"
                            onClick={() => setPanel("members")}
                            disabled={!data}
                        >
                            <Users size={20} />
                        </IconButton>
                        {data && (
                            <IconButton
                                label={
                                    data.room.state === "paused"
                                        ? "恢复工作群"
                                        : "暂停工作群"
                                }
                                onClick={() =>
                                    void perform(() =>
                                        transition(
                                            data.room.state === "paused"
                                                ? "active"
                                                : "paused",
                                        ),
                                    )
                                }
                                disabled={
                                    pending ||
                                    !["active", "paused"].includes(
                                        data.room.state,
                                    )
                                }
                            >
                                {data.room.state === "paused" ? (
                                    <Play size={18} />
                                ) : (
                                    <Pause size={18} />
                                )}
                            </IconButton>
                        )}
                        <IconButton
                            label="更多操作"
                            onClick={() => setPanel("actions")}
                            disabled={!data}
                        >
                            <MoreHorizontal size={20} />
                        </IconButton>
                    </div>
                </header>
                {(error || rooms.error || snapshot.error) && (
                    <div className="error-bar" role="alert">
                        <span>
                            {error || errorText(rooms.error || snapshot.error)}
                        </span>
                        <IconButton
                            label="关闭错误"
                            onClick={() => setError("")}
                        >
                            <X size={16} />
                        </IconButton>
                    </div>
                )}
                {data ? (
                    <>
                        {data.members.some(
                            (m) =>
                                m.role !== "boss" &&
                                ["configured", "offline"].includes(m.state),
                        ) &&
                            data.room.state === "active" && (
                                <div className="setup-bar">
                                    <span>
                                        {
                                            data.members.filter(
                                                (m) =>
                                                    m.role !== "boss" &&
                                                    ["ready", "busy"].includes(
                                                        m.state,
                                                    ),
                                            ).length
                                        }{" "}
                                        位 Agent 已就绪
                                    </span>
                                    <button onClick={() => setPanel("members")}>
                                        配置成员
                                    </button>
                                    <button
                                        className="primary compact"
                                        onClick={() => void perform(initialize)}
                                        disabled={pending}
                                    >
                                        {pending ? (
                                            <LoaderCircle
                                                className="spin"
                                                size={15}
                                            />
                                        ) : (
                                            <Play size={14} />
                                        )}
                                        初始化
                                    </button>
                                </div>
                            )}
                        {data.approvals.map((a) => (
                            <div key={a.id} className="approval">
                                <strong>
                                    {
                                        data.members.find(
                                            (m) => m.id === a.member_id,
                                        )?.name
                                    }{" "}
                                    请求授权
                                </strong>
                                <details>
                                    <summary>查看操作</summary>
                                    <pre>
                                        {JSON.stringify(a.details, null, 2)}
                                    </pre>
                                </details>
                                <div>
                                    <button
                                        onClick={() =>
                                            void perform(() =>
                                                call(root, "approval.decide", {
                                                    approval: a.id,
                                                    decision: "deny",
                                                }),
                                            )
                                        }
                                    >
                                        拒绝
                                    </button>
                                    <button
                                        className="primary"
                                        onClick={() =>
                                            void perform(() =>
                                                call(root, "approval.decide", {
                                                    approval: a.id,
                                                    decision: "allow",
                                                }),
                                            )
                                        }
                                    >
                                        允许此次
                                    </button>
                                </div>
                            </div>
                        ))}
                        <Chat key={room} root={root} snapshot={data} />
                    </>
                ) : (
                    <div className="conversation-empty">
                        <Users size={36} />
                        <p>
                            {rooms.isLoading
                                ? "正在打开…"
                                : "新建工作群，开始协作"}
                        </p>
                        <button
                            className="primary"
                            onClick={() => setPanel("new")}
                        >
                            <Plus size={17} />
                            新建工作群
                        </button>
                    </div>
                )}
            </main>
            <Sheet
                title="新建工作群"
                open={panel === "new"}
                onClose={() => setPanel(null)}
            >
                <form
                    className="sheet-form"
                    onSubmit={(e) => {
                        e.preventDefault();
                        void perform(async () => {
                            const next = await call<Room>(root, "room.create", {
                                title: newTitle.trim() || "新的任务",
                            });
                            setRoom(next.id);
                            setNewTitle("");
                            setPanel("members");
                        });
                    }}
                >
                    <label>
                        名称
                        <input
                            autoFocus
                            value={newTitle}
                            onChange={(e) => setNewTitle(e.target.value)}
                            placeholder="这次要完成什么？"
                            maxLength={120}
                        />
                    </label>
                    <button className="primary" disabled={pending}>
                        创建
                    </button>
                </form>
            </Sheet>
            <Sheet
                title="项目"
                open={panel === "workspace"}
                onClose={() => setPanel(null)}
            >
                <div className="sheet-form">
                    <button
                        className="primary"
                        onClick={() => void perform(choose)}
                    >
                        <FolderOpen size={18} />
                        打开其他项目
                    </button>
                    {recent.map((path) => (
                        <div className="recent-row" key={path}>
                            <button
                                onClick={() =>
                                    void perform(() => openWorkspace(path))
                                }
                            >
                                <strong>{path.split("/").pop()}</strong>
                                <small>{path}</small>
                            </button>
                            <IconButton
                                label="从最近项目移除"
                                onClick={() =>
                                    setRecent((previous) => {
                                        const next = previous.filter(
                                            (p) => p !== path,
                                        );
                                        localStorage.setItem(
                                            "bridge-workspaces",
                                            JSON.stringify(next),
                                        );
                                        return next;
                                    })
                                }
                            >
                                <X size={16} />
                            </IconButton>
                        </div>
                    ))}
                    <p className="muted">关闭窗口后，工作群继续在后台运行。</p>
                    <button
                        onClick={() =>
                            void perform(async () => {
                                closing.current = true;
                                try {
                                    await call(root, "service.shutdown");
                                    await getCurrentWindow().close();
                                } catch (e) {
                                    closing.current = false;
                                    throw e;
                                }
                            })
                        }
                    >
                        停止后台并退出
                    </button>
                </div>
            </Sheet>
            <Sheet
                title="成员"
                open={panel === "members"}
                onClose={() => setPanel(null)}
            >
                {data && <Members root={root} data={data} perform={perform} />}
            </Sheet>
            <Sheet
                title="工作项"
                open={panel === "work"}
                onClose={() => setPanel(null)}
            >
                <div className="sheet-form">
                    {data?.work.length ? (
                        data.work.map((w) => (
                            <article className="work-item" key={w.id}>
                                <header>
                                    <strong>{w.title}</strong>
                                    <span>{labels[w.state]}</span>
                                </header>
                                <small>
                                    {
                                        data.members.find(
                                            (m) => m.id === w.owner,
                                        )?.name
                                    }
                                </small>
                                {w.evidence && <p>{w.evidence}</p>}
                                {w.commit_id && (
                                    <code>{w.commit_id.slice(0, 12)}</code>
                                )}
                            </article>
                        ))
                    ) : (
                        <p className="muted">协调者分配的工作会出现在这里。</p>
                    )}
                </div>
            </Sheet>
            <Sheet
                title="工作群"
                open={panel === "actions"}
                onClose={() => setPanel(null)}
            >
                <div className="sheet-form">
                    {data &&
                    ["completed", "cancelled"].includes(data.room.state) ? (
                        <button
                            onClick={() =>
                                void perform(() => transition("archived"))
                            }
                        >
                            归档工作群
                        </button>
                    ) : (
                        <button
                            onClick={() =>
                                void perform(() => transition("cancelled"))
                            }
                            disabled={
                                pending || data?.room.state === "archived"
                            }
                        >
                            停止并取消任务
                        </button>
                    )}
                    <p className="muted">
                        取消会保留群历史、代码和已经合入的结果。
                    </p>
                </div>
            </Sheet>
        </div>
    );
}

function Members({
    root,
    data,
    perform,
}: {
    root: string;
    data: Snapshot;
    perform: (action: () => Promise<unknown>) => Promise<void>;
}) {
    const providers = useQuery({
        queryKey: ["providers"],
        queryFn: () => call<Provider[]>(root, "providers"),
    });
    return (
        <div className="sheet-form">
            {providers.error && (
                <p className="error" role="alert">
                    {errorText(providers.error)}
                </p>
            )}
            {data.members
                .filter((m) => m.state !== "removed")
                .map((member) =>
                    member.role === "boss" ? (
                        <div className="boss-row" key={member.id}>
                            <span className="avatar boss">你</span>
                            <strong>你</strong>
                            <small>老板</small>
                        </div>
                    ) : (
                        <MemberForm
                            key={`${member.id}:${member.provider}:${member.model}:${member.cli}`}
                            root={root}
                            member={member}
                            providers={providers.data ?? []}
                            perform={perform}
                        />
                    ),
                )}
            <button
                className="add-member"
                onClick={() =>
                    void perform(() =>
                        call(root, "member.add", {
                            room: data.room.id,
                            name: `执行者 ${data.members.filter((m) => m.role === "executor").length + 1}`,
                        }),
                    )
                }
            >
                <Plus size={18} />
                添加执行者
            </button>
        </div>
    );
}
function MemberForm({
    root,
    member,
    providers,
    perform,
}: {
    root: string;
    member: Member;
    providers: Provider[];
    perform: (action: () => Promise<unknown>) => Promise<void>;
}) {
    const [cli, setCli] = useState(member.cli);
    const [provider, setProvider] = useState(member.provider);
    const [model, setModel] = useState(member.model);
    const modelDefaulted = useRef(Boolean(member.model));
    const editable = ["configured", "offline"].includes(member.state);
    useEffect(() => {
        if (modelDefaulted.current || !provider) return;
        const configured = providers.find(
            (p) => p.cli === cli && p.id === provider,
        )?.model;
        if (configured) {
            setModel(configured);
            modelDefaulted.current = true;
        }
    }, [cli, provider, providers]);
    return (
        <form
            className="member-form"
            onSubmit={(e) => {
                e.preventDefault();
                void perform(() =>
                    call(root, "member.configure", {
                        member: member.id,
                        cli,
                        provider,
                        model,
                    }),
                );
            }}
        >
            <header>
                <span className={`avatar ${member.role}`}>
                    {member.role === "coordinator" ? "协" : "执"}
                </span>
                <div>
                    <strong>{member.name}</strong>
                    <small>{labels[member.state]}</small>
                </div>
                <IconButton
                    label={editable ? "初始化成员" : "停止成员"}
                    onClick={() =>
                        void perform(() =>
                            call(
                                root,
                                editable ? "member.initialize" : "member.stop",
                                { member: member.id },
                            ),
                        )
                    }
                >
                    {editable ? <Play size={17} /> : <Square size={15} />}
                </IconButton>
                {member.role === "executor" && editable && (
                    <IconButton
                        label="移除成员"
                        onClick={() =>
                            void perform(() =>
                                call(root, "member.remove", {
                                    member: member.id,
                                }),
                            )
                        }
                    >
                        <Trash2 size={17} />
                    </IconButton>
                )}
            </header>
            <div className="member-fields">
                <label>
                    CLI
                    <select
                        value={cli}
                        disabled={!editable}
                        onChange={(e) => {
                            setCli(e.target.value as Member["cli"]);
                            setProvider("");
                            setModel("");
                            modelDefaulted.current = false;
                        }}
                    >
                        <option value="claude">Claude Code</option>
                        <option value="codex">Codex</option>
                    </select>
                </label>
                <label>
                    供应商
                    <select
                        value={provider}
                        disabled={!editable}
                        onChange={(e) => {
                            const selected = providers.find(
                                (p) => p.cli === cli && p.id === e.target.value,
                            );
                            setProvider(e.target.value);
                            setModel(selected?.model ?? "");
                            modelDefaulted.current = Boolean(selected?.model);
                        }}
                    >
                        <option value="">选择 CC Switch 配置</option>
                        {providers
                            .filter((p) => p.cli === cli)
                            .map((p) => (
                                <option key={p.id} value={p.id}>
                                    {p.name}
                                </option>
                            ))}
                    </select>
                </label>
                <label className="model-field">
                    模型（必填）
                    <input
                        value={model}
                        disabled={!editable}
                        onChange={(e) => {
                            setModel(e.target.value);
                            modelDefaulted.current = true;
                        }}
                        placeholder="CC Switch 未配置默认模型时填写"
                        required
                    />
                </label>
            </div>
            {editable && (
                <button
                    type="submit"
                    className="save-member"
                    disabled={!provider || !model.trim()}
                >
                    保存配置
                </button>
            )}
            {editable && (!provider || !model.trim()) && (
                <small>
                    {!provider
                        ? "请选择供应商后保存。"
                        : "该供应商未配置默认模型，请填写后保存。"}
                </small>
            )}
            {member.error && (
                <p role="alert" className="error">
                    {member.error}
                </p>
            )}
        </form>
    );
}

interface Draft {
    body: string;
    recipients: string[];
    broadcast: boolean;
    replyTo: string | null;
}
const scrollPositions = new Map<string, { top: number; bottom: boolean }>();
function Chat({ root, snapshot }: { root: string; snapshot: Snapshot }) {
    const room = snapshot.room.id;
    const query = useQueryClient();
    const storageKey = `bridge-draft:${room}`;
    const coordinator = snapshot.members.find((m) => m.role === "coordinator");
    const [draft, setDraft] = useState<Draft>(() => {
        try {
            const old = JSON.parse(
                localStorage.getItem(storageKey) ?? "null",
            ) as Draft | null;
            if (
                old &&
                typeof old.body === "string" &&
                Array.isArray(old.recipients)
            )
                return old;
        } catch {
            /* Local drafts are disposable; persisted group history remains authoritative. */
        }
        return {
            body: "",
            recipients: coordinator ? [coordinator.id] : [],
            broadcast: false,
            replyTo: null,
        };
    });
    const [recipientOpen, setRecipientOpen] = useState(false);
    const [pending, setPending] = useState(false);
    const [error, setError] = useState("");
    const [failedRequest, setFailedRequest] = useState<{
        id: string;
        draft: Draft;
    }>();
    const scroller = useRef<HTMLDivElement>(null);
    const textarea = useRef<HTMLTextAreaElement>(null);
    const atBottom = useRef(scrollPositions.get(room)?.bottom ?? true);
    const restoredScroll = useRef(false);
    const [newMessages, setNewMessages] = useState(false);
    const historyKey = ["bridge", root, room, "history"];
    const history = useQuery({
        queryKey: historyKey,
        queryFn: async () => {
            const previous = query.getQueryData<HistoryCache>(historyKey);
            const pages: History[] = [];
            let changeCursor = previous?.change_cursor;
            do {
                const page = await call<History>(root, "history", {
                    room,
                    ...(changeCursor === undefined
                        ? { tail: true }
                        : { change_after: changeCursor }),
                    limit: 200,
                });
                pages.push(page);
                const initial = changeCursor === undefined;
                changeCursor = page.change_cursor;
                if (initial) {
                    break;
                }
                if (!page.has_more) break;
            } while (true);
            let merged = query.getQueryData<HistoryCache>(historyKey);
            for (const page of pages) merged = mergeHistory(merged, page);
            return merged!;
        },
    });
    async function loadOlder() {
        const first = history.data?.messages[0];
        if (!first) return;
        try {
            const page = await call<History>(root, "history", {
                room,
                before: first.seq,
                limit: 100,
            });
            const element = scroller.current;
            const oldHeight = element?.scrollHeight ?? 0;
            atBottom.current = false;
            query.setQueryData(historyKey, (old: typeof history.data) =>
                old ? mergeHistory(old, page, true) : old,
            );
            requestAnimationFrame(() => {
                if (element)
                    element.scrollTop += element.scrollHeight - oldHeight;
            });
        } catch (e) {
            setError(errorText(e));
        }
    }
    useEffect(() => {
        localStorage.setItem(storageKey, JSON.stringify(draft));
    }, [draft, storageKey]);
    useEffect(() => {
        const element = scroller.current;
        if (!element) return;
        if (!restoredScroll.current && history.data) {
            restoredScroll.current = true;
            const saved = scrollPositions.get(room);
            if (saved && !saved.bottom) {
                element.scrollTop = saved.top;
                return;
            }
        }
        if (atBottom.current) {
            element.scrollTop = element.scrollHeight;
            setNewMessages(false);
        } else setNewMessages(true);
    }, [history.data]);
    const members = snapshot.members.filter(
        (m) => m.role !== "boss" && m.state !== "removed",
    );
    async function send(e?: FormEvent, retry?: { id: string; draft: Draft }) {
        e?.preventDefault();
        const content = retry?.draft ?? draft;
        if (
            !content.body.trim() ||
            (!content.broadcast && !content.recipients.length)
        )
            return;
        const request = retry ?? {
            id: crypto.randomUUID(),
            draft: { ...content },
        };
        setPending(true);
        setError("");
        try {
            await call(root, "message.send", {
                room,
                kind: content.replyTo ? "reply" : "request",
                body: content.body,
                recipients: content.recipients,
                broadcast: content.broadcast,
                reply_to: content.replyTo,
                request_id: request.id,
            });
            setDraft((previous) =>
                previous.body === content.body
                    ? { ...previous, body: "", replyTo: null }
                    : previous,
            );
            setFailedRequest(undefined);
            atBottom.current = true;
            await query.invalidateQueries({ queryKey: ["bridge", root, room] });
            textarea.current?.focus();
        } catch (e) {
            setError(errorText(e));
            setFailedRequest(request);
        } finally {
            setPending(false);
        }
    }
    function select(member: string) {
        setDraft((previous) => ({
            ...previous,
            broadcast: false,
            recipients: previous.recipients.includes(member)
                ? previous.recipients.filter((id) => id !== member)
                : [...previous.recipients, member],
        }));
    }
    return (
        <>
            <div
                className="messages"
                ref={scroller}
                onScroll={() => {
                    const e = scroller.current;
                    if (e) {
                        atBottom.current =
                            e.scrollHeight - e.scrollTop - e.clientHeight < 64;
                        scrollPositions.set(room, {
                            top: e.scrollTop,
                            bottom: atBottom.current,
                        });
                        if (atBottom.current) setNewMessages(false);
                    }
                }}
                aria-label="群消息"
                role="log"
                aria-live="polite"
            >
                {history.data?.older_available &&
                    history.data.messages.length !== 0 && (
                        <button
                            className="load-older"
                            onClick={() => void loadOlder()}
                        >
                            更早的消息
                        </button>
                    )}
                {!history.data?.messages.length && (
                    <div className="chat-empty">
                        <Users size={30} />
                        <p>向协调者说说要做什么</p>
                    </div>
                )}
                {history.data?.messages.map((message) => {
                    const sender = snapshot.members.find(
                        (m) => m.id === message.sender,
                    );
                    const boss = sender?.role === "boss";
                    const deliveries = history.data.deliveries.filter(
                        (d) => d.message_id === message.id,
                    );
                    if (message.kind === "system")
                        return (
                            <div className="system-message" key={message.id}>
                                {message.body}
                            </div>
                        );
                    if (message.kind === "tool")
                        return (
                            <details className="tool-message" key={message.id}>
                                <summary>
                                    {message.sender_name} · 工具操作
                                </summary>
                                <pre>{message.body}</pre>
                            </details>
                        );
                    return (
                        <article
                            key={message.id}
                            className={`message ${boss ? "own" : ""}`}
                        >
                            <div className="message-meta">
                                <strong>{message.sender_name}</strong>
                                {message.recipients.length > 0 && (
                                    <span>
                                        →{" "}
                                        {message.recipients
                                            .map(
                                                (id) =>
                                                    snapshot.members.find(
                                                        (m) => m.id === id,
                                                    )?.name ?? "已移除成员",
                                            )
                                            .join("、")}
                                    </span>
                                )}
                                <time>
                                    {new Date(
                                        message.created * 1000,
                                    ).toLocaleTimeString("zh-CN", {
                                        hour: "2-digit",
                                        minute: "2-digit",
                                    })}
                                </time>
                            </div>
                            <div className="bubble">
                                {message.reply_to && (
                                    <small className="reply-context">
                                        回复：
                                        {history.data.messages
                                            .find(
                                                (m) =>
                                                    m.id === message.reply_to,
                                            )
                                            ?.body.slice(0, 80) ?? "较早的消息"}
                                    </small>
                                )}
                                <p>{message.body}</p>
                            </div>
                            <div className="message-footer">
                                {deliveries.length > 0 && (
                                    <small>
                                        {deliveries.every(
                                            (d) => d.state === "processed",
                                        )
                                            ? "已处理"
                                            : deliveries.some(
                                                    (d) => d.state === "queued",
                                                )
                                              ? "等待处理"
                                              : "已交付"}
                                    </small>
                                )}
                                {!boss && sender && (
                                    <IconButton
                                        label={`回复 ${message.sender_name}`}
                                        onClick={() => {
                                            setDraft((previous) => ({
                                                ...previous,
                                                replyTo: message.id,
                                                recipients: [sender.id],
                                                broadcast: false,
                                            }));
                                            textarea.current?.focus();
                                        }}
                                    >
                                        <Reply size={14} />
                                    </IconButton>
                                )}
                            </div>
                        </article>
                    );
                })}
            </div>
            {newMessages && (
                <button
                    className="new-messages"
                    onClick={() => {
                        const e = scroller.current;
                        if (e) e.scrollTop = e.scrollHeight;
                        atBottom.current = true;
                        setNewMessages(false);
                    }}
                >
                    <ArrowDown size={15} />
                    新消息
                </button>
            )}
            {(error || history.error) && (
                <div className="error-bar" role="alert">
                    <span>{error || errorText(history.error)}</span>
                    {failedRequest && (
                        <button
                            disabled={pending}
                            onClick={() => void send(undefined, failedRequest)}
                        >
                            <RotateCw size={15} />
                            重试
                        </button>
                    )}
                </div>
            )}
            <form className="composer" onSubmit={(e) => void send(e)}>
                <div className="recipient-line">
                    <button
                        type="button"
                        className="recipient-selector"
                        onClick={() => setRecipientOpen(!recipientOpen)}
                        aria-expanded={recipientOpen}
                    >
                        @{" "}
                        <span>
                            {draft.broadcast
                                ? "所有人"
                                : draft.recipients
                                      .map(
                                          (id) =>
                                              snapshot.members.find(
                                                  (m) => m.id === id,
                                              )?.name ?? "已移除成员",
                                      )
                                      .join("、") || "选择成员"}
                        </span>
                        <ChevronDown size={13} />
                    </button>
                    {draft.replyTo && (
                        <button
                            type="button"
                            className="reply-chip"
                            onClick={() =>
                                setDraft((previous) => ({
                                    ...previous,
                                    replyTo: null,
                                }))
                            }
                        >
                            回复
                            <X size={12} />
                        </button>
                    )}
                </div>
                {recipientOpen && (
                    <div className="recipient-menu">
                        <button
                            type="button"
                            onClick={() =>
                                setDraft((previous) => ({
                                    ...previous,
                                    broadcast: !previous.broadcast,
                                    recipients: [],
                                }))
                            }
                        >
                            {draft.broadcast && <Check size={14} />}所有人
                        </button>
                        {members.map((m) => (
                            <button
                                type="button"
                                key={m.id}
                                onClick={() => select(m.id)}
                            >
                                {draft.recipients.includes(m.id) &&
                                    !draft.broadcast && <Check size={14} />}
                                <span>{m.name}</span>
                                <small>{labels[m.state]}</small>
                            </button>
                        ))}
                        <button
                            type="button"
                            onClick={() =>
                                setDraft((previous) => ({
                                    ...previous,
                                    recipients: [],
                                    broadcast: false,
                                }))
                            }
                        >
                            清空选择
                        </button>
                        <button
                            type="button"
                            onClick={() => {
                                setRecipientOpen(false);
                                textarea.current?.focus();
                            }}
                        >
                            完成
                        </button>
                    </div>
                )}
                <div className="composer-input">
                    <textarea
                        ref={textarea}
                        aria-label="消息"
                        placeholder="发送消息…"
                        rows={2}
                        value={draft.body}
                        onChange={(e) => {
                            setDraft((previous) => ({
                                ...previous,
                                body: e.target.value,
                            }));
                            if (e.target.value === "@") setRecipientOpen(true);
                        }}
                        onKeyDown={(e) => {
                            if (
                                e.key === "Enter" &&
                                !e.shiftKey &&
                                !e.nativeEvent.isComposing
                            ) {
                                e.preventDefault();
                                if (!pending) void send();
                            }
                            if (e.key === "Escape") setRecipientOpen(false);
                        }}
                        disabled={["archived", "cancelled"].includes(
                            snapshot.room.state,
                        )}
                        maxLength={32768}
                    />
                    <button
                        type="submit"
                        className="send-button"
                        aria-label="发送消息"
                        title="发送消息"
                        disabled={
                            pending ||
                            !draft.body.trim() ||
                            (!draft.broadcast && !draft.recipients.length) ||
                            ["archived", "cancelled"].includes(
                                snapshot.room.state,
                            )
                        }
                    >
                        {pending ? (
                            <LoaderCircle size={18} className="spin" />
                        ) : (
                            <ArrowUp size={21} />
                        )}
                    </button>
                </div>
            </form>
        </>
    );
}
