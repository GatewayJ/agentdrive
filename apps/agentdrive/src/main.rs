mod integration;
use agentdrive_mcp::{Client, stdio_proxy};
use agentdrive_model::{BranchId, Error, OperationId, Result, RevisionId, WorkspaceId};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
#[command(version, about = "AgentDrive MCP client")]
struct Arguments {
    #[arg(
        long,
        env = "AGENTDRIVE_ENDPOINT",
        default_value = "http://127.0.0.1:7421/mcp"
    )]
    endpoint: String,
    #[arg(long, env = "AGENTDRIVE_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// List all tools and their JSON schemas.
    Tools,
    /// Call a tool. Use --request-id for mutations; --json - reads standard input.
    Call {
        tool: String,
        #[arg(long, default_value = "{}")]
        json: String,
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Forward MCP over standard input/output to the long-running daemon.
    Mcp,
    /// Run an ACP or Codex app-server process with automatic persistence.
    Integrate(integration::Options),
    /// 挂载工作区，并复用当前 location 的分支。
    Mount {
        #[arg(long)]
        workspace: WorkspaceId,
        path: PathBuf,
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Manage workspace branches.
    Branch {
        #[command(subcommand)]
        command: BranchCommand,
    },
    /// Print the dashboard address or open it in the system browser.
    Dashboard {
        #[arg(long)]
        open: bool,
    },
}
#[derive(Debug, Subcommand)]
enum BranchCommand {
    List {
        #[arg(long)]
        workspace: WorkspaceId,
    },
    Create {
        #[arg(long)]
        workspace: WorkspaceId,
        #[arg(long)]
        name: String,
        #[arg(long, conflicts_with = "source_branch")]
        source: Option<RevisionId>,
        #[arg(long)]
        source_branch: Option<BranchId>,
        #[arg(long)]
        request_id: Option<String>,
    },
    Switch {
        #[arg(long)]
        workspace: WorkspaceId,
        #[arg(long)]
        branch: BranchId,
        #[arg(long)]
        request_id: Option<String>,
    },
}
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let code = match run(Arguments::parse()).await {
        Ok(true) => 0,
        Ok(false) => 2,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    // stdin 的阻塞读取可能仍在等待输入；run 已完成持久化和连接清理。
    std::process::exit(code);
}
fn read_token(path: Option<&Path>) -> Result<String> {
    match path {
        Some(path) => Ok(std::fs::read_to_string(path)?.trim().to_owned()),
        None => std::env::var("AGENTDRIVE_TOKEN").map_err(|_| {
            Error::invalid("set AGENTDRIVE_TOKEN_FILE, --token-file, or AGENTDRIVE_TOKEN")
        }),
    }
}
async fn run(arguments: Arguments) -> Result<bool> {
    let command = match arguments.command {
        Command::Dashboard { open } => {
            let mut url = integration::base_url(&arguments.endpoint)?;
            println!("{url}");
            if open {
                let token = read_token(arguments.token_file.as_deref())?;
                let mut fragment = reqwest::Url::parse("http://localhost/")
                    .map_err(|error| Error::invalid(error.to_string()))?;
                fragment.query_pairs_mut().append_pair("token", &token);
                url.set_fragment(fragment.query());
                #[cfg(target_os = "linux")]
                let mut opener = std::process::Command::new("xdg-open");
                #[cfg(target_os = "macos")]
                let mut opener = std::process::Command::new("open");
                #[cfg(windows)]
                let mut opener = {
                    let mut command = std::process::Command::new("rundll32");
                    command.arg("url.dll,FileProtocolHandler");
                    command
                };
                if !opener.arg(url.as_str()).status()?.success() {
                    return Err(Error::invalid("browser could not be opened"));
                }
            }
            return Ok(true);
        }
        command => command,
    };
    let token = read_token(arguments.token_file.as_deref())?;
    if let Command::Integrate(options) = command {
        return integration::run(&arguments.endpoint, token, options).await;
    }
    let client = Client::connect(&arguments.endpoint, token).await?;
    let result = async {
        match command {
            Command::Tools => println!("{}", serde_json::to_string_pretty(&client.tools().await?)?),
            Command::Call {
                tool,
                json: input,
                request_id,
            } => {
                let input = if input == "-" {
                    let mut input = String::new();
                    std::io::stdin()
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_string(&mut input)?;
                    input
                } else {
                    input
                };
                if input.len() > 8 * 1024 * 1024 {
                    return Err(Error::invalid("tool arguments exceed 8 MiB"));
                }
                let mut arguments: Value = serde_json::from_str(&input)
                    .map_err(|error| Error::invalid(error.to_string()))?;
                if let Some(request_id) = request_id {
                    arguments = json!({"request_id": request_id, "request": arguments});
                }
                let result = client.call(tool, arguments).await?;
                let success = result.is_error != Some(true);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result.structured_content.unwrap_or_else(
                        || json!({"content": result.content, "is_error": result.is_error})
                    ))?
                );
                return Ok(success);
            }
            Command::Mcp => stdio_proxy(client.clone()).await?,
            Command::Mount { workspace, path, request_id } => {
                let path = if path.is_absolute() { path } else { std::env::current_dir()?.join(path) };
                let result = client.call("mount_prepare".into(), json!({
                    "request_id": request_id.unwrap_or_else(|| OperationId::new().to_string()),
                    "request": {"workspace": workspace, "path": path},
                })).await?;
                let success = result.is_error != Some(true);
                println!("{}", serde_json::to_string_pretty(&result.structured_content)?);
                return Ok(success);
            }
            Command::Branch { command } => {
                let (tool, input) = match command {
                    BranchCommand::List { workspace } => ("workspace_status", json!({"workspace": workspace})),
                    BranchCommand::Create { workspace, name, source, source_branch, request_id } => ("branch_create", json!({"request_id": request_id.unwrap_or_else(|| OperationId::new().to_string()), "request": {"workspace":workspace,"name":name,"source":source,"source_branch":source_branch}})),
                    BranchCommand::Switch { workspace, branch, request_id } => ("branch_switch", json!({"request_id": request_id.unwrap_or_else(|| OperationId::new().to_string()), "request": {"workspace":workspace,"branch":branch}})),
                };
                let mut result = client.call(tool.into(), input).await?;
                if tool == "branch_switch"
                    && let Some(record) = &result.structured_content
                    && record["phase"] == "waiting_for_quiesce"
                {
                    let operation = record["id"].clone();
                    let generation = record["pending"]["old"]["generation"].clone();
                    let released = client.call("mount_release".into(), json!({
                        "request_id": OperationId::new().to_string(),
                        "request": {"operation":operation,"expected_binding_generation":generation},
                    })).await?;
                    let current = client.call("operation_get".into(), json!({"operation":operation})).await?;
                    result = if released.is_error == Some(true) && current.is_error != Some(true) { released } else { current };
                }
                let success = result.is_error != Some(true);
                println!("{}", serde_json::to_string_pretty(&result.structured_content)?);
                return Ok(success);
            }
            Command::Integrate(_) | Command::Dashboard { .. } => unreachable!(),
        }
        Ok(true)
    }
    .await;
    if let Err(error) = client.close().await {
        tracing::warn!(%error, "MCP connection cleanup failed");
    }
    result
}
