CREATE TABLE blocking_fingerprints (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    blocker_code TEXT NOT NULL CHECK (length(blocker_code) > 0),
    entity_id TEXT,
    dependency_version INTEGER NOT NULL CHECK (dependency_version >= 0),
    fingerprint TEXT NOT NULL CHECK (length(fingerprint) > 0),
    first_observed_cursor INTEGER NOT NULL CHECK (first_observed_cursor >= 0),
    last_observed_cursor INTEGER NOT NULL CHECK (last_observed_cursor >= first_observed_cursor),
    unchanged_since INTEGER NOT NULL,
    resolved_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (participant_id, task_id) REFERENCES task_participants(id, task_id) ON DELETE CASCADE,
    UNIQUE (participant_id)
);

CREATE INDEX idx_blocking_fingerprints_active
    ON blocking_fingerprints(task_id, resolved_at, unchanged_since);
