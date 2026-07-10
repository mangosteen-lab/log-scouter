use anyhow::Context;
use clap::Parser;
use log_scouter::core::project::Project;
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
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let Some(folder_arg) = args.folder else {
        let root = std::env::current_dir().context("logscout: could not read current folder")?;
        return log_scouter::tui::run(Project::new(root));
    };

    let folder = std::fs::canonicalize(&folder_arg)
        .with_context(|| format!("logscout: not a folder: {folder_arg}"))?;
    if !folder.is_dir() {
        anyhow::bail!("logscout: not a folder: {}", folder.display());
    }
    let mut project = Project::load(&folder);

    if args.files.is_empty() {
        project
            .add_text_files_from_dir(&folder)
            .with_context(|| format!("logscout: could not read folder: {}", folder.display()))?;
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

    log_scouter::tui::run(project)
}
