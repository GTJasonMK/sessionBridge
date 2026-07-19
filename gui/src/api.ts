import { invoke } from "@tauri-apps/api/core";
import type { ChatMessage, ChatMessageKind, GuiError, InvitationManifest, LaunchInput, LaunchPreflight, TaskDashboard, TaskSummary, WorkspaceView } from "./types";

export async function command<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw normalizeError(error);
  }
}

export function normalizeError(error: unknown): GuiError {
  if (typeof error === "object" && error !== null && "code" in error) return error as GuiError;
  return { code: "INTERNAL", message: String(error), retryable: false, details: null, operation_id: "unavailable" };
}

export const api = {
  restoreWorkspace: () => command<WorkspaceView | null>("workspace_restore"),
  openWorkspace: (path: string) => command<WorkspaceView>("workspace_open", { path }),
  initializeWorkspace: (path: string) => command<WorkspaceView>("workspace_initialize", { path }),
  listTasks: (workspace: string) => command<TaskSummary[]>("task_list", { workspace }),
  snapshot: (workspace: string, taskId: string) => command<TaskDashboard>("task_snapshot", { workspace, taskId }),
  sendChat: (workspace: string, taskId: string, input: { kind: ChatMessageKind; body: string; replyToMessageId?: string }) =>
    command<ChatMessage>("chat_send", { workspace, taskId, input }),
  preflight: (workspace: string, input: LaunchInput) => command<LaunchPreflight>("launch_preflight", { workspace, input }),
  launch: (workspace: string, input: LaunchInput) => command<any>("launch_execute", { workspace, input }),
  prompts: (workspace: string, taskId: string) => command<InvitationManifest[]>("prompt_list", { workspace, taskId }),
  prompt: (workspace: string, invitationId: string) => command<InvitationManifest>("prompt_show", { workspace, invitationId }),
  transition: (workspace: string, taskId: string, action: string, reason?: string) => command("task_transition", { workspace, taskId, action, reason }),
  doctor: (workspace: string) => command<any>("doctor_run", { workspace }),
  collaborationDiagnostics: (workspace: string, taskId: string) => command<any>("collaboration_diagnostics", { workspace, taskId }),
  logs: (lines = 200) => command<any>("logs_tail", { lines }),
};
