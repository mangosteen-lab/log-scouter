use anyhow::Context;
use clap::Parser;
use log_scouter::core::project::Project;
use log_scouter::mcp::{random_token, McpServer};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "logscout",
    about = "A keyboard-driven Rust TUI for browsing large server logs."
)]
struct Args {
    /// Folder to open. When provided without explicit files, every direct text file in
    /// the folder is added as a log source. With no folder, Log Scouter starts empty.
    #[arg()]
    folder: Option<String>,
    #[arg()]
    files: Vec<String>,
    /// Serve an MCP endpoint so an external agent (Claude Code, Codex, ...) can drive the
    /// app in real time from another terminal. Prints the URL and a bearer token on start.
    #[arg(long)]
    mcp: bool,
    /// Port for the MCP endpoint. Defaults to an OS-assigned free port.
    #[arg(long)]
    mcp_port: Option<u16>,
    /// Do not require a bearer token on the MCP endpoint. It still binds to localhost only.
    #[arg(long)]
    mcp_no_auth: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let project = match &args.folder {
        None => {
            let root =
                std::env::current_dir().context("logscout: could not read current folder")?;
            Project::new(root)
        }
        Some(folder_arg) => {
            let folder = std::fs::canonicalize(folder_arg)
                .with_context(|| format!("logscout: not a folder: {folder_arg}"))?;
            if !folder.is_dir() {
                anyhow::bail!("logscout: not a folder: {}", folder.display());
            }
            let mut project = Project::load(&folder);
            if args.files.is_empty() {
                project.add_text_files_from_dir(&folder).with_context(|| {
                    format!("logscout: could not read folder: {}", folder.display())
                })?;
            } else {
                for path in &args.files {
                    let path = PathBuf::from(path);
                    let resolved = if path.is_absolute() {
                        path
                    } else {
                        folder.join(path)
                    };
                    if resolved.is_file() {
                        project.add_file(&resolved, None);
                    } else {
                        eprintln!("logscout: skipping missing file: {}", resolved.display());
                    }
                }
            }
            project
        }
    };

    if !args.mcp {
        return log_scouter::tui::run(project);
    }

    let token = (!args.mcp_no_auth).then(random_token);
    let server = McpServer::start(args.mcp_port.unwrap_or(0), token)
        .context("logscout: could not start the MCP server")?;
    announce_mcp(&project, &server);
    log_scouter::tui::run_with_mcp(project, Some(server))
}

/// Print how to connect an agent, and drop the same details in the project folder so they
/// can be read from the other terminal while the TUI owns this one.
fn announce_mcp(project: &Project, server: &McpServer) {
    let info = mcp_connection_info(server);
    eprintln!("\n{info}\n");

    let dir = project.config_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        let path = dir.join("mcp.txt");
        if std::fs::write(&path, format!("{info}\nWritten to: {}\n", path.display())).is_ok() {
            restrict(&path);
            eprintln!("(also written to {})\n", path.display());
        }
    }
}

fn mcp_connection_info(server: &McpServer) -> String {
    let url = server.url();
    let mut out = format!("log-scouter MCP server is live.\n\n  URL: {url}\n");
    match server.token() {
        Some(token) => {
            out.push_str(&format!("  Auth header: Authorization: Bearer {token}\n\n"));
            out.push_str("Connect Claude Code (in another terminal):\n");
            out.push_str(&format!(
                "  claude mcp add --transport http log-scouter \"{url}\" \\\n    --header \"Authorization: Bearer {token}\"\n\n"
            ));
            out.push_str("Connect a stdio-only client (Codex, ...) via the mcp-remote bridge:\n");
            out.push_str(&format!(
                "  npx -y mcp-remote {url} --header \"Authorization: Bearer {token}\"\n"
            ));
        }
        None => {
            out.push_str("  (no auth token — localhost only)\n\n");
            out.push_str("Connect Claude Code (in another terminal):\n");
            out.push_str(&format!(
                "  claude mcp add --transport http log-scouter \"{url}\"\n\n"
            ));
            out.push_str("Connect a stdio-only client (Codex, ...) via the mcp-remote bridge:\n");
            out.push_str(&format!("  npx -y mcp-remote {url}\n"));
        }
    }
    out
}

/// Best-effort `chmod 600`: the file names a token.
#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}
