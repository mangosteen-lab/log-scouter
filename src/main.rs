use anyhow::Context;
use clap::{Parser, Subcommand};
use log_scouter::ai::{config::AiConfig, Provider};
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
    /// Print the logscout version.
    Version,
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
        None => run_tui(cli.folder, cli.files),
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

fn run_tui(folder: Option<String>, files: Vec<String>) -> anyhow::Result<()> {
    let Some(folder_arg) = folder else {
        let root = std::env::current_dir().context("logscout: could not read current folder")?;
        return log_scouter::tui::run(Project::new(root));
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
        for path in &files {
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

    log_scouter::tui::run(project)
}
