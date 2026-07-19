CREATE TABLE deliverables (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    key TEXT NOT NULL CHECK (length(trim(key)) > 0),
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    description TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'capability' CHECK (length(trim(kind)) > 0),
    required INTEGER NOT NULL DEFAULT 1 CHECK (required IN (0, 1)),
    source TEXT NOT NULL CHECK (source IN ('user', 'agent', 'system')),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'planned', 'implemented', 'verified', 'waived')),
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    UNIQUE (task_id, key),
    UNIQUE (id, task_id)
);

CREATE INDEX idx_deliverables_task_status ON deliverables(task_id, required, status);

CREATE TABLE deliverable_coverage (
    deliverable_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (deliverable_id, work_item_id),
    FOREIGN KEY (deliverable_id) REFERENCES deliverables(id) ON DELETE CASCADE,
    FOREIGN KEY (work_item_id) REFERENCES work_items(id) ON DELETE CASCADE
);

CREATE INDEX idx_deliverable_coverage_item ON deliverable_coverage(work_item_id);

CREATE TABLE deliverable_evidence (
    deliverable_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (deliverable_id, evidence_id),
    FOREIGN KEY (deliverable_id) REFERENCES deliverables(id) ON DELETE CASCADE,
    FOREIGN KEY (evidence_id) REFERENCES evidence(id) ON DELETE CASCADE
);

CREATE INDEX idx_deliverable_evidence_evidence ON deliverable_evidence(evidence_id);
