CREATE TABLE agent_waits (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('all', 'any')),
    status TEXT NOT NULL CHECK (status IN (
        'active', 'satisfied', 'dependency_failed', 'task_terminal', 'participant_retired'
    )),
    created_event_cursor INTEGER NOT NULL CHECK (created_event_cursor >= 0),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    completion_event_cursor INTEGER,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    request_id TEXT NOT NULL CHECK (length(request_id) > 0),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (participant_id, task_id) REFERENCES task_participants(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (completion_event_cursor) REFERENCES events(sequence) ON DELETE RESTRICT,
    UNIQUE (id, task_id),
    UNIQUE (session_id, request_id),
    CHECK ((status = 'active' AND completed_at IS NULL AND completion_event_cursor IS NULL) OR
           (status <> 'active' AND completed_at IS NOT NULL AND completion_event_cursor IS NOT NULL))
);

CREATE UNIQUE INDEX uq_agent_waits_active_participant
    ON agent_waits(participant_id) WHERE status = 'active';
CREATE INDEX idx_agent_waits_task_status ON agent_waits(task_id, status, created_at);

CREATE TABLE agent_wait_targets (
    wait_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    work_item_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'satisfied', 'impossible', 'not_required')),
    terminal_state TEXT,
    triggering_event_sequence INTEGER,
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (wait_id, ordinal),
    FOREIGN KEY (wait_id) REFERENCES agent_waits(id) ON DELETE CASCADE,
    FOREIGN KEY (work_item_id) REFERENCES work_items(id) ON DELETE RESTRICT,
    FOREIGN KEY (triggering_event_sequence) REFERENCES events(sequence) ON DELETE RESTRICT,
    UNIQUE (wait_id, work_item_id)
);

CREATE INDEX idx_agent_wait_targets_item_status
    ON agent_wait_targets(work_item_id, status, wait_id);
