import { invoke } from "@tauri-apps/api/core";

export interface Room {
    id: string;
    title: string;
    state: string;
    target: string | null;
    integrated_head: string | null;
}
export interface Member {
    id: string;
    room_id: string;
    name: string;
    role: "boss" | "coordinator" | "executor";
    cli: "claude" | "codex";
    provider: string;
    model: string;
    state: string;
    error: string | null;
    cwd: string | null;
}
export interface Work {
    id: string;
    title: string;
    owner: string;
    state: string;
    version: number;
    evidence: string;
    commit_id: string | null;
}
export interface Message {
    id: string;
    seq: number;
    sender: string;
    sender_name: string;
    kind: string;
    body: string;
    recipients: string[];
    reply_to: string | null;
    created: number;
}
export interface Delivery {
    message_id: string;
    member_id: string;
    state: string;
}
export interface History {
    messages: Message[];
    deliveries: Delivery[];
    cursor: number;
    change_cursor: number;
    has_more: boolean;
}
export interface Snapshot {
    room: Room;
    members: Member[];
    work: Work[];
    approvals: { id: string; member_id: string; details: unknown }[];
}
export interface Provider {
    id: string;
    name: string;
    cli: string;
    model: string;
}
export interface Workspace {
    root: string;
    name: string;
    id: string;
}

export interface HistoryCache {
    messages: Message[];
    deliveries: Delivery[];
    change_cursor: number;
    older_available: boolean;
}
export function mergeHistory(
    previous: HistoryCache | undefined,
    page: History,
    older = false,
): HistoryCache {
    const oldest = previous?.messages[0]?.seq ?? 0;
    const incoming = page.messages.filter(
        (m) => older || !previous || m.seq >= oldest,
    );
    const messages = new Map(
        (older
            ? [...incoming, ...(previous?.messages ?? [])]
            : [...(previous?.messages ?? []), ...incoming]
        ).map((m) => [m.id, m]),
    );
    const deliveries = new Map(
        (older
            ? [...page.deliveries, ...(previous?.deliveries ?? [])]
            : [...(previous?.deliveries ?? []), ...page.deliveries]
        )
            .filter((d) => messages.has(d.message_id))
            .map((d) => [`${d.message_id}:${d.member_id}`, d]),
    );
    return {
        messages: [...messages.values()].sort((a, b) => a.seq - b.seq),
        deliveries: [...deliveries.values()],
        change_cursor:
            older && previous
                ? previous.change_cursor
                : Math.max(previous?.change_cursor ?? 0, page.change_cursor),
        older_available:
            older || !previous ? page.has_more : previous.older_available,
    };
}

export function call<T>(
    workspace: string,
    operation: string,
    args: Record<string, unknown> = {},
): Promise<T> {
    return invoke<T>("bridge_call", {
        workspace,
        operation,
        args: { request_id: crypto.randomUUID(), ...args },
    });
}
export function errorText(error: unknown): string {
    return typeof error === "object" && error !== null && "message" in error
        ? String(error.message)
        : String(error);
}
export const labels: Record<string, string> = {
    configured: "未初始化",
    starting: "接入中",
    ready: "就绪",
    busy: "工作中",
    offline: "离线",
    removed: "已移除",
    active: "进行中",
    paused: "已暂停",
    completed: "已完成",
    cancelled: "已取消",
    archived: "已归档",
    todo: "待办",
    running: "执行中",
    review: "待验收",
    done: "已验收",
    blocked: "阻塞",
    queued: "等待处理",
    delivered: "已交付",
    processed: "已处理",
};
