ALTER TABLE evidence ADD COLUMN subject_type TEXT;
ALTER TABLE evidence ADD COLUMN subject_id TEXT;
ALTER TABLE evidence ADD COLUMN repository_identity TEXT;
ALTER TABLE evidence ADD COLUMN commit_hash TEXT;
ALTER TABLE evidence ADD COLUMN verifier TEXT;
ALTER TABLE evidence ADD COLUMN environment_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(environment_json));
ALTER TABLE evidence ADD COLUMN observed_at INTEGER;

CREATE INDEX idx_evidence_subject
    ON evidence(task_id, subject_type, subject_id, result);

CREATE INDEX idx_evidence_commit
    ON evidence(task_id, repository_identity, commit_hash, result);
