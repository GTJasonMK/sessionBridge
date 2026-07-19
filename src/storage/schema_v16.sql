ALTER TABLE agent_sessions ADD COLUMN executor_identity TEXT;
ALTER TABLE agent_sessions ADD COLUMN executor_identity_source TEXT NOT NULL DEFAULT 'legacy_session'
    CHECK (executor_identity_source IN ('invitation','legacy_session'));

UPDATE agent_sessions
SET executor_identity = COALESCE(
    (SELECT p.invitation_id FROM task_participants p
     WHERE p.session_id=agent_sessions.id AND p.task_id=agent_sessions.task_id
       AND p.invitation_id IS NOT NULL ORDER BY p.joined_at LIMIT 1),
    id
),
executor_identity_source = CASE WHEN EXISTS(
    SELECT 1 FROM task_participants p WHERE p.session_id=agent_sessions.id
      AND p.task_id=agent_sessions.task_id AND p.invitation_id IS NOT NULL
) THEN 'invitation' ELSE 'legacy_session' END;

CREATE INDEX idx_agent_sessions_executor ON agent_sessions(task_id,executor_identity,ended_at);
