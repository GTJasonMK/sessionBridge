use std::path::Path;

use sessionbridge::invitation::{
    InviteSecret, InviteSecretInput, InviteToken, PromptDelivery, PromptDestination,
    PromptOutputFormat, PromptOutputOptions, PromptRendererV1, PromptShell, PromptSpec,
    SecretDeliveryMode, parse_duration, validate_output_options,
};
use time::{Duration as TimeDuration, OffsetDateTime};

fn secret(token: InviteToken) -> InviteSecret {
    InviteSecret::new(InviteSecretInput {
        invite_id: "inv-123".into(),
        task_id: "task-123".into(),
        completion_epoch: 1,
        batch_id: Some("batch-123".into()),
        token,
        protocol_version: "1".into(),
        prompt_template_version: "1".into(),
        expires_at: OffsetDateTime::now_utc() + TimeDuration::hours(1),
    })
    .expect("secret")
}

#[test]
fn token_round_trip_hashes_and_redacts_debug_output() {
    let token = InviteToken::generate().expect("token");
    let parsed = InviteToken::parse(token.expose_secret()).expect("parse token");
    let hash = token.hash();

    assert_eq!(parsed, token);
    assert!(hash.verify(&parsed));
    assert!(!format!("{token:?}").contains(token.expose_secret()));
    assert_eq!(hash.to_hex().len(), 64);
}

#[test]
fn invite_secret_json_round_trips_but_debug_redacts_token() {
    let token = InviteToken::generate().expect("token");
    let secret = secret(token.clone());
    let json = secret.to_json_bytes().expect("serialize secret");
    let decoded = InviteSecret::from_json_slice(&json).expect("deserialize secret");

    assert_eq!(decoded, secret);
    assert!(
        String::from_utf8(json)
            .unwrap()
            .contains(token.expose_secret())
    );
    assert!(!format!("{secret:?}").contains(token.expose_secret()));
}

#[test]
fn default_file_prompt_never_contains_the_token() {
    let token = InviteToken::generate().expect("token");
    let secret = secret(token.clone());
    let invite_path = Path::new("/tmp/sessionbridge/invite's secret.json");
    let prompt = PromptRendererV1
        .render(&PromptSpec {
            task_id: &secret.task_id,
            invite_id: &secret.invite_id,
            expires_at: &secret.expires_at,
            protocol_version: &secret.protocol_version,
            shell: PromptShell::Posix,
            delivery: PromptDelivery::File { invite_path },
            provision_worktree: true,
        })
        .expect("render prompt");

    assert!(!prompt.text.contains(token.expose_secret()));
    assert!(prompt.text.contains("Task ID: task-123"));
    assert!(prompt.text.contains("INVITE_CONSUMED"));
    assert!(prompt.text.contains("agent await work-items"));
    assert!(prompt.text.contains("await 没有时间超时"));
    assert!(prompt.text.contains("--nickname '<你的昵称>'"));
    assert!(prompt.text.contains("invite'\"'\"'s secret.json"));
    assert_eq!(prompt.sha256.len(), 64);
}

#[test]
fn powershell_prompt_quotes_single_quotes() {
    let token = InviteToken::generate().expect("token");
    let secret = secret(token);
    let prompt = PromptRendererV1
        .render(&PromptSpec {
            task_id: &secret.task_id,
            invite_id: &secret.invite_id,
            expires_at: &secret.expires_at,
            protocol_version: &secret.protocol_version,
            shell: PromptShell::PowerShell,
            delivery: PromptDelivery::File {
                invite_path: Path::new("C:\\Users\\O'Brien\\invite.json"),
            },
            provision_worktree: true,
        })
        .expect("render prompt");

    assert!(prompt.text.contains("C:\\Users\\O''Brien\\invite.json"));
}

#[test]
fn inline_prompt_contains_secret_only_when_explicitly_selected() {
    let token = InviteToken::generate().expect("token");
    let secret = secret(token.clone());
    let prompt = PromptRendererV1
        .render(&PromptSpec {
            task_id: &secret.task_id,
            invite_id: &secret.invite_id,
            expires_at: &secret.expires_at,
            protocol_version: &secret.protocol_version,
            shell: PromptShell::Posix,
            delivery: PromptDelivery::Inline { secret: &secret },
            provision_worktree: false,
        })
        .expect("render inline prompt");

    assert!(prompt.text.contains(token.expose_secret()));
    assert!(prompt.text.contains("--invite-stdin"));
}

#[test]
fn output_matrix_rejects_batch_and_machine_inline_secret_output() {
    let batch_inline = PromptOutputOptions {
        count: 2,
        format: PromptOutputFormat::Text,
        print_prompt: true,
        prompt_dir: None,
        secret_delivery: SecretDeliveryMode::Inline,
        allow_secret_output: true,
    };
    assert!(validate_output_options(&batch_inline).is_err());

    let json_inline = PromptOutputOptions {
        count: 1,
        format: PromptOutputFormat::Json,
        print_prompt: false,
        prompt_dir: None,
        secret_delivery: SecretDeliveryMode::Inline,
        allow_secret_output: true,
    };
    assert!(validate_output_options(&json_inline).is_err());

    let unacknowledged_inline = PromptOutputOptions {
        count: 1,
        format: PromptOutputFormat::Text,
        print_prompt: true,
        prompt_dir: None,
        secret_delivery: SecretDeliveryMode::Inline,
        allow_secret_output: false,
    };
    assert!(validate_output_options(&unacknowledged_inline).is_err());
}

#[test]
fn output_matrix_accepts_single_text_and_batch_manifest() {
    let single = validate_output_options(&PromptOutputOptions {
        count: 1,
        format: PromptOutputFormat::Text,
        print_prompt: true,
        prompt_dir: None,
        secret_delivery: SecretDeliveryMode::File,
        allow_secret_output: false,
    })
    .expect("single output");
    assert_eq!(single.destination, PromptDestination::Stdout);

    let batch = validate_output_options(&PromptOutputOptions {
        count: 4,
        format: PromptOutputFormat::Json,
        print_prompt: false,
        prompt_dir: None,
        secret_delivery: SecretDeliveryMode::File,
        allow_secret_output: false,
    })
    .expect("batch output");
    assert_eq!(
        batch.destination,
        PromptDestination::ManifestWithDefaultFiles
    );
}

#[test]
fn compact_duration_parser_is_checked_and_rejects_zero() {
    assert_eq!(parse_duration("1h30m").unwrap().as_secs(), 5_400);
    assert_eq!(parse_duration("2d").unwrap().as_secs(), 172_800);
    assert!(parse_duration("0s").is_err());
    assert!(parse_duration("1month").is_err());
    assert!(parse_duration("18446744073709551615w").is_err());
}
