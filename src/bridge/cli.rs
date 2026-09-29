use super::*;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sessionbridge", about = "Local workspace collaboration")]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Open the desktop workgroup application.
    Gui,
    /// Run the local workspace service.
    Serve,
    /// Agent MCP transport. Context is issued by the host, never by the model.
    Mcp {
        #[arg(long)]
        context: PathBuf,
    },
    /// Call a v2 operation with JSON input (stdin with --input -).
    Call {
        #[arg(long)]
        workspace: PathBuf,
        operation: String,
        #[arg(long, default_value = "{}")]
        input: String,
    },
}
pub fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Gui => {
            let path = if let Some(path) = std::env::var_os("SESSIONBRIDGE_GUI") {
                PathBuf::from(path)
            } else {
                let sibling = std::env::current_exe()?.with_file_name("sessionbridge-gui");
                if sibling.is_file() {
                    sibling
                } else {
                    PathBuf::from("sessionbridge-gui")
                }
            };
            std::process::Command::new(path).spawn()?;
            Ok(())
        }
        Commands::Serve => ipc::serve(),
        Commands::Mcp { context } => mcp::serve(&context),
        Commands::Call {
            workspace,
            operation,
            input,
        } => {
            ipc::start(&std::env::current_exe()?)?;
            let input = if input == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                input
            };
            let mut args: Value = serde_json::from_str(&input)?;
            if !args.is_object() {
                return Err(error("INVALID_INPUT", "input must be a JSON object"));
            }
            if args.get("request_id").is_none() {
                args["request_id"] = Value::String(id());
            }
            let result = ipc::call(&Request {
                workspace: workspace.canonicalize()?.to_string_lossy().into_owned(),
                operation,
                args,
                token: ipc::operator_token()?,
            })?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}
