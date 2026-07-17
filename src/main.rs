use anyhow::Context;
use clap::{Parser, Subcommand};
use log_scouter::ai::{config::AiConfig, Provider};
use log_scouter::core::hub::{self, HubConfig};
use log_scouter::core::project::Project;
use std::path::PathBuf;

/// Where the LLM config lives, for messages.
const AI_CONFIG_PATH: &str = "~/.log-scouter/ai.json";

#[derive(Parser)]
#[command(
    name = "logscout",
    version,
    about = "A keyboard-driven Rust TUI for browsing large server logs.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Folder to open. When provided without explicit files, every direct text file in
    /// the folder is added as a log source. With no folder, Log Scouter starts empty.
    #[arg()]
    folder: Option<String>,
    #[arg()]
    files: Vec<String>,
    /// Open a specific log file directly, without a folder: `logscout -f app.log`. Repeat
    /// `-f` for several files. Works alongside a folder too.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    file: Vec<String>,
    /// Read the process's own stdin as a live log source, e.g.
    /// `kubectl logs -f ... | logscout -i`. Works alongside an optional folder.
    #[arg(short = 'i', long = "stdin")]
    stdin: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Configure the AI assistant's LLM provider and API key (saved to ~/.log-scouter/ai.json,
    /// so pressing `A` in a later session uses it without asking again).
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Manage hubs: shared libraries of schemas, filters and saved searches, published as
    /// GitHub repos. With no action, lists them.
    Hub {
        #[command(subcommand)]
        action: Option<HubCommand>,
    },
    /// Print the logscout version.
    Version,
}

#[derive(Subcommand)]
enum HubCommand {
    /// List the configured hubs: what each holds and when it last synced.
    List,
    /// Add a hub and sync it now: `logscout hub add acme/log-scouter-hub`. Accepts
    /// owner/repo, an HTTPS or SSH URL, or a /tree/<branch> URL to pin a branch.
    Add {
        /// The repo to track.
        repo: String,
        /// Local name for the hub, and the namespace its items appear under.
        /// Defaults to the repo's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget a hub and delete its cache. Your own schemas, filters and searches are
    /// untouched, as are any already imported into a project.
    Remove { name: String },
    /// Refresh every hub, or just the one named.
    Sync { name: Option<String> },
    /// Let a hub contribute schemas again.
    Enable { name: String },
    /// Keep a hub configured and cached, but contributing nothing.
    Disable { name: String },
    /// Refresh stale hubs on start (`on`), or only when asked (`off`).
    AutoSync {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show the configured provider, model, and whether an API key is set.
    List,
    /// Set the provider, API key, and/or model. Only the options you pass are changed.
    Set {
        /// LLM provider: openai, anthropic, or deepseek.
        #[arg(long)]
        provider: Option<String>,
        /// API key for the provider (stored in ~/.log-scouter/ai.json, readable only by you).
        #[arg(long)]
        api_key: Option<String>,
        /// Model name. Left unset, the provider's recommended model is used.
        #[arg(long)]
        model: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Version) => {
            println!("logscout {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Config { action }) => run_config(action.unwrap_or(ConfigAction::List)),
        Some(Command::Hub { action }) => run_hub(action.unwrap_or(HubCommand::List)),
        None => run_tui(cli.folder, cli.files, cli.file, cli.stdin),
    }
}

/// The `hub` subcommands. Everything here is the same core the TUI's Hubs prompt drives, so
/// a hub added from a script and one added from the app are the same hub.
fn run_hub(action: HubCommand) -> anyhow::Result<()> {
    let mut config = HubConfig::load().context("could not read ~/.log-scouter/hubs.json")?;
    // A script that only ever uses the CLI should still get the official hub.
    let seeded = config.ensure_official();
    if seeded {
        config.save().context("could not write hubs.json")?;
    }

    match action {
        HubCommand::List => print_hubs(&config, seeded),
        HubCommand::Add { repo, name } => {
            let (hub, report) =
                hub::add_and_sync(&mut config, &repo, name).map_err(anyhow::Error::msg)?;
            println!("Added hub '{}': {}", hub.name, report.describe());
            println!("  {}", hub::describe_hub(&hub));
        }
        HubCommand::Remove { name } => {
            if config.remove(&name).is_none() {
                anyhow::bail!("no hub '{name}'");
            }
            config.save().context("could not write hubs.json")?;
            hub::remove_hub_cache(&name).context("could not delete the hub's cache")?;
            println!("Removed hub '{name}'.");
        }
        HubCommand::Sync { name } => {
            let summary =
                hub::sync_named(&mut config, name.as_deref()).map_err(anyhow::Error::msg)?;
            println!(
                "Synced {} hub(s), {} item(s).",
                summary.synced, summary.items
            );
            for failure in &summary.failures {
                eprintln!("  failed: {failure}");
            }
            // A sync that reached nothing is a failure worth an exit code, so a CI step or a
            // `&&` chain notices instead of reporting success.
            if summary.synced == 0 && !summary.failures.is_empty() {
                anyhow::bail!("no hub synced");
            }
        }
        HubCommand::Enable { name } => set_hub_enabled(&mut config, &name, true)?,
        HubCommand::Disable { name } => set_hub_enabled(&mut config, &name, false)?,
        HubCommand::AutoSync { state } => {
            config.auto_sync = state == "on";
            config.save().context("could not write hubs.json")?;
            if config.auto_sync && hub::auto_sync_disabled_by_env() {
                println!(
                    "Auto-sync on, but {} is set in the environment and still wins.",
                    hub::NO_AUTO_SYNC_VAR
                );
            } else if config.auto_sync {
                println!("Auto-sync on: stale hubs refresh on start, at most daily.");
            } else {
                println!("Auto-sync off: hubs refresh only when you run `logscout hub sync`.");
            }
        }
    }
    Ok(())
}

fn set_hub_enabled(config: &mut HubConfig, name: &str, enabled: bool) -> anyhow::Result<()> {
    let hub = config
        .get_mut(name)
        .with_context(|| format!("no hub '{name}'"))?;
    hub.enabled = enabled;
    config.save().context("could not write hubs.json")?;
    let state = if enabled { "enabled" } else { "disabled" };
    println!("Hub '{name}' {state}.");
    Ok(())
}

/// The configured hubs, one per line.
fn print_hubs(config: &HubConfig, seeded: bool) {
    if seeded {
        println!(
            "Configured the official hub ({}).\n",
            hub::OFFICIAL_HUB_REPO
        );
    }
    if config.hubs.is_empty() {
        println!("No hubs configured. Add one with `logscout hub add <owner/repo>`.");
        return;
    }
    for hub in &config.hubs {
        println!("{}", hub::describe_hub(hub));
    }
    println!();
    match (config.auto_sync, hub::auto_sync_disabled_by_env()) {
        (_, true) => println!(
            "Auto-sync: off ({} is set in the environment).",
            hub::NO_AUTO_SYNC_VAR
        ),
        (true, false) => println!("Auto-sync: stale hubs refresh on start, at most daily."),
        (false, false) => println!("Auto-sync: off — hubs refresh only when you sync them."),
    }
    if let Some(path) = hub::hub_config_path() {
        println!("Configured in {}", path.display());
    }
}

fn run_config(action: ConfigAction) -> anyhow::Result<()> {
    let mut config = AiConfig::load();
    match action {
        ConfigAction::List => print_config(&config),
        ConfigAction::Set {
            provider,
            api_key,
            model,
        } => {
            if let Some(label) = provider {
                config.provider = Provider::from_label(&label).with_context(|| {
                    format!("unknown provider {label:?}; use openai, anthropic, or deepseek")
                })?;
            }
            if let Some(model) = model {
                config.model = model;
            }
            if let Some(key) = api_key {
                config.api_key = key.trim().to_string();
            }
            config
                .save()
                .with_context(|| format!("could not write {AI_CONFIG_PATH}"))?;
            let saved_to = log_scouter::ai::config::config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| AI_CONFIG_PATH.to_string());
            println!("Saved LLM config to {saved_to}\n");
            print_config(&config);
        }
    }
    Ok(())
}

/// Print the current provider, model, and API-key status (never the key itself).
fn print_config(config: &AiConfig) {
    println!("Provider: {}", config.provider.label());
    println!("Model:    {}", config.model());
    match key_status(config) {
        Some((source, masked)) => println!("API key:  {masked}  (from {source})"),
        None => println!(
            "API key:  not set — run `logscout config set --api-key <KEY>`, \
             or set the {} environment variable",
            config.provider.key_var()
        ),
    }
}

/// Where the effective key comes from and a masked form of it, matching the precedence the
/// assistant uses (environment variable first, then the stored key).
fn key_status(config: &AiConfig) -> Option<(String, String)> {
    if let Ok(key) = std::env::var(config.provider.key_var()) {
        let key = key.trim();
        if !key.is_empty() {
            return Some((format!("${}", config.provider.key_var()), mask(key)));
        }
    }
    let stored = config.api_key.trim();
    (!stored.is_empty()).then(|| (AI_CONFIG_PATH.to_string(), mask(stored)))
}

/// Show only the first and last four characters of a secret.
fn mask(key: &str) -> String {
    let count = key.chars().count();
    if count <= 8 {
        return "*".repeat(count.max(3));
    }
    let first: String = key.chars().take(4).collect();
    let last: String = key.chars().skip(count - 4).collect();
    format!("{first}…{last}")
}

fn run_tui(
    folder: Option<String>,
    files: Vec<String>,
    file_flags: Vec<String>,
    stdin: bool,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("logscout: could not read current folder")?;

    let Some(folder_arg) = folder else {
        // No folder: `logscout -f app.log` (and/or `-i`) opens just what was named, rooted at
        // the current directory. Auto-detection still picks a schema from the libraries.
        let mut project = Project::new(cwd.clone());
        add_files(&mut project, &cwd, &file_flags);
        if stdin {
            project.add_stdin_source();
        }
        return log_scouter::tui::run(project);
    };

    let folder = std::fs::canonicalize(&folder_arg)
        .with_context(|| format!("logscout: not a folder: {folder_arg}"))?;
    if !folder.is_dir() {
        anyhow::bail!("logscout: not a folder: {}", folder.display());
    }
    let mut project = Project::load(&folder);

    if files.is_empty() {
        project
            .add_text_files_from_dir(&folder)
            .with_context(|| format!("logscout: could not read folder: {}", folder.display()))?;
    } else {
        // Positional files are named relative to the folder being opened.
        add_files(&mut project, &folder, &files);
    }
    // `-f` files are explicit paths the user typed, so resolve them against the current dir.
    add_files(&mut project, &cwd, &file_flags);

    if stdin {
        project.add_stdin_source();
    }

    log_scouter::tui::run(project)
}

/// Add each path in `files` as a log source, resolving a relative path against `base` and
/// skipping (with a note) anything that is not a file.
fn add_files(project: &mut Project, base: &std::path::Path, files: &[String]) {
    for path in files {
        let path = PathBuf::from(path);
        let resolved = if path.is_absolute() {
            path
        } else {
            base.join(path)
        };
        if resolved.is_file() {
            project.add_file(&resolved, None);
        } else {
            eprintln!("logscout: skipping missing file: {}", resolved.display());
        }
    }
}
