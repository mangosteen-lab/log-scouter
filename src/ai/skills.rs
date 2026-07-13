//! User-authored skills: markdown files in `~/.log-scouter/skills/`.
//!
//! A skill is a plain `.md` file the user writes with an editor -- a reusable set of
//! instructions for the assistant ("how we triage OOM incidents", say). `/skill <name>`
//! in the chat activates one for the session; its text is appended to the system prompt,
//! re-read each turn so edits take effect without a restart.

use crate::core::filters::{home_dir, USER_DIR};
use std::fs;
use std::path::{Path, PathBuf};

/// One skill file: its name (the file stem), a one-line description, and the full body
/// handed to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// `~/.log-scouter/skills/`, or `None` when `$HOME` is unset.
pub fn skills_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join("skills"))
}

/// Build a skill from its name and markdown body. The description is the first Markdown
/// heading, or the first non-empty line, with any leading `#`s stripped.
pub fn parse_skill(name: &str, body: &str) -> Skill {
    let description = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .unwrap_or_default();
    Skill {
        name: name.to_string(),
        description,
        body: body.to_string(),
    }
}

/// Every `.md` skill in `dir`, sorted by name. A missing or unreadable directory is not an
/// error -- it just means the user has not written any skills yet.
pub fn list_in(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();

    let mut skills = Vec::new();
    for path in paths {
        if let (Some(stem), Ok(body)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            fs::read_to_string(&path),
        ) {
            skills.push(parse_skill(stem, &body));
        }
    }
    skills
}

/// Every skill in the user's skills directory.
pub fn list() -> Vec<Skill> {
    skills_dir().map(|dir| list_in(&dir)).unwrap_or_default()
}

/// The skill with this name, if it exists.
pub fn load(name: &str) -> Option<Skill> {
    list().into_iter().find(|skill| skill.name == name)
}
