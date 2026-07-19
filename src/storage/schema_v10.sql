CREATE TABLE external_waits (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    detector_type TEXT NOT NULL CHECK (detector_type IN ('file_exists','file_absent')),
    detector_json TEXT NOT NULL CHECK (json_valid(detector_json)),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','satisfied','deadline_reached','detector_failed','cancelled')),
    deadline_at INTEGER,
    reassess_interval_seconds INTEGER NOT NULL CHECK (reassess_interval_seconds > 0),
    next_reassessment_at INTEGER NOT NULL,
    last_observed_at INTEGER,
    observation_count INTEGER NOT NULL DEFAULT 0 CHECK (observation_count >= 0),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    request_id TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK (length(request_hash) > 0),
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (participant_id, task_id) REFERENCES task_participants(id, task_id) ON DELETE CASCADE,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE CASCADE,
    UNIQUE(session_id, request_id)
);

CREATE UNIQUE INDEX idx_external_waits_active_participant
    ON external_waits(participant_id) WHERE status='active';

CREATE INDEX idx_external_waits_due
    ON external_waits(status, next_reassessment_at);
