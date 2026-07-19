use sessionbridge::{
    ErrorCode,
    app::{
        chat::{ChatEntityRef, ChatService, SendChatMessageRequest},
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::FakeClock,
    domain::{AutonomyPolicy, CompletionPolicy, ParticipantKind, SessionContext},
    invitation::PromptShell,
    storage::SqliteStore,
    workspace::{LocatedWorkspace, read_json_private},
};
use tempfile::tempdir;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[test]
fn task_chat_is_audited_idempotent_and_shared_between_agent_and_operator() {
    let project = tempdir().unwrap();
    let state = tempdir().unwrap();
    let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + Duration::hours(3));
    WorkspaceService::new(clock.clone())
        .initialize(project.path(), "chat", Some(state.path()))
        .unwrap();
    let workspace =
        LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();
    let mut store = SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )
    .unwrap();
    let task = TaskService::new(clock.clone())
        .publish(
            store.connection_mut(),
            WorkspaceBinding::from(&workspace),
            PublishTaskRequest {
                title: "Chat task".into(),
                objective: "Coordinate through a visible workgroup".into(),
                constraints: Vec::new(),
                non_goals: Vec::new(),
                done_conditions: vec!["Chat remains auditable".into()],
                autonomy_policy: AutonomyPolicy::Bounded,
                completion_policy: CompletionPolicy::AllWorkersConcluded,
                target_branch: None,
                request_id: Some(Uuid::now_v7().to_string()),
            },
        )
        .unwrap();
    let service = ChatService::new(clock.clone());
    let pre_join_message = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            task.task.id,
            SendChatMessageRequest {
                kind: "note".into(),
                body: "Published before this Session joined.".into(),
                reply_to_message_id: None,
                mentions: Vec::new(),
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let invitation = InvitationService::new(
        &mut store,
        &clock,
        workspace.config.project_id,
        workspace.paths.state_root.join("chat-invites"),
    )
    .unwrap()
    .create_batch(CreateInvitationBatchRequest {
        task_id: task.task.id,
        count: 1,
        participant_kind: ParticipantKind::Worker,
        expires_in: Duration::hours(1),
        capability_limits: Vec::new(),
        request_id: Uuid::now_v7().to_string(),
        shell: PromptShell::Posix,
        provision_worktree: false,
    })
    .unwrap();
    let invite_file = match &invitation.prompts[0].secret_delivery {
        PublicSecretDelivery::File { invite_path } => invite_path.clone(),
    };
    let joined = JoinService::new(clock.clone())
        .join(
            &mut store,
            &workspace,
            JoinRequest {
                invite_file,
                provision_worktree: false,
                worktree: None,
                declared_capabilities: Vec::new(),
                label: Some("Implementer".into()),
                client_name: Some("chat-test".into()),
                model: None,
                request_id: None,
            },
        )
        .unwrap();
    let context: SessionContext = read_json_private(&joined.context_path).unwrap();
    let request_id = Uuid::now_v7().to_string();
    let agent_message = service
        .send_agent(
            &mut store,
            &context,
            SendChatMessageRequest {
                kind: "intent".into(),
                body: "I will inspect the runtime boundary before changing code.".into(),
                reply_to_message_id: None,
                mentions: Vec::new(),
                related_entities: vec![ChatEntityRef {
                    entity_type: "task".into(),
                    entity_id: task.task.id.to_string(),
                }],
                request_id: request_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(agent_message.sender_label, "Implementer");
    let replay = service
        .send_agent(
            &mut store,
            &context,
            SendChatMessageRequest {
                kind: "intent".into(),
                body: "I will inspect the runtime boundary before changing code.".into(),
                reply_to_message_id: None,
                mentions: Vec::new(),
                related_entities: vec![ChatEntityRef {
                    entity_type: "task".into(),
                    entity_id: task.task.id.to_string(),
                }],
                request_id,
            },
        )
        .unwrap();
    assert_eq!(replay.id, agent_message.id);

    let operator_message = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            task.task.id,
            SendChatMessageRequest {
                kind: "question".into(),
                body: "Which boundary is most likely to affect the integration?".into(),
                reply_to_message_id: Some(agent_message.id.clone()),
                mentions: vec![context.session_id.to_string()],
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert!(operator_message.event_sequence > agent_message.event_sequence);
    let inbox = service.inbox(store.connection(), &context, 20).unwrap();
    assert_eq!(inbox.unread_count, 1);
    assert_eq!(inbox.directed_unread_count, 1);
    assert_eq!(inbox.messages[0].message.id, operator_message.id);
    assert_eq!(inbox.messages[0].direction_reason, "mention");
    assert!(
        inbox
            .messages
            .iter()
            .all(|message| message.message.id != pre_join_message.id)
    );
    let ack = service
        .ack_agent(&mut store, &context, vec![operator_message.id.clone()])
        .unwrap();
    assert_eq!(ack.newly_acknowledged, 1);
    let replayed_ack = service
        .ack_agent(&mut store, &context, vec![operator_message.id.clone()])
        .unwrap();
    assert_eq!(replayed_ack.newly_acknowledged, 0);
    assert_eq!(replayed_ack.unread_count, 0);

    for index in 0..3 {
        service
            .send_operator(
                &mut store,
                workspace.config.project_id,
                task.task.id,
                SendChatMessageRequest {
                    kind: "progress".into(),
                    body: format!("Broadcast {index}"),
                    reply_to_message_id: None,
                    mentions: Vec::new(),
                    related_entities: Vec::new(),
                    request_id: Uuid::now_v7().to_string(),
                },
            )
            .unwrap();
    }
    let priority_message = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            task.task.id,
            SendChatMessageRequest {
                kind: "question".into(),
                body: "Directed messages must not be displaced.".into(),
                reply_to_message_id: None,
                mentions: vec![context.session_id.to_string()],
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let priority_inbox = service.inbox(store.connection(), &context, 1).unwrap();
    assert_eq!(priority_inbox.messages[0].message.id, priority_message.id);
    assert!(priority_inbox.messages[0].directed);
    assert!(priority_inbox.has_more);
    let awaited = service.await_agent(&mut store, &context, 1, true).unwrap();
    assert_eq!(awaited.messages.len(), 1);
    assert_eq!(awaited.messages[0].message.id, priority_message.id);
    assert!(awaited.messages[0].directed);

    let page = service
        .history(store.connection(), task.task.id, 0, 20)
        .unwrap();
    assert_eq!(page.messages.len(), 7);
    assert_eq!(page.next_cursor, priority_message.event_sequence);
    let event_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE task_id=?1 AND event_type='chat.message_sent'",
            [task.task.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 7);

    let invalid_reply = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            task.task.id,
            SendChatMessageRequest {
                kind: "note".into(),
                body: "Invalid reply".into(),
                reply_to_message_id: Some(Uuid::now_v7().to_string()),
                mentions: Vec::new(),
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(invalid_reply.code(), ErrorCode::NotFound);

    let other_task = TaskService::new(clock.clone())
        .publish(
            store.connection_mut(),
            WorkspaceBinding::from(&workspace),
            PublishTaskRequest {
                title: "Other chat task".into(),
                objective: "Remain isolated from the first Task chat".into(),
                constraints: Vec::new(),
                non_goals: Vec::new(),
                done_conditions: vec!["Cross-Task references are rejected".into()],
                autonomy_policy: AutonomyPolicy::Bounded,
                completion_policy: CompletionPolicy::AllWorkersConcluded,
                target_branch: None,
                request_id: Some(Uuid::now_v7().to_string()),
            },
        )
        .unwrap();
    let other_message = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            other_task.task.id,
            SendChatMessageRequest {
                kind: "note".into(),
                body: "This message belongs to another Task.".into(),
                reply_to_message_id: None,
                mentions: Vec::new(),
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    let cross_task_reply = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            task.task.id,
            SendChatMessageRequest {
                kind: "note".into(),
                body: "Cross-Task reply".into(),
                reply_to_message_id: Some(other_message.id),
                mentions: Vec::new(),
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(cross_task_reply.code(), ErrorCode::NotFound);

    let cross_task_mention = service
        .send_operator(
            &mut store,
            workspace.config.project_id,
            other_task.task.id,
            SendChatMessageRequest {
                kind: "question".into(),
                body: "Cross-Task mention".into(),
                reply_to_message_id: None,
                mentions: vec![context.session_id.to_string()],
                related_entities: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(cross_task_mention.code(), ErrorCode::NotFound);
}
