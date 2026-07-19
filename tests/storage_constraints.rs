use rusqlite::Connection;
use sessionbridge::storage::SqliteStore;
use tempfile::tempdir;

fn migrated_store() -> (tempfile::TempDir, SqliteStore) {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    SqliteStore::migrate(&database_path).unwrap();
    let store = SqliteStore::open(&database_path).unwrap();
    (directory, store)
}

fn seed_task(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO workspaces (id, name, repository_identity, created_at)
             VALUES ('workspace', 'Workspace', 'repo:workspace', 1);

             INSERT INTO tasks (
                 id, workspace_id, title, phase, autonomy_policy, completion_policy,
                 created_at, updated_at
             ) VALUES (
                 'task', 'workspace', 'Task', 'active', 'bounded',
                 'all_workers_concluded', 1, 1
             );",
        )
        .unwrap();
}

fn seed_sessions_and_work_item(connection: &Connection) {
    seed_task(connection);
    connection
        .execute_batch(
            "INSERT INTO agent_sessions (
                 id, task_id, token_hash, label, protocol_version, last_seen_at,
                 created_at, updated_at
             ) VALUES (
                 'session-1', 'task',
                 X'0000000000000000000000000000000000000000000000000000000000000000',
                 'Agent One', 1, 1, 1, 1
             );
             INSERT INTO agent_sessions (
                 id, task_id, token_hash, label, protocol_version, last_seen_at,
                 created_at, updated_at
             ) VALUES (
                 'session-2', 'task',
                 X'0101010101010101010101010101010101010101010101010101010101010101',
                 'Agent Two', 1, 1, 1, 1
             );

             INSERT INTO work_items (
                 id, task_id, kind, title, risk, plan_state, result_state,
                 created_at, updated_at
             ) VALUES (
                 'work-item', 'task', 'implementation', 'Implement storage', 'medium',
                 'active', 'open', 1, 1
             );",
        )
        .unwrap();
}

fn assert_constraint_violation(result: rusqlite::Result<usize>) {
    let error = result.expect_err("operation must violate a storage constraint");
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation),
        "unexpected SQLite error: {error}"
    );
}

#[test]
fn invitation_kind_determines_whether_it_blocks_completion() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();
    seed_task(connection);

    assert_constraint_violation(connection.execute(
        "INSERT INTO agent_invitations (
             id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
             state, blocks_completion, prompt_template_version, protocol_version,
             expires_at, created_at, updated_at
         ) VALUES (
             'bad-worker', 'task', 1, 'worker', 'batch', randomblob(32),
             'pending', 0, 1, 1, 100, 1, 1
         )",
        [],
    ));

    assert_constraint_violation(connection.execute(
        "INSERT INTO agent_invitations (
             id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
             state, blocks_completion, prompt_template_version, protocol_version,
             expires_at, created_at, updated_at
         ) VALUES (
             'bad-observer', 'task', 1, 'observer', 'batch', randomblob(32),
             'pending', 1, 1, 1, 100, 1, 1
         )",
        [],
    ));

    connection
        .execute(
            "INSERT INTO agent_invitations (
                 id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
                 state, blocks_completion, prompt_template_version, protocol_version,
                 expires_at, created_at, updated_at
             ) VALUES (
                 'worker', 'task', 1, 'worker', 'batch', randomblob(32),
                 'pending', 1, 1, 1, 100, 1, 1
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO agent_invitations (
                 id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
                 state, blocks_completion, prompt_template_version, protocol_version,
                 expires_at, created_at, updated_at
             ) VALUES (
                 'observer', 'task', 1, 'observer', 'batch', randomblob(32),
                 'pending', 0, 1, 1, 100, 1, 1
             )",
            [],
        )
        .unwrap();
}

#[test]
fn only_one_unclosed_execute_claim_exists_per_work_item() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();
    seed_sessions_and_work_item(connection);

    connection
        .execute(
            "INSERT INTO work_claims (
                 id, task_id, work_item_id, session_id, mode, generation,
                 claimed_at, lease_expires_at
             ) VALUES (
                 'claim-1', 'task', 'work-item', 'session-1', 'execute', 1, 1, 100
             )",
            [],
        )
        .unwrap();

    assert_constraint_violation(connection.execute(
        "INSERT INTO work_claims (
             id, task_id, work_item_id, session_id, mode, generation,
             claimed_at, lease_expires_at
         ) VALUES (
             'claim-2', 'task', 'work-item', 'session-2', 'execute', 2, 2, 100
         )",
        [],
    ));

    connection
        .execute(
            "UPDATE work_claims
             SET closed_at = 3, close_reason = 'released'
             WHERE id = 'claim-1'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO work_claims (
                 id, task_id, work_item_id, session_id, mode, generation,
                 claimed_at, lease_expires_at
             ) VALUES (
                 'claim-2', 'task', 'work-item', 'session-2', 'execute', 2, 3, 100
             )",
            [],
        )
        .unwrap();
}

#[test]
fn participant_cohort_membership_is_unique_per_session_and_epoch() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();
    seed_sessions_and_work_item(connection);

    connection
        .execute(
            "INSERT INTO task_participants (
                 id, task_id, session_id, kind, completion_epoch, state,
                 joined_at, created_at, updated_at
             ) VALUES (
                 'participant-1', 'task', 'session-1', 'worker', 1, 'active', 1, 1, 1
             )",
            [],
        )
        .unwrap();

    assert_constraint_violation(connection.execute(
        "INSERT INTO task_participants (
             id, task_id, session_id, kind, completion_epoch, state,
             joined_at, created_at, updated_at
         ) VALUES (
             'participant-2', 'task', 'session-1', 'worker', 1, 'active', 1, 1, 1
         )",
        [],
    ));
}

#[test]
fn conclusions_are_immutable_and_must_match_the_participant_state() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();
    seed_sessions_and_work_item(connection);

    connection
        .execute_batch(
            "INSERT INTO task_participants (
                 id, task_id, session_id, kind, completion_epoch, state,
                 joined_at, created_at, updated_at
             ) VALUES (
                 'participant', 'task', 'session-1', 'worker', 1, 'active', 1, 1, 1
             );

             INSERT INTO agent_conclusions (
                 id, task_id, participant_id, generation, outcome,
                 basis_completion_revision, basis_event_cursor, basis_plan_version,
                 assessment_schema_version, assessment_json, signal_snapshot_json,
                 warning_snapshot_json, created_at
             ) VALUES (
                 'conclusion-done', 'task', 'participant', 1, 'no_further_work',
                 0, 0, 0, 1, '{}', '{}', '[]', 2
             );

             INSERT INTO agent_conclusions (
                 id, task_id, participant_id, generation, outcome,
                 basis_completion_revision, basis_event_cursor, basis_plan_version,
                 assessment_schema_version, assessment_json, signal_snapshot_json,
                 warning_snapshot_json, created_at
             ) VALUES (
                 'conclusion-blocked', 'task', 'participant', 2, 'blocked',
                 0, 0, 0, 1, '{}', '{}', '[]', 3
             );",
        )
        .unwrap();

    connection
        .execute(
            "UPDATE task_participants
             SET state = 'concluded', current_conclusion_id = 'conclusion-done'
             WHERE id = 'participant'",
            [],
        )
        .unwrap();

    assert_constraint_violation(connection.execute(
        "UPDATE task_participants
         SET current_conclusion_id = 'conclusion-blocked'
         WHERE id = 'participant'",
        [],
    ));
    assert_constraint_violation(connection.execute(
        "UPDATE agent_conclusions SET assessment_json = '{\"changed\":true}'
         WHERE id = 'conclusion-done'",
        [],
    ));
    assert_constraint_violation(connection.execute(
        "DELETE FROM agent_conclusions WHERE id = 'conclusion-done'",
        [],
    ));

    connection
        .execute(
            "UPDATE task_participants
             SET state = 'blocked', current_conclusion_id = 'conclusion-blocked'
             WHERE id = 'participant'",
            [],
        )
        .unwrap();
}

#[test]
fn completion_events_and_idempotency_records_are_append_only_and_unique() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();
    seed_task(connection);

    let first_sequence = connection
        .query_row(
            "INSERT INTO events (
                 workspace_id, task_id, event_type, actor_type, entity_type,
                 payload_json, created_at
             ) VALUES (
                 'workspace', 'task', 'task.completed', 'system', 'task', '{}', 1
             ) RETURNING sequence",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let second_sequence = connection
        .query_row(
            "INSERT INTO events (
                 workspace_id, task_id, event_type, actor_type, entity_type,
                 payload_json, created_at
             ) VALUES (
                 'workspace', 'task', 'task.completed', 'system', 'task', '{}', 2
             ) RETURNING sequence",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert!(second_sequence > first_sequence);

    connection
        .execute(
            "INSERT INTO task_completions (
                 id, task_id, completion_epoch, completion_policy, trigger_actor_type,
                 conclusion_set_digest, snapshot_completion_revision,
                 basis_event_cursor, basis_plan_version, completion_event_sequence,
                 signal_snapshot_json, completed_at
             ) VALUES (
                 'completion-1', 'task', 1, 'all_workers_concluded', 'system',
                 zeroblob(32), 0, 0, 0, ?1, '{}', 1
             )",
            [first_sequence],
        )
        .unwrap();

    assert_constraint_violation(connection.execute(
        "INSERT INTO task_completions (
             id, task_id, completion_epoch, completion_policy, trigger_actor_type,
             conclusion_set_digest, snapshot_completion_revision,
             basis_event_cursor, basis_plan_version, completion_event_sequence,
             signal_snapshot_json, completed_at
         ) VALUES (
             'completion-2', 'task', 1, 'all_workers_concluded', 'system',
             zeroblob(32), 0, 0, 0, ?1, '{}', 2
         )",
        [second_sequence],
    ));
    assert_constraint_violation(connection.execute(
        "UPDATE events SET payload_json = '{\"changed\":true}' WHERE sequence = ?1",
        [first_sequence],
    ));
    assert_constraint_violation(
        connection.execute("DELETE FROM task_completions WHERE id = 'completion-1'", []),
    );

    connection
        .execute(
            "INSERT INTO idempotency_records (
                 actor_type, actor_id, request_id, payload_hash, result_json, created_at
             ) VALUES ('session', 'session-1', 'request-1', zeroblob(32), '{}', 1)",
            [],
        )
        .unwrap();
    assert_constraint_violation(connection.execute(
        "INSERT INTO idempotency_records (
             actor_type, actor_id, request_id, payload_hash, result_json, created_at
         ) VALUES ('session', 'session-1', 'request-1', zeroblob(32), '{}', 2)",
        [],
    ));
}
