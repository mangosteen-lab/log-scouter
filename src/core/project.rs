use crate::core::extractor::{
    builtin_extractors, default_extractor, detect, extractor_from_project, Extractor,
    BRACKETED_DEFAULT_FORMAT, BRACKETED_LEGACY_FORMAT, DEFAULT_EXTRACTOR_NAME, DETECT_LINES,
};
use crate::core::filters::FilterSet;
use crate::core::models::{display_name, merge_files, LogFileModel};
use crate::core::parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const CONFIG_DIR: &str = ".logscouter";
pub const CONFIG_FILE: &str = "project.json";

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub files: Vec<LogFileModel>,
    pub extractors: HashMap<String, Extractor>,
    pub saved_searches: Vec<String>,
    pub filters: FilterSet,
    /// What was on screen when the folder was last closed. Filters live in `filters`
    /// because they are project-wide; this is the per-pane part.
    pub session: Option<Session>,
    pub settings: Value,
    file_counter: usize,
}

/// The panes as they were at quit, so reopening a folder resumes where it left off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub panes: Vec<PaneSession>,
    #[serde(default)]
    pub focused_pane: usize,
    /// "horizontal" or "vertical"; anything else falls back to horizontal.
    #[serde(default)]
    pub split_mode: String,
    // ---- Workspace layout. Stored as `hide_*` so an older session (no field) shows them. ----
    /// Sidebar width override in columns; `None` uses the automatic width.
    #[serde(default)]
    pub sidebar_width: Option<u16>,
    /// Height overrides in rows for the stacked panels.
    #[serde(default)]
    pub results_height: Option<u16>,
    #[serde(default)]
    pub detail_height: Option<u16>,
    #[serde(default)]
    pub chat_height: Option<u16>,
    /// Per-pane size weights; empty or the wrong length means equal panes.
    #[serde(default)]
    pub pane_weights: Vec<u16>,
    #[serde(default)]
    pub hide_sidebar: bool,
    #[serde(default)]
    pub hide_detail: bool,
    #[serde(default)]
    pub hide_chat: bool,
    #[serde(default)]
    pub hide_results: bool,
    #[serde(default)]
    pub focus_mode: bool,
    #[serde(default)]
    pub timeline_field: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSession {
    /// The files feeding this pane. More than one means a timestamp merge, which cannot
    /// be rebuilt until those files have loaded.
    #[serde(default)]
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub context: usize,
}

impl Session {
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectData {
    #[serde(default = "project_version")]
    version: u32,
    #[serde(default)]
    file_counter: usize,
    #[serde(default)]
    files: Vec<FileData>,
    #[serde(default)]
    extractors: Vec<Extractor>,
    #[serde(default)]
    saved_searches: Vec<String>,
    #[serde(default)]
    filters: FilterSet,
    #[serde(default)]
    session: Option<Session>,
    #[serde(default = "default_settings")]
    settings: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileData {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    extractor_name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
}

impl Project {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let mut project = Self {
            root: absolute_path(root.into()),
            files: Vec::new(),
            extractors: HashMap::new(),
            saved_searches: Vec::new(),
            filters: FilterSet::default(),
            session: None,
            settings: default_settings(),
            file_counter: 0,
        };
        project.ensure_builtin_extractors();
        project
    }

    pub fn load(root: impl Into<PathBuf>) -> Self {
        let mut project = Self::new(root);
        let path = project.config_path();
        if path.exists() {
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<ProjectData>(&raw) {
                    project.apply(data);
                }
            }
        }
        // A project saved before `Generic line` existed does not list it, and `detect`
        // can only choose among the schemas it is handed.
        project.ensure_builtin_extractors();
        project
    }

    /// Add any built-in schema the project does not already define. A schema the user has
    /// edited under a built-in name wins: it is theirs, and repointing files at a fresh
    /// copy would silently change how those files parse.
    fn ensure_builtin_extractors(&mut self) {
        for extractor in builtin_extractors() {
            self.extractors
                .entry(extractor.name.clone())
                .or_insert(extractor);
        }
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join(CONFIG_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir().join(CONFIG_FILE)
    }

    pub fn add_extractor(&mut self, mut extractor: Extractor) -> Result<(), String> {
        extractor.compile()?;
        self.extractors.insert(extractor.name.clone(), extractor);
        Ok(())
    }

    pub fn get_extractor(&mut self, name: &str) -> Extractor {
        if let Some(extractor) = self.extractors.get(name) {
            return extractor.clone();
        }
        self.default_extractor_obj()
    }

    /// The schema to fall back on when nothing is known about a file.
    ///
    /// `extractors` is a `HashMap`, so `values().next()` is whatever the hasher felt like
    /// that run -- with more than one schema in the project it picked a different default
    /// on every launch. Prefer the built-in, then the first name alphabetically.
    pub fn default_extractor_obj(&mut self) -> Extractor {
        if let Some(extractor) = self.extractors.get(DEFAULT_EXTRACTOR_NAME) {
            return extractor.clone();
        }
        let first_by_name = self
            .extractors
            .keys()
            .min()
            .cloned()
            .and_then(|name| self.extractors.get(&name).cloned());
        if let Some(extractor) = first_by_name {
            return extractor;
        }

        let extractor = default_extractor();
        self.extractors
            .insert(extractor.name.clone(), extractor.clone());
        extractor
    }

    /// The schema whose format best explains the start of `path`, or the fallback.
    /// An unreadable file yields the fallback rather than an error: the load will report
    /// the read failure on its own.
    pub fn detect_extractor_for(&mut self, path: &Path) -> Extractor {
        let Ok(lines) = parser::read_first_lines(path, DETECT_LINES) else {
            return self.default_extractor_obj();
        };
        match detect(self.extractors.values(), &lines) {
            Some(extractor) => extractor.clone(),
            None => self.default_extractor_obj(),
        }
    }

    pub fn add_file(
        &mut self,
        path: impl AsRef<Path>,
        extractor_name: Option<String>,
    ) -> &mut LogFileModel {
        let absolute = self.resolve_path(path.as_ref());
        if let Some(index) = self
            .files
            .iter()
            .position(|file| absolute_path(file.path.clone()) == absolute)
        {
            return &mut self.files[index];
        }

        self.file_counter += 1;
        // No schema asked for: work it out from the file rather than from hash order.
        let extractor_name =
            extractor_name.unwrap_or_else(|| self.detect_extractor_for(&absolute).name);
        let extractor = Some(self.get_extractor(&extractor_name));
        let model = LogFileModel::new(
            format!("f{}", self.file_counter),
            absolute.clone(),
            extractor_name,
            display_name(&absolute),
            extractor,
        );
        self.files.push(model);
        self.files.last_mut().expect("just pushed file")
    }

    pub fn remove_file(&mut self, file_id: &str) {
        self.files.retain(|file| file.file_id != file_id);
        // A merged view over a removed file no longer reflects the project.
        self.files
            .retain(|file| !file.merged_from.iter().any(|id| id == file_id));
    }

    /// Add every regular text file directly inside `folder`, sorted by path.
    /// Text detection is intentionally conservative: a file is text when the first
    /// chunk contains no NUL byte. That accepts extensionless logs while skipping
    /// obvious binaries.
    pub fn add_text_files_from_dir(&mut self, folder: impl AsRef<Path>) -> io::Result<usize> {
        let paths = text_files_in_dir(folder.as_ref())?;
        let before = self.files.iter().filter(|file| !file.is_merged()).count();
        for path in paths {
            self.add_file(path, None);
        }
        let after = self.files.iter().filter(|file| !file.is_merged()).count();
        Ok(after.saturating_sub(before))
    }

    /// Build a timestamp-ordered merge of `file_ids` and add it as a virtual file.
    /// Returns its id, or an error naming what is not ready yet.
    pub fn add_merged(&mut self, file_ids: &[String]) -> Result<String, String> {
        if file_ids.len() < 2 {
            return Err("select at least two files to merge".to_string());
        }

        let mut sources = Vec::with_capacity(file_ids.len());
        for file_id in file_ids {
            let Some(file) = self.get_file(file_id) else {
                return Err(format!("unknown file {file_id}"));
            };
            if file.is_merged() {
                return Err("cannot merge a merged view".to_string());
            }
            if !file.loaded {
                return Err(format!("{} is still loading", file.display_name));
            }
            sources.push(file);
        }

        // Reuse an existing merge of exactly these files rather than stacking copies.
        if let Some(existing) = self
            .files
            .iter()
            .find(|file| file.merged_from == *file_ids)
            .map(|file| file.file_id.clone())
        {
            return Ok(existing);
        }

        // Build while `sources` still borrows `self.files`, then commit.
        let next = self.file_counter + 1;
        let merged = merge_files(format!("m{next}"), &sources);
        drop(sources);

        let file_id = merged.file_id.clone();
        self.file_counter = next;
        self.files.push(merged);
        Ok(file_id)
    }

    /// Point a file at a different schema. The caller must re-read the file: multi-line
    /// grouping depends on the extractor, so existing entries are wrong.
    pub fn set_file_extractor(
        &mut self,
        file_id: &str,
        extractor_name: &str,
    ) -> Result<(), String> {
        let extractor = self.get_extractor(extractor_name);
        let Some(file) = self.get_file_mut(file_id) else {
            return Err(format!("unknown file {file_id}"));
        };
        if file.is_merged() {
            return Err("a merged view takes each file's own schema".to_string());
        }
        file.extractor_name = extractor_name.to_string();
        file.refresh_extractor(Some(extractor));
        file.loaded = false;
        file.entries.clear();

        // Merged views captured the old schema and the old entries; drop them.
        let file_id = file_id.to_string();
        self.files
            .retain(|file| !file.merged_from.contains(&file_id));
        Ok(())
    }

    pub fn get_file(&self, file_id: &str) -> Option<&LogFileModel> {
        self.files.iter().find(|file| file.file_id == file_id)
    }

    pub fn get_file_mut(&mut self, file_id: &str) -> Option<&mut LogFileModel> {
        self.files.iter_mut().find(|file| file.file_id == file_id)
    }

    pub fn file_index(&self, file_id: &str) -> Option<usize> {
        self.files.iter().position(|file| file.file_id == file_id)
    }

    pub fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.config_dir())?;
        let tmp = self.config_path().with_extension("json.tmp");
        let data = self.to_data();
        fs::write(&tmp, serde_json::to_string_pretty(&data).unwrap())?;
        fs::rename(tmp, self.config_path())
    }

    /// Re-detect any not-yet-loaded file whose schema explains none of its opening lines.
    ///
    /// A schema that matches nothing is never a deliberate choice, and it is not merely
    /// unhelpful: under it no line starts a record, so every line folds into the one above
    /// and the file loads as a single timestamp-less entry. Projects written before
    /// `Generic line` existed pinned the bracketed schema on exactly the files it could
    /// not read, so heal them on the way in rather than making the user notice.
    pub fn redetect_mismatched_schemas(&mut self) {
        let candidates: Vec<(usize, PathBuf)> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| !file.is_merged() && !file.loaded)
            .map(|(index, file)| (index, file.path.clone()))
            .collect();

        for (index, path) in candidates {
            let Ok(lines) = parser::read_first_lines(&path, DETECT_LINES) else {
                continue;
            };
            if lines.iter().all(|line| line.trim().is_empty()) {
                continue;
            }
            let explains = self.files[index]
                .extractor
                .as_ref()
                .map(|extractor| extractor.match_score(&lines) > 0)
                .unwrap_or(false);
            if explains {
                continue;
            }

            let Some(better) = detect(self.extractors.values(), &lines).cloned() else {
                continue;
            };
            if better.name == self.files[index].extractor_name {
                continue;
            }
            self.files[index].extractor_name = better.name.clone();
            self.files[index].refresh_extractor(Some(better));
        }
    }

    pub fn load_all_files(&mut self) {
        self.redetect_mismatched_schemas();
        let extractor_names: Vec<String> = self
            .files
            .iter()
            .map(|f| f.extractor_name.clone())
            .collect();
        let extractors: Vec<Extractor> = extractor_names
            .iter()
            .map(|name| self.get_extractor(name))
            .collect();
        for (file, extractor) in self.files.iter_mut().zip(extractors) {
            if file.is_merged() {
                continue;
            }
            file.refresh_extractor(Some(extractor));
            if !file.loaded && file.error.is_empty() {
                if let Err(exc) = file.load() {
                    file.error = format!("read error: {exc}");
                }
            }
        }
    }

    fn apply(&mut self, data: ProjectData) {
        self.file_counter = data.file_counter;
        self.saved_searches = data.saved_searches;
        self.filters = data.filters;
        // A project written before the time slot was enforced can hold several ranges;
        // OR'd together they widen the window. Keep only the last.
        self.filters.dedupe_time_range();
        self.session = data.session;
        self.settings = data.settings;

        for mut extractor in data.extractors {
            // Projects written before `<error_code?>` stored the old format verbatim, and
            // under it every `[<hex code>]` error line mis-parses its level. Heal them.
            if extractor.format == BRACKETED_LEGACY_FORMAT {
                extractor.format = BRACKETED_DEFAULT_FORMAT.to_string();
            }
            if let Some(extractor) = extractor_from_project(extractor) {
                self.extractors.insert(extractor.name.clone(), extractor);
            }
        }

        for file_data in data.files {
            let path = if Path::new(&file_data.path).is_absolute() {
                PathBuf::from(&file_data.path)
            } else {
                self.root.join(&file_data.path)
            };
            let extractor_name = file_data.extractor_name;
            let extractor = Some(self.get_extractor(&extractor_name));
            let mut model = LogFileModel::new(
                file_data.file_id,
                absolute_path(path),
                extractor_name,
                file_data.display_name,
                extractor,
            );
            model.label = file_data.label;
            model.description = file_data.description;
            self.files.push(model);
        }
    }

    fn to_data(&self) -> ProjectData {
        ProjectData {
            version: project_version(),
            file_counter: self.file_counter,
            // Merged views are derived, not files on disk; they are rebuilt on demand.
            files: self
                .files
                .iter()
                .filter(|file| !file.is_merged())
                .map(|file| FileData {
                    file_id: file.file_id.clone(),
                    path: self.rel(&file.path),
                    extractor_name: file.extractor_name.clone(),
                    display_name: file.display_name.clone(),
                    label: file.label.clone(),
                    description: file.description.clone(),
                })
                .collect(),
            extractors: self.extractors.values().cloned().collect(),
            saved_searches: self.saved_searches.clone(),
            filters: self.filters.clone(),
            session: self.session.clone(),
            settings: self.settings.clone(),
        }
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            absolute_path(path)
        } else {
            absolute_path(self.root.join(path))
        }
    }
}

fn absolute_path(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path.as_ref()).unwrap_or_else(|_| {
        if path.as_ref().is_absolute() {
            path.as_ref().to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

pub fn text_files_in_dir(folder: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && is_text_file(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn is_text_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buffer = [0_u8; 8192];
    match file.read(&mut buffer) {
        Ok(n) => !buffer[..n].contains(&0),
        Err(_) => false,
    }
}

fn project_version() -> u32 {
    1
}

fn default_settings() -> Value {
    serde_json::json!({ "context_default": 3 })
}
