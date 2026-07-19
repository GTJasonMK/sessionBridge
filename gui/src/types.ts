export type TaskPhase = "active" | "paused" | "completed" | "cancelled" | "archived";

export interface WorkspaceView {
  project_id: string;
  name: string;
  root: string;
  repository_kind: "git" | "directory";
  repository_instance_id: string;
  database_path: string;
}

export interface TaskSummary {
  id: string;
  title: string;
  phase: TaskPhase;
  autonomy_policy: string;
  target_branch?: string;
  plan_version: number;
  completion_epoch: number;
  completion_revision: number;
  created_at_micros: number;
  updated_at_micros: number;
}

export interface EventView {
  sequence: number;
  event_type: string;
  actor_type: string;
  actor_id?: string;
  entity_type: string;
  entity_id?: string;
  payload: unknown;
  created_at_micros: number;
}

export type ChatMessageKind = "progress" | "intent" | "question" | "decision" | "warning" | "handoff" | "note";

export interface ChatMessage {
  id: string;
  task_id: string;
  sender_type: "agent" | "operator" | "system";
  sender_id?: string;
  sender_label: string;
  kind: ChatMessageKind;
  body: string;
  reply_to_message_id?: string;
  mentions: string[];
  related_entities: Array<{ entity_type: string; entity_id: string }>;
  event_sequence: number;
  created_at_micros: number;
}

export interface TaskDashboard {
  pipeline: {
    required_deliverables_total: number; required_deliverables_verified: number;
    work_items_total: number; work_items_accepted: number;
    reviews_required: number; reviews_approved: number;
    integrations_required: number; integrations_completed: number;
    completion_blocker_count: number;
  };
  deliverables: Array<{
    id: string; task_id: string; key: string; title: string; description: string;
    kind: string; required: boolean; source: string; status: string; version: number;
    work_item_ids: string[]; evidence_ids: string[];
  }>;
  details: {
    task: TaskSummary & { workspace_id: string; completion_policy: string; version: number };
    criteria: Array<{ id: string; kind: string; content: string; required: boolean }>;
    event_cursor: number;
  };
  health: {
    task: TaskSummary;
    signals: Record<string, number>;
    planning_required: boolean;
    completion_cohort_open: boolean;
    completed_with_warnings: boolean;
    no_progress_possible: boolean;
    completion_signal_warnings: Array<{ code: string; severity: string; count: number }>;
    event_cursor: number;
  };
  participants: Array<{
    id: string; session_id: string; label: string; kind: string; state: string;
    completion_epoch: number; last_seen_at_micros: number; worktree_path?: string;
    branch?: string; model?: string;
  }>;
  invitations: Array<{
    id: string; batch_id: string; state: string; participant_kind: string;
    blocks_completion: boolean; completion_epoch: number; expires_at_micros: number;
    consumed_by_session_id?: string;
  }>;
  work_items: Array<{
    id: string; parent_id?: string; kind: string; title: string; description: string;
    required: boolean; priority: number; risk: string; plan_state: string;
    result_state: string; version: number;
    active_claim?: { id: string; session_id: string; mode: string; generation: number; lease_expires_at_micros: number };
  }>;
  relations: Array<{ from_id: string; to_id: string; relation_type: string }>;
  gaps: Array<{ id: string; kind: string; severity: string; required: boolean; status: string; title: string; description: string }>;
  blockers: Array<{ id: string; work_item_id: string; waits_for_type: string; waits_for_id?: string; status: string; reason: string; resolution?: string }>;
  waits: Array<{ id: string; participant_id: string; session_id: string; mode: string; status: string; created_at_micros: number; completed_at_micros?: number; targets: string[] }>;
  external_waits: Array<{ id: string; participant_id: string; session_id: string; detector_type: string; detector: { type: string; path: string }; status: string; deadline_at_micros?: number; next_reassessment_at_micros: number; last_observed_at_micros?: number; observation_count: number }>;
  speculative_work: Array<{ id: string; session_id: string; title: string; branch: string; base_commit: string; base_plan_version: number; merge_ref: string; expected_merge_point: string; head_commit?: string; status: string; summary?: string; assumptions: string[]; updated_at_micros: number }>;
  recent_events: { events: EventView[]; previous_cursor: number; next_cursor: number; has_more: boolean };
  chat_messages: ChatMessage[];
}

export interface GuiError {
  code: string;
  message: string;
  retryable: boolean;
  details: unknown;
  operation_id: string;
}

export interface LaunchInput {
  title: string;
  objective: string;
  profile?: string;
  agents?: number;
}

export interface LaunchPreflight {
  title: string; objective_sha256: string; agents: number; expires_in_seconds: number;
  autonomy_policy: string; target_branch: string; baseline_commit: string;
  dirty_paths: string[]; ready: boolean; warnings: string[];
}

export interface InvitationManifest {
  invite_id: string; task_id: string; batch_id: string; batch_index: number;
  batch_count: number; status: string; participant_kind: string; expires_at: string;
  sendable: boolean; prompt?: string;
}
