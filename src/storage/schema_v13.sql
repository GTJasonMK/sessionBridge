CREATE TABLE speculative_work (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    branch TEXT NOT NULL CHECK (length(trim(branch)) > 0),
    base_commit TEXT NOT NULL CHECK (length(base_commit) > 0),
    base_plan_version INTEGER NOT NULL CHECK (base_plan_version >= 0),
    expected_merge_point TEXT NOT NULL CHECK (length(expected_merge_point) > 0),
    merge_ref TEXT NOT NULL CHECK (length(trim(merge_ref)) > 0),
    assumptions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(assumptions_json)),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','ready_for_adoption','revalidation_required','adopted','abandoned')),
    head_commit TEXT,
    summary TEXT,
    provenance_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(provenance_json)),
    request_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT,
    UNIQUE(session_id,request_id)
);

CREATE INDEX idx_speculative_work_task_status ON speculative_work(task_id,status,updated_at);
