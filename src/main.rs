use anyhow::Context;
use clap::Parser;
use log_scouter::core::project::Project;

#[derive(Debug, Parser)]
#[command(
    name = "scout",
    about = "A keyboard-driven Rust TUI for browsing large server logs."
)]
struct Args {
    #[arg(default_value = ".")]
    folder: String,
    #[arg()]
    files: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let folder = std::fs::canonicalize(&args.folder)
        .with_context(|| format!("scout: not a folder: {}", args.folder))?;
    if !folder.is_dir() {
        anyhow::bail!("scout: not a folder: {}", folder.display());
    }

    let mut project = Project::load(&folder);
    for path in &args.files {
        let path = std::path::PathBuf::from(path);
        if path.is_file() {
            project.add_file(&path, None);
        } else {
            eprintln!("scout: skipping missing file: {}", path.display());
        }
    }

    log_scouter::tui::run(project)
}
