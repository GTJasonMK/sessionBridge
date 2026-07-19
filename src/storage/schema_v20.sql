CREATE TABLE chat_inbox_states (
    session_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    starts_after_sequence INTEGER NOT NULL CHECK (starts_after_sequence >= 0),
    FOREIGN KEY (session_id,task_id) REFERENCES agent_sessions(id,task_id) ON DELETE CASCADE,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

-- Existing Sessions retain at-least-once delivery after migration. New Sessions start at join time.
INSERT INTO chat_inbox_states(session_id,task_id,starts_after_sequence)
SELECT id,task_id,0 FROM agent_sessions;

CREATE TRIGGER initialize_chat_inbox_after_session_insert
AFTER INSERT ON agent_sessions
BEGIN
    INSERT INTO chat_inbox_states(session_id,task_id,starts_after_sequence)
    VALUES(
        NEW.id,
        NEW.task_id,
        COALESCE((SELECT MAX(event_sequence) FROM chat_messages WHERE task_id=NEW.task_id),0)
    );
END;
