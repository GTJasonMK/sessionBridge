use std::path::PathBuf;

use super::{InvitationError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutputFormat {
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretDeliveryMode {
    File,
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptOutputOptions {
    pub count: usize,
    pub format: PromptOutputFormat,
    pub print_prompt: bool,
    pub prompt_dir: Option<PathBuf>,
    pub secret_delivery: SecretDeliveryMode,
    pub allow_secret_output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptDestination {
    Stdout,
    Files { directory: PathBuf },
    ManifestWithDefaultFiles,
    ManifestWithFiles { directory: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPromptOutput {
    pub count: usize,
    pub format: PromptOutputFormat,
    pub destination: PromptDestination,
    pub secret_delivery: SecretDeliveryMode,
}

pub fn validate_output_options(options: &PromptOutputOptions) -> Result<ValidatedPromptOutput> {
    if options.count == 0 {
        return Err(invalid("count must be greater than zero"));
    }

    if options.secret_delivery == SecretDeliveryMode::Inline {
        if options.count != 1 {
            return Err(invalid(
                "inline secret delivery is limited to one invitation",
            ));
        }
        if options.format != PromptOutputFormat::Text || !options.print_prompt {
            return Err(invalid(
                "inline secret delivery requires --print-prompt --format text",
            ));
        }
        if options.prompt_dir.is_some() {
            return Err(invalid(
                "inline secret delivery cannot persist a prompt file",
            ));
        }
        if !options.allow_secret_output {
            return Err(invalid(
                "inline secret delivery requires explicit secret-output acknowledgement",
            ));
        }

        return Ok(ValidatedPromptOutput {
            count: 1,
            format: PromptOutputFormat::Text,
            destination: PromptDestination::Stdout,
            secret_delivery: SecretDeliveryMode::Inline,
        });
    }

    if options.allow_secret_output {
        return Err(invalid(
            "secret-output acknowledgement is only valid for inline delivery",
        ));
    }

    let destination = match options.format {
        PromptOutputFormat::Text if options.count == 1 => {
            if !options.print_prompt {
                return Err(invalid("single text output requires --print-prompt"));
            }
            if options.prompt_dir.is_some() {
                return Err(invalid(
                    "single text output cannot use --print-prompt and --prompt-dir together",
                ));
            }
            PromptDestination::Stdout
        }
        PromptOutputFormat::Text => {
            if options.print_prompt {
                return Err(invalid("batch prompts cannot be concatenated on stdout"));
            }
            let directory = options
                .prompt_dir
                .clone()
                .ok_or_else(|| invalid("batch text output requires a private prompt directory"))?;
            PromptDestination::Files { directory }
        }
        PromptOutputFormat::Json | PromptOutputFormat::Jsonl => {
            if options.print_prompt {
                return Err(invalid(
                    "--print-prompt is only valid with single text output",
                ));
            }
            match &options.prompt_dir {
                Some(directory) => PromptDestination::ManifestWithFiles {
                    directory: directory.clone(),
                },
                None => PromptDestination::ManifestWithDefaultFiles,
            }
        }
    };

    Ok(ValidatedPromptOutput {
        count: options.count,
        format: options.format,
        destination,
        secret_delivery: SecretDeliveryMode::File,
    })
}

fn invalid(message: impl Into<String>) -> InvitationError {
    InvitationError::InvalidOutput(message.into())
}
