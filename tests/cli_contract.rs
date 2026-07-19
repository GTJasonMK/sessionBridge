use std::path::PathBuf;

use clap::{Parser, error::ErrorKind};
use sessionbridge::{
    cli::{
        AgentCommand, AgentIntentCommand, AgentInviteCommand, ClaimCommand, ClaimScopeCommand, Cli,
        Command, ConfigCommand, ItemCommand, LaunchCommand, PromptCommand, PromptShell,
        SecretDelivery, TaskCommand,
    },
    domain::{ClaimMode, ConclusionOutcome},
    output::OutputFormat,
};

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn parses_the_documented_user_workflow() {
    let init = parse(&["sessionbridge", "init", "--name", "SessionBridge"]);
    let Command::Init(init) = init.command else {
        panic!("expected init command")
    };
    assert_eq!(init.name.as_deref(), Some("SessionBridge"));
    assert_eq!(init.path, PathBuf::from("."));

    let publish = parse(&[
        "sessionbridge",
        "task",
        "publish",
        "--title",
        "实现多 Agent 自主协同 MVP",
        "--objective-file",
        "objective.md",
        "--constraints-file",
        "constraints.md",
        "--done-file",
        "definition-of-done.md",
        "--autonomy",
        "bounded",
    ]);
    let Command::Task(task) = publish.command else {
        panic!("expected task command")
    };
    let TaskCommand::Publish(publish) = task.command else {
        panic!("expected task publish command")
    };
    assert_eq!(publish.title, "实现多 Agent 自主协同 MVP");
    assert_eq!(publish.objective_file, Some(PathBuf::from("objective.md")));

    let invite = parse(&[
        "sessionbridge",
        "agent",
        "invite",
        "create",
        "--task",
        "task-1",
        "--count",
        "4",
        "--expires-in",
        "1h",
        "--prompt-dir",
        "/private/prompts",
        "--format",
        "json",
    ]);
    assert_eq!(invite.format, OutputFormat::Json);
    let Command::Agent(agent) = invite.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Invite(invite) = agent.command else {
        panic!("expected invite command")
    };
    let AgentInviteCommand::Create(invite) = invite.command else {
        panic!("expected invite create command")
    };
    assert_eq!(invite.task, "task-1");
    assert_eq!(invite.count, 4);
    assert_eq!(invite.expires_in, "1h");
    assert_eq!(invite.prompt_dir, Some(PathBuf::from("/private/prompts")));
    assert_eq!(invite.secret_delivery, SecretDelivery::File);

    let watch = parse(&["sessionbridge", "task", "watch", "task-1"]);
    let Command::Task(task) = watch.command else {
        panic!("expected task command")
    };
    let TaskCommand::Watch(watch) = task.command else {
        panic!("expected task watch command")
    };
    assert_eq!(watch.task, "task-1");
    assert_eq!(watch.poll_interval, "500ms");
}

#[test]
fn parses_launch_prompt_and_shortcut_commands() {
    let gui = parse(&["sessionbridge", "gui", "--foreground"]);
    let Command::Gui(gui) = gui.command else {
        panic!("expected gui command")
    };
    assert!(gui.foreground);

    let launch = parse(&[
        "sessionbridge",
        "launch",
        "--brief",
        "task.md",
        "--agents",
        "6",
        "--expires-in",
        "4h",
        "--profile",
        "large",
    ]);
    let Command::Launch(launch) = launch.command else {
        panic!("expected launch command")
    };
    assert!(launch.command.is_none());
    assert_eq!(launch.brief, Some(PathBuf::from("task.md")));
    assert_eq!(launch.agents, Some(6));

    let resume = parse(&[
        "sessionbridge",
        "launch",
        "resume",
        "019f4d40-1111-7111-8111-111111111111",
        "--accept-baseline-change",
    ]);
    let Command::Launch(resume) = resume.command else {
        panic!("expected launch resume command")
    };
    assert!(matches!(resume.command, Some(LaunchCommand::Resume(_))));

    let prompt = parse(&["sessionbridge", "prompt", "show", "current", "--index", "1"]);
    let Command::Prompt(prompt) = prompt.command else {
        panic!("expected prompt command")
    };
    assert!(matches!(prompt.command, PromptCommand::Show(_)));

    assert!(matches!(
        parse(&["sessionbridge", "status"]).command,
        Command::Status(_)
    ));
    assert!(matches!(
        parse(&["sessionbridge", "watch", "--once"]).command,
        Command::Watch(_)
    ));
    let config = parse(&["sessionbridge", "config", "validate"]);
    let Command::Config(config) = config.command else {
        panic!("expected config command")
    };
    assert!(matches!(config.command, ConfigCommand::Validate));

    let conflict = Cli::try_parse_from([
        "sessionbridge",
        "launch",
        "resume",
        "019f4d40-1111-7111-8111-111111111111",
        "--agents",
        "8",
    ])
    .unwrap_err();
    assert_eq!(conflict.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn parses_the_documented_agent_loop_commands() {
    let commands: &[&[&str]] = &[
        &[
            "sessionbridge",
            "agent",
            "join",
            "--invite-file",
            "invite.json",
            "--capabilities-file",
            "capabilities.json",
            "--provision-worktree",
        ],
        &[
            "sessionbridge",
            "agent",
            "next",
            "--limit",
            "20",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "agent",
            "consolidate",
            "claim",
            "consolidation-1",
        ],
        &[
            "sessionbridge",
            "agent",
            "consolidate",
            "resolve",
            "consolidation-1",
            "--input",
            "resolution.json",
        ],
        &[
            "sessionbridge",
            "agent",
            "propose-action",
            "--kind",
            "alternative_validation",
            "--input",
            "proposal.json",
        ],
        &[
            "sessionbridge",
            "agent",
            "sync",
            "--full",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "work",
            "start",
            "item-1",
            "--resources-file",
            "resources.json",
            "--lease",
            "5m",
        ],
        &[
            "sessionbridge",
            "work",
            "submit",
            "item-1",
            "--bundle",
            "submission.json",
        ],
        &[
            "sessionbridge",
            "work",
            "review",
            "item-1",
            "--decision",
            "approved",
            "--evidence-file",
            "evidence-ids.json",
        ],
        &[
            "sessionbridge",
            "work",
            "speculate",
            "start",
            "--input-json",
            "speculation.json",
        ],
        &[
            "sessionbridge",
            "work",
            "speculate",
            "checkpoint",
            "speculation-1",
            "--input",
            "checkpoint.json",
        ],
        &[
            "sessionbridge",
            "work",
            "speculate",
            "adopt",
            "speculation-1",
        ],
        &[
            "sessionbridge",
            "work",
            "speculate",
            "abandon",
            "speculation-1",
            "--reason",
            "superseded by validated plan",
        ],
        &[
            "sessionbridge",
            "work",
            "integrate",
            "prepare",
            "item-1",
            "--lease",
            "15m",
        ],
        &[
            "sessionbridge",
            "work",
            "integrate",
            "confirm",
            "preparation-1",
            "--details-file",
            "integration.json",
        ],
        &["sessionbridge", "logs", "path", "--format", "json"],
        &[
            "sessionbridge",
            "daemon",
            "--once",
            "--interval",
            "1s",
            "--limit",
            "100",
        ],
        &[
            "sessionbridge",
            "logs",
            "tail",
            "--lines",
            "200",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "agent",
            "candidates",
            "--explain",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "agent",
            "intent",
            "set",
            "--input-json",
            "-",
        ],
        &["sessionbridge", "gap", "report", "--input-json", "-"],
        &["sessionbridge", "item", "propose", "--input-json", "-"],
        &[
            "sessionbridge",
            "item",
            "propose",
            "--claim",
            "--input-json",
            "-",
        ],
        &[
            "sessionbridge",
            "item",
            "split",
            "item-1",
            "--input-json",
            "-",
        ],
        &[
            "sessionbridge",
            "item",
            "claim",
            "item-1",
            "--mode",
            "execute",
            "--expected-version",
            "3",
            "--observed-plan-version",
            "8",
            "--resources-file",
            "resources.json",
            "--request-id",
            "request-1",
        ],
        &["sessionbridge", "claim", "renew", "claim-1", "--fence", "2"],
        &[
            "sessionbridge",
            "claim",
            "scope",
            "expand",
            "claim-1",
            "--input-json",
            "-",
        ],
        &[
            "sessionbridge",
            "attempt",
            "checkpoint",
            "attempt-1",
            "--input-json",
            "-",
        ],
        &["sessionbridge", "coord", "request", "--input-json", "-"],
        &[
            "sessionbridge",
            "agent",
            "inbox",
            "--since",
            "42",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "agent",
            "await",
            "work-items",
            "--mode",
            "all",
            "--item",
            "01900000-0000-7000-8000-000000000001",
            "--item",
            "01900000-0000-7000-8000-000000000002",
            "--format",
            "json",
        ],
        &[
            "sessionbridge",
            "agent",
            "await",
            "external",
            "--detector",
            "detector.json",
            "--deadline-in",
            "2h",
            "--reassess-every",
            "30s",
        ],
        &[
            "sessionbridge",
            "agent",
            "await",
            "resume",
            "01900000-0000-7000-8000-000000000003",
        ],
        &[
            "sessionbridge",
            "item",
            "submit",
            "item-1",
            "--claim",
            "claim-1",
            "--fence",
            "2",
            "--evidence-file",
            "evidence.json",
            "--changeset-file",
            "changeset.json",
        ],
    ];

    for args in commands {
        Cli::try_parse_from(*args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
    }

    let intent = parse(&[
        "sessionbridge",
        "agent",
        "intent",
        "set",
        "--input-json",
        "-",
    ]);
    let Command::Agent(agent) = intent.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Intent(intent) = agent.command else {
        panic!("expected agent intent command")
    };
    let AgentIntentCommand::Set(input) = intent.command else {
        panic!("expected intent set command")
    };
    assert_eq!(input.input_json, PathBuf::from("-"));

    let claim = parse(&[
        "sessionbridge",
        "item",
        "claim",
        "item-1",
        "--mode",
        "execute",
        "--resources-file",
        "resources.json",
    ]);
    let Command::Item(item) = claim.command else {
        panic!("expected item command")
    };
    let ItemCommand::Claim(claim) = item.command else {
        panic!("expected item claim command")
    };
    assert_eq!(claim.mode, ClaimMode::Execute);

    let scope = parse(&[
        "sessionbridge",
        "claim",
        "scope",
        "expand",
        "claim-1",
        "--input-json",
        "-",
    ]);
    let Command::Claim(claim) = scope.command else {
        panic!("expected claim command")
    };
    let ClaimCommand::Scope(scope) = claim.command else {
        panic!("expected claim scope command")
    };
    assert!(matches!(scope.command, ClaimScopeCommand::Expand(_)));
}

#[test]
fn legacy_polling_wait_is_not_a_supported_agent_command() {
    assert!(
        Cli::try_parse_from([
            "sessionbridge",
            "agent",
            "wait",
            "--since",
            "42",
            "--timeout",
            "30s",
        ])
        .is_err()
    );
}

#[test]
fn parses_agent_chat_send_and_history() {
    for args in [
        vec![
            "sessionbridge",
            "agent",
            "chat",
            "send",
            "--input",
            "message.json",
        ],
        vec![
            "sessionbridge",
            "agent",
            "chat",
            "history",
            "--since",
            "42",
            "--limit",
            "100",
        ],
    ] {
        Cli::try_parse_from(args).unwrap();
    }
}

#[test]
fn parses_completion_challenge_and_capability_request_commands() {
    for args in [
        [
            "sessionbridge",
            "agent",
            "completion-challenge",
            "--input",
            "challenge.json",
        ],
        [
            "sessionbridge",
            "agent",
            "request-capability",
            "--input",
            "capability.json",
        ],
    ] {
        Cli::try_parse_from(args).unwrap();
    }
}

#[test]
fn facade_commands_accept_dry_run_planning() {
    for args in [
        vec!["sessionbridge", "work", "start", "item", "--dry-run"],
        vec![
            "sessionbridge",
            "work",
            "submit",
            "item",
            "--bundle",
            "bundle.json",
            "--dry-run",
        ],
        vec![
            "sessionbridge",
            "work",
            "review",
            "item",
            "--decision",
            "approved",
            "--dry-run",
        ],
        vec![
            "sessionbridge",
            "work",
            "integrate",
            "prepare",
            "item",
            "--dry-run",
        ],
        vec![
            "sessionbridge",
            "work",
            "integrate",
            "confirm",
            "preparation",
            "--dry-run",
        ],
    ] {
        Cli::try_parse_from(args).unwrap();
    }
}

#[test]
fn incremental_agent_sync_requires_full_mode_and_a_snapshot_baseline() {
    assert!(Cli::try_parse_from(["sessionbridge", "agent", "sync"]).is_err());
    assert!(Cli::try_parse_from(["sessionbridge", "agent", "sync", "--since", "10"]).is_err());
    Cli::try_parse_from([
        "sessionbridge",
        "agent",
        "sync",
        "--full",
        "--since",
        "10",
        "--snapshot-id",
        "signed-snapshot",
    ])
    .unwrap();
}

#[test]
fn global_machine_arguments_work_after_deep_subcommands() {
    let cli = parse(&[
        "sessionbridge",
        "item",
        "claim",
        "item-1",
        "--mode",
        "review",
        "--format",
        "jsonl",
        "--context",
        "/state/session.json",
        "--workspace",
        "workspace-1",
        "--non-interactive",
        "--request-id",
        "request-1",
        "--expected-version",
        "7",
        "--expected-plan-version",
        "11",
        "--observed-plan-version",
        "10",
        "--no-color",
    ]);

    assert_eq!(cli.format, OutputFormat::Jsonl);
    assert_eq!(cli.context, Some(PathBuf::from("/state/session.json")));
    assert_eq!(cli.workspace, Some(PathBuf::from("workspace-1")));
    assert!(cli.non_interactive);
    assert_eq!(cli.request_id.as_deref(), Some("request-1"));
    assert_eq!(cli.expected_version, Some(7));
    assert_eq!(cli.expected_plan_version, Some(11));
    assert_eq!(cli.observed_plan_version, Some(10));
    assert!(cli.no_color);
}

#[test]
fn parses_no_further_work_as_the_kebab_case_cli_value() {
    let cli = parse(&[
        "sessionbridge",
        "agent",
        "conclude",
        "--outcome",
        "no-further-work",
        "--completion-token",
        "signed-completion-token",
        "--assessment-file",
        "completion-assessment.json",
        "--request-id",
        "request-1",
    ]);

    let Command::Agent(agent) = cli.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Conclude(conclude) = agent.command else {
        panic!("expected conclude command")
    };
    assert_eq!(conclude.outcome, ConclusionOutcome::NoFurtherWork);
    assert_eq!(
        conclude.assessment_file,
        Some(PathBuf::from("completion-assessment.json"))
    );
    assert_eq!(
        conclude.completion_token.as_deref(),
        Some("signed-completion-token")
    );
}

#[test]
fn conclude_supports_typed_assessment_without_a_manual_completion_token() {
    let cli = parse(&[
        "sessionbridge",
        "agent",
        "conclude",
        "--outcome",
        "no-further-work",
        "--summary",
        "No further work remains",
        "--checked",
        "requirements and repository state",
    ]);
    let Command::Agent(agent) = cli.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Conclude(conclude) = agent.command else {
        panic!("expected conclude command")
    };
    assert!(conclude.assessment_file.is_none());
    assert!(conclude.completion_token.is_none());
    assert_eq!(conclude.checked, ["requirements and repository state"]);

    assert!(
        Cli::try_parse_from([
            "sessionbridge",
            "agent",
            "conclude",
            "--outcome",
            "no-further-work",
            "--assessment-file",
            "completion-assessment.json",
            "--summary",
            "conflicting source",
            "--checked",
            "repository",
        ])
        .is_err()
    );
}

#[test]
fn parses_all_invitation_output_controls() {
    let cli = parse(&[
        "sessionbridge",
        "agent",
        "invite",
        "create",
        "--task",
        "task-1",
        "--count",
        "4",
        "--expires-in",
        "45m",
        "--prompt-dir",
        "/private/prompts",
        "--participant-kind",
        "worker",
        "--capabilities-file",
        "capabilities.json",
        "--secret-delivery",
        "file",
        "--shell",
        "powershell",
        "--format",
        "jsonl",
    ]);

    assert_eq!(cli.format, OutputFormat::Jsonl);
    let Command::Agent(agent) = cli.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Invite(invite) = agent.command else {
        panic!("expected invite command")
    };
    let AgentInviteCommand::Create(create) = invite.command else {
        panic!("expected invite create command")
    };
    assert_eq!(create.count, 4);
    assert_eq!(create.secret_delivery, SecretDelivery::File);
    assert_eq!(create.shell, PromptShell::Powershell);

    let inline = parse(&[
        "sessionbridge",
        "agent",
        "invite",
        "create",
        "--task",
        "task-1",
        "--secret-delivery",
        "inline",
        "--allow-secret-output",
        "--format",
        "text",
    ]);
    let Command::Agent(agent) = inline.command else {
        panic!("expected agent command")
    };
    let AgentCommand::Invite(invite) = agent.command else {
        panic!("expected invite command")
    };
    let AgentInviteCommand::Create(create) = invite.command else {
        panic!("expected invite create command")
    };
    assert_eq!(create.secret_delivery, SecretDelivery::Inline);
    assert!(create.allow_secret_output);
}

#[test]
fn parses_invitation_show_revoke_and_rotate() {
    let commands: &[&[&str]] = &[
        &[
            "sessionbridge",
            "agent",
            "invite",
            "show",
            "invite-1",
            "--prompt",
            "--audit",
            "--format",
            "text",
        ],
        &[
            "sessionbridge",
            "agent",
            "invite",
            "revoke",
            "invite-1",
            "--reason",
            "sent to wrong session",
        ],
        &[
            "sessionbridge",
            "agent",
            "invite",
            "rotate",
            "invite-1",
            "--expires-in",
            "1h",
            "--format",
            "text",
        ],
    ];

    for args in commands {
        Cli::try_parse_from(*args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
    }
}

#[test]
fn join_requires_exactly_one_invitation_source() {
    let missing = Cli::try_parse_from(["sessionbridge", "agent", "join"]).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);

    let both = Cli::try_parse_from([
        "sessionbridge",
        "agent",
        "join",
        "--invite-file",
        "invite.json",
        "--invite-stdin",
    ])
    .unwrap_err();
    assert_eq!(both.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn join_rejects_two_worktree_provisioning_modes() {
    let error = Cli::try_parse_from([
        "sessionbridge",
        "agent",
        "join",
        "--invite-file",
        "invite.json",
        "--provision-worktree",
        "--worktree",
        "/prepared/worktree",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn agent_join_exposes_nickname_instead_of_the_internal_label_name() {
    Cli::try_parse_from([
        "sessionbridge",
        "agent",
        "join",
        "--invite-file",
        "invite.json",
        "--nickname",
        "Build Reviewer",
    ])
    .unwrap();
    assert!(
        Cli::try_parse_from([
            "sessionbridge",
            "agent",
            "join",
            "--invite-file",
            "invite.json",
            "--label",
            "legacy",
        ])
        .is_err()
    );
}

#[test]
fn task_publish_requires_exactly_one_objective_source() {
    let missing =
        Cli::try_parse_from(["sessionbridge", "task", "publish", "--title", "task"]).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);

    let both = Cli::try_parse_from([
        "sessionbridge",
        "task",
        "publish",
        "--title",
        "task",
        "--objective",
        "objective",
        "--objective-file",
        "objective.md",
    ])
    .unwrap_err();
    assert_eq!(both.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn inline_secret_delivery_requires_explicit_secret_output_consent() {
    let error = Cli::try_parse_from([
        "sessionbridge",
        "agent",
        "invite",
        "create",
        "--task",
        "task-1",
        "--secret-delivery",
        "inline",
        "--format",
        "text",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn typed_commands_reject_unmodelled_trailing_arguments() {
    let error = Cli::try_parse_from([
        "sessionbridge",
        "requirement",
        "add",
        "--input-json",
        "requirement.json",
        "unmodelled-tail",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}
