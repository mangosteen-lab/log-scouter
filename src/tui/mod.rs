use crate::ai::config::AiConfig;
use crate::ai::message::{ChatMsg, ToolResult};
use crate::ai::{tools as ai_tools, AgentRequest, AiWorker};
use crate::core::extractor::{
    export_schemas_to_folder, load_schemas_from_folder, user_schema_dir, Extractor,
    BRACKETED_DEFAULT_FORMAT, DEFAULT_TIMESTAMP_FORMAT, GENERIC_EXTRACTOR_NAME,
    USER_SCHEMAS_SUBDIR,
};
use crate::core::filters::{
    common_message_pattern, expand_tilde, export_filters_to_folder, hide_like, home_dir,
    json_file_paths, load_filters_from_folder, message_template, pattern_candidates,
    sanitize_file_component, user_filter_dir, FilterRule, FilterSet, PatternOption, USER_DIR,
    USER_FILTERS_SUBDIR,
};
use crate::core::models::{
    apply_context, merge_files, LiveSourceConfig, LiveSourceKind, LogEntry, LogFileModel,
    ViewModel, VisibleIndices,
};
use crate::core::parser::{self, EntryBuilder};
use crate::core::project::{Bookmark, PaneSession, Project, Session, CONFIG_DIR};
use crate::core::search::{
    compile_query, export_searches_to_folder, install_default_search_library,
    load_searches_from_folder, parse_datetime, user_search_dir, Predicate, Query,
    USER_SEARCHES_SUBDIR,
};
use chrono::{Duration as ChronoDuration, NaiveDateTime};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, Read, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const CONTEXT_CYCLE: &[usize] = &[0, 3, 10];
const DETAIL_LABEL_WIDTH: usize = 14;
const USER_BOOKMARKS_SUBDIR: &str = "bookmarks";
const SCHEMA_INFER_SAMPLE_LINES: usize = 100;
const SOURCE_POLL_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BookmarkFile {
    name: String,
    #[serde(default)]
    description: String,
    file_path: String,
    line_no: usize,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Pane,
    Results,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ThemeName {
    Classic,
    Light,
    Amber,
    Mono,
}

impl Default for ThemeName {
    fn default() -> Self {
        Self::Classic
    }
}

impl ThemeName {
    const ALL: [Self; 4] = [Self::Classic, Self::Light, Self::Amber, Self::Mono];

    fn label(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Light => "light",
            Self::Amber => "amber",
            Self::Mono => "mono",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Classic => "blue chrome with bright semantic colors",
            Self::Light => "light chrome for bright terminal backgrounds",
            Self::Amber => "warm high-contrast incident-room palette",
            Self::Mono => "minimal monochrome with restrained accents",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UiConfig {
    #[serde(default)]
    theme: ThemeName,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeName::Classic,
        }
    }
}

impl UiConfig {
    fn load() -> Self {
        ui_config_path()
            .and_then(|path| Self::load_from(&path).ok())
            .unwrap_or_default()
    }

    fn load_from(path: &Path) -> std::io::Result<Self> {
        let body = fs::read_to_string(path)?;
        let config = serde_json::from_str(&body).map_err(std::io::Error::other)?;
        Ok(config)
    }

    fn save(&self) -> std::io::Result<()> {
        let path = ui_config_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not find your home directory (set HOME or USERPROFILE)",
            )
        })?;
        self.save_to(&path)
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, body)
    }
}

fn ui_config_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join("ui.json"))
}

#[derive(Debug, Clone, Copy)]
struct Theme {
    name: ThemeName,
}

impl Theme {
    fn new(name: ThemeName) -> Self {
        Self { name }
    }

    fn header(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default().bg(Color::Blue).fg(Color::White),
            ThemeName::Light => Style::default().bg(Color::White).fg(Color::Black),
            ThemeName::Amber => Style::default().bg(Color::Yellow).fg(Color::Black),
            ThemeName::Mono => Style::default().bg(Color::DarkGray).fg(Color::White),
        }
    }

    fn status(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default().bg(Color::DarkGray).fg(Color::White),
            ThemeName::Light => Style::default().bg(Color::Gray).fg(Color::Black),
            ThemeName::Amber => Style::default().bg(Color::Black).fg(Color::Yellow),
            ThemeName::Mono => Style::default().bg(Color::Black).fg(Color::White),
        }
    }

    fn accent(self) -> Color {
        match self.name {
            ThemeName::Classic => Color::Cyan,
            ThemeName::Light => Color::Blue,
            ThemeName::Amber => Color::Yellow,
            ThemeName::Mono => Color::White,
        }
    }

    fn dim(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Mono => Style::default().fg(Color::DarkGray),
            ThemeName::Light => Style::default().fg(Color::Gray),
            ThemeName::Amber => Style::default().fg(Color::Yellow),
        }
    }

    fn selected(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default().bg(Color::Gray).fg(Color::Black),
            ThemeName::Light => Style::default().bg(Color::Blue).fg(Color::White),
            ThemeName::Amber => Style::default().bg(Color::Yellow).fg(Color::Black),
            ThemeName::Mono => Style::default().bg(Color::White).fg(Color::Black),
        }
    }

    fn selected_strong(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default().bg(Color::LightBlue).fg(Color::Black),
            ThemeName::Light => Style::default().bg(Color::Blue).fg(Color::White),
            ThemeName::Amber => Style::default().bg(Color::Yellow).fg(Color::Black),
            ThemeName::Mono => Style::default().bg(Color::White).fg(Color::Black),
        }
    }

    fn section(self) -> Style {
        Style::default()
            .fg(self.accent())
            .add_modifier(Modifier::BOLD)
    }

    fn subsection(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            ThemeName::Light => Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            ThemeName::Amber => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            ThemeName::Mono => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn filter(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default().fg(Color::Yellow),
            ThemeName::Amber => Style::default().fg(Color::Yellow),
            ThemeName::Mono => Style::default().fg(Color::White),
        }
    }

    fn time_filter(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default().fg(Color::Magenta),
            ThemeName::Amber => Style::default().fg(Color::Yellow),
            ThemeName::Mono => Style::default().fg(Color::White),
        }
    }

    fn saved_search(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default().fg(Color::Green),
            ThemeName::Amber => Style::default().fg(Color::Yellow),
            ThemeName::Mono => Style::default().fg(Color::White),
        }
    }

    fn bookmark(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default().fg(Color::Magenta),
            ThemeName::Amber => Style::default().fg(Color::Yellow),
            ThemeName::Mono => Style::default().fg(Color::White),
        }
    }

    fn matched(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default().fg(Color::Yellow),
            ThemeName::Amber => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            ThemeName::Mono => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn search_hit(self) -> Style {
        match self.name {
            ThemeName::Classic | ThemeName::Light => Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            ThemeName::Amber => Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            ThemeName::Mono => Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn inline_input(self) -> Style {
        match self.name {
            ThemeName::Classic => Style::default().bg(Color::DarkGray).fg(Color::White),
            ThemeName::Light => Style::default().bg(Color::Gray).fg(Color::Black),
            ThemeName::Amber => Style::default().bg(Color::Black).fg(Color::Yellow),
            ThemeName::Mono => Style::default().bg(Color::Black).fg(Color::White),
        }
    }
}

#[derive(Debug, Clone)]
struct PaneState {
    view: ViewModel,
}

/// A restorable slice of state for undo/redo: everything a user or the AI mutates through the
/// app's operations. The `session` descriptor already carries the pane composition (including
/// merges), per-pane search, and the workspace layout.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    filters: crate::core::filters::FilterSet,
    saved_searches: Vec<String>,
    bookmarks: Vec<Bookmark>,
    session: crate::core::project::Session,
}

/// Who performed an action, for the history popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Actor {
    User,
    Ai,
}

impl Actor {
    fn label(self) -> &'static str {
        match self {
            Actor::User => "User",
            Actor::Ai => "AI",
        }
    }
}

/// One line in the action-history popup.
#[derive(Debug, Clone)]
struct ActionEntry {
    time: String,
    actor: Actor,
    description: String,
}

/// Cap on the undo stack and the action log, to bound memory.
const HISTORY_LIMIT: usize = 100;

/// The adjustable workspace layout: panel visibility, sidebar width, per-pane size weights,
/// and focus mode. Saved with the session so a folder reopens the way you left it.
#[derive(Debug, Clone)]
struct Workspace {
    /// Sidebar width override in columns; `None` uses the automatic width.
    sidebar_width: Option<u16>,
    /// Height overrides in rows for the stacked panels; `None` uses the automatic height.
    results_height: Option<u16>,
    detail_height: Option<u16>,
    chat_height: Option<u16>,
    /// Per-pane size weights; a wrong length is reset to equal weights on demand.
    pane_weights: Vec<u16>,
    show_sidebar: bool,
    show_detail: bool,
    show_chat: bool,
    show_results: bool,
    /// Show only the active log pane, hiding the sidebar, results, and other panes.
    focus_mode: bool,
    /// The field the timeline histogram buckets by, or `None` when the timeline is hidden.
    timeline_field: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            sidebar_width: None,
            results_height: None,
            detail_height: None,
            chat_height: None,
            pane_weights: Vec::new(),
            show_sidebar: true,
            show_detail: true,
            show_chat: true,
            show_results: true,
            focus_mode: false,
            timeline_field: None,
        }
    }
}

/// Where the timeline columns map in time, recorded each frame for mouse selection.
#[derive(Debug, Clone, Copy)]
struct TimelineGeom {
    /// The x of the first histogram column (past the row labels).
    x0: u16,
    buckets: usize,
    /// The rows the histogram sparklines occupy, for hit-testing.
    y0: u16,
    y1: u16,
    min: NaiveDateTime,
    max: NaiveDateTime,
}

/// The bars-per-bucket histogram to draw: one row per aggregation value.
struct TimelineData {
    min: NaiveDateTime,
    max: NaiveDateTime,
    rows: Vec<(String, Vec<u32>)>,
}

/// Sparkline bars from empty to full.
const SPARK_BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Aggregation values shown as separate histogram rows.
const TIMELINE_MAX_ROWS: usize = 4;
/// Columns reserved for each row's value label, before the sparkline.
const TIMELINE_LABEL_WIDTH: usize = 10;

/// One rendered line in the chat transcript.
#[derive(Debug, Clone)]
enum ChatLine {
    User(String),
    Assistant(String),
    /// A tool the assistant ran, and what it reported.
    Action(String),
    Error(String),
    /// Status text such as "thinking…".
    Info(String),
}

/// The AI chat panel: the worker, the conversation the model sees, and what to draw.
///
/// `conversation` is the model's view (system + user + assistant + tool turns);
/// `transcript` is the human's view. They are kept in step but are not the same list --
/// a tool result is a full turn for the model but a one-line `[ran …]` for the reader.
struct AiChat {
    worker: AiWorker,
    config: AiConfig,
    /// Keys typed in the chat with `/key`, per provider. Kept in memory only -- they are
    /// never written to disk, so nothing sensitive lands in `ai.json`.
    keys: std::collections::HashMap<crate::ai::Provider, String>,
    /// Skills switched on with `/skill`, by name. Their text is appended to the system
    /// prompt each turn.
    active_skills: Vec<String>,
    conversation: Vec<ChatMsg>,
    transcript: Vec<ChatLine>,
    input: String,
    /// Bumped on every question and on Esc, so a stale reply is ignored.
    generation: u64,
    /// A turn is in flight (waiting on the model or running its tools).
    pending: bool,
    scroll: usize,
    /// How many turns this exchange has taken, to stop a runaway tool loop.
    turns: usize,
}

/// Cap the tool-calling loop so a confused model cannot spin forever.
const AI_MAX_TURNS: usize = 12;

impl AiChat {
    fn new() -> Self {
        Self {
            worker: AiWorker::spawn(),
            config: AiConfig::load(),
            keys: std::collections::HashMap::new(),
            active_skills: Vec::new(),
            conversation: Vec::new(),
            transcript: Vec::new(),
            input: String::new(),
            generation: 0,
            pending: false,
            scroll: 0,
            turns: 0,
        }
    }

    /// The key to use for the current provider: one typed with `/key` if present, else the
    /// environment variable.
    fn resolved_key(&self) -> Option<String> {
        self.keys
            .get(&self.config.provider)
            .cloned()
            .or_else(|| self.config.api_key())
    }
}

/// A one-off request to infer a log schema from a file's first lines. Kept apart from the
/// chat so the two never share a worker thread or a generation counter.
struct SchemaInfer {
    worker: AiWorker,
    /// The `(generation, file_id)` of the request in flight, if any; replies to older ones,
    /// or with a mismatched generation, are ignored.
    pending: Option<(u64, String)>,
    next_gen: u64,
}

impl SchemaInfer {
    fn new() -> Self {
        Self {
            worker: AiWorker::spawn(),
            pending: None,
            next_gen: 0,
        }
    }
}

/// Rows the hide-pattern preview will read before it stops counting. Large enough that a
/// normal pane is covered exactly, small enough that a million-line view still redraws
/// between keystrokes.
const PATTERN_PREVIEW_LIMIT: usize = 50_000;
const PATTERN_PREVIEW_SAMPLES: usize = 2;

/// What a candidate hide pattern would do to the rows on screen.
#[derive(Debug, Default, Clone)]
struct PatternPreview {
    matched: usize,
    scanned: usize,
    total: usize,
    samples: Vec<String>,
    error: Option<String>,
}

impl PatternPreview {
    /// True when the scan stopped short of the pane, so `matched` is a floor, not a total.
    fn capped(&self) -> bool {
        self.scanned < self.total
    }
}

/// One offered template, with the rows it matches. Measured once, when the templates are
/// derived: the set does not change while the user picks among it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternCandidate {
    option: PatternOption,
    matched: usize,
}

/// The `H` field menu for one log line: every field of that line's schema, with the value
/// the line carries, and which of them the user has picked.
///
/// Picking several ANDs them. One field alone stays an `equals` rule, which reads better
/// in the sidebar and holds even on a line the schema cannot fully parse.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HideMenu {
    /// `(field, value)` in schema order.
    fields: Vec<(String, String)>,
    cursor: usize,
    picked: Vec<bool>,
    /// Which way the rule points. `H` is named for hiding, so it opens on exclude; Tab
    /// flips it to keep-only, matching the pattern popup.
    exclude: bool,
}

impl HideMenu {
    fn new(fields: Vec<(String, String)>) -> Self {
        Self {
            picked: vec![false; fields.len()],
            fields,
            cursor: 0,
            exclude: true,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.fields.is_empty() {
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.fields.len() - 1);
    }

    fn toggle(&mut self, index: usize) {
        if let Some(picked) = self.picked.get_mut(index) {
            *picked = !*picked;
        }
    }

    /// The picked fields with their values; the cursor's field when nothing is picked, so
    /// Enter always does something.
    fn chosen(&self) -> Vec<(String, String)> {
        let picked: Vec<(String, String)> = self
            .fields
            .iter()
            .zip(&self.picked)
            .filter(|(_, picked)| **picked)
            .map(|(field, _)| field.clone())
            .collect();
        if !picked.is_empty() {
            return picked;
        }
        self.fields.get(self.cursor).cloned().into_iter().collect()
    }
}

/// The `H` popup: an editable regex, and the templates the selection supports.
///
/// A single derived pattern is a guess about how greedy the user wanted to be. Offering
/// the whole ladder, each with the rows it would take out, turns that guess into a choice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternPrompt {
    text: String,
    /// The field the regex is matched against. `message` for a template derived from log
    /// text; `raw` for one built out of several of a line's fields at once.
    field: String,
    /// Which way the rule points. Tab flips it, so one derivation serves both
    /// "hide lines like this" and "show only lines like this".
    exclude: bool,
    /// Greediest first. Empty when the caller had no ladder to offer.
    candidates: Vec<PatternCandidate>,
    selected: usize,
    /// Rows read while counting, and rows the pane holds. They differ on a huge pane.
    scanned: usize,
    total: usize,
}

impl PatternPrompt {
    /// A bare prompt with no ladder behind it. Every real one comes from a selection.
    #[cfg(test)]
    fn new(text: impl Into<String>, exclude: bool) -> Self {
        Self {
            text: text.into(),
            field: "message".to_string(),
            exclude,
            candidates: Vec::new(),
            selected: 0,
            scanned: 0,
            total: 0,
        }
    }

    /// Step through the ladder, loading the template into the editable field. An edit the
    /// user made is discarded: they asked for a different template.
    fn pick(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.candidates.len() - 1);
        self.text = self.candidates[self.selected].option.pattern.clone();
    }

    /// True once the field no longer holds the template it was loaded from.
    fn edited(&self) -> bool {
        self.candidates
            .get(self.selected)
            .map(|candidate| candidate.option.pattern != self.text)
            .unwrap_or(false)
    }

    fn capped(&self) -> bool {
        self.scanned < self.total
    }
}

/// The guided filter builder's state: one editable value per row, plus the suggestion pools
/// and the live preview the drawer shows.
#[derive(Debug, Clone)]
struct FilterBuilder {
    /// `Some(i)` edits `project.filters.rules[i]`; `None` adds a new rule.
    edit_index: Option<usize>,
    /// Schema scope: `None` is "Any", else a schema name.
    schema: Option<String>,
    field: String,
    /// Index into `crate::core::filters::OPS`.
    op: usize,
    exclude: bool,
    value: String,
    /// The focused row, `0..ROWS`.
    row: usize,
    /// Schema names to cycle among (after "Any").
    schemas: Vec<String>,
    /// Field-name suggestions from the active schema, for the field row.
    fields: Vec<String>,
    /// Frequent values of the current field in the view, for the value row.
    values: Vec<String>,
    preview: PatternPreview,
    error: Option<String>,
}

impl FilterBuilder {
    const ROWS: usize = 5;
    const SCHEMA: usize = 0;
    const FIELD: usize = 1;
    const OP: usize = 2;
    const ACTION: usize = 3;
    const VALUE: usize = 4;

    fn op_name(&self) -> &'static str {
        crate::core::filters::OPS
            .get(self.op)
            .copied()
            .unwrap_or("equals")
    }

    fn action_name(&self) -> &'static str {
        if self.exclude {
            "exclude"
        } else {
            "include"
        }
    }

    fn schema_label(&self) -> String {
        self.schema.clone().unwrap_or_else(|| "Any".to_string())
    }

    /// Build a rule from the current fields, or an error naming what is missing/invalid.
    fn rule(&self) -> Result<FilterRule, String> {
        let field = self.field.trim();
        if field.is_empty() {
            return Err("choose a field".to_string());
        }
        let value = self.value.trim();
        if value.is_empty() {
            return Err("value cannot be empty".to_string());
        }
        if self.op_name() == "regex" {
            regex::Regex::new(value).map_err(|error| one_line(&error.to_string()))?;
        }
        let mut rule = FilterRule::new(field, self.op_name(), value, self.action_name());
        if let Some(schema) = self.schema.as_ref() {
            rule = rule.for_log_schema(schema.clone());
        }
        Ok(rule)
    }

    /// The equivalent raw-grammar text, for switching to the raw editor.
    fn to_input(&self) -> String {
        let field = if self.field.trim().is_empty() {
            "field"
        } else {
            self.field.trim()
        };
        let mut rule =
            FilterRule::new(field, self.op_name(), self.value.trim(), self.action_name());
        if let Some(schema) = self.schema.as_ref() {
            rule = rule.for_log_schema(schema.clone());
        }
        rule.to_input()
    }
}

#[derive(Debug, Clone)]
struct ThemePicker {
    selected: usize,
}

impl ThemePicker {
    fn new(current: ThemeName) -> Self {
        let selected = ThemeName::ALL
            .iter()
            .position(|theme| *theme == current)
            .unwrap_or(0);
        Self { selected }
    }

    fn pick(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(ThemeName::ALL.len().saturating_sub(1));
    }

    fn theme(&self) -> ThemeName {
        ThemeName::ALL
            .get(self.selected)
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
struct SourceEditor {
    file_id: String,
    row: usize,
    short_name: String,
    description: String,
    tag: String,
    schema: String,
}

impl SourceEditor {
    const SHORT_NAME: usize = 0;
    const DESCRIPTION: usize = 1;
    const TAG: usize = 2;
    const SCHEMA: usize = 3;
    const ROWS: usize = 4;

    fn new(file: &LogFileModel) -> Self {
        Self {
            file_id: file.file_id.clone(),
            row: Self::SHORT_NAME,
            short_name: source_default_short_name(file),
            description: file.description.clone(),
            tag: file.tag.clone(),
            schema: file.extractor_name.clone(),
        }
    }

    fn pick(&mut self, delta: isize) {
        self.row = self
            .row
            .saturating_add_signed(delta)
            .min(Self::ROWS.saturating_sub(1));
    }

    fn field_mut(&mut self) -> &mut String {
        match self.row {
            Self::SHORT_NAME => &mut self.short_name,
            Self::DESCRIPTION => &mut self.description,
            Self::TAG => &mut self.tag,
            Self::SCHEMA => &mut self.schema,
            _ => &mut self.short_name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSourceField {
    Kind,
    ShortName,
    Description,
    Tag,
    Namespace,
    Pod,
    Container,
    Context,
    DockerContainer,
    Unit,
    Since,
    Tail,
    Schema,
}

#[derive(Debug, Clone)]
struct LiveSourceEditor {
    file_id: Option<String>,
    row: usize,
    kind: LiveSourceKind,
    short_name: String,
    description: String,
    tag: String,
    namespace: String,
    pod: String,
    container: String,
    context: String,
    docker_container: String,
    unit: String,
    since: String,
    tail: String,
    schema: String,
}

impl LiveSourceEditor {
    fn new() -> Self {
        Self {
            file_id: None,
            row: 0,
            kind: LiveSourceKind::Kubernetes,
            short_name: String::new(),
            description: String::new(),
            tag: String::new(),
            namespace: String::new(),
            pod: String::new(),
            container: String::new(),
            context: String::new(),
            docker_container: String::new(),
            unit: String::new(),
            since: String::new(),
            tail: String::new(),
            schema: GENERIC_EXTRACTOR_NAME.to_string(),
        }
    }

    fn from_file(file: &LogFileModel) -> Self {
        let config = file.live.clone().unwrap_or_default();
        Self {
            file_id: Some(file.file_id.clone()),
            row: 0,
            kind: config.kind,
            short_name: source_default_short_name(file),
            description: file.description.clone(),
            tag: file.tag.clone(),
            namespace: config.namespace,
            pod: config.pod,
            container: config.container,
            context: config.context,
            docker_container: config.docker_container,
            unit: config.unit,
            since: config.since,
            tail: config.tail,
            schema: file.extractor_name.clone(),
        }
    }

    fn rows(&self) -> Vec<(LiveSourceField, &'static str, String)> {
        let mut rows = vec![
            (LiveSourceField::Kind, "Type", self.kind.label().to_string()),
            (
                LiveSourceField::ShortName,
                "Short name",
                self.short_name.clone(),
            ),
            (
                LiveSourceField::Description,
                "Description",
                self.description.clone(),
            ),
            (LiveSourceField::Tag, "Tag", self.tag.clone()),
        ];
        match self.kind {
            LiveSourceKind::Kubernetes => rows.extend([
                (
                    LiveSourceField::Namespace,
                    "Namespace",
                    self.namespace.clone(),
                ),
                (LiveSourceField::Pod, "Pod", self.pod.clone()),
                (
                    LiveSourceField::Container,
                    "Container",
                    self.container.clone(),
                ),
                (LiveSourceField::Context, "Context", self.context.clone()),
            ]),
            LiveSourceKind::Docker => rows.push((
                LiveSourceField::DockerContainer,
                "Container",
                self.docker_container.clone(),
            )),
            LiveSourceKind::Journalctl => {
                rows.push((LiveSourceField::Unit, "Unit", self.unit.clone()));
            }
        }
        rows.extend([
            (LiveSourceField::Since, "Since", self.since.clone()),
            (LiveSourceField::Tail, "Tail lines", self.tail.clone()),
            (LiveSourceField::Schema, "Schema", self.schema.clone()),
        ]);
        rows
    }

    fn pick(&mut self, delta: isize) {
        self.row = self
            .row
            .saturating_add_signed(delta)
            .min(self.rows().len().saturating_sub(1));
    }

    fn cycle_kind(&mut self, delta: isize) {
        let kinds = [
            LiveSourceKind::Kubernetes,
            LiveSourceKind::Docker,
            LiveSourceKind::Journalctl,
        ];
        let current = kinds
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(kinds.len().saturating_sub(1));
        self.kind = kinds[next];
        self.row = self.row.min(self.rows().len().saturating_sub(1));
    }

    fn set_kind(&mut self, kind: LiveSourceKind) {
        self.kind = kind;
        self.row = self.row.min(self.rows().len().saturating_sub(1));
    }

    fn field_for_row(&self) -> LiveSourceField {
        self.rows()
            .get(self.row)
            .map(|(field, _, _)| *field)
            .unwrap_or(LiveSourceField::Kind)
    }

    fn focus_field(&mut self, field: LiveSourceField) {
        if let Some(index) = self
            .rows()
            .iter()
            .position(|(candidate, _, _)| *candidate == field)
        {
            self.row = index;
        }
    }

    fn field_mut(&mut self) -> Option<&mut String> {
        match self.field_for_row() {
            LiveSourceField::Kind => None,
            LiveSourceField::ShortName => Some(&mut self.short_name),
            LiveSourceField::Description => Some(&mut self.description),
            LiveSourceField::Tag => Some(&mut self.tag),
            LiveSourceField::Namespace => Some(&mut self.namespace),
            LiveSourceField::Pod => Some(&mut self.pod),
            LiveSourceField::Container => Some(&mut self.container),
            LiveSourceField::Context => Some(&mut self.context),
            LiveSourceField::DockerContainer => Some(&mut self.docker_container),
            LiveSourceField::Unit => Some(&mut self.unit),
            LiveSourceField::Since => Some(&mut self.since),
            LiveSourceField::Tail => Some(&mut self.tail),
            LiveSourceField::Schema => Some(&mut self.schema),
        }
    }

    fn config(&self) -> LiveSourceConfig {
        LiveSourceConfig {
            kind: self.kind,
            namespace: self.namespace.trim().to_string(),
            pod: self.pod.trim().to_string(),
            container: self.container.trim().to_string(),
            context: self.context.trim().to_string(),
            docker_container: self.docker_container.trim().to_string(),
            unit: self.unit.trim().to_string(),
            since: self.since.trim().to_string(),
            tail: self.tail.trim().to_string(),
            follow: true,
        }
    }
}

#[derive(Debug, Clone)]
struct SchemaLibraryPicker {
    target: SchemaPickerTarget,
    options: Vec<Extractor>,
    selected: usize,
}

#[derive(Debug, Clone)]
enum SchemaPickerTarget {
    File(String),
    LiveEditor(LiveSourceEditor),
}

#[derive(Debug, Clone)]
struct LiveQuickPick {
    editor: LiveSourceEditor,
    target: LiveSourceField,
    options: Vec<String>,
    selected: usize,
    loading: bool,
    message: String,
}

struct LivePickResult {
    target: LiveSourceField,
    options: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
enum Mode {
    Normal,
    Search(String),
    AddFile(String),
    /// The `o` popup: walk the filesystem and pick a folder, rather than typing its path.
    OpenFolder(FolderBrowser),
    Filter(String),
    TimePicker(TimePicker),
    ExportFilters(String),
    LoadFilters(String),
    ExportSearches(String),
    ImportSearches(String),
    ExportBookmarks(String),
    ImportBookmarks(String),
    ExportSchemas(String),
    ImportSchemas(String),
    ExportIncident(String),
    Extractor(String),
    SaveSourceSchema {
        file_id: String,
        text: String,
    },
    SourceEditor(SourceEditor),
    LiveSourceEditor(LiveSourceEditor),
    SchemaLibraryPicker(SchemaLibraryPicker),
    LiveQuickPick(LiveQuickPick),
    LiveSchemaEditor {
        editor: LiveSourceEditor,
        text: String,
    },
    /// Enter on a sidebar filter: edit that rule in place. `index` addresses
    /// `project.filters.rules`.
    EditFilter {
        index: usize,
        text: String,
    },
    /// Enter on a saved search: edit that query in place.
    EditSearch {
        index: usize,
        text: String,
    },
    /// Add or edit a note on a bookmarked line. Empty text is allowed: the bookmark itself
    /// remains, and `m` removes it.
    BookmarkNote {
        file_id: String,
        line_no: usize,
        text: String,
    },
    /// `H` on a single line: hide by one of that line's fields, or by several at once.
    HideChoice(HideMenu),
    /// Regexes derived from the selected lines, awaiting the user's sign-off.
    HidePattern(PatternPrompt),
    EntryDetail {
        scroll: usize,
    },
    PrettyPrint {
        title: String,
        body: String,
        scroll: usize,
    },
    Help,
    /// The action-history popup (`U`): recent user and AI actions.
    ActionLog,
    /// The command palette (`Ctrl+P` / `:`): a searchable, context-aware action list.
    Palette(Palette),
    /// The guided filter builder (`f`): dropdowns for schema/field/op/action/value with a
    /// live match-count preview, an alternative to the raw filter grammar.
    FilterBuilder(FilterBuilder),
    ThemePicker(ThemePicker),
}

/// A user action the palette can run. Both the palette and the keyboard funnel through
/// `AppState::dispatch_command`, so the behaviour of each action lives in one place. Every
/// variant maps to an existing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Search,
    AddFilter,
    ClearFilters,
    TimeRange,
    ImportFilters,
    ExportFilters,
    ImportSearches,
    ExportSearches,
    ImportSchemas,
    ExportSchemas,
    ExportIncident,
    AskAi,
    Copy,
    BookmarkLine,
    EditBookmarkNote,
    PreviousBookmark,
    NextBookmark,
    HideSimilar,
    PrettyPrint,
    MarkElapsed,
    ShowDetail,
    OpenSource,
    MergeSource,
    DeleteSelected,
    EditItem,
    ToggleItem,
    SplitColumns,
    SplitRows,
    ClosePane,
    Undo,
    Redo,
    ActionHistory,
    FocusMode,
    Timeline,
    ToggleSidebar,
    ToggleDetail,
    ToggleResults,
    ToggleChat,
    ChooseTheme,
    AddFileBrowse,
    AddLiveSource,
    OpenFolder,
    Help,
}

impl Command {
    /// The label shown in the palette.
    fn label(self) -> &'static str {
        match self {
            Command::Search => "Search",
            Command::AddFilter => "Add text filter",
            Command::ClearFilters => "Clear all filters",
            Command::TimeRange => "Set time range",
            Command::ImportFilters => "Import filter pack",
            Command::ExportFilters => "Export filter pack",
            Command::ImportSearches => "Import saved-search library",
            Command::ExportSearches => "Export saved-search library",
            Command::ImportSchemas => "Import schema library",
            Command::ExportSchemas => "Export schema library",
            Command::ExportIncident => "Export incident Markdown",
            Command::AskAi => "Ask AI to help",
            Command::Copy => "Copy selection",
            Command::BookmarkLine => "Bookmark line",
            Command::EditBookmarkNote => "Edit bookmark note",
            Command::PreviousBookmark => "Previous bookmark",
            Command::NextBookmark => "Next bookmark",
            Command::HideSimilar => "Hide / keep similar lines",
            Command::PrettyPrint => "Pretty-print message",
            Command::MarkElapsed => "Mark elapsed time from here",
            Command::ShowDetail => "Show line detail",
            Command::OpenSource => "Open this source",
            Command::MergeSource => "Add source to view (merge)",
            Command::DeleteSelected => "Delete selected",
            Command::EditItem => "Edit",
            Command::ToggleItem => "Enable / disable",
            Command::SplitColumns => "Split into columns",
            Command::SplitRows => "Split into rows",
            Command::ClosePane => "Close pane",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::ActionHistory => "Action history",
            Command::FocusMode => "Focus mode (only the active pane)",
            Command::Timeline => "Timeline (cycle by level / module / source)",
            Command::ToggleSidebar => "Toggle sidebar",
            Command::ToggleDetail => "Toggle detail panel",
            Command::ToggleResults => "Toggle results panel",
            Command::ToggleChat => "Toggle chat panel",
            Command::ChooseTheme => "Choose theme",
            Command::AddFileBrowse => "Add a log file",
            Command::AddLiveSource => "Add live log source",
            Command::OpenFolder => "Open a folder",
            Command::Help => "Help",
        }
    }

    /// The key that also runs this action, shown on the right (empty when there is none).
    fn key_hint(self) -> &'static str {
        match self {
            Command::Search => "/",
            Command::AddFilter => "f",
            Command::ClearFilters => "",
            Command::TimeRange => "t",
            Command::ImportFilters => "L",
            Command::ExportFilters => "X",
            Command::ImportSearches => "L",
            Command::ExportSearches => "X",
            Command::ImportSchemas => "",
            Command::ExportSchemas => "",
            Command::ExportIncident => "E",
            Command::AskAi => "A",
            Command::Copy => "y",
            Command::BookmarkLine => "m",
            Command::EditBookmarkNote => "M",
            Command::PreviousBookmark => "[m",
            Command::NextBookmark => "]m",
            Command::HideSimilar => "H",
            Command::PrettyPrint => "P",
            Command::MarkElapsed => "T",
            Command::ShowDetail => "Enter",
            Command::OpenSource => "",
            Command::MergeSource => "Space",
            Command::DeleteSelected => "d",
            Command::EditItem => "Enter",
            Command::ToggleItem => "Space",
            Command::SplitColumns => "|",
            Command::SplitRows => "-",
            Command::ClosePane => "w",
            Command::Undo => "u",
            Command::Redo => "Ctrl+r",
            Command::ActionHistory => "U",
            Command::FocusMode => "z",
            Command::Timeline => "b",
            Command::ToggleSidebar => "",
            Command::ToggleDetail => "",
            Command::ToggleResults => "",
            Command::ToggleChat => "",
            Command::ChooseTheme => "",
            Command::AddFileBrowse => "a",
            Command::AddLiveSource => "",
            Command::OpenFolder => "o",
            Command::Help => "?",
        }
    }
}

/// The command palette's state: the query typed so far, the context's command list, and the
/// cursor into the filtered view.
#[derive(Debug, Clone)]
struct Palette {
    query: String,
    commands: Vec<Command>,
    selected: usize,
}

impl Palette {
    fn new(commands: Vec<Command>) -> Self {
        Self {
            query: String::new(),
            commands,
            selected: 0,
        }
    }

    /// The commands matching the query (case-insensitive substring on the label).
    fn filtered(&self) -> Vec<Command> {
        let query = self.query.trim().to_lowercase();
        self.commands
            .iter()
            .copied()
            .filter(|command| query.is_empty() || command.label().to_lowercase().contains(&query))
            .collect()
    }

    fn clamp(&mut self) {
        let count = self.filtered().len();
        self.selected = self.selected.min(count.saturating_sub(1));
    }
}

#[derive(Debug, Clone)]
enum SidebarItem {
    Section(String),
    /// A heading inside a section: `Text` and `Time` under `Filters`.
    SubSection(String),
    File {
        file_id: String,
        label: String,
    },
    /// `index` addresses `project.filters.rules`, so Space and Enter act on the rule
    /// itself rather than re-deriving it by counting rows.
    Filter {
        index: usize,
        label: String,
    },
    /// The project's one time range. Enter reopens the picker on it rather than the
    /// filter text editor: nobody wants to hand-edit `timestamp range 'a..b'`.
    TimeFilter {
        index: usize,
        label: String,
    },
    /// `index` addresses `project.saved_searches`.
    Search {
        index: usize,
        text: String,
        label: String,
    },
    Bookmark {
        index: usize,
        label: String,
    },
    Hint(String),
}

#[derive(Debug, Clone)]
struct BookmarkNavPending {
    forward: bool,
    count: usize,
    previous_sidebar_width: Option<u16>,
    previous_show_sidebar: bool,
    previous_focus_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailSurface {
    Inline,
    Popup,
    PrettyPrint,
    Help,
}

/// Quick ranges offered by the time picker, as (label, seconds back from the anchor).
/// Zero means "the whole log".
const TIME_PRESETS: &[(&str, i64)] = &[
    ("Last 15 minutes", 15 * 60),
    ("Last 1 hour", 60 * 60),
    ("Last 24 hours", 24 * 60 * 60),
    ("Last 7 days", 7 * 24 * 60 * 60),
    ("All time", 0),
];

/// The `t` popup: a preset list above an editable start and end.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TimePicker {
    start: String,
    end: String,
    /// Focused row. `0..TIME_PRESETS.len()` are presets, then `START_ROW`, then `END_ROW`.
    row: usize,
    /// Caret within whichever of `start`/`end` has focus.
    cursor: usize,
    earliest: Option<NaiveDateTime>,
    latest: Option<NaiveDateTime>,
}

impl TimePicker {
    const START_ROW: usize = TIME_PRESETS.len();
    const END_ROW: usize = TIME_PRESETS.len() + 1;
    const ROWS: usize = TIME_PRESETS.len() + 2;
    /// Columns before the value in a `> Start   <value>` row, for placing the caret.
    const FIELD_PREFIX: usize = 10;
    /// "Last 1 hour", the row the picker opens on.
    const DEFAULT_PRESET: usize = 1;

    fn new(bounds: Option<(NaiveDateTime, NaiveDateTime)>) -> Self {
        let (earliest, latest) = match bounds {
            Some((earliest, latest)) => (Some(earliest), Some(latest)),
            None => (None, None),
        };
        let mut picker = Self {
            start: String::new(),
            end: String::new(),
            row: Self::DEFAULT_PRESET,
            cursor: 0,
            earliest,
            latest,
        };
        picker.apply_preset(Self::DEFAULT_PRESET);
        picker
    }

    /// "Last 1 hour" counts back from the newest entry across the loaded logs, not from
    /// wall-clock
    /// now: a log opened a week after it was written would otherwise select nothing.
    fn anchor(&self) -> NaiveDateTime {
        self.latest
            .unwrap_or_else(|| chrono::Local::now().naive_local())
    }

    fn apply_preset(&mut self, index: usize) {
        let Some((_, seconds)) = TIME_PRESETS.get(index) else {
            return;
        };
        let end = self.anchor();
        let start = if *seconds == 0 {
            self.earliest.unwrap_or(end)
        } else {
            end - ChronoDuration::seconds(*seconds)
        };
        self.start = format_filter_datetime(start);
        self.end = format_filter_datetime(end);
    }

    /// Show an existing range, with the caret on its start rather than on a preset that
    /// would overwrite it the moment the user pressed Space.
    fn load_range(&mut self, start: &str, end: &str) {
        self.start = start.to_string();
        self.end = end.to_string();
        self.row = Self::START_ROW;
        self.cursor = self.start.chars().count();
    }

    fn on_preset(&self) -> bool {
        self.row < TIME_PRESETS.len()
    }

    fn field(&self) -> Option<&String> {
        match self.row {
            Self::START_ROW => Some(&self.start),
            Self::END_ROW => Some(&self.end),
            _ => None,
        }
    }

    fn field_mut(&mut self) -> Option<&mut String> {
        match self.row {
            Self::START_ROW => Some(&mut self.start),
            Self::END_ROW => Some(&mut self.end),
            _ => None,
        }
    }

    /// Clamped rather than wrapping: `Down` off the End field must not land on a preset
    /// and overwrite what was just typed there.
    fn move_row(&mut self, delta: isize) {
        self.row = (self.row as isize + delta).clamp(0, Self::ROWS as isize - 1) as usize;
        // Landing on a preset fills the fields from it, so the picker always shows the
        // range Enter would apply. Stepping onto Start or End leaves them alone.
        if self.on_preset() {
            self.apply_preset(self.row);
        }
        // Land the caret at the end of a field you step onto, as the input popups do.
        self.cursor = self.field().map(|value| value.chars().count()).unwrap_or(0);
    }

    /// The `range` filter value, `start..end`. Either side may be blank for an open end.
    fn to_range(&self) -> Result<String, String> {
        let start = self.start.trim();
        let end = self.end.trim();
        if start.is_empty() && end.is_empty() {
            return Err("give a start, an end, or both".to_string());
        }

        let low = (!start.is_empty()).then(|| parse_datetime(start)).flatten();
        let high = (!end.is_empty()).then(|| parse_datetime(end)).flatten();
        if !start.is_empty() && low.is_none() {
            return Err(format!("start is not a timestamp: {start}"));
        }
        if !end.is_empty() && high.is_none() {
            return Err(format!("end is not a timestamp: {end}"));
        }
        if let (Some(low), Some(high)) = (low, high) {
            if low > high {
                return Err("start is after end".to_string());
            }
        }
        Ok(format!("{start}..{end}"))
    }
}

/// One row of the folder browser. The first row opens the folder being browsed, so Enter
/// always means "do the thing this row names" and never has to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserRow {
    OpenCurrent,
    Parent,
    Child(PathBuf),
    /// A file that can be added as a log source. Only listed in `File` purpose.
    File(PathBuf),
}

/// What the browser is picking: a folder to open (`o`, add every text file in it) or a
/// single file to add (`a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserPurpose {
    Folder,
    File,
}

/// The `o`/`a` popup: the folder being browsed, plus its subfolders (and, when picking a
/// file, the text files in it).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderBrowser {
    purpose: BrowserPurpose,
    current: PathBuf,
    rows: Vec<BrowserRow>,
    selected: usize,
    /// First row on screen. The drawer owns this: only it knows how tall the popup is.
    scroll: usize,
    /// Dot-folders are hidden by default; a project's own `.logscouter` is not a
    /// destination, and neither is most of what lives under `~`.
    show_hidden: bool,
    /// Files directly inside `current`, the pool `o` would draw its logs from.
    file_count: usize,
}

impl FolderBrowser {
    /// Browse `folder` to open a whole folder (the `o` flow).
    fn open(folder: PathBuf) -> std::io::Result<Self> {
        Self::with_purpose(folder, BrowserPurpose::Folder)
    }

    /// Browse `folder` to pick one file to add (the `a` flow).
    fn open_for_file(folder: PathBuf) -> std::io::Result<Self> {
        Self::with_purpose(folder, BrowserPurpose::File)
    }

    fn with_purpose(folder: PathBuf, purpose: BrowserPurpose) -> std::io::Result<Self> {
        let mut browser = Self {
            purpose,
            current: folder,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            show_hidden: false,
            file_count: 0,
        };
        browser.reload()?;
        Ok(browser)
    }

    /// Re-read `current`. Selection returns to the top.
    fn reload(&mut self) -> std::io::Result<()> {
        let picking_file = matches!(self.purpose, BrowserPurpose::File);
        let mut children = Vec::new();
        let mut files = Vec::new();
        let mut file_count = 0;
        for entry in std::fs::read_dir(&self.current)? {
            let path = entry?.path();
            if path.is_dir() {
                if self.show_hidden || !is_hidden(&path) {
                    children.push(path);
                }
            } else if path.is_file() {
                file_count += 1;
                // Offer only text files as log sources, and respect the hidden toggle.
                if picking_file
                    && (self.show_hidden || !is_hidden(&path))
                    && crate::core::project::is_text_file(&path)
                {
                    files.push(path);
                }
            }
        }
        children.sort_by_key(|path| folder_name(path).to_lowercase());
        files.sort_by_key(|path| folder_name(path).to_lowercase());

        // Folder purpose leads with "open this folder"; file purpose has no such row.
        self.rows = if picking_file {
            Vec::new()
        } else {
            vec![BrowserRow::OpenCurrent]
        };
        if self.current.parent().is_some() {
            self.rows.push(BrowserRow::Parent);
        }
        self.rows
            .extend(children.into_iter().map(BrowserRow::Child));
        self.rows.extend(files.into_iter().map(BrowserRow::File));
        self.file_count = file_count;
        self.selected = 0;
        self.scroll = 0;
        Ok(())
    }

    /// Browse `folder`. An unreadable folder leaves the browser exactly where it was,
    /// so a wrong turn into `/root` is not a dead end.
    fn go_to(&mut self, folder: PathBuf) -> std::io::Result<()> {
        let previous = std::mem::replace(&mut self.current, folder);
        if let Err(error) = self.reload() {
            self.current = previous;
            let _ = self.reload();
            return Err(error);
        }
        Ok(())
    }

    /// `None` at the filesystem root, where there is nowhere left to go up to.
    fn parent_path(&self) -> Option<PathBuf> {
        self.current.parent().map(Path::to_path_buf)
    }

    fn toggle_hidden(&mut self) -> std::io::Result<()> {
        self.show_hidden = !self.show_hidden;
        self.reload()
    }

    fn selected_row(&self) -> Option<&BrowserRow> {
        self.rows.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len() - 1);
    }

    fn label(&self, row: &BrowserRow) -> String {
        match row {
            BrowserRow::OpenCurrent => "./     open this folder".to_string(),
            BrowserRow::Parent => "../    go up".to_string(),
            BrowserRow::Child(path) => format!("{}/", folder_name(path)),
            BrowserRow::File(path) => folder_name(path),
        }
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

/// The last component of `path`, or the whole path for a root like `/`.
fn folder_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or(""))
        .to_string()
}

/// The `>`, `+` and `*` columns in front of every pane row. Not selectable: a drag from
/// the left edge should start at the timestamp, not at the cursor marker.
const ROW_MARKER_WIDTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseDrag {
    Pane {
        pane: usize,
        anchor: usize,
    },
    /// A drag that has so far stayed inside one row, so it is selecting characters.
    /// It becomes a `Pane` row drag the moment it crosses into another row.
    PaneText {
        pane: usize,
        position: usize,
        anchor: usize,
    },
    Detail {
        surface: DetailSurface,
        anchor: usize,
    },
    /// Dragging the sidebar/pane separator to resize the sidebar.
    Sidebar,
    /// Dragging the border between pane `boundary` and `boundary + 1` to reweight them.
    PaneSeparator {
        boundary: usize,
    },
    /// Dragging a panel's top border to resize its height. `bottom` is the fixed lower edge,
    /// so the new height is `bottom - drag_row`.
    PanelHeight {
        panel: PanelEdge,
        bottom: u16,
    },
    /// Dragging across timeline buckets to build a time-range filter; `anchor_col` is where
    /// the press landed.
    Timeline {
        anchor_col: u16,
    },
}

/// A stacked panel whose height can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelEdge {
    Results,
    Detail,
    Chat,
}

/// A draggable horizontal border above a panel, recorded each frame for hit-testing.
#[derive(Debug, Clone, Copy)]
struct PanelSeparator {
    panel: PanelEdge,
    /// The border row.
    top: u16,
    /// The fixed bottom edge; a drag to row `r` sets the height to `bottom - r`.
    bottom: u16,
    /// The horizontal span the border occupies (`x0..x1`).
    x0: u16,
    x1: u16,
}

/// The line every other line's time is measured from, set with `T`.
///
/// It stores the timestamp rather than only the row, so the offsets stay meaningful when
/// a filter or search moves rows out from under it. It is scoped to one file: a pane
/// showing something else keeps absolute timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ElapsedMark {
    file_id: String,
    at: NaiveDateTime,
    line_no: usize,
}

/// A run of characters picked out of one pane row with the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextSelection {
    pane: usize,
    /// Visible position of the row within the pane.
    position: usize,
    /// Character offsets into the row as `row_line` builds it. Inclusive on both ends.
    anchor: usize,
    cursor: usize,
}

impl TextSelection {
    fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetailSelection {
    surface: DetailSurface,
    anchor: usize,
    cursor: usize,
}

/// Work is sliced across frames rather than blocking the draw loop, so a multi-second
/// load or filter shows a moving bar instead of a frozen screen.
const WORK_BUDGET: Duration = Duration::from_millis(16);
const LOAD_CHUNK_LINES: usize = 4_000;
const SCAN_CHUNK_ENTRIES: usize = 8_000;

struct Progress {
    label: String,
    done: u64,
    total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// What to do once a recompute finishes; the caller cannot act on results immediately
/// because the scan spans frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    GotoFirstMatch,
}

enum Job {
    // Boxed: a LoadJob owns a BufReader and dwarfs the other variant.
    Load(Box<LoadJob>),
    Recompute(RecomputeJob),
}

struct LoadJob {
    file_id: String,
    display_name: String,
    lines: std::io::Lines<std::io::BufReader<std::fs::File>>,
    builder: EntryBuilder,
    extractor: Option<Extractor>,
    bytes: u64,
    total: u64,
    error: Option<String>,
}

enum LiveEvent {
    /// A physical line off one of the child's pipes, plus the stream tag that pipe carries
    /// (`[logscout stderr] ` for stderr, empty for stdout). The tag is applied later, in
    /// `drain_live_events`, where the schema is known -- so it can land only on lines that
    /// start an entry and leave continuation lines clean.
    Line { text: String, prefix: &'static str },
    Closed,
}

struct LiveRuntime {
    child: Child,
    rx: Receiver<LiveEvent>,
    builder: EntryBuilder,
    base_entries: usize,
    open_pipes: usize,
    exit_status: Option<ExitStatus>,
}

impl Drop for LiveRuntime {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct RecomputeJob {
    pane: usize,
    after: After,
    stage: Stage,
}

/// Tag a live line with its stream prefix, but only when the line *starts* a new entry.
///
/// stderr lines arrive tagged `[logscout stderr] ` so a merged live stream can tell the
/// two pipes apart, and a schema's `entry_start` is written expecting that tag on the
/// header line. Continuation lines -- the wrapped remainder of a multi-line message like a
/// PostgreSQL `statement:` -- gain nothing from it and only read as noise, so they are
/// left clean. The test is `is_start` on the *prefixed* candidate: a header keeps the tag
/// (and still matches `entry_start`), a continuation drops it. stdout carries no prefix
/// and is returned untouched; with no schema every line is treated as a start, preserving
/// the old always-prefix behaviour.
fn apply_live_prefix(text: String, prefix: &'static str, extractor: Option<&Extractor>) -> String {
    if prefix.is_empty() {
        return text;
    }
    let prefixed = format!("{prefix}{text}");
    let starts_entry = extractor
        .map(|extractor| extractor.is_start(&prefixed))
        .unwrap_or(true);
    if starts_entry {
        prefixed
    } else {
        text
    }
}

fn spawn_live_pipe_reader<R>(reader: R, tx: Sender<LiveEvent>, prefix: &'static str)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        for line in io::BufReader::new(reader).lines() {
            match line {
                Ok(text) => {
                    if tx.send(LiveEvent::Line { text, prefix }).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(LiveEvent::Line {
                        text: format!("[logscout stream read error] {error}"),
                        prefix: "",
                    });
                    break;
                }
            }
        }
        let _ = tx.send(LiveEvent::Closed);
    });
}

enum Stage {
    /// The expensive pass: which entries survive the filters.
    Filter {
        filters: FilterSet,
        next: usize,
        base: Vec<usize>,
    },
    /// The cheap pass: which of the surviving entries match the query.
    Search {
        base: VisibleIndices,
        query: Option<Query>,
        context: usize,
        next: usize,
        positions: Vec<usize>,
        matches: HashSet<usize>,
    },
}

pub fn run(project: Project) -> anyhow::Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let mut app = AppState::new(project);
    app.queue_initial_loads();

    loop {
        // A reply from the AI worker may have arrived; run any tools it asked for. Wrap it
        // so any state the AI changed becomes one undo step, attributed to the AI.
        if app.ai_busy() {
            let before = app.undo_snapshot();
            app.drain_ai_events();
            app.commit_change(before, Actor::Ai);
        } else {
            app.drain_ai_events();
        }
        // A schema-inference reply may have arrived; open its suggestion for review.
        app.drain_schema_events();
        app.drain_live_events();
        app.drain_live_pick_events();
        app.check_source_modifications();

        // Long work runs in slices between frames, so the progress bar animates and
        // Esc still gets through. Everything else waits until the work is done.
        if app.work_pending() {
            app.step_work(WORK_BUDGET);
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::ZERO)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                        app.cancel_work();
                    }
                }
            }
            continue;
        }

        terminal.draw(|frame| app.draw(frame))?;
        // A chat turn or schema inference in flight arrives asynchronously, so poll briefly
        // instead of blocking a whole 100ms -- the reply then lands promptly.
        let poll = if app.ai_busy()
            || app.schema_pending()
            || app.live_active()
            || app.live_pick_rx.is_some()
        {
            Duration::from_millis(30)
        } else {
            Duration::from_millis(100)
        };
        if event::poll(poll)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    // Record the pre-key state, act, then commit an undo step if it changed.
                    let before = app.undo_snapshot();
                    let quit = app.handle_key(key)?;
                    app.commit_change(before, Actor::User);
                    if quit {
                        break;
                    }
                }
                // Drags fire many events; capture on press and commit on release so a whole
                // drag is a single undo step.
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            app.undo_pending = Some(app.undo_snapshot());
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            app.handle_mouse(mouse);
                            if let Some(before) = app.undo_pending.take() {
                                app.commit_change(before, Actor::User);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    app.handle_mouse(mouse);
                }
                _ => {}
            }
        }
    }

    // Record the panes, their files and their searches, so reopening this folder
    // resumes where it left off.
    app.capture_session();
    app.project.save().ok();
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal.draw(f).map(|_| ())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

struct AppState {
    project: Project,
    panes: Vec<PaneState>,
    focused_pane: usize,
    focus: Focus,
    split_mode: SplitMode,
    sidebar_selected: usize,
    mode: Mode,
    status: String,
    count: usize,
    g_pending: bool,
    bookmark_nav_pending: Option<BookmarkNavPending>,
    results_selected: usize,
    results_scroll: usize,
    results_area: Rect,
    /// Inner rect of the sidebar list, for click hit-testing.
    sidebar_area: Rect,
    /// Inner rects of log panes, for mouse hit-testing.
    pane_areas: Vec<Rect>,
    /// Inner rect of the inline detail panel.
    detail_area: Rect,
    /// Inner rect of the large entry detail popup.
    entry_detail_area: Rect,
    /// Inner rect of the pretty-print popup.
    pretty_print_area: Rect,
    /// Inner rect of the help popup.
    help_area: Rect,
    mouse_drag: Option<MouseDrag>,
    detail_selection: Option<DetailSelection>,
    /// Characters dragged out of a single pane row; `y` and right-click copy exactly it.
    text_selection: Option<TextSelection>,
    /// While set, the timestamp column shows each line's offset from this one.
    elapsed_mark: Option<ElapsedMark>,
    /// Sidebar item index where a Shift range began.
    sidebar_anchor: Option<usize>,
    work: VecDeque<Job>,
    progress: Option<Progress>,
    live_sources: HashMap<String, LiveRuntime>,
    source_stamps: HashMap<String, SourceStamp>,
    dirty_sources: HashSet<String>,
    last_source_check: Instant,
    live_pick_rx: Option<Receiver<LivePickResult>>,
    /// Char index of the edit caret within the active input popup.
    input_cursor: usize,
    /// Panes of the restored session that want a merge. A merge interleaves entries by
    /// timestamp, so it cannot be rebuilt until those files have finished loading;
    /// `finish_load` drains this once they have.
    pending_merges: Vec<(usize, Vec<String>)>,
    /// The AI chat panel. Created lazily on first use so the worker thread and its runtime
    /// only exist for a session that asks for them.
    ai: Option<AiChat>,
    /// A pending AI schema-inference request, created lazily on first use (`i`).
    ai_schema: Option<SchemaInfer>,
    /// Panel visibility, sidebar width, pane weights, and focus mode.
    workspace: Workspace,
    /// The x of the sidebar/pane separator this frame, for the draggable border. `None` when
    /// the sidebar is hidden.
    separator_x: Option<u16>,
    /// The body area (between header and status) from the last frame, so `[`/`]` and a
    /// separator drag can resize from the current geometry.
    body_area: Rect,
    /// The outer rect of each pane this frame, for dragging the borders between panes.
    pane_layout: Vec<Rect>,
    /// Draggable panel top-borders (results, detail, chat) recorded this frame.
    panel_separators: Vec<PanelSeparator>,
    /// The timeline's column-to-time mapping this frame, for drag-to-filter.
    timeline_geom: Option<TimelineGeom>,
    /// Undo/redo stacks of state snapshots, and the human-readable action log.
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// The state captured on mouse-down, committed on mouse-up so a drag is one undo step.
    undo_pending: Option<Snapshot>,
    /// Set by undo/redo so the surrounding commit does not re-record their effect.
    did_undo_redo: bool,
    action_log: Vec<ActionEntry>,
    ui_config: UiConfig,
}

impl AppState {
    fn new(project: Project) -> Self {
        let ui_config = UiConfig::load();
        let mut app = Self {
            project,
            panes: Vec::new(),
            focused_pane: 0,
            focus: Focus::Pane,
            split_mode: SplitMode::Horizontal,
            sidebar_selected: 1,
            mode: Mode::Normal,
            status: String::new(),
            count: 0,
            g_pending: false,
            bookmark_nav_pending: None,
            results_selected: 0,
            results_scroll: 0,
            results_area: Rect::default(),
            sidebar_area: Rect::default(),
            pane_areas: Vec::new(),
            detail_area: Rect::default(),
            entry_detail_area: Rect::default(),
            pretty_print_area: Rect::default(),
            help_area: Rect::default(),
            mouse_drag: None,
            detail_selection: None,
            text_selection: None,
            elapsed_mark: None,
            sidebar_anchor: None,
            work: VecDeque::new(),
            progress: None,
            live_sources: HashMap::new(),
            source_stamps: HashMap::new(),
            dirty_sources: HashSet::new(),
            last_source_check: Instant::now(),
            live_pick_rx: None,
            input_cursor: 0,
            pending_merges: Vec::new(),
            ai: None,
            ai_schema: None,
            workspace: Workspace::default(),
            separator_x: None,
            body_area: Rect::default(),
            pane_layout: Vec::new(),
            panel_separators: Vec::new(),
            timeline_geom: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_pending: None,
            did_undo_redo: false,
            action_log: Vec::new(),
            ui_config,
        };
        app.restore_session();
        app
    }

    fn theme(&self) -> Theme {
        Theme::new(self.ui_config.theme)
    }

    // ---- session ---------------------------------------------------------------

    /// Rebuild the panes recorded at the last quit. Falls back to a single pane on the
    /// first file, which is what a fresh project gets.
    fn restore_session(&mut self) {
        let session = self.project.session.clone().unwrap_or_default();
        self.split_mode = match session.split_mode.as_str() {
            "vertical" => SplitMode::Vertical,
            _ => SplitMode::Horizontal,
        };
        self.workspace = Workspace {
            sidebar_width: session.sidebar_width,
            results_height: session.results_height,
            detail_height: session.detail_height,
            chat_height: session.chat_height,
            pane_weights: session.pane_weights.clone(),
            show_sidebar: !session.hide_sidebar,
            show_detail: !session.hide_detail,
            show_chat: !session.hide_chat,
            show_results: !session.hide_results,
            focus_mode: session.focus_mode,
            timeline_field: session.timeline_field.clone(),
        };

        for pane_session in &session.panes {
            // A file recorded here may have been deleted from disk since.
            let present: Vec<String> = pane_session
                .file_ids
                .iter()
                .filter(|id| self.project.get_file(id).is_some())
                .cloned()
                .collect();
            let Some(first) = present.first() else {
                continue;
            };
            let file_index = self.project.file_index(first).expect("just checked");

            let leaf_id = format!("L{}", self.panes.len() + 1);
            let mut view = build_view(
                leaf_id,
                &self.project.files[file_index],
                &self.project.filters,
            );
            view.context = pane_session.context;
            view.query_text = pane_session.query.clone();
            view.query =
                (!pane_session.query.is_empty()).then(|| compile_query(&pane_session.query));

            let pane = self.panes.len();
            self.panes.push(PaneState { view });
            if present.len() > 1 {
                self.pending_merges.push((pane, present));
            }
        }

        if self.panes.is_empty() {
            if let Some(file) = self.project.files.first() {
                self.panes.push(PaneState {
                    view: build_view("L1", file, &self.project.filters),
                });
            }
            self.pending_merges.clear();
            return;
        }
        self.focused_pane = session.focused_pane.min(self.panes.len() - 1);
    }

    /// Snapshot the panes into the project so the next `save` records them.
    fn capture_session(&mut self) {
        let panes = self
            .panes
            .iter()
            .map(|pane| {
                let file_ids = match self.project.get_file(&pane.view.file_id) {
                    Some(file) if file.is_merged() => file.merged_from.clone(),
                    Some(file) => vec![file.file_id.clone()],
                    None => Vec::new(),
                };
                PaneSession {
                    file_ids,
                    query: pane.view.query_text.clone(),
                    context: pane.view.context,
                }
            })
            .filter(|pane| !pane.file_ids.is_empty())
            .collect();

        self.project.session = Some(Session {
            panes,
            focused_pane: self.focused_pane,
            split_mode: match self.split_mode {
                SplitMode::Horizontal => "horizontal".to_string(),
                SplitMode::Vertical => "vertical".to_string(),
            },
            sidebar_width: self.workspace.sidebar_width,
            results_height: self.workspace.results_height,
            detail_height: self.workspace.detail_height,
            chat_height: self.workspace.chat_height,
            pane_weights: self.workspace.pane_weights.clone(),
            hide_sidebar: !self.workspace.show_sidebar,
            hide_detail: !self.workspace.show_detail,
            hide_chat: !self.workspace.show_chat,
            hide_results: !self.workspace.show_results,
            focus_mode: self.workspace.focus_mode,
            timeline_field: self.workspace.timeline_field.clone(),
        });
    }

    // ---- Undo / redo ------------------------------------------------------------------

    /// Snapshot the state undo/redo tracks: filters, saved searches, and the pane/layout
    /// session descriptor.
    fn undo_snapshot(&mut self) -> Snapshot {
        self.capture_session();
        Snapshot {
            filters: self.project.filters.clone(),
            saved_searches: self.project.saved_searches.clone(),
            bookmarks: self.project.bookmarks.clone(),
            session: self.project.session.clone().unwrap_or_default(),
        }
    }

    /// After an action, record `before` on the undo stack if the state actually changed, and
    /// log it. A no-op right after an undo/redo, which manage the stacks themselves.
    fn commit_change(&mut self, before: Snapshot, actor: Actor) {
        if self.did_undo_redo {
            self.did_undo_redo = false;
            return;
        }
        let after = self.undo_snapshot();
        if after == before {
            return;
        }
        let description = describe_change(&before, &after);
        if self.undo_stack.len() >= HISTORY_LIMIT {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(before);
        self.redo_stack.clear();
        self.action_log.push(ActionEntry {
            time: chrono::Local::now().format("%H:%M").to_string(),
            actor,
            description,
        });
        if self.action_log.len() > HISTORY_LIMIT {
            self.action_log.remove(0);
        }
    }

    fn restore_snapshot(&mut self, snapshot: Snapshot) {
        self.project.filters = snapshot.filters;
        self.project.saved_searches = snapshot.saved_searches;
        self.project.bookmarks = snapshot.bookmarks;
        self.project.session = Some(snapshot.session);
        self.panes.clear();
        self.pending_merges.clear();
        self.restore_session();
        self.apply_pending_merges();
        self.requeue_all_panes();
        self.autosave_project();
    }

    fn undo(&mut self) {
        let Some(previous) = self.undo_stack.pop() else {
            self.status = "nothing to undo".to_string();
            return;
        };
        let current = self.undo_snapshot();
        self.redo_stack.push(current);
        self.restore_snapshot(previous);
        self.did_undo_redo = true;
        self.status = "undid last action".to_string();
    }

    fn redo(&mut self) {
        let Some(next) = self.redo_stack.pop() else {
            self.status = "nothing to redo".to_string();
            return;
        };
        let current = self.undo_snapshot();
        self.undo_stack.push(current);
        self.restore_snapshot(next);
        self.did_undo_redo = true;
        self.status = "redid action".to_string();
    }

    /// Rebuild any restored merged pane whose files have all arrived.
    fn apply_pending_merges(&mut self) {
        if self.pending_merges.is_empty() {
            return;
        }
        let ready = |app: &Self, ids: &[String]| {
            ids.iter().all(|id| {
                app.project
                    .get_file(id)
                    .map(|file| file.loaded || !file.error.is_empty())
                    .unwrap_or(false)
            })
        };

        let mut still_pending = Vec::new();
        for (pane, ids) in std::mem::take(&mut self.pending_merges) {
            if pane >= self.panes.len() {
                continue;
            }
            if !ready(self, &ids) {
                still_pending.push((pane, ids));
                continue;
            }
            // `show_files_in_focused` acts on the focused pane; borrow it briefly.
            let (focus, focused) = (self.focus, self.focused_pane);
            self.focused_pane = pane;
            let query = self.panes[pane].view.query_text.clone();
            let context = self.panes[pane].view.context;
            self.show_files_in_focused(&ids);
            if let Some(state) = self.panes.get_mut(pane) {
                state.view.context = context;
                state.view.query_text = query.clone();
                state.view.query = (!query.is_empty()).then(|| compile_query(&query));
            }
            self.focused_pane = focused;
            self.focus = focus;
            self.queue_recompute(pane, After::Nothing);
        }
        self.pending_merges = still_pending;
        self.status.clear();
    }

    // ---- background work ------------------------------------------------------

    fn work_pending(&self) -> bool {
        !self.work.is_empty()
    }

    /// True while an AI chat turn is waiting on the model or running its tools.
    fn ai_busy(&self) -> bool {
        self.ai.as_ref().map(|ai| ai.pending).unwrap_or(false)
    }

    fn schema_pending(&self) -> bool {
        self.ai_schema
            .as_ref()
            .map(|schema| schema.pending.is_some())
            .unwrap_or(false)
    }

    fn live_active(&self) -> bool {
        !self.live_sources.is_empty()
    }

    fn drain_live_pick_events(&mut self) {
        let Some(rx) = &self.live_pick_rx else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.live_pick_rx = None;
        let Mode::LiveQuickPick(mut picker) = self.mode.clone() else {
            return;
        };
        if picker.target != result.target {
            return;
        }
        picker.loading = false;
        picker.options = result.options;
        picker.selected = 0;
        picker.message = result
            .error
            .unwrap_or_else(|| format!("select {}", live_quick_pick_label(picker.target)));
        self.mode = Mode::LiveQuickPick(picker);
    }

    fn drain_live_events(&mut self) {
        let ids: Vec<String> = self.live_sources.keys().cloned().collect();
        let mut dirty = Vec::new();
        for file_id in ids {
            let events: Vec<LiveEvent> = self
                .live_sources
                .get(&file_id)
                .map(|runtime| runtime.rx.try_iter().collect())
                .unwrap_or_default();
            let mut closed_pipes = 0usize;
            let mut raw_lines: Vec<(String, &'static str)> = Vec::new();
            for event in events {
                match event {
                    LiveEvent::Line { text, prefix } => raw_lines.push((text, prefix)),
                    LiveEvent::Closed => closed_pipes += 1,
                }
            }
            if let Some(runtime) = self.live_sources.get_mut(&file_id) {
                runtime.open_pipes = runtime.open_pipes.saturating_sub(closed_pipes);
            }
            if raw_lines.is_empty() {
                continue;
            }

            let Some((path, extractor)) = self
                .project
                .get_file(&file_id)
                .map(|file| (file.path.clone(), file.extractor.clone()))
            else {
                self.live_sources.remove(&file_id);
                continue;
            };

            let lines: Vec<String> = raw_lines
                .into_iter()
                .map(|(text, prefix)| apply_live_prefix(text, prefix, extractor.as_ref()))
                .collect();

            let mut append_error = None;
            if let Some(parent) = path.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    append_error = Some(error.to_string());
                }
            }
            let mut spool = if append_error.is_none() {
                match fs::OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(handle) => Some(handle),
                    Err(error) => {
                        append_error = Some(error.to_string());
                        None
                    }
                }
            } else {
                None
            };

            let Some(runtime) = self.live_sources.get_mut(&file_id) else {
                continue;
            };
            for line in &lines {
                if let Some(handle) = &mut spool {
                    if let Err(error) = writeln!(handle, "{line}") {
                        append_error = Some(error.to_string());
                        spool = None;
                    }
                }
                runtime.builder.push_line(line, extractor.as_ref());
            }
            let base_entries = runtime.base_entries;
            let snapshot = runtime.builder.snapshot();

            if let Some(file) = self.project.get_file_mut(&file_id) {
                file.entries.truncate(base_entries);
                file.entries.extend(snapshot.into_iter().map(|mut entry| {
                    entry.index += base_entries;
                    entry
                }));
                file.loaded = true;
                match append_error {
                    Some(error) => file.error = format!("live spool write error: {error}"),
                    None if file.error.starts_with("live spool write error:") => {
                        file.error.clear();
                    }
                    None => {}
                }
            }
            self.record_source_stamp(&file_id);
            dirty.push(file_id);
        }

        for file_id in dirty {
            self.refresh_merged_views_for_source(&file_id);
            for pane in self.panes_using_file(&file_id) {
                self.queue_recompute(pane, After::Nothing);
            }
        }
        self.collect_finished_live_sources();
    }

    fn collect_finished_live_sources(&mut self) {
        let mut finished = Vec::new();
        for (file_id, runtime) in &mut self.live_sources {
            if runtime.exit_status.is_none() {
                if let Ok(Some(status)) = runtime.child.try_wait() {
                    runtime.exit_status = Some(status);
                }
            }
            if runtime.open_pipes == 0 {
                if let Some(status) = runtime.exit_status.take() {
                    finished.push((file_id.clone(), status));
                }
            }
        }

        for (file_id, status) in finished {
            self.live_sources.remove(&file_id);
            if let Some(file) = self.project.get_file_mut(&file_id) {
                if status.success() {
                    if file.error.starts_with("live command exited") {
                        file.error.clear();
                    }
                    self.status = format!("live source stopped: {}", file.display_name);
                } else {
                    file.error = format!("live command exited with {status}");
                    self.status = format!("live source stopped with {status}");
                }
            }
        }
    }

    fn start_ready_live_sources(&mut self) {
        let ids: Vec<String> = self
            .project
            .files
            .iter()
            .filter(|file| {
                file.is_live()
                    && file.loaded
                    && file.error.is_empty()
                    && !self.live_sources.contains_key(&file.file_id)
            })
            .map(|file| file.file_id.clone())
            .collect();
        for file_id in ids {
            self.start_live_source(&file_id);
        }
    }

    fn start_live_source(&mut self, file_id: &str) {
        if self.live_sources.contains_key(file_id) {
            return;
        }
        let Some((config, display_name, path, entries_len)) =
            self.project.get_file(file_id).and_then(|file| {
                file.live.clone().map(|config| {
                    (
                        config,
                        file.display_name.clone(),
                        file.path.clone(),
                        file.entries.len(),
                    )
                })
            })
        else {
            return;
        };
        if let Err(error) = config.validate() {
            if let Some(file) = self.project.get_file_mut(file_id) {
                file.error = format!("live source not started: {error}");
            }
            return;
        }
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                if let Some(file) = self.project.get_file_mut(file_id) {
                    file.error = format!("live spool create failed: {error}");
                }
                return;
            }
        }

        let (program, args) = config.command_parts();
        let mut command = ProcessCommand::new(&program);
        command
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(file) = self.project.get_file_mut(file_id) {
                    file.error = format!("live command failed: {error}");
                }
                self.status = format!("live source failed: {}", config.command_preview());
                return;
            }
        };

        let (tx, rx) = mpsc::channel();
        let mut open_pipes = 0usize;
        if let Some(stdout) = child.stdout.take() {
            open_pipes += 1;
            spawn_live_pipe_reader(stdout, tx.clone(), "");
        }
        if let Some(stderr) = child.stderr.take() {
            open_pipes += 1;
            spawn_live_pipe_reader(stderr, tx, "[logscout stderr] ");
        }

        self.live_sources.insert(
            file_id.to_string(),
            LiveRuntime {
                child,
                rx,
                builder: EntryBuilder::new(),
                base_entries: entries_len,
                open_pipes,
                exit_status: None,
            },
        );
        if let Some(file) = self.project.get_file_mut(file_id) {
            file.loaded = true;
            file.error.clear();
        }
        self.status = format!("live source started: {display_name}");
    }

    fn panes_using_file(&self, file_id: &str) -> Vec<usize> {
        self.panes
            .iter()
            .enumerate()
            .filter_map(|(pane, state)| {
                if state.view.file_id == file_id {
                    return Some(pane);
                }
                self.project
                    .get_file(&state.view.file_id)
                    .filter(|file| file.merged_from.iter().any(|id| id == file_id))
                    .map(|_| pane)
            })
            .collect()
    }

    fn refresh_merged_views_for_source(&mut self, source_id: &str) {
        let merged: Vec<(usize, String, Vec<String>)> = self
            .project
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.merged_from.iter().any(|id| id == source_id))
            .map(|(index, file)| (index, file.file_id.clone(), file.merged_from.clone()))
            .collect();

        for (index, merged_id, source_ids) in merged {
            let refreshed = {
                let sources: Vec<&LogFileModel> = source_ids
                    .iter()
                    .filter_map(|id| self.project.get_file(id))
                    .collect();
                if sources.len() != source_ids.len() {
                    continue;
                }
                merge_files(merged_id, &sources)
            };
            if let Some(slot) = self.project.files.get_mut(index) {
                *slot = refreshed;
            }
        }
    }

    fn source_stamp(path: &Path) -> Option<SourceStamp> {
        let metadata = fs::metadata(path).ok()?;
        Some(SourceStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn record_source_stamp(&mut self, file_id: &str) {
        let Some(path) = self.project.get_file(file_id).map(|file| file.path.clone()) else {
            self.source_stamps.remove(file_id);
            return;
        };
        match Self::source_stamp(&path) {
            Some(stamp) => {
                self.source_stamps.insert(file_id.to_string(), stamp);
            }
            None => {
                self.source_stamps.remove(file_id);
            }
        }
    }

    fn check_source_modifications(&mut self) {
        if Instant::now().duration_since(self.last_source_check) < SOURCE_POLL_INTERVAL {
            return;
        }
        self.last_source_check = Instant::now();

        let sources: Vec<(String, String, PathBuf, bool)> = self
            .project
            .files
            .iter()
            .filter(|file| !file.is_merged() && file.loaded)
            .map(|file| {
                (
                    file.file_id.clone(),
                    file.display_name.clone(),
                    file.path.clone(),
                    file.is_live(),
                )
            })
            .collect();

        let mut changed = Vec::new();
        for (file_id, display_name, path, is_live) in sources {
            if self.live_sources.contains_key(&file_id) && is_live {
                // Active live sources update the view through the stream path; their spool
                // mtime changes are recorded there so they do not produce self-notifications.
                continue;
            }
            let Some(current) = Self::source_stamp(&path) else {
                continue;
            };
            match self.source_stamps.get(&file_id).copied() {
                Some(previous) if previous != current => {
                    if self.dirty_sources.insert(file_id) {
                        changed.push(display_name);
                    }
                }
                Some(_) => {}
                None => {
                    self.source_stamps.insert(file_id, current);
                }
            }
        }

        if changed.is_empty() {
            return;
        }
        self.status = match changed.as_slice() {
            [one] => format!("{one} changed on disk; press r to refresh"),
            _ => format!(
                "{} log sources changed on disk; press r to refresh",
                changed.len()
            ),
        };
    }

    fn refresh_dirty_sources(&mut self) {
        let ids: Vec<String> = if self.dirty_sources.is_empty() {
            self.schema_target().into_iter().collect()
        } else {
            self.dirty_sources.iter().cloned().collect()
        };
        if ids.is_empty() {
            self.status = "no source to refresh".to_string();
            return;
        }

        let mut queued = 0usize;
        for file_id in ids {
            let Some(file) = self.project.get_file(&file_id) else {
                self.dirty_sources.remove(&file_id);
                continue;
            };
            if file.is_merged() {
                continue;
            }
            self.live_sources.remove(&file_id);
            if let Some(file) = self.project.get_file_mut(&file_id) {
                file.loaded = false;
                file.entries.clear();
                file.error.clear();
            }
            self.dirty_sources.remove(&file_id);
            self.queue_load(&file_id);
            queued += 1;
        }

        self.status = match queued {
            0 => "no changed source to refresh".to_string(),
            1 => "refreshing log source".to_string(),
            n => format!("refreshing {n} log sources"),
        };
    }

    fn cancel_work(&mut self) {
        self.work.clear();
        self.progress = None;
        self.status = "cancelled".to_string();
    }

    /// Run queued work to completion without rendering. Tests use this where the real
    /// loop would instead slice the work across frames.
    #[cfg(test)]
    fn finish_work(&mut self) {
        while self.work_pending() {
            self.step_work(Duration::from_secs(3600));
        }
    }

    /// Advance the head job for at most `budget`, then return so a frame can be drawn.
    fn step_work(&mut self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !self.work.is_empty() {
            let is_load = matches!(self.work.front(), Some(Job::Load(_)));
            let finished = if is_load {
                self.step_load(deadline)
            } else {
                self.step_recompute(deadline)
            };
            // Not finished means the budget ran out mid-job; resume on the next frame.
            if !finished || Instant::now() >= deadline {
                break;
            }
        }
        if self.work.is_empty() {
            self.progress = None;
        }
    }

    /// Returns true when the head job completed and was removed from the queue.
    fn step_load(&mut self, deadline: Instant) -> bool {
        let mut outcome = None;
        if let Some(Job::Load(job)) = self.work.front_mut() {
            'chunks: loop {
                for _ in 0..LOAD_CHUNK_LINES {
                    match job.lines.next() {
                        Some(Ok(line)) => {
                            job.bytes += line.len() as u64 + 1;
                            job.builder.push_line(&line, job.extractor.as_ref());
                        }
                        Some(Err(error)) => {
                            job.error = Some(format!("read error: {error}"));
                            outcome = Some(true);
                            break 'chunks;
                        }
                        None => {
                            outcome = Some(true);
                            break 'chunks;
                        }
                    }
                }
                if Instant::now() >= deadline {
                    outcome = Some(false);
                    break 'chunks;
                }
            }

            self.progress = Some(Progress {
                label: format!("Loading {}", job.display_name),
                done: job.bytes,
                total: job.total.max(1),
            });
        }

        match outcome {
            Some(true) => self.finish_load(),
            Some(false) => false,
            // Head is not a load job; drop it rather than spin.
            None => {
                self.work.pop_front();
                true
            }
        }
    }

    fn finish_load(&mut self) -> bool {
        let Some(Job::Load(job)) = self.work.pop_front() else {
            return true;
        };
        let file_id = job.file_id;
        let error = job.error;
        let entries = job.builder.finish();

        if let Some(file) = self.project.get_file_mut(&file_id) {
            match error {
                Some(error) => file.error = error,
                None => {
                    file.entries = entries;
                    file.loaded = true;
                    file.error.clear();
                }
            }
        }
        if self
            .project
            .get_file(&file_id)
            .map(|file| file.error.is_empty())
            .unwrap_or(false)
        {
            self.record_source_stamp(&file_id);
            self.dirty_sources.remove(&file_id);
            self.refresh_merged_views_for_source(&file_id);
        }

        // Views built before the entries arrived hold an empty, stale result.
        for pane in self.panes_using_file(&file_id) {
            self.queue_recompute(pane, After::Nothing);
        }
        // A restored merged pane has been waiting for exactly this.
        self.apply_pending_merges();
        let should_start_live = self
            .project
            .get_file(&file_id)
            .map(|file| file.is_live() && file.error.is_empty())
            .unwrap_or(false);
        if should_start_live {
            self.start_live_source(&file_id);
        }
        true
    }

    fn step_recompute(&mut self, deadline: Instant) -> bool {
        let Some(Job::Recompute(head)) = self.work.front() else {
            self.work.pop_front();
            return true;
        };
        let pane = head.pane;
        let Some(file_index) = self
            .panes
            .get(pane)
            .and_then(|state| self.project.file_index(&state.view.file_id))
        else {
            self.work.pop_front();
            return true;
        };

        loop {
            let file = &self.project.files[file_index];
            let Some(Job::Recompute(job)) = self.work.front_mut() else {
                self.work.pop_front();
                return true;
            };

            // `Some(result)` means the scan is complete for this pane.
            let mut result = None;
            match &mut job.stage {
                Stage::Filter {
                    filters,
                    next,
                    base,
                } => {
                    let total = file.entries.len();
                    let done = if filters.has_enabled_rules() {
                        let end = (*next + SCAN_CHUNK_ENTRIES).min(total);
                        let prepared = filters.prepare();
                        for index in *next..end {
                            if prepared.visible(file, &file.entries[index]) {
                                base.push(index);
                            }
                        }
                        *next = end;
                        self.progress = Some(Progress {
                            label: "Filtering".to_string(),
                            done: end as u64,
                            total: total.max(1) as u64,
                        });
                        *next >= total
                    } else {
                        true
                    };

                    if done {
                        let computed = if filters.has_enabled_rules() {
                            VisibleIndices::List(std::mem::take(base))
                        } else {
                            VisibleIndices::Range(total)
                        };
                        let view = &mut self.panes[pane].view;
                        view.install_base(computed.clone(), file);
                        job.stage = Stage::Search {
                            base: computed,
                            query: view.query.clone(),
                            context: view.context,
                            next: 0,
                            positions: Vec::new(),
                            matches: HashSet::new(),
                        };
                    }
                }
                Stage::Search {
                    base,
                    query,
                    context,
                    next,
                    positions,
                    matches,
                } => match query.as_ref() {
                    None => result = Some((base.clone(), HashSet::new())),
                    Some(query) => {
                        let total = base.len();
                        let end = (*next + SCAN_CHUNK_ENTRIES).min(total);
                        for position in *next..end {
                            let Some(global_index) = base.get(position) else {
                                continue;
                            };
                            let Some(entry) = file.entries.get(global_index) else {
                                continue;
                            };
                            if query.matches(file, entry) {
                                positions.push(position);
                                matches.insert(global_index);
                            }
                        }
                        *next = end;
                        self.progress = Some(Progress {
                            label: "Searching".to_string(),
                            done: end as u64,
                            total: total.max(1) as u64,
                        });
                        if *next >= total {
                            result = Some(apply_context(
                                base,
                                positions,
                                std::mem::take(matches),
                                *context,
                            ));
                        }
                    }
                },
            }

            if let Some(result) = result {
                let after = job.after;
                self.panes[pane].view.apply(result);
                self.work.pop_front();
                self.sync_selected_result_to_cursor();
                if after == After::GotoFirstMatch {
                    self.goto_first_match();
                }
                return true;
            }

            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Attach extractors, queue a load for every unread file, and recompute each pane.
    fn queue_initial_loads(&mut self) {
        self.project.redetect_mismatched_schemas();
        let names: Vec<String> = self
            .project
            .files
            .iter()
            .map(|file| file.extractor_name.clone())
            .collect();
        let extractors: Vec<Extractor> = names
            .iter()
            .map(|name| self.project.get_extractor(name))
            .collect();
        for (index, extractor) in extractors.into_iter().enumerate() {
            self.project.files[index].refresh_extractor(Some(extractor));
        }

        let ids: Vec<String> = self
            .project
            .files
            .iter()
            .filter(|file| !file.loaded && file.error.is_empty())
            .map(|file| file.file_id.clone())
            .collect();
        for file_id in ids {
            self.queue_load(&file_id);
        }
        for pane in 0..self.panes.len() {
            self.queue_recompute(pane, After::Nothing);
        }
        self.start_ready_live_sources();
        let loaded_ids: Vec<String> = self
            .project
            .files
            .iter()
            .filter(|file| !file.is_merged() && file.loaded)
            .map(|file| file.file_id.clone())
            .collect();
        for file_id in loaded_ids {
            self.record_source_stamp(&file_id);
        }
    }

    fn queue_load(&mut self, file_id: &str) {
        let Some(file) = self.project.get_file(file_id) else {
            return;
        };
        let path = file.path.clone();
        let display_name = file.display_name.clone();
        let extractor = file.extractor.clone();
        let total = parser::file_size(&path);
        let is_live = file.is_live();

        if is_live && !path.exists() {
            if let Some(file) = self.project.get_file_mut(file_id) {
                file.loaded = true;
                file.error.clear();
            }
            self.start_live_source(file_id);
            return;
        }

        match std::fs::File::open(&path) {
            Ok(handle) => self.work.push_back(Job::Load(Box::new(LoadJob {
                file_id: file_id.to_string(),
                display_name,
                lines: std::io::BufReader::new(handle).lines(),
                builder: EntryBuilder::new(),
                extractor,
                bytes: 0,
                total,
                error: None,
            }))),
            Err(error) => {
                if is_live {
                    if let Some(file) = self.project.get_file_mut(file_id) {
                        file.loaded = true;
                        file.error.clear();
                    }
                    self.start_live_source(file_id);
                } else if let Some(file) = self.project.get_file_mut(file_id) {
                    file.error = format!("read error: {error}");
                }
            }
        }
    }

    /// Replace any queued recompute for this pane; only the newest request matters.
    fn queue_recompute(&mut self, pane: usize, after: After) {
        self.work
            .retain(|job| !matches!(job, Job::Recompute(queued) if queued.pane == pane));

        let Some(view) = self.panes.get(pane).map(|state| &state.view) else {
            return;
        };
        let Some(file_index) = self.project.file_index(&view.file_id) else {
            return;
        };
        let file = &self.project.files[file_index];

        // Reuse the cached filter pass when the filters have not changed. This is what
        // makes a search after filtering cheap: it walks only the surviving lines.
        let stage = if view.base_is_current(file) {
            Stage::Search {
                base: view.base().clone(),
                query: view.query.clone(),
                context: view.context,
                next: 0,
                positions: Vec::new(),
                matches: HashSet::new(),
            }
        } else {
            Stage::Filter {
                filters: view.filters.clone(),
                next: 0,
                base: Vec::new(),
            }
        };
        self.work
            .push_back(Job::Recompute(RecomputeJob { pane, after, stage }));
    }

    /// Pane indices shift when panes or files are removed, invalidating queued jobs.
    fn requeue_all_panes(&mut self) {
        self.work.retain(|job| !matches!(job, Job::Recompute(_)));
        for pane in 0..self.panes.len() {
            self.queue_recompute(pane, After::Nothing);
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let root = frame.area();
        let focus = self.workspace.focus_mode;
        // Focus mode hides the results panel too; otherwise it shows when a search is running
        // and the user has not toggled it off.
        let show_results = self.search_results_visible() && self.workspace.show_results && !focus;
        self.panel_separators.clear();
        let result_height = self
            .workspace
            .results_height
            .unwrap_or_else(|| root.height.saturating_sub(4).clamp(3, 8))
            .clamp(3, root.height.saturating_sub(6));
        let constraints = if show_results {
            vec![
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(result_height),
                Constraint::Length(1),
            ]
        } else {
            vec![
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ]
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(root);

        self.draw_header(frame, rows[0]);
        self.body_area = rows[1];

        // The left column (sidebar + detail + chat) is hidden in focus mode or when toggled
        // off, giving the panes the whole width.
        let pane_area = if self.workspace.show_sidebar && !focus {
            let sidebar_cols = self.effective_sidebar_width(rows[1].width);
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(sidebar_cols), Constraint::Min(1)])
                .split(rows[1]);
            self.separator_x = Some(body[1].x);
            self.draw_left_column(frame, body[0]);
            body[1]
        } else {
            self.separator_x = None;
            self.sidebar_area = Rect::default();
            self.detail_area = Rect::default();
            rows[1]
        };

        // The timeline histogram, when on, sits above the panes.
        self.timeline_geom = None;
        let panes_area = match self.workspace.timeline_field.clone() {
            Some(field) if !focus => match self.compute_timeline(&field, pane_area.width) {
                Some(data) => {
                    let height =
                        (data.rows.len() as u16 + 3).min(pane_area.height.saturating_sub(4));
                    let split = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(height), Constraint::Min(4)])
                        .split(pane_area);
                    self.draw_timeline(frame, split[0], &field, &data);
                    split[1]
                }
                None => pane_area,
            },
            _ => pane_area,
        };
        self.draw_panes(frame, panes_area);

        if show_results {
            // The border above the results panel is draggable to resize its height.
            self.panel_separators.push(PanelSeparator {
                panel: PanelEdge::Results,
                top: rows[2].y,
                bottom: rows[3].y,
                x0: root.x,
                x1: root.x + root.width,
            });
            self.draw_search_results(frame, rows[2]);
            self.draw_status(frame, rows[3]);
        } else {
            self.results_area = Rect::default();
            self.draw_status(frame, rows[rows.len() - 1]);
        }
        self.draw_mode(frame, root);
        self.draw_progress(frame, root);
    }

    /// The sidebar, detail, and chat panels stacked in the left column.
    fn draw_left_column(&mut self, frame: &mut Frame, area: Rect) {
        // The chat panel, when open and shown, takes the bottom, below the detail panel. It
        // grows when focused so there is room to read and type.
        let chat_height = if self.workspace.show_chat {
            self.chat_panel_height(area.height)
        } else {
            0
        };
        let above_chat = if chat_height > 0 {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(chat_height)])
                .split(area);
            // The chat panel's top border is draggable.
            self.panel_separators.push(PanelSeparator {
                panel: PanelEdge::Chat,
                top: split[1].y,
                bottom: split[1].y + split[1].height,
                x0: area.x,
                x1: area.x + area.width,
            });
            self.draw_chat(frame, split[1]);
            split[0]
        } else {
            area
        };

        let detail_height = if !self.workspace.show_detail {
            0
        } else {
            match self.workspace.detail_height {
                Some(rows) => rows.min(above_chat.height.saturating_sub(4)),
                None => detail_panel_height(above_chat.height),
            }
        };
        if detail_height > 0 {
            let column = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(detail_height)])
                .split(above_chat);
            // The detail panel's top border is draggable.
            self.panel_separators.push(PanelSeparator {
                panel: PanelEdge::Detail,
                top: column[1].y,
                bottom: column[1].y + column[1].height,
                x0: above_chat.x,
                x1: above_chat.x + above_chat.width,
            });
            self.draw_sidebar(frame, column[0]);
            self.draw_detail(frame, column[1]);
        } else {
            self.detail_area = Rect::default();
            self.draw_sidebar(frame, above_chat);
        }
    }

    /// The sidebar width to use: the override, clamped so neither the sidebar nor the panes
    /// starve, else the automatic width.
    fn effective_sidebar_width(&self, body_width: u16) -> u16 {
        match self.workspace.sidebar_width {
            Some(cols) => cols
                .max(MIN_SIDEBAR_WIDTH)
                .min(body_width.saturating_sub(MIN_PANE_WIDTH))
                .min(body_width),
            None => sidebar_width(body_width),
        }
    }

    // ---- Timeline histogram -----------------------------------------------------------

    /// Bucket the focused view's entries over their time span, counting each value of
    /// `field` per bucket. `None` when there is nothing to show (no timestamps).
    fn compute_timeline(&self, field: &str, panel_width: u16) -> Option<TimelineData> {
        let buckets = (panel_width as usize)
            .saturating_sub(TIMELINE_LABEL_WIDTH + 2)
            .max(1);
        let (file, view) = self.active_file_view()?;
        if view.visible.is_empty() {
            return None;
        }

        // Pass 1: the time span across the visible entries (capped).
        let mut min: Option<NaiveDateTime> = None;
        let mut max: Option<NaiveDateTime> = None;
        for global in view.visible.iter().take(PATTERN_PREVIEW_LIMIT) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            if let Some(ts) = file.timestamp(entry) {
                min = Some(min.map_or(ts, |m| m.min(ts)));
                max = Some(max.map_or(ts, |m| m.max(ts)));
            }
        }
        let (min, max) = (min?, max?);
        let span_ms = (max - min).num_milliseconds().max(0) as f64;

        // Pass 2: count each value of `field` into its bucket.
        let mut counts: std::collections::HashMap<String, Vec<u32>> =
            std::collections::HashMap::new();
        for global in view.visible.iter().take(PATTERN_PREVIEW_LIMIT) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            let Some(ts) = file.timestamp(entry) else {
                continue;
            };
            let value = file.with_field(entry, field, |text| text.trim().to_string());
            if value.is_empty() {
                continue;
            }
            let bucket = if span_ms <= 0.0 {
                0
            } else {
                let position = (ts - min).num_milliseconds() as f64 / span_ms;
                ((position * (buckets as f64 - 1.0)).round() as usize).min(buckets - 1)
            };
            counts.entry(value).or_insert_with(|| vec![0; buckets])[bucket] += 1;
        }
        if counts.is_empty() {
            return None;
        }

        // Keep the busiest values, most frequent first.
        let mut rows: Vec<(String, Vec<u32>)> = counts.into_iter().collect();
        rows.sort_by(|a, b| {
            b.1.iter()
                .sum::<u32>()
                .cmp(&a.1.iter().sum::<u32>())
                .then_with(|| a.0.cmp(&b.0))
        });
        rows.truncate(TIMELINE_MAX_ROWS);
        Some(TimelineData { min, max, rows })
    }

    fn draw_timeline(&mut self, frame: &mut Frame, area: Rect, field: &str, data: &TimelineData) {
        let level_field = field == "level" || field.ends_with("_level");
        let block = Block::default()
            .title(format!("Timeline · {field}   (b change · drag to filter)"))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let global_max = data
            .rows
            .iter()
            .flat_map(|(_, counts)| counts.iter())
            .copied()
            .max()
            .unwrap_or(0)
            .max(1) as f64;
        let buckets = data.rows.first().map(|(_, c)| c.len()).unwrap_or(0);

        for (index, (value, counts)) in data.rows.iter().enumerate() {
            if index as u16 >= inner.height.saturating_sub(1) {
                break;
            }
            let label: String = value.chars().take(TIMELINE_LABEL_WIDTH - 1).collect();
            let spark: String = counts
                .iter()
                .map(|&count| {
                    if count == 0 {
                        ' '
                    } else {
                        let level = ((count as f64 / global_max) * 7.0).round() as usize;
                        SPARK_BARS[level.min(7)]
                    }
                })
                .collect();
            let style = if level_field {
                level_style(value)
            } else {
                Style::default()
            };
            let line = format!("{label:<width$}{spark}", width = TIMELINE_LABEL_WIDTH);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(line, style))),
                Rect {
                    y: inner.y + index as u16,
                    height: 1,
                    ..inner
                },
            );
        }

        // A time axis under the bars: start on the left, end on the right.
        let axis_y = inner.y + inner.height - 1;
        let fmt = if data.max.date() == data.min.date() {
            "%H:%M:%S"
        } else {
            "%m-%d %H:%M"
        };
        let start = data.min.format(fmt).to_string();
        let end = data.max.format(fmt).to_string();
        let x0 = inner.x + TIMELINE_LABEL_WIDTH as u16;
        let axis_width = inner.width.saturating_sub(TIMELINE_LABEL_WIDTH as u16) as usize;
        let mut axis = format!("{start:<width$}", width = axis_width);
        if end.len() < axis_width {
            axis.replace_range(axis_width - end.len().., &end);
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                axis,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: x0,
                y: axis_y,
                width: inner.width.saturating_sub(TIMELINE_LABEL_WIDTH as u16),
                height: 1,
            },
        );

        self.timeline_geom = Some(TimelineGeom {
            x0,
            buckets,
            y0: inner.y,
            y1: axis_y.saturating_sub(1),
            min: data.min,
            max: data.max,
        });
    }

    /// `b`: cycle the timeline through off → by level → by module → by source → off.
    fn cycle_timeline(&mut self) {
        let choices = self.timeline_field_choices();
        let next = match &self.workspace.timeline_field {
            None => choices.first().cloned(),
            Some(current) => match choices.iter().position(|choice| choice == current) {
                Some(index) if index + 1 < choices.len() => Some(choices[index + 1].clone()),
                _ => None,
            },
        };
        self.workspace.timeline_field = next;
        self.status = match &self.workspace.timeline_field {
            Some(field) => format!("timeline by {field}"),
            None => "timeline off".to_string(),
        };
    }

    /// The fields the timeline can aggregate by, in cycle order: the level, a module field
    /// if the schema has one, and the source.
    fn timeline_field_choices(&self) -> Vec<String> {
        let fields = self.active_field_names();
        let mut choices = Vec::new();
        if let Some(level) = fields
            .iter()
            .find(|name| name.as_str() == "level" || name.ends_with("_level"))
        {
            choices.push(level.clone());
        }
        if let Some(module) = fields.iter().find(|name| name.contains("module")) {
            choices.push(module.clone());
        }
        choices.push("source".to_string());
        choices
    }

    /// Turn a drag (or click) across timeline buckets into the project time-range filter.
    fn apply_timeline_range(&mut self, col_a: u16, col_b: u16) {
        let Some(geom) = self.timeline_geom else {
            return;
        };
        if geom.buckets == 0 {
            return;
        }
        let to_bucket =
            |column: u16| (column.saturating_sub(geom.x0) as usize).min(geom.buckets - 1);
        let (lo, hi) = {
            let (a, b) = (to_bucket(col_a), to_bucket(col_b));
            (a.min(b), a.max(b))
        };
        let span_ms = (geom.max - geom.min).num_milliseconds().max(1);
        let per = span_ms / geom.buckets as i64;
        let start = geom.min + ChronoDuration::milliseconds(per * lo as i64);
        let end = geom.min + ChronoDuration::milliseconds((per * (hi as i64 + 1)).min(span_ms));
        self.apply_time_range(format!(
            "{}..{}",
            format_filter_datetime(start),
            format_filter_datetime(end)
        ));
    }

    /// Whether `(column, row)` is on the timeline's histogram bars.
    fn on_timeline(&self, column: u16, row: u16) -> bool {
        self.timeline_geom.is_some_and(|geom| {
            row >= geom.y0
                && row <= geom.y1
                && column >= geom.x0
                && column < geom.x0 + geom.buckets as u16
        })
    }

    fn draw_progress(&self, frame: &mut Frame, root: Rect) {
        let Some(progress) = &self.progress else {
            return;
        };
        let ratio = if progress.total == 0 {
            0.0
        } else {
            (progress.done as f64 / progress.total as f64).clamp(0.0, 1.0)
        };
        let percent = (ratio * 100.0).round() as u16;

        let area = centered_rect(60, 5, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(truncate_label(&progress.label, 56))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(self.theme().accent()))
                .ratio(ratio)
                .label(format!("{percent}%")),
            rows[0],
        );
        if rows[1].height > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled("Esc to cancel", self.theme().dim())),
                rows[1],
            );
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let text = format!(
            " Log Scouter  {}  q quit  / search  f filter  t time  H hide  y copy  ? help ",
            self.project.root.display()
        );
        frame.render_widget(Paragraph::new(text).style(self.theme().header()), area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let mut status = self.status.clone();
        if status.is_empty() {
            if let Some((file, view)) = self.active_file_view() {
                if let Some(entry) = view.current_entry(file) {
                    let loc = match (file.get_field(entry, "file"), file.get_field(entry, "line")) {
                        (file_name, line) if !file_name.is_empty() => {
                            format!(" {file_name}:{line}")
                        }
                        _ => String::new(),
                    };
                    let picked = match view.selection_count() {
                        0 => String::new(),
                        n => format!("  {n} selected"),
                    };
                    status = format!(
                        " row {}/{} filtered  line {}/{} total  {}  {}  {}{}{}",
                        view.cursor.saturating_add(1).min(view.visible.len()),
                        view.visible.len(),
                        entry.line_no,
                        total_line_count(file),
                        file.get_field(entry, "timestamp"),
                        file.get_field(entry, "module"),
                        file.get_field(entry, "level"),
                        loc,
                        picked
                    );
                }
            }
        }
        frame.render_widget(
            Paragraph::new(format!(" {status}")).style(self.theme().status()),
            area,
        );
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let items = self.sidebar_items();
        let theme = self.theme();
        if self.sidebar_selected >= items.len() {
            self.sidebar_selected = items.len().saturating_sub(1);
        }
        let label_width = area.width.saturating_sub(2) as usize;
        let rows: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let selected = self.focus == Focus::Sidebar && index == self.sidebar_selected;
                let (label, style) = match item {
                    SidebarItem::Section(label) => (label.clone(), theme.section()),
                    SidebarItem::SubSection(label) => (format!("  {label}"), theme.subsection()),
                    SidebarItem::File { label, .. } => (format!("  {label}"), Style::default()),
                    SidebarItem::Filter { label, .. } => (format!("    {label}"), theme.filter()),
                    SidebarItem::TimeFilter { label, .. } => {
                        (format!("    {label}"), theme.time_filter())
                    }
                    SidebarItem::Search { label, .. } => {
                        (format!("  {label}"), theme.saved_search())
                    }
                    SidebarItem::Bookmark { label, .. } => (format!("  {label}"), theme.bookmark()),
                    SidebarItem::Hint(label) => (format!("  {label}"), theme.dim()),
                };
                let style = if selected { theme.selected() } else { style };
                ListItem::new(Line::from(Span::styled(
                    truncate_label(&label, label_width),
                    style,
                )))
            })
            .collect();

        let border_style = if self.focus == Focus::Sidebar {
            Style::default().fg(theme.accent())
        } else {
            Style::default()
        };
        let block = Block::default()
            .title("Project")
            .borders(Borders::ALL)
            .border_style(border_style);
        self.sidebar_area = block.inner(area);
        frame.render_widget(List::new(rows).block(block), area);
    }

    /// Full, word-wrapped content of the selected entry. Log lines are routinely wider
    /// than the pane, so this is where you read one without scrolling sideways.
    /// Height of the chat panel in the left column, or 0 when it is closed. It gets more of
    /// the column while focused, so a conversation is comfortable to read.
    fn chat_panel_height(&self, column_height: u16) -> u16 {
        if self.ai.is_none() {
            return 0;
        }
        let want = self.workspace.chat_height.unwrap_or_else(|| {
            if self.focus == Focus::Chat {
                column_height.saturating_sub(6)
            } else {
                8
            }
        });
        // Always leave the sidebar at least a few rows.
        want.min(column_height.saturating_sub(6))
    }

    /// The chat transcript above a one-line input. Kept scrolled to the tail unless the
    /// user has paged up.
    fn draw_chat(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Chat;
        let pending = self.ai.as_ref().map(|ai| ai.pending).unwrap_or(false);
        // Show which model we talk to and whether it is usable, so the user can see the
        // provider is configured without sending a message. `no key` means neither the env
        // var, ai.json, nor a `/key` in this session has supplied one.
        let model = self
            .ai
            .as_ref()
            .map(|ai| {
                let status = if ai.resolved_key().is_some() {
                    String::new()
                } else {
                    " · no key".to_string()
                };
                format!(
                    "{} {}{status}",
                    ai.config.provider.label(),
                    ai.config.model()
                )
            })
            .unwrap_or_default();
        let hint = match (focused, pending) {
            (_, true) => "thinking…",
            (true, false) => "Enter send · Esc leave",
            (false, false) => "A to focus",
        };
        let title = format!("AI · {model} · {hint}");
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().fg(self.theme().accent())
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height < 2 {
            return;
        }

        let width = inner.width as usize;
        let transcript_rows = (inner.height as usize).saturating_sub(1);

        // Wrap every transcript line to the panel width, tagging each wrapped row with its
        // colour, then show the tail (respecting the user's scroll-back).
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(ai) = &self.ai {
            for entry in &ai.transcript {
                let (prefix, style) = match entry {
                    ChatLine::User(_) => (
                        "you ",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    ChatLine::Assistant(_) => ("ai  ", Style::default().fg(self.theme().accent())),
                    ChatLine::Action(_) => ("  » ", Style::default().fg(Color::Green)),
                    ChatLine::Error(_) => ("  ! ", Style::default().fg(Color::Red)),
                    ChatLine::Info(_) => ("  · ", self.theme().dim()),
                };
                let body = match entry {
                    ChatLine::User(text)
                    | ChatLine::Assistant(text)
                    | ChatLine::Action(text)
                    | ChatLine::Error(text)
                    | ChatLine::Info(text) => text,
                };
                for (index, chunk) in chunk_chars(&format!("{prefix}{body}"), width)
                    .into_iter()
                    .enumerate()
                {
                    // Continuation rows line up under the message, not the tag.
                    let text = if index == 0 {
                        chunk
                    } else {
                        format!("    {}", chunk.trim_start())
                    };
                    lines.push(Line::from(Span::styled(text, style)));
                }
            }
        }

        let scroll = self.ai.as_ref().map(|ai| ai.scroll).unwrap_or(0);
        let max_start = lines.len().saturating_sub(transcript_rows);
        let start = max_start.saturating_sub(scroll);
        let shown: Vec<Line<'static>> = lines
            .into_iter()
            .skip(start)
            .take(transcript_rows)
            .collect();

        let transcript_area = Rect {
            height: transcript_rows as u16,
            ..inner
        };
        frame.render_widget(Paragraph::new(Text::from(shown)), transcript_area);

        // The input line at the bottom.
        let input_area = Rect {
            y: inner.y + transcript_rows as u16,
            height: 1,
            ..inner
        };
        let input = self.ai.as_ref().map(|ai| ai.input.as_str()).unwrap_or("");
        // The input scrolls horizontally to keep the caret in view, so a long question is
        // always visible where you are typing it. Two columns go to the "> " prompt.
        let field_width = width.saturating_sub(2).max(1);
        let caret = self.input_cursor;
        let offset = caret.saturating_sub(field_width.saturating_sub(1));
        let shown: String = input.chars().skip(offset).take(field_width).collect();
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(format!("> {shown}")))),
            input_area,
        );
        if focused {
            let column = 2 + (caret - offset);
            frame.set_cursor_position((
                input_area.x + column.min(width.saturating_sub(1)) as u16,
                input_area.y,
            ));
        }
    }

    fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title("Detail")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme().accent()));
        let inner = block.inner(area);
        self.detail_area = inner;
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let lines = if self.focus == Focus::Sidebar {
            self.sidebar_detail_lines(inner.width as usize)
                .unwrap_or_else(|| self.active_entry_detail_lines(inner.width as usize))
        } else {
            self.active_entry_detail_lines(inner.width as usize)
        };
        let lines = self.apply_detail_selection(lines, DetailSurface::Inline);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn active_entry_detail_lines(&self, width: usize) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        match self.active_file_view() {
            None => vec![Line::from(Span::styled("no file open", dim))],
            Some((file, view)) => match view.current_entry(file) {
                None => vec![Line::from(Span::styled("no line selected", dim))],
                Some(entry) => detail_lines(file, entry, width),
            },
        }
    }

    fn full_entry_detail_lines(&self, width: usize) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        match self.entry_detail_target() {
            None => {
                if self.active_file_view().is_none() {
                    vec![Line::from(Span::styled("no file open", dim))]
                } else {
                    vec![Line::from(Span::styled("no line selected", dim))]
                }
            }
            Some((file, entry)) => full_detail_lines(file, entry, width),
        }
    }

    fn apply_detail_selection(
        &self,
        lines: Vec<Line<'static>>,
        surface: DetailSurface,
    ) -> Vec<Line<'static>> {
        let Some((lo, hi)) = self.detail_selection_range(surface) else {
            return lines;
        };
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if index >= lo && index <= hi {
                    selected_detail_line(line)
                } else {
                    line
                }
            })
            .collect()
    }

    fn detail_selection_range(&self, surface: DetailSurface) -> Option<(usize, usize)> {
        let selection = self.detail_selection?;
        if selection.surface != surface {
            return None;
        }
        Some((
            selection.anchor.min(selection.cursor),
            selection.anchor.max(selection.cursor),
        ))
    }

    fn sidebar_detail_lines(&self, width: usize) -> Option<Vec<Line<'static>>> {
        let items = self.sidebar_items();
        let item = items.get(self.sidebar_selected)?;
        match item {
            SidebarItem::File { file_id, .. } => self
                .project
                .get_file(file_id)
                .map(|file| file_detail_lines(file, width)),
            SidebarItem::Filter { index, .. } => self
                .project
                .filters
                .rules
                .get(*index)
                .map(|rule| filter_detail_lines(rule, width)),
            SidebarItem::TimeFilter { index, .. } => self
                .project
                .filters
                .rules
                .get(*index)
                .map(|rule| time_filter_detail_lines(rule, width)),
            SidebarItem::Search { text, .. } => Some(search_detail_lines(text, width)),
            SidebarItem::Bookmark { index, .. } => self
                .project
                .bookmarks
                .get(*index)
                .map(|bookmark| self.bookmark_detail_lines(bookmark, width)),
            SidebarItem::Section(label) | SidebarItem::SubSection(label) => {
                Some(label_detail_lines("section", label, width))
            }
            SidebarItem::Hint(label) => Some(label_detail_lines("hint", label.trim(), width)),
        }
    }

    fn draw_panes(&mut self, frame: &mut Frame, area: Rect) {
        if self.panes.is_empty() {
            frame.render_widget(
                Paragraph::new("No file open. Press a to add a log file.")
                    .block(Block::default().borders(Borders::ALL).title("empty")),
                area,
            );
            return;
        }

        let pane_count = self.panes.len();
        self.pane_areas.clear();
        self.pane_layout.clear();

        // Focus mode shows only the active pane, filling the area.
        if self.workspace.focus_mode {
            let index = self.focused_pane.min(pane_count - 1);
            self.draw_pane(frame, area, index);
            return;
        }

        let direction = match self.split_mode {
            SplitMode::Horizontal => Direction::Horizontal,
            SplitMode::Vertical => Direction::Vertical,
        };
        // Per-pane weights let one pane (short structured logs) yield space to another (long
        // stack traces). Equal weights reproduce the old even split.
        self.ensure_pane_weights();
        let weights = &self.workspace.pane_weights;
        let total: u32 = weights
            .iter()
            .map(|weight| *weight as u32)
            .sum::<u32>()
            .max(1);
        let constraints: Vec<Constraint> = weights
            .iter()
            .map(|weight| Constraint::Ratio(*weight as u32, total))
            .collect();
        let areas = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(area);
        self.pane_layout = areas.to_vec();

        for index in 0..pane_count {
            self.draw_pane(frame, areas[index], index);
        }
    }

    fn draw_pane(&mut self, frame: &mut Frame, area: Rect, pane_index: usize) {
        let theme = self.theme();
        let focused = self.focus == Focus::Pane && self.focused_pane == pane_index;
        let Some(file_id) = self
            .panes
            .get(pane_index)
            .map(|pane| pane.view.file_id.clone())
        else {
            return;
        };
        let Some(file_index) = self.project.file_index(&file_id) else {
            return;
        };

        let title = {
            let view = &self.panes[pane_index].view;
            let file = &self.project.files[file_index];
            let current_filtered = view.cursor.saturating_add(1).min(view.visible.len());
            let current_line = view
                .current_entry(file)
                .map(|entry| entry.line_no)
                .unwrap_or(0);
            let mut bits = vec![
                file.display_name.clone(),
                format!("row {current_filtered}/{} filtered", view.visible.len()),
                format!("line {current_line}/{} total", total_line_count(file)),
            ];
            let active_filters = view
                .filters
                .rules
                .iter()
                .filter(|rule| rule.enabled)
                .count();
            if active_filters > 0 {
                bits.push(format!("filters:{active_filters}"));
            }
            if view.query.as_ref().map(|q| !q.is_empty()).unwrap_or(false) {
                let ctx = if view.context > 0 {
                    format!(" +/-{}", view.context)
                } else {
                    String::new()
                };
                bits.push(format!("/{}{}", view.query_text, ctx));
            }
            // The timestamp column no longer shows timestamps; say so, and from where.
            if let Some(mark) = self
                .elapsed_mark
                .as_ref()
                .filter(|mark| mark.file_id == file_id)
            {
                bits.push(format!("elapsed from line {}", mark.line_no));
            }
            if !file.error.is_empty() {
                bits.push(format!("error: {}", file.error));
            }
            bits.join("  ")
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if focused {
                Style::default().fg(theme.accent())
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        if self.pane_areas.len() <= pane_index {
            self.pane_areas.resize(pane_index + 1, Rect::default());
        }
        self.pane_areas[pane_index] = inner;
        frame.render_widget(block, area);

        let search_footer = matches!(self.mode, Mode::Search(_)) && pane_index == self.focused_pane;
        let (list_area, search_area) = if search_footer && inner.height > 1 {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(inner);
            (split[0], Some(split[1]))
        } else {
            (inner, None)
        };

        let row_height = list_area.height as usize;
        if row_height == 0 {
            if let Some(area) = search_area {
                self.draw_inline_search(frame, area);
            }
            return;
        }

        self.clamp_scroll(pane_index, row_height);

        let elapsed_from = self.elapsed_from(&file_id);
        let file = &self.project.files[file_index];
        let view = &self.panes[pane_index].view;
        let mut rows = Vec::new();
        for visual_row in 0..row_height {
            let position = view.scroll_y + visual_row;
            let Some(global_index) = view.visible.get(position) else {
                rows.push(ListItem::new(""));
                continue;
            };
            let Some(entry) = file.entries.get(global_index) else {
                rows.push(ListItem::new(""));
                continue;
            };

            let at_cursor = position == view.cursor;
            let matched = view.match_set.contains(&global_index);
            let picked = view.is_selected(position, global_index);
            let bookmarked = self.is_bookmarked_entry(file, entry);
            let raw_line = row_line(
                file,
                entry,
                at_cursor,
                picked,
                matched,
                bookmarked,
                elapsed_from,
            );
            let line = crop(&raw_line, view.scroll_x, list_area.width as usize);
            let search_ranges = query_highlight_ranges(view.query.as_ref(), &raw_line);
            let style = match (picked, at_cursor) {
                (true, true) => theme.selected_strong(),
                (true, false) => theme.selected(),
                (false, true) => theme.selected(),
                (false, false) if matched => theme.matched(),
                (false, false) => level_style(&file.get_field(entry, "level")),
            };

            // A mouse-dragged substring highlights within the row, on top of whatever
            // style the row already has.
            let selected = self
                .text_selection
                .filter(|sel| sel.pane == pane_index && sel.position == position)
                .map(|sel| sel.range());
            rows.push(ListItem::new(match selected {
                Some((lo, hi)) => highlighted_row(&line, lo, hi, view.scroll_x, style),
                None if !search_ranges.is_empty() => highlighted_ranges(
                    &line,
                    &search_ranges,
                    view.scroll_x,
                    style,
                    theme.search_hit(),
                ),
                None => Line::from(Span::styled(line, style)),
            }));
        }

        frame.render_widget(List::new(rows), list_area);
        if let Some(area) = search_area {
            self.draw_inline_search(frame, area);
        }
    }

    fn draw_inline_search(&self, frame: &mut Frame, area: Rect) {
        let Mode::Search(text) = &self.mode else {
            return;
        };
        if area.width == 0 {
            return;
        }

        let full = format!("/{text}");
        let caret = self.input_cursor.min(text.chars().count()) + 1;
        let (shown, caret_column) = input_window(&full, caret, area.width as usize);
        let padded = pad_to_width(&shown, area.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                padded,
                self.theme().inline_input(),
            ))),
            area,
        );
        let x = area.x + caret_column.min(area.width.saturating_sub(1) as usize) as u16;
        frame.set_cursor_position((x, area.y));
    }

    /// The row exactly as `draw_pane` builds it, before horizontal cropping. Substring
    /// selection indexes into this string, so the two must not drift apart.
    fn pane_row_line(&self, pane: usize, position: usize) -> Option<String> {
        let view = &self.panes.get(pane)?.view;
        let file = self.project.get_file(&view.file_id)?;
        let global_index = view.visible.get(position)?;
        let entry = file.entries.get(global_index)?;
        Some(row_line(
            file,
            entry,
            position == view.cursor,
            view.is_selected(position, global_index),
            view.match_set.contains(&global_index),
            self.is_bookmarked_entry(file, entry),
            self.elapsed_from(&view.file_id),
        ))
    }

    /// The mark's timestamp, if it belongs to this file.
    fn elapsed_from(&self, file_id: &str) -> Option<NaiveDateTime> {
        self.elapsed_mark
            .as_ref()
            .filter(|mark| mark.file_id == file_id)
            .map(|mark| mark.at)
    }

    /// `T`: measure every line from the cursor line, or stop measuring.
    fn toggle_elapsed_mark(&mut self) {
        if self.elapsed_mark.is_some() {
            self.elapsed_mark = None;
            self.status = "elapsed time off".to_string();
            return;
        }

        let Some((file, view)) = self.active_file_view() else {
            return;
        };
        let Some(entry) = view.current_entry(file) else {
            self.status = "no line selected".to_string();
            return;
        };
        let Some(at) = file.timestamp(entry) else {
            self.status = "this line has no timestamp to measure from".to_string();
            return;
        };

        let line_no = entry.line_no;
        self.elapsed_mark = Some(ElapsedMark {
            file_id: view.file_id.clone(),
            at,
            line_no,
        });
        self.status = format!("elapsed time from line {line_no}");
    }

    fn draw_search_results(&mut self, frame: &mut Frame, area: Rect) {
        let theme = self.theme();
        let positions = self.active_result_positions();
        let count = positions.len();
        if self.results_selected >= count {
            self.results_selected = count.saturating_sub(1);
        }

        let title = self
            .active_view()
            .map(|view| {
                let selected = if count == 0 {
                    0
                } else {
                    self.results_selected + 1
                };
                format!(
                    "Matches {selected}/{count}  /{}/  click or Enter to jump",
                    view.query_text
                )
            })
            .unwrap_or_else(|| "Matches".to_string());

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if self.focus == Focus::Results {
                Style::default().fg(theme.accent())
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        self.results_area = inner;
        frame.render_widget(block, area);

        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        self.clamp_results_scroll(height, count);

        let Some(file_index) = self.active_file_index() else {
            return;
        };
        let file = &self.project.files[file_index];
        let view = &self.panes[self.focused_pane].view;

        let mut rows = Vec::with_capacity(height);
        for visual_row in 0..height {
            let result_index = self.results_scroll + visual_row;
            let Some(visible_pos) = positions.get(result_index).copied() else {
                rows.push(ListItem::new(""));
                continue;
            };
            let Some(global_index) = view.visible.get(visible_pos) else {
                rows.push(ListItem::new(""));
                continue;
            };
            let Some(entry) = file.entries.get(global_index) else {
                rows.push(ListItem::new(""));
                continue;
            };

            let selected = result_index == self.results_selected;
            let cursor = if selected { ">" } else { " " };
            let timestamp = pad(&file.get_field(entry, "timestamp"), 23);
            let module = pad(&file.get_field(entry, "module"), 14);
            let level = pad(&file.get_field(entry, "level"), 8);
            let message = file.message(entry).lines().next().unwrap_or("").to_string();
            let raw_line = format!(
                "{cursor} {:>4}/{:<4} row {:>5} line {:>6}  {} {} {} {}",
                result_index + 1,
                count,
                visible_pos + 1,
                entry.line_no,
                timestamp,
                module,
                level,
                message
            );
            let style = if selected {
                theme.selected()
            } else {
                theme.matched()
            };
            let line = crop(&raw_line, 0, inner.width as usize);
            let search_ranges = query_highlight_ranges(view.query.as_ref(), &raw_line);
            rows.push(ListItem::new(if search_ranges.is_empty() {
                Line::from(Span::styled(line, style))
            } else {
                highlighted_ranges(&line, &search_ranges, 0, style, theme.search_hit())
            }));
        }

        frame.render_widget(List::new(rows), inner);
    }

    fn draw_mode(&mut self, frame: &mut Frame, root: Rect) {
        self.entry_detail_area = Rect::default();
        self.pretty_print_area = Rect::default();
        self.help_area = Rect::default();
        match self.mode.clone() {
            Mode::Normal => {}
            Mode::Search(_) => {}
            Mode::AddFile(text) => {
                self.draw_input_popup(frame, root, "Add File", "Type a path and press Enter", &text)
            }
            Mode::OpenFolder(browser) => self.draw_folder_browser(frame, root, browser),
            Mode::Filter(text) => self.draw_input_popup(
                frame,
                root,
                "Add Filter",
                "field op [include|exclude] value   e.g. level equals exclude Trace",
                &text,
            ),
            Mode::TimePicker(picker) => self.draw_time_picker(frame, root, &picker),
            Mode::ExportFilters(text) => self.draw_input_popup(
                frame,
                root,
                "Export Filters",
                "Folder to write one JSON file per project filter",
                &text,
            ),
            Mode::LoadFilters(text) => self.draw_input_popup(
                frame,
                root,
                "Import Filters",
                "Folder of exported filter JSON files to merge into this project",
                &text,
            ),
            Mode::ExportSearches(text) => self.draw_input_popup(
                frame,
                root,
                "Export Saved Searches",
                "Folder to write one JSON file per saved search",
                &text,
            ),
            Mode::ImportSearches(text) => self.draw_input_popup(
                frame,
                root,
                "Import Saved Searches",
                "Folder of saved-search JSON files to merge into this project",
                &text,
            ),
            Mode::ExportBookmarks(text) => self.draw_input_popup(
                frame,
                root,
                "Export Bookmarks",
                "Folder to write one JSON file per incident bookmark",
                &text,
            ),
            Mode::ImportBookmarks(text) => self.draw_input_popup(
                frame,
                root,
                "Import Bookmarks",
                "Folder of bookmark JSON files to merge into this project",
                &text,
            ),
            Mode::ExportSchemas(text) => self.draw_input_popup(
                frame,
                root,
                "Export Log Schemas",
                "Folder to write one JSON file per log schema in this project",
                &text,
            ),
            Mode::ImportSchemas(text) => self.draw_input_popup(
                frame,
                root,
                "Import Log Schemas",
                "Folder of exported schema JSON files to merge into this project",
                &text,
            ),
            Mode::ExportIncident(text) => self.draw_input_popup(
                frame,
                root,
                "Export Incident Markdown",
                "Markdown file for selected lines, bookmarks, filters, and the latest AI summary",
                &text,
            ),
            Mode::Extractor(text) => self.draw_input_popup(
                frame,
                root,
                "Apply Log Schema",
                "schema name OR name | format template | [timestamp strptime format] | [description]",
                &text,
            ),
            Mode::SaveSourceSchema { text, .. } => self.draw_input_popup(
                frame,
                root,
                "Save Schema to Library",
                "schema name | description",
                &text,
            ),
            Mode::LiveSchemaEditor { text, .. } => self.draw_input_popup(
                frame,
                root,
                "Edit Live Source Schema",
                "schema name OR name | format template | [timestamp strptime format] | [description]",
                &text,
            ),
            Mode::SourceEditor(editor) => self.draw_source_editor(frame, root, &editor),
            Mode::LiveSourceEditor(editor) => {
                self.draw_live_source_editor(frame, root, &editor)
            }
            Mode::SchemaLibraryPicker(picker) => {
                self.draw_schema_library_picker(frame, root, &picker)
            }
            Mode::LiveQuickPick(picker) => self.draw_live_quick_pick(frame, root, &picker),
            Mode::EditFilter { text, .. } => self.draw_input_popup(
                frame,
                root,
                "Edit Filter",
                "[schema=<name>] field op [include|exclude] value",
                &text,
            ),
            Mode::EditSearch { text, .. } => self.draw_input_popup(
                frame,
                root,
                "Edit Saved Search",
                "text | \"phrase\" | /regex/ | field=value | after:<ts>",
                &text,
            ),
            Mode::BookmarkNote { text, .. } => self.draw_input_popup(
                frame,
                root,
                "Bookmark Note",
                "Optional label or note for this incident bookmark",
                &text,
            ),
            Mode::HideChoice(menu) => self.draw_hide_choice_popup(frame, root, &menu),
            Mode::HidePattern(prompt) => self.draw_hide_pattern_popup(frame, root, &prompt),
            Mode::EntryDetail { scroll } => self.draw_entry_detail_popup(frame, root, scroll),
            Mode::PrettyPrint {
                title,
                body,
                scroll,
            } => self.draw_pretty_print_popup(frame, root, &title, &body, scroll),
            Mode::Help => self.draw_help_popup(frame, root),
            Mode::Palette(palette) => self.draw_palette(frame, root, palette),
            Mode::FilterBuilder(builder) => self.draw_filter_builder(frame, root, &builder),
            Mode::ThemePicker(picker) => self.draw_theme_picker(frame, root, &picker),
            Mode::ActionLog => self.draw_action_log(frame, root),
        }
    }

    /// The folder browser. Scrolling lives here rather than in the key handler: only the
    /// drawer knows how many rows the popup has room for.
    fn draw_folder_browser(&mut self, frame: &mut Frame, root: Rect, mut browser: FolderBrowser) {
        const CHROME_ROWS: u16 = 7; // borders, path, blank, blank, footer
        let width = 82.min(root.width);
        let height = (browser.rows.len() as u16 + CHROME_ROWS).min(root.height.max(9));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);

        let picking_file = matches!(browser.purpose, BrowserPurpose::File);
        let title = if picking_file {
            "Add Log File"
        } else {
            "Open Folder"
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        let text_width = inner.width as usize;
        let listed = (inner.height as usize)
            .saturating_sub(CHROME_ROWS as usize - 2)
            .max(1);

        // Keep the selection on screen, scrolling by the least that achieves it.
        if browser.selected < browser.scroll {
            browser.scroll = browser.selected;
        } else if browser.selected >= browser.scroll + listed {
            browser.scroll = browser.selected + 1 - listed;
        }

        let files = if picking_file {
            "pick a file to add, or enter a folder".to_string()
        } else {
            match browser.file_count {
                0 => "no files here".to_string(),
                1 => "1 file here".to_string(),
                n => format!("{n} files here"),
            }
        };
        let mut lines = vec![
            Line::from(Span::styled(
                truncate_head(&browser.current.display().to_string(), text_width),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(Span::styled(files, Style::default().fg(Color::DarkGray))),
            Line::from(""),
        ];

        for (offset, row) in browser
            .rows
            .iter()
            .enumerate()
            .skip(browser.scroll)
            .take(listed)
        {
            let at_cursor = offset == browser.selected;
            let marker = if at_cursor { "> " } else { "  " };
            let style = if at_cursor {
                Style::default().bg(Color::DarkGray)
            } else if matches!(row, BrowserRow::OpenCurrent) {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                crop(&format!("{marker}{}", browser.label(row)), 0, text_width),
                style,
            )));
        }

        let footer = if picking_file {
            "j/k move   Enter add file / enter folder   Left up   p type path   . hidden   Esc"
        } else {
            "j/k move   Enter select   Right in   Left up   . hidden   Esc cancel"
        };
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            footer,
            Style::default().fg(Color::DarkGray),
        )));

        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .alignment(Alignment::Left),
            area,
        );
        self.mode = Mode::OpenFolder(browser);
    }

    fn draw_palette(&self, frame: &mut Frame, root: Rect, palette: Palette) {
        let theme = self.theme();
        let filtered = palette.filtered();
        let width = 62.min(root.width);
        // query + blank + up to 12 rows, inside the border.
        let rows = filtered.len().clamp(1, 12) as u16;
        let height = (rows + 4).min(root.height.max(6));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default().title("Command").borders(Borders::ALL);
        let inner = block.inner(area);
        let text_width = inner.width as usize;

        let mut lines = vec![
            Line::from(Span::styled(
                format!("> {}", palette.query),
                theme.section(),
            )),
            Line::from(""),
        ];

        let visible = (inner.height as usize).saturating_sub(2).max(1);
        let start = palette.selected.saturating_sub(visible.saturating_sub(1));
        for (index, command) in filtered.iter().enumerate().skip(start).take(visible) {
            let selected = index == palette.selected;
            let marker = if selected { "> " } else { "  " };
            let label = command.label();
            let hint = command.key_hint();
            // Right-align the key hint against the popup edge.
            let gap = text_width
                .saturating_sub(2 + label.chars().count() + hint.chars().count())
                .max(1);
            let row = format!("{marker}{label}{}{hint}", " ".repeat(gap));
            let style = if selected {
                theme.selected()
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&row, 0, text_width), style)));
        }
        if filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no matching action)",
                theme.dim(),
            )));
        }

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
        // Caret sits in the query line, right after what has been typed.
        let caret = (2 + palette.query.chars().count()).min(text_width.saturating_sub(1));
        frame.set_cursor_position((inner.x + caret as u16, inner.y));
    }

    fn draw_theme_picker(&self, frame: &mut Frame, root: Rect, picker: &ThemePicker) {
        let theme = self.theme();
        let width = 72.min(root.width);
        let height = (ThemeName::ALL.len() as u16 + 4).min(root.height.max(7));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title("Theme  Enter save  Esc cancel")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent()));
        let inner = block.inner(area);
        let text_width = inner.width as usize;

        let mut lines = Vec::new();
        for (index, option) in ThemeName::ALL.iter().copied().enumerate() {
            let selected = index == picker.selected;
            let current = option == self.ui_config.theme;
            let marker = match (selected, current) {
                (true, true) => ">*",
                (true, false) => "> ",
                (false, true) => " *",
                (false, false) => "  ",
            };
            let row = format!("{marker} {:<9} {}", option.label(), option.description());
            let style = if selected {
                theme.selected()
            } else if current {
                theme.section()
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&row, 0, text_width), style)));
        }
        lines.push(Line::from(Span::styled(
            "Saved to ~/.log-scouter/ui.json",
            theme.dim(),
        )));

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_source_editor(&self, frame: &mut Frame, root: Rect, editor: &SourceEditor) {
        let theme = self.theme();
        let width = root
            .width
            .saturating_sub(4)
            .min(112)
            .max(root.width.min(72));
        let height = 11.min(root.height.max(8));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title("Log Source  Enter save  Esc cancel")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent()));
        let inner = block.inner(area);
        let text_width = inner.width as usize;
        let label_width = 13usize;
        let rows = [
            ("Short name", editor.short_name.as_str()),
            ("Description", editor.description.as_str()),
            ("Tag", editor.tag.as_str()),
            ("Schema", editor.schema.as_str()),
        ];

        let mut lines = Vec::new();
        for (index, (label, value)) in rows.into_iter().enumerate() {
            let marker = if index == editor.row { "> " } else { "  " };
            let row = format!(
                "{marker}{label:<label_width$} {}",
                value,
                label_width = label_width
            );
            let style = if index == editor.row {
                theme.selected()
            } else if index == SourceEditor::SCHEMA {
                theme.section()
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&row, 0, text_width), style)));
        }
        lines.push(Line::from(""));
        if editor.row == SourceEditor::SCHEMA {
            lines.push(Line::from(Span::styled(
                "Schema: i detect with LLM  e edit manually  L load library  X save to library",
                theme.dim(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Use Up/Down or Tab to move; type to edit the focused field",
                theme.dim(),
            )));
        }

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_live_source_editor(&self, frame: &mut Frame, root: Rect, editor: &LiveSourceEditor) {
        let theme = self.theme();
        let rows = editor.rows();
        let width = root
            .width
            .saturating_sub(4)
            .min(112)
            .max(root.width.min(78));
        let height = (rows.len() as u16 + 8).min(root.height.max(10));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let title = if editor.file_id.is_some() {
            "Live Log Source  Enter save  Esc cancel"
        } else {
            "Live Log Source  Enter start  Esc cancel"
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent()));
        let inner = block.inner(area);
        let text_width = inner.width as usize;
        let label_width = 13usize;

        let mut lines = Vec::new();
        for (index, (_, label, value)) in rows.iter().enumerate() {
            let marker = if index == editor.row { "> " } else { "  " };
            let row = format!(
                "{marker}{label:<label_width$} {}",
                value,
                label_width = label_width
            );
            let style = if index == editor.row {
                theme.selected()
            } else if rows[index].0 == LiveSourceField::Schema {
                theme.section()
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&row, 0, text_width), style)));
        }
        lines.push(Line::from(""));
        let preview = editor.config().command_preview();
        lines.push(Line::from(Span::styled(
            crop(&format!("Command: {preview}"), 0, text_width),
            theme.dim(),
        )));
        let hint = match editor.field_for_row() {
            LiveSourceField::Kind => "Type: Left/Right or k/d/j selects kubectl/docker/journalctl",
            field if live_quick_pick_field(field) => {
                "Right quick pick from the local runtime; type to edit manually"
            }
            LiveSourceField::Since => "Since is optional: examples 10m, 2026-07-15 09:00:00",
            LiveSourceField::Tail => "Tail lines is optional; leave empty for the command default",
            LiveSourceField::Schema => {
                "Schema: i detect with LLM  e edit manually  L load library  X save to library"
            }
            _ => "Use Up/Down or Tab to move; type to edit the focused field",
        };
        lines.push(Line::from(Span::styled(hint, theme.dim())));

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_live_quick_pick(&self, frame: &mut Frame, root: Rect, picker: &LiveQuickPick) {
        let theme = self.theme();
        let width = root.width.saturating_sub(4).min(86).max(root.width.min(56));
        let rows = picker.options.len().clamp(1, 10) as u16;
        let height = (rows + 5).min(root.height.max(7));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let title = format!(
            "Quick Pick {}  Enter select  Esc back",
            live_quick_pick_label(picker.target)
        );
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent()));
        let inner = block.inner(area);
        let text_width = inner.width as usize;

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            crop(&picker.message, 0, text_width),
            theme.dim(),
        )));
        lines.push(Line::from(""));

        if picker.loading {
            lines.push(Line::from(Span::styled("  loading...", theme.section())));
        } else if picker.options.is_empty() {
            lines.push(Line::from(Span::styled("  no options", theme.dim())));
        } else {
            for (index, option) in picker.options.iter().enumerate().take(10) {
                let marker = if index == picker.selected { "> " } else { "  " };
                let style = if index == picker.selected {
                    theme.selected()
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(
                    crop(&format!("{marker}{option}"), 0, text_width),
                    style,
                )));
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_schema_library_picker(
        &self,
        frame: &mut Frame,
        root: Rect,
        picker: &SchemaLibraryPicker,
    ) {
        let theme = self.theme();
        let width = root.width.saturating_sub(4).min(96).max(root.width.min(64));
        let visible = 10usize;
        let height = ((picker.options.len().min(visible) + 3) as u16).min(root.height.max(7));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title("Schema Library  Enter apply  Esc cancel")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent()));
        let inner = block.inner(area);
        let text_width = inner.width as usize;
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));

        let mut lines = Vec::new();
        for (index, schema) in picker.options.iter().enumerate().skip(start).take(visible) {
            let selected = index == picker.selected;
            let marker = if selected { "> " } else { "  " };
            let desc = if schema.description.trim().is_empty() {
                schema.format.as_str()
            } else {
                schema.description.as_str()
            };
            let row = format!("{marker}{:<24} {}", schema.name, desc);
            lines.push(Line::from(Span::styled(
                crop(&row, 0, text_width),
                if selected {
                    theme.selected()
                } else {
                    Style::default()
                },
            )));
        }
        if picker.options.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no schemas in ~/.log-scouter/schemas",
                theme.dim(),
            )));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_action_log(&self, frame: &mut Frame, root: Rect) {
        let width = 72.min(root.width);
        let visible = 16usize;
        let height = ((self.action_log.len().min(visible) + 3) as u16).min(root.height.max(6));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title("Action History  (u undo · Ctrl+r redo · any key closes)")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        let text_width = inner.width as usize;

        let mut lines: Vec<Line> = Vec::new();
        if self.action_log.is_empty() {
            lines.push(Line::from(Span::styled(
                "no actions yet",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // Most recent last, showing the tail that fits.
            let start = self.action_log.len().saturating_sub(inner.height as usize);
            for entry in &self.action_log[start..] {
                let actor = Style::default().fg(match entry.actor {
                    Actor::Ai => Color::Cyan,
                    Actor::User => Color::White,
                });
                let line = format!(
                    "{} {:<4} {}",
                    entry.time,
                    entry.actor.label(),
                    entry.description
                );
                lines.push(Line::from(Span::styled(crop(&line, 0, text_width), actor)));
            }
        }
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    fn draw_filter_builder(&self, frame: &mut Frame, root: Rect, builder: &FilterBuilder) {
        let width = 68.min(root.width);
        // scope + blank + 5 rows + blank + preview + up to 2 samples + footer, inside border.
        let height = 13u16.min(root.height.max(9));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let title = if builder.edit_index.is_some() {
            "Edit Filter"
        } else {
            "Filter Builder"
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        let text_width = inner.width as usize;
        let dim = Style::default().fg(Color::DarkGray);

        // The five editable rows: label, value, and whether Left/Right cycles it.
        let rows = [
            (
                FilterBuilder::SCHEMA,
                "Schema",
                builder.schema_label(),
                true,
            ),
            (FilterBuilder::FIELD, "Field", builder.field.clone(), true),
            (
                FilterBuilder::OP,
                "Operator",
                builder.op_name().to_string(),
                true,
            ),
            (
                FilterBuilder::ACTION,
                "Action",
                if builder.exclude {
                    "Exclude".to_string()
                } else {
                    "Include".to_string()
                },
                true,
            ),
            (FilterBuilder::VALUE, "Value", builder.value.clone(), false),
        ];

        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled("Scope     Project", dim)),
            Line::from(""),
        ];
        for (index, label, value, cyclable) in rows {
            let selected = index == builder.row;
            let marker = if selected { "> " } else { "  " };
            let shown = if value.is_empty() {
                "—".to_string()
            } else {
                value
            };
            let arrows = if selected && cyclable {
                "  ◀ ▶"
            } else {
                ""
            };
            let text = format!("{marker}{label:<10}{shown}{arrows}");
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&text, 0, text_width), style)));
        }

        lines.push(Line::from(""));
        // Live validation wins over the preview; otherwise the match count.
        match &builder.error {
            Some(error) => lines.push(Line::from(Span::styled(
                crop(&format!("! {error}"), 0, text_width),
                Style::default().fg(Color::Red),
            ))),
            None => {
                lines.push(Line::from(Span::styled(
                    crop(
                        &format!(
                            "Preview: {}",
                            preview_summary(&builder.preview, builder.exclude)
                        ),
                        0,
                        text_width,
                    ),
                    Style::default().fg(Color::Green),
                )));
                for sample in builder.preview.samples.iter().take(2) {
                    lines.push(Line::from(Span::styled(
                        crop(&format!("    {sample}"), 0, text_width),
                        dim,
                    )));
                }
            }
        }

        lines.push(Line::from(Span::styled(
            "↑↓ row   ←→ change   type to edit   Tab raw   Enter apply   Esc",
            dim,
        )));

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);

        // Show the caret on the text rows being edited.
        if matches!(builder.row, FilterBuilder::FIELD | FilterBuilder::VALUE) {
            let value_len = if builder.row == FilterBuilder::FIELD {
                builder.field.chars().count()
            } else {
                builder.value.chars().count()
            };
            let column = 2 + 10 + value_len;
            let row_offset = 2 + builder.row; // scope + blank + row index
            frame.set_cursor_position((
                inner.x + (column as u16).min(inner.width.saturating_sub(1)),
                inner.y + row_offset as u16,
            ));
        }
    }

    fn draw_time_picker(&mut self, frame: &mut Frame, root: Rect, picker: &TimePicker) {
        let height = (TimePicker::ROWS as u16 + 7).min(root.height.max(9));
        let area = centered_rect(68, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default().title("Time Range").borders(Borders::ALL);
        let inner = block.inner(area);

        let highlight = |on: bool, style: Style| {
            if on {
                style.bg(Color::Gray).fg(Color::Black)
            } else {
                style
            }
        };

        let mut lines = vec![Line::from(Span::styled(
            "Quick select",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))];
        for (index, (label, _)) in TIME_PRESETS.iter().enumerate() {
            let on = picker.row == index;
            let marker = if on { '>' } else { ' ' };
            lines.push(Line::from(Span::styled(
                format!("{marker} {}  {label}", index + 1),
                highlight(on, Style::default()),
            )));
        }

        lines.push(Line::from(""));
        for (row, name, value) in [
            (TimePicker::START_ROW, "Start", &picker.start),
            (TimePicker::END_ROW, "End", &picker.end),
        ] {
            let on = picker.row == row;
            let marker = if on { '>' } else { ' ' };
            lines.push(Line::from(Span::styled(
                format!("{marker} {name:<6}  {value}"),
                highlight(on, Style::default().fg(Color::Yellow)),
            )));
        }

        lines.push(Line::from(""));
        if let (Some(earliest), Some(latest)) = (picker.earliest, picker.latest) {
            lines.push(Line::from(Span::styled(
                format!(
                    "log spans {} .. {}",
                    format_filter_datetime(earliest),
                    format_filter_datetime(latest)
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(Span::styled(
            "Up/Down move (a preset fills the fields)  Enter apply  Esc cancel",
            Style::default().fg(Color::DarkGray),
        )));

        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);

        // Put a real caret in whichever of Start/End has focus.
        if let Some(value) = picker.field() {
            let row_offset = 1 + TIME_PRESETS.len() + 1 + (picker.row - TimePicker::START_ROW);
            let caret = picker.cursor.min(value.chars().count());
            let x = inner.x + TimePicker::FIELD_PREFIX as u16 + caret as u16;
            let y = inner.y + row_offset as u16;
            if x < inner.right() && y < inner.bottom() {
                frame.set_cursor_position((x, y));
            }
        }
    }

    fn draw_entry_detail_popup(&mut self, frame: &mut Frame, root: Rect, scroll: usize) {
        let width = root
            .width
            .saturating_sub(4)
            .min(120)
            .max(root.width.min(40));
        let height = root
            .height
            .saturating_sub(4)
            .min(34)
            .max(root.height.min(10));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);

        let block = Block::default()
            .title("Log Detail  P pretty-print  Enter/Esc close  j/k scroll")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        self.entry_detail_area = inner;
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let lines = self.full_entry_detail_lines(inner.width as usize);
        let lines = self.apply_detail_selection(lines, DetailSurface::Popup);
        frame.render_widget(
            Paragraph::new(Text::from(lines)).scroll((scroll as u16, 0)),
            inner,
        );
    }

    fn draw_pretty_print_popup(
        &mut self,
        frame: &mut Frame,
        root: Rect,
        title: &str,
        body: &str,
        scroll: usize,
    ) {
        let width = root
            .width
            .saturating_sub(4)
            .min(120)
            .max(root.width.min(40));
        let height = root
            .height
            .saturating_sub(4)
            .min(34)
            .max(root.height.min(10));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);

        let block = Block::default()
            .title(format!("{title}  y copy  Esc/q close  j/k scroll"))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        self.pretty_print_area = inner;
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let lines = pretty_body_lines(body, inner.width as usize);
        let lines = self.apply_detail_selection(lines, DetailSurface::PrettyPrint);
        frame.render_widget(
            Paragraph::new(Text::from(lines)).scroll((scroll as u16, 0)),
            inner,
        );
    }

    fn draw_help_popup(&mut self, frame: &mut Frame, root: Rect) {
        let width = root
            .width
            .saturating_sub(4)
            .min(160)
            .max(root.width.min(84));
        let height = root
            .height
            .saturating_sub(4)
            .min(70)
            .max(root.height.min(20));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title("Keys  y copy  Esc/q close")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        self.help_area = inner;
        let lines = self.detail_surface_lines(DetailSurface::Help, inner.width as usize);
        let lines = self.apply_detail_selection(lines, DetailSurface::Help);
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }

    /// Long values (a derived regex, a full log message) must wrap: with a single
    /// clipped line, typing and Backspace appear to do nothing.
    fn draw_input_popup(
        &self,
        frame: &mut Frame,
        root: Rect,
        title: &str,
        hint: &str,
        value: &str,
    ) {
        let width = 82.min(root.width);
        let value_width = (width as usize).saturating_sub(4).max(1);
        let wrapped = chunk_chars(value, value_width);
        let height = (wrapped.len() as u16 + 5).min(root.height.max(5));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);

        let mut lines = vec![
            Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            Line::from(""),
        ];
        for (index, chunk) in wrapped.iter().enumerate() {
            let prefix = if index == 0 { "> " } else { "  " };
            lines.push(Line::from(Span::raw(format!("{prefix}{chunk}"))));
        }

        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .alignment(Alignment::Left),
            area,
        );

        // Put a real terminal caret where the next character lands.
        let caret = self.input_cursor.min(value.chars().count());
        let (row, column) = (caret / value_width, caret % value_width);
        let x = inner.x + 2 + column as u16;
        let y = inner.y + 2 + row as u16;
        if x < inner.right() && y < inner.bottom() {
            frame.set_cursor_position((x, y));
        }
    }

    /// One line's fields, each with the value that line carries, so a rule can be picked
    /// by what it will match rather than by the field's name alone.
    fn draw_hide_choice_popup(&self, frame: &mut Frame, root: Rect, menu: &HideMenu) {
        let width = 76.min(root.width);
        // Heading, blank, the fields, blank, three hints, blank, footer, two borders.
        let height = (menu.fields.len() as u16 + 10).clamp(12, root.height.max(12));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);

        let picked = menu.picked.iter().filter(|picked| **picked).count();
        // "Hide" drops the matching lines; "Keep only" drops everything else.
        let verb = if menu.exclude { "Hide" } else { "Keep only" };
        let heading = match picked {
            0 | 1 => format!("{verb} logs where this field has the current value"),
            n => format!("{verb} logs matching all {n} picked fields"),
        };
        let heading_style = if menu.exclude {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        };
        let mut lines = vec![
            Line::from(Span::styled(
                heading,
                heading_style.add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];

        let inner_width = (width as usize).saturating_sub(4);
        for (index, (field, value)) in menu.fields.iter().enumerate() {
            let Some(key) = hide_choice_key(index) else {
                break;
            };
            let at_cursor = index == menu.cursor;
            let row = format!(
                "{}{} {key}  {field:<14} {}",
                if at_cursor { ">" } else { " " },
                if menu.picked[index] { "+" } else { " " },
                field_value_preview(value),
            );
            let style = if at_cursor {
                Style::default().bg(Color::DarkGray)
            } else if menu.picked[index] {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(crop(&row, 0, inner_width), style)));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Space picks a field   Enter combines the picks with AND",
            Style::default().fg(Color::Cyan),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "Tab  switch to {}",
                if menu.exclude { "keep only" } else { "hide" }
            ),
            Style::default().fg(Color::Cyan),
        )));
        lines.push(Line::from(Span::styled(
            "H  message pattern, with the ids and counters generalised",
            Style::default().fg(Color::Cyan),
        )));
        lines.push(Line::from(""));
        let verb = if menu.exclude { "hides" } else { "keeps" };
        lines.push(Line::from(Span::styled(
            format!("A field's own key {verb} by it at once   Esc cancel"),
            Style::default().fg(Color::DarkGray),
        )));

        let title = if menu.exclude { "Hide" } else { "Keep" };
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().title(title).borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// The chosen regex, the ladder it came from, and what it would do if committed.
    /// Nothing else in the app commits a project-wide rule from a text field, so nothing
    /// else needs the preview.
    fn draw_hide_pattern_popup(&self, frame: &mut Frame, root: Rect, prompt: &PatternPrompt) {
        let value = prompt.text.as_str();
        let exclude = prompt.exclude;
        let preview = self.hide_pattern_preview(value, &prompt.field);
        let width = 82.min(root.width);
        let value_width = (width as usize).saturating_sub(4).max(1);
        let wrapped = chunk_chars(value, value_width);

        let target = match prompt.field.as_str() {
            "raw" => "the whole log line".to_string(),
            field => format!("the {field} field"),
        };
        let hint = if prompt.candidates.len() > 1 {
            format!("Regex over {target} - Up/Down pick a template, or edit it")
        } else {
            format!("Regex over {target} - edit it, then Enter")
        };
        let mut lines = vec![
            Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            Line::from(""),
        ];
        for (index, chunk) in wrapped.iter().enumerate() {
            let prefix = if index == 0 { "> " } else { "  " };
            lines.push(Line::from(Span::raw(format!("{prefix}{chunk}"))));
        }

        // The ladder, greediest first, each with the rows it would take out.
        if prompt.candidates.len() > 1 {
            lines.push(Line::from(""));
            for (index, candidate) in prompt.candidates.iter().enumerate() {
                let at_cursor = index == prompt.selected;
                let marker = if at_cursor { "  \u{25b8} " } else { "    " };
                let edited = if at_cursor && prompt.edited() {
                    "  (edited)"
                } else {
                    ""
                };
                let counted = format!(
                    "matches {}{}",
                    thousands(candidate.matched),
                    if prompt.capped() { "+" } else { "" }
                );
                let row = format!(
                    "{marker}{:<9} {:<32} {counted}{edited}",
                    candidate.option.name, candidate.option.hint
                );
                let style = if at_cursor {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(Span::styled(crop(&row, 0, value_width), style)));
            }
        }
        lines.push(Line::from(""));

        match &preview.error {
            Some(error) => lines.push(Line::from(Span::styled(
                format!("  invalid regex: {}", crop(error, 0, value_width)),
                Style::default().fg(Color::Red),
            ))),
            None => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", preview_summary(&preview, exclude)),
                    Style::default()
                        .fg(if exclude { Color::Yellow } else { Color::Green })
                        .add_modifier(Modifier::BOLD),
                )));
                for sample in &preview.samples {
                    lines.push(Line::from(Span::styled(
                        format!("    {}", crop(sample, 0, value_width.saturating_sub(2))),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Tab hide/keep   Enter apply   Esc cancel",
            Style::default().fg(Color::DarkGray),
        )));

        let height = (lines.len() as u16 + 2).min(root.height.max(7));
        let area = centered_rect(width, height, root);
        frame.render_widget(Clear, area);
        let title = if exclude {
            "Hide Pattern"
        } else {
            "Keep Pattern"
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .alignment(Alignment::Left),
            area,
        );

        let caret = self.input_cursor.min(value.chars().count());
        let (row, column) = (caret / value_width, caret % value_width);
        let x = inner.x + 2 + column as u16;
        let y = inner.y + 2 + row as u16;
        if x < inner.right() && y < inner.bottom() {
            frame.set_cursor_position((x, y));
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let mode_kind = match &self.mode {
            Mode::Normal => 0,
            Mode::Search(_)
            | Mode::AddFile(_)
            | Mode::Filter(_)
            | Mode::ExportFilters(_)
            | Mode::LoadFilters(_)
            | Mode::ExportSearches(_)
            | Mode::ImportSearches(_)
            | Mode::ExportBookmarks(_)
            | Mode::ImportBookmarks(_)
            | Mode::ExportSchemas(_)
            | Mode::ImportSchemas(_)
            | Mode::ExportIncident(_)
            | Mode::Extractor(_)
            | Mode::SaveSourceSchema { .. }
            | Mode::LiveSchemaEditor { .. }
            | Mode::EditFilter { .. }
            | Mode::EditSearch { .. }
            | Mode::BookmarkNote { .. }
            | Mode::HidePattern(_) => 1,
            Mode::HideChoice(_) => 2,
            Mode::EntryDetail { .. } | Mode::PrettyPrint { .. } => 3,
            Mode::ActionLog => 4,
            Mode::TimePicker(_) => 5,
            Mode::OpenFolder(_) => 6,
            Mode::Palette(_) => 7,
            Mode::FilterBuilder(_) => 8,
            Mode::ThemePicker(_) => 9,
            Mode::Help => 10,
            Mode::SourceEditor(_) => 11,
            Mode::SchemaLibraryPicker(_) => 12,
            Mode::LiveSourceEditor(_) => 13,
            Mode::LiveQuickPick(_) => 14,
        };

        match mode_kind {
            0 => self.handle_normal_key(key),
            1 => self.handle_input_key(key),
            2 => self.handle_hide_choice_key(key),
            3 => self.handle_entry_detail_key(key),
            4 => {
                self.mode = Mode::Normal;
                Ok(false)
            }
            5 => self.handle_time_picker_key(key),
            6 => self.handle_folder_browser_key(key),
            7 => self.handle_palette_key(key),
            8 => self.handle_filter_builder_key(key),
            9 => self.handle_theme_picker_key(key),
            10 => self.handle_help_key(key),
            11 => self.handle_source_editor_key(key),
            12 => self.handle_schema_library_picker_key(key),
            13 => self.handle_live_source_editor_key(key),
            14 => self.handle_live_quick_pick_key(key),
            _ => Ok(false),
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(self.mode, Mode::EntryDetail { .. }) {
            self.handle_detail_popup_mouse(DetailSurface::Popup, mouse);
            return;
        }
        if matches!(self.mode, Mode::PrettyPrint { .. }) {
            self.handle_detail_popup_mouse(DetailSurface::PrettyPrint, mouse);
            return;
        }
        if matches!(self.mode, Mode::Help) {
            self.handle_detail_popup_mouse(DetailSurface::Help, mouse);
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => self.copy_from_mouse(mouse),
            MouseEventKind::Down(MouseButton::Left) => self.begin_mouse_selection(mouse),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_mouse_selection(mouse),
            MouseEventKind::Up(MouseButton::Left) => {
                // A timeline drag (or click) applies its range on release.
                if let Some(MouseDrag::Timeline { anchor_col }) = self.mouse_drag {
                    self.apply_timeline_range(anchor_col, mouse.column);
                }
                self.mouse_drag = None;
            }
            _ => {}
        }
    }

    fn handle_detail_popup_mouse(&mut self, surface: DetailSurface, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if rect_contains(self.detail_surface_area(surface), mouse.column, mouse.row) {
                    self.copy_detail_text(surface);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.begin_detail_mouse_selection(surface, mouse);
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_mouse_selection(mouse),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_drag = None,
            _ => {}
        }
    }

    fn copy_from_mouse(&mut self, mouse: MouseEvent) {
        if rect_contains(self.detail_area, mouse.column, mouse.row) {
            self.copy_detail_text(DetailSurface::Inline);
            return;
        }

        if let Some((pane, _)) = self.pane_at(mouse.column, mouse.row) {
            self.copy_from_pane_click(pane, mouse);
            return;
        }

        // Preserve the old behavior for right-clicks outside a mouse-aware region.
        self.copy_selection();
    }

    fn begin_mouse_selection(&mut self, mouse: MouseEvent) {
        // The sidebar/pane separator: a press here starts a resize drag.
        if self.on_separator(mouse.column, mouse.row) {
            self.mouse_drag = Some(MouseDrag::Sidebar);
            return;
        }
        // A border between panes resizes those two panes (heights on a rows split).
        if let Some(boundary) = self.pane_separator_at(mouse.column, mouse.row) {
            self.mouse_drag = Some(MouseDrag::PaneSeparator { boundary });
            return;
        }
        // A panel's top border resizes that panel's height (results, detail, chat).
        if let Some(separator) = self.panel_separator_at(mouse.column, mouse.row) {
            self.mouse_drag = Some(MouseDrag::PanelHeight {
                panel: separator.panel,
                bottom: separator.bottom,
            });
            return;
        }
        // A press on the timeline starts a drag that becomes a time-range filter on release.
        if self.on_timeline(mouse.column, mouse.row) {
            self.mouse_drag = Some(MouseDrag::Timeline {
                anchor_col: mouse.column,
            });
            return;
        }
        if rect_contains(self.detail_area, mouse.column, mouse.row) {
            self.begin_detail_mouse_selection(DetailSurface::Inline, mouse);
            return;
        }

        if let Some((pane, _)) = self.pane_at(mouse.column, mouse.row) {
            self.begin_pane_mouse_selection(pane, mouse);
            return;
        }

        if rect_contains(self.sidebar_area, mouse.column, mouse.row) {
            let row = mouse.row.saturating_sub(self.sidebar_area.y) as usize;
            let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
            self.click_sidebar(row, ctrl);
            return;
        }

        if rect_contains(self.results_area, mouse.column, mouse.row) {
            self.click_result(mouse);
        }
    }

    fn drag_mouse_selection(&mut self, mouse: MouseEvent) {
        match self.mouse_drag {
            Some(MouseDrag::Sidebar) => self.drag_sidebar_to(mouse.column),
            Some(MouseDrag::PaneSeparator { boundary }) => {
                self.drag_pane_separator(boundary, mouse.column, mouse.row)
            }
            Some(MouseDrag::PanelHeight { panel, bottom }) => {
                let height = bottom.saturating_sub(mouse.row).clamp(2, 60);
                match panel {
                    PanelEdge::Results => self.workspace.results_height = Some(height),
                    PanelEdge::Detail => self.workspace.detail_height = Some(height),
                    PanelEdge::Chat => self.workspace.chat_height = Some(height),
                }
            }
            Some(MouseDrag::PaneText {
                pane,
                position,
                anchor,
            }) => {
                let Some(row) = self.pane_row_at(pane, mouse.row) else {
                    return;
                };
                if row != position {
                    // Left the row, so this was a row selection all along. Anchor it on
                    // the row the press landed in.
                    self.text_selection = None;
                    self.focus = Focus::Pane;
                    self.focused_pane = pane;
                    if let Some(view) = self.active_view_mut() {
                        view.anchor = Some(position);
                        view.move_cursor_to(row);
                    }
                    self.mouse_drag = Some(MouseDrag::Pane {
                        pane,
                        anchor: position,
                    });
                    self.report_selection();
                    return;
                }

                let Some(column) = self.pane_column_at(pane, mouse.column, position) else {
                    return;
                };
                self.focus = Focus::Pane;
                self.focused_pane = pane;
                self.text_selection = Some(TextSelection {
                    pane,
                    position,
                    anchor,
                    cursor: column,
                });
                self.report_text_selection();
            }
            Some(MouseDrag::Pane { pane, anchor }) => {
                let Some(position) = self.pane_position_at(pane, mouse.column, mouse.row) else {
                    return;
                };
                self.focus = Focus::Pane;
                self.focused_pane = pane;
                if let Some(view) = self.active_view_mut() {
                    view.anchor = Some(anchor);
                    view.move_cursor_to(position);
                }
                self.report_selection();
            }
            Some(MouseDrag::Detail { surface, anchor }) => {
                let Some(line) = self.detail_line_at(surface, mouse.column, mouse.row) else {
                    return;
                };
                self.detail_selection = Some(DetailSelection {
                    surface,
                    anchor,
                    cursor: line,
                });
                self.report_detail_selection();
            }
            // A timeline drag applies its range on release, not continuously.
            Some(MouseDrag::Timeline { .. }) => {}
            None => {}
        }
    }

    fn click_result(&mut self, mouse: MouseEvent) {
        let row = mouse.row.saturating_sub(self.results_area.y) as usize;
        let result_index = self.results_scroll + row;
        let count = self.active_result_positions().len();
        if result_index >= count {
            return;
        }
        self.focus = Focus::Results;
        self.results_selected = result_index;
        self.jump_to_selected_result();
    }

    fn begin_pane_mouse_selection(&mut self, pane: usize, mouse: MouseEvent) {
        let Some(position) = self.pane_position_at(pane, mouse.column, mouse.row) else {
            return;
        };
        self.focus = Focus::Pane;
        self.focused_pane = pane;
        self.detail_selection = None;
        // Any fresh press in a pane retires the previous substring.
        self.text_selection = None;

        if mouse.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(view) = self.active_view_mut() {
                view.move_cursor_to(position);
                view.toggle_current();
            }
            self.mouse_drag = None;
            self.report_selection();
            return;
        }

        // Shift+drag keeps extending the row selection from wherever it started.
        if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            let anchor = self
                .active_view()
                .map(|view| view.anchor.unwrap_or(view.cursor))
                .unwrap_or(position);
            if let Some(view) = self.active_view_mut() {
                view.anchor = Some(anchor);
                view.move_cursor_to(position);
            }
            self.mouse_drag = Some(MouseDrag::Pane { pane, anchor });
            self.report_selection();
            return;
        }

        // A plain press could still become either gesture. Assume characters until the
        // pointer leaves the row, which is the only thing that distinguishes them.
        if let Some(view) = self.active_view_mut() {
            view.clear_selection();
            view.move_cursor_to(position);
        }
        self.mouse_drag = self
            .pane_column_at(pane, mouse.column, position)
            .map(|anchor| MouseDrag::PaneText {
                pane,
                position,
                anchor,
            });
    }

    fn begin_detail_mouse_selection(&mut self, surface: DetailSurface, mouse: MouseEvent) {
        let Some(line) = self.detail_line_at(surface, mouse.column, mouse.row) else {
            return;
        };
        self.detail_selection = Some(DetailSelection {
            surface,
            anchor: line,
            cursor: line,
        });
        self.mouse_drag = Some(MouseDrag::Detail {
            surface,
            anchor: line,
        });
        self.report_detail_selection();
    }

    fn copy_from_pane_click(&mut self, pane: usize, mouse: MouseEvent) {
        // Right-clicking with a substring selected copies it, wherever the click landed;
        // moving the cursor first would copy a row the user never pointed at.
        if self.text_selection.is_some() {
            self.copy_selection();
            return;
        }

        if let Some(position) = self.pane_position_at(pane, mouse.column, mouse.row) {
            self.focus = Focus::Pane;
            self.focused_pane = pane;
            let clicked_selected = self
                .active_view()
                .and_then(|view| view.visible.get(position).map(|global| (view, global)))
                .map(|(view, global)| view.is_selected(position, global))
                .unwrap_or(false);
            if !clicked_selected {
                if let Some(view) = self.active_view_mut() {
                    view.clear_selection();
                    view.move_cursor_to(position);
                }
            }
        }
        self.copy_selection();
    }

    fn copy_detail_text(&mut self, surface: DetailSurface) {
        let area = self.detail_surface_area(surface);
        let width = area.width as usize;
        let lines = self.detail_surface_lines(surface, width);
        let selected = self
            .detail_selection_range(surface)
            .map(|(lo, hi)| {
                lines
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index >= lo && *index <= hi)
                    .map(|(_, line)| line_to_plain(line))
                    .collect::<Vec<_>>()
            })
            .filter(|lines| !lines.is_empty())
            .unwrap_or_else(|| lines.iter().map(line_to_plain).collect());

        let text = selected.join("\n");
        if text.trim().is_empty() {
            self.status = "nothing to copy".to_string();
            return;
        }

        let count = selected.len();
        let label = detail_surface_label(surface);
        self.status = match copy_to_clipboard(&text) {
            Ok(()) => format!("copied {count} {label} line(s), {} bytes", text.len()),
            Err(error) => format!("copy failed: {error}"),
        };
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(usize, Rect)> {
        self.pane_areas
            .iter()
            .copied()
            .enumerate()
            .find(|(_, area)| rect_contains(*area, x, y))
    }

    fn pane_position_at(&self, pane: usize, x: u16, y: u16) -> Option<usize> {
        let area = *self.pane_areas.get(pane)?;
        if !rect_contains(area, x, y) {
            return None;
        }
        self.pane_row_at(pane, y)
    }

    /// The row under `y`, ignoring `x`. A drag that wanders past the left or right edge
    /// is still working on the row it is level with.
    fn pane_row_at(&self, pane: usize, y: u16) -> Option<usize> {
        let area = *self.pane_areas.get(pane)?;
        if y < area.y || y >= area.bottom() {
            return None;
        }
        let view = self.panes.get(pane).map(|pane| &pane.view)?;
        let position = view.scroll_y + (y - area.y) as usize;
        (position < view.visible.len()).then_some(position)
    }

    /// The character offset of `x` within row `position`, clamped past the markers and
    /// to the end of the text so a drag off either edge saturates instead of vanishing.
    fn pane_column_at(&self, pane: usize, x: u16, position: usize) -> Option<usize> {
        let area = *self.pane_areas.get(pane)?;
        let view = self.panes.get(pane).map(|pane| &pane.view)?;
        let clamped = x.clamp(area.x, area.right().saturating_sub(1));
        let column = view.scroll_x + (clamped - area.x) as usize;

        let length = self.pane_row_line(pane, position)?.chars().count();
        if length <= ROW_MARKER_WIDTH {
            return None;
        }
        Some(column.clamp(ROW_MARKER_WIDTH, length - 1))
    }

    fn selected_substring(&self) -> Option<String> {
        let selection = self.text_selection?;
        let line = self.pane_row_line(selection.pane, selection.position)?;
        let (lo, hi) = selection.range();
        let text: String = line.chars().skip(lo).take(hi + 1 - lo).collect();
        (!text.trim().is_empty()).then_some(text)
    }

    fn report_text_selection(&mut self) {
        let Some(selection) = self.text_selection else {
            return;
        };
        let (lo, hi) = selection.range();
        self.status = format!("{} char(s) selected", hi + 1 - lo);
    }

    fn detail_line_at(&self, surface: DetailSurface, x: u16, y: u16) -> Option<usize> {
        let area = self.detail_surface_area(surface);
        if !rect_contains(area, x, y) {
            return None;
        }
        let row = y.saturating_sub(area.y) as usize;
        let line = self.detail_surface_scroll(surface).saturating_add(row);
        let lines = self.detail_surface_lines(surface, area.width as usize);
        (line < lines.len()).then_some(line)
    }

    fn detail_surface_area(&self, surface: DetailSurface) -> Rect {
        match surface {
            DetailSurface::Inline => self.detail_area,
            DetailSurface::Popup => self.entry_detail_area,
            DetailSurface::PrettyPrint => self.pretty_print_area,
            DetailSurface::Help => self.help_area,
        }
    }

    fn detail_surface_lines(&self, surface: DetailSurface, width: usize) -> Vec<Line<'static>> {
        match surface {
            DetailSurface::Inline => {
                if self.focus == Focus::Sidebar {
                    self.sidebar_detail_lines(width)
                        .unwrap_or_else(|| self.active_entry_detail_lines(width))
                } else {
                    self.active_entry_detail_lines(width)
                }
            }
            DetailSurface::Popup => self.full_entry_detail_lines(width),
            DetailSurface::PrettyPrint => match &self.mode {
                Mode::PrettyPrint { body, .. } => pretty_body_lines(body, width),
                _ => Vec::new(),
            },
            DetailSurface::Help => pretty_body_lines(help_text(), width),
        }
    }

    fn detail_surface_scroll(&self, surface: DetailSurface) -> usize {
        match (surface, &self.mode) {
            (DetailSurface::Popup, Mode::EntryDetail { scroll }) => *scroll,
            (DetailSurface::PrettyPrint, Mode::PrettyPrint { scroll, .. }) => *scroll,
            _ => 0,
        }
    }

    fn report_detail_selection(&mut self) {
        let Some(selection) = self.detail_selection else {
            return;
        };
        let count = selection.anchor.abs_diff(selection.cursor) + 1;
        let label = detail_surface_label(selection.surface);
        self.status = match count {
            1 => format!("1 {label} line selected"),
            n => format!("{n} {label} lines selected"),
        };
    }

    fn selected_sidebar_item(&self) -> Option<SidebarItem> {
        (self.focus == Focus::Sidebar)
            .then(|| self.sidebar_items().get(self.sidebar_selected).cloned())
            .flatten()
    }

    fn open_library_load(&mut self) {
        match self.selected_sidebar_item() {
            Some(SidebarItem::File { file_id, .. }) => self.open_schema_library_picker(file_id),
            Some(SidebarItem::Search { .. }) => {
                self.open_input(Mode::ImportSearches(self.default_search_folder_input()))
            }
            Some(SidebarItem::Bookmark { .. }) => {
                self.open_input(Mode::ImportBookmarks(self.default_bookmark_folder_input()))
            }
            Some(SidebarItem::Filter { .. } | SidebarItem::TimeFilter { .. }) | None => {
                self.open_input(Mode::LoadFilters(self.default_filter_folder_input()))
            }
            _ => self.open_input(Mode::LoadFilters(self.default_filter_folder_input())),
        }
    }

    fn open_library_save(&mut self) {
        match self.selected_sidebar_item() {
            Some(SidebarItem::File { file_id, .. }) => self.open_save_source_schema(file_id),
            Some(SidebarItem::Search { .. }) => {
                self.open_input(Mode::ExportSearches(self.default_search_folder_input()))
            }
            Some(SidebarItem::Bookmark { .. }) => {
                self.open_input(Mode::ExportBookmarks(self.default_bookmark_folder_input()))
            }
            Some(SidebarItem::Filter { .. } | SidebarItem::TimeFilter { .. }) | None => {
                self.open_input(Mode::ExportFilters(self.default_filter_folder_input()))
            }
            _ => self.open_input(Mode::ExportFilters(self.default_filter_folder_input())),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        // The chat panel is a focus, not a mode: when it has focus, keystrokes are its
        // input. `A` from anywhere else opens and focuses it.
        if self.focus == Focus::Chat {
            return self.handle_chat_key(key);
        }
        if let Some(pending) = self.bookmark_nav_pending.take() {
            if key.code == KeyCode::Char('m') && !key.modifiers.contains(KeyModifiers::CONTROL) {
                self.workspace.sidebar_width = pending.previous_sidebar_width;
                self.workspace.show_sidebar = pending.previous_show_sidebar;
                self.workspace.focus_mode = pending.previous_focus_mode;
                self.jump_bookmark(pending.forward, pending.count);
                return Ok(false);
            }
        }
        if key.code == KeyCode::Char('A') && !key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_ai_chat();
            self.input_cursor = 0;
            return Ok(false);
        }

        // Ctrl+<motion> never disturbs the selection, so you can travel to a distant
        // line and Space it in without losing what you already picked.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => {
                    self.save_project();
                    return Ok(false);
                }
                KeyCode::Char('p') => {
                    self.open_palette();
                    return Ok(false);
                }
                KeyCode::Char('r') => {
                    self.redo();
                    return Ok(false);
                }
                // Along a vertical (stacked) split, Ctrl+Up/Down resize the focused pane;
                // otherwise they travel while keeping the selection.
                KeyCode::Up => {
                    if self.panes.len() > 1 && self.split_mode == SplitMode::Vertical {
                        self.resize_pane(-1);
                    } else {
                        self.move_keeping_selection(-1);
                    }
                    return Ok(false);
                }
                KeyCode::Down => {
                    if self.panes.len() > 1 && self.split_mode == SplitMode::Vertical {
                        self.resize_pane(1);
                    } else {
                        self.move_keeping_selection(1);
                    }
                    return Ok(false);
                }
                KeyCode::PageUp => {
                    self.move_keeping_selection(-(self.active_page_size().max(1) as isize));
                    return Ok(false);
                }
                KeyCode::PageDown => {
                    self.move_keeping_selection(self.active_page_size().max(1) as isize);
                    return Ok(false);
                }
                KeyCode::Char('d') => {
                    self.move_keeping_selection((self.active_page_size() / 2).max(1) as isize);
                    return Ok(false);
                }
                KeyCode::Char('u') => {
                    self.move_keeping_selection(-((self.active_page_size() / 2).max(1) as isize));
                    return Ok(false);
                }
                KeyCode::Char('f') => {
                    self.move_keeping_selection(self.active_page_size().max(1) as isize);
                    return Ok(false);
                }
                KeyCode::Char('b') => {
                    self.move_keeping_selection(-(self.active_page_size().max(1) as isize));
                    return Ok(false);
                }
                // Along a horizontal (side-by-side) split, Ctrl+Left/Right resize the focused
                // pane; otherwise they scroll it horizontally.
                KeyCode::Right => {
                    if self.panes.len() > 1 && self.split_mode == SplitMode::Horizontal {
                        self.resize_pane(1);
                    } else if let Some(view) = self.active_view_mut() {
                        view.scroll_x += 8;
                    }
                    return Ok(false);
                }
                KeyCode::Left => {
                    if self.panes.len() > 1 && self.split_mode == SplitMode::Horizontal {
                        self.resize_pane(-1);
                    } else if let Some(view) = self.active_view_mut() {
                        view.scroll_x = view.scroll_x.saturating_sub(8);
                    }
                    return Ok(false);
                }
                _ => {}
            }
        }

        if let KeyCode::Char(ch) = key.code {
            if ch.is_ascii_digit() && !(ch == '0' && self.count == 0) {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(ch as usize - '0' as usize);
                return Ok(false);
            }
        }

        if self.g_pending {
            self.g_pending = false;
            self.count = 0;
            if key.code == KeyCode::Char('g') {
                self.clear_active_selection();
                if let Some(view) = self.active_view_mut() {
                    view.move_cursor_to(0);
                }
            }
            return Ok(false);
        }

        let count = self.count.max(1);
        self.count = 0;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.active_page_size().max(1) as isize;

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Tab => self.cycle_focus(1),
            KeyCode::BackTab => self.cycle_focus(-1),
            // Shift+<motion> grows or shrinks a contiguous run from where it started.
            // Scrolling past the viewport keeps extending, so runs span pages.
            KeyCode::Down if shift => self.extend_active_selection(count as isize),
            KeyCode::Up if shift => self.extend_active_selection(-(count as isize)),
            KeyCode::PageDown if shift => self.extend_active_selection(page),
            KeyCode::PageUp if shift => self.extend_active_selection(-page),
            KeyCode::Char(' ') => self.toggle_active_selection(),
            KeyCode::Char('y') => self.copy_selection(),
            KeyCode::Char('m') => self.toggle_bookmark_current(),
            KeyCode::Char('M') => self.open_bookmark_note(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_after_clearing_selection(count as isize);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_after_clearing_selection(-(count as isize));
            }
            KeyCode::Char('g') => {
                self.g_pending = true;
                self.count = 0;
            }
            KeyCode::Char('G') => {
                self.clear_active_selection();
                if let Some(view) = self.active_view_mut() {
                    let target = if count > 1 {
                        count - 1
                    } else {
                        view.visible.len().saturating_sub(1)
                    };
                    view.move_cursor_to(target);
                }
            }
            KeyCode::PageDown => {
                self.clear_active_selection();
                self.move_active_cursor(page);
            }
            KeyCode::PageUp => {
                self.clear_active_selection();
                self.move_active_cursor(-page);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(view) = self.active_view_mut() {
                    view.scroll_x = view.scroll_x.saturating_sub(8 * count);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(view) = self.active_view_mut() {
                    view.scroll_x += 8 * count;
                }
            }
            KeyCode::Char('0') | KeyCode::Home => {
                if let Some(view) = self.active_view_mut() {
                    view.scroll_x = 0;
                    if key.code == KeyCode::Home {
                        view.move_cursor_to(0);
                    }
                }
            }
            KeyCode::Char('$') | KeyCode::End => {
                if key.code == KeyCode::End {
                    if let Some(view) = self.active_view_mut() {
                        view.move_cursor_to(view.visible.len().saturating_sub(1));
                    }
                } else if let Some(view) = self.active_view_mut() {
                    view.scroll_x += 200;
                }
            }
            KeyCode::Char('/') => {
                let existing = self
                    .active_view()
                    .map(|view| view.query_text.clone())
                    .unwrap_or_default();
                self.open_input(Mode::Search(existing));
            }
            KeyCode::Char('n') => self.jump_match(true),
            KeyCode::Char('N') => self.jump_match(false),
            KeyCode::Char('c') => self.cycle_context(),
            // Esc backs out one layer at a time: selection first, then the search.
            KeyCode::Esc => {
                let selected = self.text_selection.is_some()
                    || self
                        .active_view()
                        .map(ViewModel::has_selection)
                        .unwrap_or(false);
                if selected {
                    self.clear_active_selection();
                    self.status = "selection cleared".to_string();
                } else {
                    self.clear_search();
                }
            }
            KeyCode::Char('f') => self.open_filter_builder(None),
            KeyCode::Char('t') => self.open_time_picker(),
            KeyCode::Char('T') => self.toggle_elapsed_mark(),
            KeyCode::Char('P') | KeyCode::Char('p')
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.open_pretty_print();
            }
            KeyCode::Char('E') => {
                self.open_input(Mode::ExportIncident(self.default_incident_file_input()))
            }
            KeyCode::Char('r') => self.refresh_dirty_sources(),
            KeyCode::Char('L') => self.open_library_load(),
            KeyCode::Char('X') => self.open_library_save(),
            KeyCode::Char('H') => self.begin_hide(),
            KeyCode::Char('a') => self.open_file_browser(),
            KeyCode::Char('o') => self.open_folder_browser(),
            KeyCode::Char('d') => self.delete_selected(),
            KeyCode::Delete if self.focus == Focus::Sidebar => self.delete_selected(),
            KeyCode::Char('|') | KeyCode::Char('\\') => self.split_active(SplitMode::Horizontal),
            KeyCode::Char('-') => self.split_active(SplitMode::Vertical),
            KeyCode::Char('[') => self.resize_sidebar_or_start_bookmark_nav(false, count),
            KeyCode::Char(']') => self.resize_sidebar_or_start_bookmark_nav(true, count),
            KeyCode::Char('z') => self.toggle_focus_mode(),
            KeyCode::Char('b') => self.cycle_timeline(),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('U') => self.mode = Mode::ActionLog,
            KeyCode::Char('w') => self.close_active_pane(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char(':') => self.open_palette(),
            KeyCode::Enter => match self.focus {
                Focus::Sidebar => self.activate_sidebar_item(),
                Focus::Results => self.jump_to_selected_result(),
                Focus::Pane => self.open_entry_detail_popup(),
                Focus::Chat => {}
            },
            _ => {}
        }

        Ok(false)
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        if matches!(self.mode, Mode::Search(_)) {
            return self.handle_search_key(key);
        }

        let caret = self.input_cursor;
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Tab => match &self.mode {
                // The pattern popup flips hide/keep.
                Mode::HidePattern(_) => {
                    if let Mode::HidePattern(prompt) = &mut self.mode {
                        prompt.exclude = !prompt.exclude;
                    }
                }
                // The raw filter editors switch back to the guided builder.
                Mode::Filter(text) => {
                    let text = text.clone();
                    self.open_filter_builder_from_text(&text, None);
                }
                Mode::EditFilter { index, text } => {
                    let (index, text) = (*index, text.clone());
                    self.open_filter_builder_from_text(&text, Some(index));
                }
                _ => {}
            },
            // Up/Down step the template ladder; no input popup is multi-line, so they are
            // free everywhere else.
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Down { 1 } else { -1 };
                if let Mode::HidePattern(prompt) = &mut self.mode {
                    prompt.pick(delta);
                    let caret = prompt.text.chars().count();
                    self.input_cursor = caret;
                }
            }
            KeyCode::Backspace => {
                if caret > 0 {
                    if let Some(input) = self.input_mut() {
                        remove_char(input, caret - 1);
                    }
                    self.input_cursor = caret - 1;
                }
            }
            KeyCode::Delete => {
                if let Some(input) = self.input_mut() {
                    if caret < input.chars().count() {
                        remove_char(input, caret);
                    }
                }
            }
            KeyCode::Left => self.input_cursor = caret.saturating_sub(1),
            KeyCode::Right => {
                let len = self.input_mut().map(|input| input.chars().count());
                if let Some(len) = len {
                    self.input_cursor = (caret + 1).min(len);
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => {
                if let Some(len) = self.input_mut().map(|input| input.chars().count()) {
                    self.input_cursor = len;
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = self.input_mut() {
                    input.clear();
                }
                self.input_cursor = 0;
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let Some(input) = self.input_mut() {
                        insert_char(input, caret, ch);
                    }
                    self.input_cursor = caret + 1;
                }
            }
            KeyCode::Enter => self.submit_input()?,
            _ => {}
        }
        Ok(false)
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let caret = self.input_cursor;
        let mut changed = false;
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                if caret > 0 {
                    if let Some(input) = self.input_mut() {
                        remove_char(input, caret - 1);
                    }
                    self.input_cursor = caret - 1;
                    changed = true;
                }
            }
            KeyCode::Delete => {
                if let Some(input) = self.input_mut() {
                    if caret < input.chars().count() {
                        remove_char(input, caret);
                        changed = true;
                    }
                }
            }
            KeyCode::Left => self.input_cursor = caret.saturating_sub(1),
            KeyCode::Right => {
                let len = self.input_mut().map(|input| input.chars().count());
                if let Some(len) = len {
                    self.input_cursor = (caret + 1).min(len);
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => {
                if let Some(len) = self.input_mut().map(|input| input.chars().count()) {
                    self.input_cursor = len;
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = self.input_mut() {
                    if !input.is_empty() {
                        input.clear();
                        changed = true;
                    }
                }
                self.input_cursor = 0;
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let Some(input) = self.input_mut() {
                        insert_char(input, caret, ch);
                    }
                    self.input_cursor = caret + 1;
                    changed = true;
                }
            }
            KeyCode::Enter => self.submit_input()?,
            _ => {}
        }

        if changed {
            if let Mode::Search(text) = &self.mode {
                self.apply_search_text(text.clone(), After::GotoFirstMatch);
            }
        }
        Ok(false)
    }

    /// Every path into an input popup routes through here so the caret starts at the
    /// end of the prefilled text instead of a stale offset.
    fn open_input(&mut self, mode: Mode) {
        self.input_cursor = match &mode {
            Mode::Search(text)
            | Mode::AddFile(text)
            | Mode::Filter(text)
            | Mode::ExportFilters(text)
            | Mode::LoadFilters(text)
            | Mode::ExportSearches(text)
            | Mode::ImportSearches(text)
            | Mode::ExportBookmarks(text)
            | Mode::ImportBookmarks(text)
            | Mode::ExportSchemas(text)
            | Mode::ImportSchemas(text)
            | Mode::ExportIncident(text)
            | Mode::Extractor(text)
            | Mode::SaveSourceSchema { text, .. }
            | Mode::LiveSchemaEditor { text, .. }
            | Mode::EditFilter { text, .. }
            | Mode::EditSearch { text, .. }
            | Mode::BookmarkNote { text, .. } => text.chars().count(),
            Mode::HidePattern(prompt) => prompt.text.chars().count(),
            _ => 0,
        };
        self.mode = mode;
    }

    fn handle_hide_choice_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::HideChoice(mut menu) = self.mode.clone() else {
            return Ok(false);
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            // Uppercase, so it cannot collide with the lowercase field keys.
            KeyCode::Char('H') => {
                self.begin_hide_pattern_from_one_line();
                return Ok(false);
            }
            // Arrows only: `j` and `k` are field keys here.
            KeyCode::Down => menu.move_cursor(1),
            KeyCode::Up => menu.move_cursor(-1),
            KeyCode::Char(' ') => menu.toggle(menu.cursor),
            KeyCode::Tab => menu.exclude = !menu.exclude,
            KeyCode::Enter => {
                self.apply_hide_menu(&menu);
                return Ok(false);
            }
            // A field's own key still hides by it in one press, as it always has. It
            // honours the current direction, so a keep menu keeps by it.
            KeyCode::Char(ch) => {
                if let Some(index) = hide_choice_index(ch) {
                    if let Some((field, _)) = menu.fields.get(index).cloned() {
                        self.hide_like(&field, "", menu.exclude);
                        return Ok(false);
                    }
                }
            }
            _ => {}
        }

        self.mode = Mode::HideChoice(menu);
        Ok(false)
    }

    /// Enter in the field menu. One field is an `equals` rule; several become a single
    /// regex over the raw line, which the user then vets in the pattern popup. Either way
    /// the menu's direction (hide or keep) carries through.
    fn apply_hide_menu(&mut self, menu: &HideMenu) {
        let chosen = menu.chosen();
        match chosen.len() {
            0 => self.mode = Mode::Normal,
            1 => self.hide_like(&chosen[0].0, "", menu.exclude),
            n => match self.combined_field_pattern(&chosen) {
                Some(pattern) => {
                    let fields: Vec<&str> =
                        chosen.iter().map(|(field, _)| field.as_str()).collect();
                    let verb = if menu.exclude { "hide" } else { "keep" };
                    self.status =
                        format!("{verb} pattern from {n} fields: {}", fields.join(" and "));
                    self.open_pattern_prompt_for("raw", Vec::new(), pattern, menu.exclude);
                }
                None => {
                    self.status = "this log format cannot combine fields".to_string();
                    self.mode = Mode::Normal;
                }
            },
        }
    }

    /// The targeted line's schema, with `chosen` pinned and every other field left free.
    fn combined_field_pattern(&self, chosen: &[(String, String)]) -> Option<String> {
        let (file, _) = self.active_file_view()?;
        let entry = file.entries.get(self.target_globals().first().copied()?)?;
        let pattern = file.extractor_for(entry)?.field_pattern(chosen)?;
        // A format that yields a regex the engine rejects is not usable for this.
        regex::Regex::new(&pattern).ok()?;
        Some(pattern)
    }

    fn handle_entry_detail_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        if matches!(self.mode, Mode::PrettyPrint { .. }) {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.mode = Mode::Normal,
                KeyCode::Char('y') => self.copy_detail_text(DetailSurface::PrettyPrint),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_pretty_print(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_pretty_print(-1),
                KeyCode::PageDown => self.scroll_pretty_print(10),
                KeyCode::PageUp => self.scroll_pretty_print(-10),
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.scroll_pretty_print(10)
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.scroll_pretty_print(-10)
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    if let Mode::PrettyPrint { scroll, .. } = &mut self.mode {
                        *scroll = 0;
                    }
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('P') | KeyCode::Char('p')
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.open_pretty_print();
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_entry_detail(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_entry_detail(-1),
            KeyCode::PageDown => self.scroll_entry_detail(10),
            KeyCode::PageUp => self.scroll_entry_detail(-10),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_entry_detail(10)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_entry_detail(-10)
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let Mode::EntryDetail { scroll } = &mut self.mode {
                    *scroll = 0;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        match key.code {
            KeyCode::Char('y') => self.copy_detail_text(DetailSurface::Help),
            _ => self.mode = Mode::Normal,
        }
        Ok(false)
    }

    fn scroll_entry_detail(&mut self, delta: isize) {
        if let Mode::EntryDetail { scroll } = &mut self.mode {
            *scroll = scroll.saturating_add_signed(delta);
        }
    }

    fn scroll_pretty_print(&mut self, delta: isize) {
        if let Mode::PrettyPrint { scroll, .. } = &mut self.mode {
            *scroll = scroll.saturating_add_signed(delta);
        }
    }

    fn input_mut(&mut self) -> Option<&mut String> {
        match &mut self.mode {
            Mode::Search(input)
            | Mode::AddFile(input)
            | Mode::Filter(input)
            | Mode::ExportFilters(input)
            | Mode::LoadFilters(input)
            | Mode::ExportSearches(input)
            | Mode::ImportSearches(input)
            | Mode::ExportBookmarks(input)
            | Mode::ImportBookmarks(input)
            | Mode::ExportSchemas(input)
            | Mode::ImportSchemas(input)
            | Mode::ExportIncident(input)
            | Mode::Extractor(input)
            | Mode::SaveSourceSchema { text: input, .. }
            | Mode::LiveSchemaEditor { text: input, .. }
            | Mode::EditFilter { text: input, .. }
            | Mode::EditSearch { text: input, .. }
            | Mode::BookmarkNote { text: input, .. } => Some(input),
            Mode::HidePattern(prompt) => Some(&mut prompt.text),
            _ => None,
        }
    }

    fn submit_input(&mut self) -> anyhow::Result<()> {
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Search(text) => self.submit_search(text),
            Mode::AddFile(path) => self.submit_add_file(path)?,
            Mode::Filter(text) => self.submit_filter(text),
            Mode::ExportFilters(folder) => self.submit_export_filters(folder),
            Mode::LoadFilters(folder) => self.submit_load_filters(folder),
            Mode::ExportSearches(folder) => self.submit_export_searches(folder),
            Mode::ImportSearches(folder) => self.submit_import_searches(folder),
            Mode::ExportBookmarks(folder) => self.submit_export_bookmarks(folder),
            Mode::ImportBookmarks(folder) => self.submit_import_bookmarks(folder),
            Mode::ExportSchemas(folder) => self.submit_export_schemas(folder),
            Mode::ImportSchemas(folder) => self.submit_import_schemas(folder),
            Mode::ExportIncident(path) => self.submit_export_incident(path),
            Mode::Extractor(text) => self.submit_extractor(text),
            Mode::SaveSourceSchema { file_id, text } => {
                self.submit_save_source_schema(file_id, text)
            }
            Mode::LiveSchemaEditor { mut editor, text } => {
                editor.schema = text.trim().to_string();
                self.mode = Mode::LiveSourceEditor(editor);
            }
            Mode::EditFilter { index, text } => self.submit_edit_filter(index, text),
            Mode::EditSearch { index, text } => self.submit_edit_search(index, text),
            Mode::BookmarkNote {
                file_id,
                line_no,
                text,
            } => self.submit_bookmark_note(file_id, line_no, text),
            Mode::HidePattern(prompt) => {
                self.submit_hide_pattern(prompt.text, prompt.field, prompt.exclude)
            }
            _ => {}
        }
        Ok(())
    }

    fn submit_search(&mut self, text: String) {
        let query_text = text.trim().to_string();
        if !query_text.is_empty() && !self.project.saved_searches.contains(&query_text) {
            self.project.saved_searches.insert(0, query_text.clone());
            self.project.saved_searches.truncate(8);
        }
        self.apply_search_text(query_text, After::GotoFirstMatch);
        self.save_project();
    }

    fn apply_search_text(&mut self, query_text: String, after: After) {
        if let Some(view) = self.active_view_mut() {
            view.query_text = query_text.trim().to_string();
            view.query = if view.query_text.is_empty() {
                None
            } else {
                Some(compile_query(&view.query_text))
            };
        }
        self.results_selected = 0;
        self.results_scroll = 0;
        // The scan spans frames, so jumping to the first match has to wait for it.
        self.queue_recompute(self.focused_pane, after);

        match self.active_view().and_then(|view| view.query.as_ref()) {
            Some(query) if !query.error.is_empty() => {
                self.status = format!("search: {}", query.error);
            }
            Some(_) | None => self.status.clear(),
        }
    }

    fn submit_add_file(&mut self, path: String) -> anyhow::Result<()> {
        let path = PathBuf::from(path.trim());
        if path.as_os_str().is_empty() {
            return Ok(());
        }

        if !path.exists() {
            self.status = format!("missing file: {}", path.display());
            return Ok(());
        }

        let added = self.project.add_file(&path, None);
        let (file_id, schema) = (added.file_id.clone(), added.extractor_name.clone());
        let needs_load = self
            .project
            .get_file(&file_id)
            .map(|file| !file.loaded)
            .unwrap_or(false);
        self.open_file_in_focused(&file_id);
        if needs_load {
            self.queue_load(&file_id);
        }
        // Name the schema: it was detected, not chosen, so say which one won.
        self.status = format!("opened {} as schema '{schema}'", path.display());
        self.autosave_project();
        Ok(())
    }

    fn submit_open_folder(&mut self, path: String) -> anyhow::Result<()> {
        let path = PathBuf::from(path.trim());
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        let Ok(folder) = std::fs::canonicalize(&path) else {
            self.status = format!("missing folder: {}", path.display());
            return Ok(());
        };
        if !folder.is_dir() {
            self.status = format!("not a folder: {}", folder.display());
            return Ok(());
        }

        self.capture_session();
        self.project.save().ok();

        let mut project = Project::load(&folder);
        let added = match project.add_text_files_from_dir(&folder) {
            Ok(added) => added,
            Err(error) => {
                self.status = format!("could not read folder {}: {error}", folder.display());
                return Ok(());
            }
        };

        let mut next = AppState::new(project);
        next.status = match added {
            0 => format!("opened {} with no new text files", folder.display()),
            1 => format!("opened {} with 1 text file", folder.display()),
            n => format!("opened {} with {n} text files", folder.display()),
        };
        next.queue_initial_loads();
        *self = next;
        Ok(())
    }

    /// Read the `f` popup's syntax into a rule. Shared by adding a filter and by editing
    /// one through its detail view, so both accept exactly the same text.
    fn parse_filter_rule(&self, text: &str) -> Result<FilterRule, String> {
        const USAGE: &str = "filter needs: [schema=<name>] field op [include|exclude] value";
        let mut tokens = shell_words::split(text).unwrap_or_else(|_| {
            text.split_whitespace()
                .map(|token| token.to_string())
                .collect()
        });

        let log_schema = tokens
            .first()
            .and_then(|token| log_schema_scope_from_token(token));
        if log_schema.is_some() {
            tokens.remove(0);
        }
        if let Some(log_schema) = log_schema.as_ref() {
            if !self.project.extractors.contains_key(log_schema) {
                return Err(format!("unknown log schema: {log_schema}"));
            }
        }
        if tokens.len() < 3 {
            return Err(USAGE.to_string());
        }

        let field = tokens[0].clone();
        let op = tokens[1].clone();
        let (action, value_start) = if matches!(
            tokens.get(2).map(String::as_str),
            Some("include" | "exclude")
        ) {
            (tokens[2].clone(), 3)
        } else {
            ("exclude".to_string(), 2)
        };
        let value = tokens[value_start..].join(" ");
        if value.is_empty() {
            return Err("filter value cannot be empty".to_string());
        }

        let mut rule = FilterRule::new(field, op, value, action);
        if let Some(log_schema) = log_schema {
            rule = rule.for_log_schema(log_schema);
        }
        Ok(rule)
    }

    fn submit_filter(&mut self, text: String) {
        let rule = match self.parse_filter_rule(&text) {
            Ok(rule) => rule,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        self.mutate_filters(|filters| filters.add(rule));
        self.status = "filter added".to_string();
    }

    /// Enter on a sidebar filter opened an editor on it; write the edit back in place so
    /// the rule keeps its position in the set (order decides nothing today, but the
    /// sidebar row would otherwise jump to the bottom under the cursor).
    fn submit_edit_filter(&mut self, index: usize, text: String) {
        let mut rule = match self.parse_filter_rule(&text) {
            Ok(rule) => rule,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if index >= self.project.filters.rules.len() {
            self.status = "that filter is gone".to_string();
            return;
        }
        // Editing the rule text must not silently re-enable a rule the user had off.
        rule.enabled = self.project.filters.rules[index].enabled;
        self.mutate_filters(|filters| filters.rules[index] = rule);
        self.status = "filter updated".to_string();
    }

    /// Space on a sidebar filter: enable or disable it without deleting it.
    fn toggle_filter_enabled(&mut self, index: usize) {
        let Some(rule) = self.project.filters.rules.get(index) else {
            return;
        };
        let now_enabled = !rule.enabled;
        self.mutate_filters(|filters| filters.rules[index].enabled = now_enabled);
        self.status = format!(
            "filter {}",
            if now_enabled { "enabled" } else { "disabled" }
        );
    }

    /// Space on a saved search: apply it to the focused pane, or clear it if it is
    /// already the pane's query.
    fn toggle_saved_search(&mut self, index: usize) {
        let Some(search) = self.project.saved_searches.get(index).cloned() else {
            return;
        };
        let active = self
            .active_view()
            .map(|view| view.query_text == search)
            .unwrap_or(false);
        if active {
            self.clear_search();
        } else {
            self.submit_search(search);
        }
    }

    /// Enter on a saved search opened an editor on it: rewrite the entry and run it.
    fn submit_edit_search(&mut self, index: usize, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            self.status = "search cannot be empty".to_string();
            return;
        }
        if let Some(slot) = self.project.saved_searches.get_mut(index) {
            *slot = text.clone();
        }
        // `submit_search` re-inserts at the head of the MRU; it already holds `text`, so
        // this only applies and saves it.
        self.submit_search(text);
        self.status = "search updated".to_string();
    }

    /// The project holds one time range, so a new one replaces whatever was there.
    fn apply_time_range(&mut self, value: String) {
        let rule = FilterRule::new("timestamp", "range", value.as_str(), "include");
        let label = summarize_time_range(&rule);
        self.mutate_filters(|filters| filters.set_time(rule));
        self.status = format!("time range: {label}");
    }

    // ---- AI assistant ----------------------------------------------------------------

    /// Open the chat panel and focus it, creating it (and its worker) on first use.
    fn open_ai_chat(&mut self) {
        if self.ai.is_none() {
            self.ai = Some(AiChat::new());
        }
        self.focus = Focus::Chat;
        // A first-run hint about the key, shown only while the transcript is empty.
        if let Some(ai) = &mut self.ai {
            if ai.transcript.is_empty() {
                let provider = ai.config.provider;
                if ai.resolved_key().is_none() {
                    ai.transcript.push(ChatLine::Error(format!(
                        "No API key. Set one for good with `logscout config set --api-key \
                         <key>`, or type /key <your-key> here, or set {} in the environment. \
                         Switch provider with /provider openai|anthropic|deepseek.",
                        provider.key_var()
                    )));
                } else {
                    ai.transcript.push(ChatLine::Info(format!(
                        "Ask me to troubleshoot these logs. Using {} ({}).",
                        provider.label(),
                        ai.config.model()
                    )));
                }
            }
        }
    }

    /// The project context handed to the model as a system prompt. Rebuilt each turn so it
    /// always reflects the current sources, filters, search, and focused view.
    fn ai_system_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "You are a log-troubleshooting assistant embedded in log-scouter, a terminal log \
             browser. Help the user find and understand problems in their server logs. Use the \
             tools to inspect the logs and to apply filters, searches, and time ranges on their \
             behalf; the UI updates as you do. Prefer inspecting (sample_lines, count_matches, \
             level_breakdown) before changing anything. Keep replies short and concrete.\n\n",
        );
        out.push_str(&format!(
            "Project folder: {}\n",
            self.project.root.display()
        ));

        out.push_str("Log sources:\n");
        let real: Vec<&LogFileModel> = self
            .project
            .files
            .iter()
            .filter(|file| !file.is_merged())
            .collect();
        if real.is_empty() {
            out.push_str("  (none loaded)\n");
        } else {
            for file in real {
                let state = if !file.error.is_empty() {
                    format!("error: {}", file.error)
                } else if file.loaded {
                    format!("{} entries", file.entries.len())
                } else {
                    "loading".to_string()
                };
                let label = if file.label.is_empty() {
                    String::new()
                } else {
                    format!(" (\"{}\")", file.label)
                };
                out.push_str(&format!(
                    "  - {}{label} [schema {}] {state}\n",
                    file.display_name, file.extractor_name
                ));
                if !file.description.is_empty() {
                    out.push_str(&format!("      note: {}\n", file.description));
                }
                if !file.tag.is_empty() {
                    out.push_str(&format!("      tag: {}\n", file.tag));
                }
            }
        }

        if let Some((file, view)) = self.active_file_view() {
            out.push_str(&format!(
                "\nFocused log: {} — showing {} of {} entries\n",
                file.display_name,
                view.visible.len(),
                file.entries.len()
            ));
            if let Some(entry) = file.entries.first() {
                if let Some(extractor) = file.extractor_for(entry) {
                    out.push_str(&format!("Fields: {}\n", extractor.field_names.join(", ")));
                }
            }
            if !view.query_text.trim().is_empty() {
                out.push_str(&format!("Active search: {}\n", view.query_text));
            }
        }

        out.push_str("\nFilters:\n");
        if self.project.filters.rules.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for rule in &self.project.filters.rules {
                out.push_str(&format!("  - {}\n", rule.describe()));
            }
        }

        out.push_str("\nBookmarks:\n");
        if self.project.bookmarks.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for bookmark in &self.project.bookmarks {
                let source = self
                    .project
                    .get_file(&bookmark.file_id)
                    .map(|file| file.display_name.clone())
                    .unwrap_or_else(|| bookmark.file_id.clone());
                let note = if bookmark.note.trim().is_empty() {
                    String::new()
                } else {
                    format!(" note: {}", bookmark.note.trim())
                };
                let preview = self
                    .bookmark_entry(bookmark)
                    .map(|(file, entry)| {
                        file.message(entry).lines().next().unwrap_or("").to_string()
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  - {source}:{}{} | {}\n",
                    bookmark.line_no, note, preview
                ));
            }
        }

        // Any skills the user has switched on with `/skill`, re-read each turn so edits to
        // the file take effect without reopening the chat.
        if let Some(ai) = &self.ai {
            for name in &ai.active_skills {
                if let Some(skill) = crate::ai::skills::load(name) {
                    out.push_str(&format!(
                        "\n--- Skill: {} ---\n{}\n",
                        skill.name,
                        skill.body.trim()
                    ));
                }
            }
        }
        out
    }

    /// Run one tool the model asked for, against the live project. Returns text the model
    /// reads next, and (as a side effect) mutates the UI through the normal paths.
    fn dispatch_ai_tool(&mut self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let string = |key: &str| {
            args.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        match name {
            ai_tools::LIST_SOURCES => Ok(self.ai_list_sources()),
            ai_tools::LIST_FILTERS => Ok(self.ai_list_filters()),
            ai_tools::LEVEL_BREAKDOWN => self.ai_level_breakdown(),
            ai_tools::SAMPLE_LINES => {
                let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(10);
                self.ai_sample_lines(count.clamp(1, 50) as usize)
            }
            ai_tools::COUNT_MATCHES => self.ai_count_matches(&string("query")),
            ai_tools::ADD_FILTER => self.ai_add_filter(
                &string("field"),
                &string("op"),
                &string("value"),
                &string("action"),
            ),
            ai_tools::SET_TIME_RANGE => self.ai_set_time_range(&string("start"), &string("end")),
            ai_tools::SEARCH => {
                let query = string("query");
                if query.trim().is_empty() {
                    return Err("search needs a non-empty query".to_string());
                }
                self.submit_search(query.clone());
                let shown = self.active_view().map(|v| v.visible.len()).unwrap_or(0);
                Ok(format!("searched for {query:?}; {shown} rows now shown"))
            }
            ai_tools::ADD_SOURCE => self.ai_add_source(&string("path")),
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn ai_list_sources(&self) -> String {
        let mut out = String::new();
        for file in self.project.files.iter().filter(|f| !f.is_merged()) {
            let state = if !file.error.is_empty() {
                format!("error: {}", file.error)
            } else if file.loaded {
                format!("{} entries", file.entries.len())
            } else {
                "loading".to_string()
            };
            let label = if file.label.is_empty() {
                String::new()
            } else {
                format!(" | label \"{}\"", file.label)
            };
            let note = if file.description.is_empty() {
                String::new()
            } else {
                format!(" | {}", file.description)
            };
            let tag = if file.tag.is_empty() {
                String::new()
            } else {
                format!(" | tag {}", file.tag)
            };
            out.push_str(&format!(
                "{} | schema {} | {state}{label}{tag}{note}\n",
                file.display_name, file.extractor_name
            ));
        }
        if out.is_empty() {
            "no log sources loaded".to_string()
        } else {
            out
        }
    }

    fn ai_list_filters(&self) -> String {
        if self.project.filters.rules.is_empty() {
            return "no filters applied".to_string();
        }
        self.project
            .filters
            .rules
            .iter()
            .map(|rule| rule.describe())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ai_sample_lines(&self, count: usize) -> Result<String, String> {
        let (file, view) = self.active_file_view().ok_or("no log is open")?;
        let mut out = String::new();
        for position in 0..view.visible.len().min(count) {
            if let Some(global) = view.visible.get(position) {
                if let Some(entry) = file.entries.get(global) {
                    let line = entry.raw.lines().next().unwrap_or("");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        if out.is_empty() {
            Ok("the focused view is empty".to_string())
        } else {
            Ok(out)
        }
    }

    fn ai_count_matches(&self, query: &str) -> Result<String, String> {
        if query.trim().is_empty() {
            return Err("count_matches needs a non-empty query".to_string());
        }
        let (file, _) = self.active_file_view().ok_or("no log is open")?;
        let compiled = compile_query(query);
        let matched = file
            .entries
            .iter()
            .filter(|entry| compiled.matches(file, entry))
            .count();
        Ok(format!(
            "{matched} of {} entries match {query:?}",
            file.entries.len()
        ))
    }

    fn ai_level_breakdown(&self) -> Result<String, String> {
        let (file, _) = self.active_file_view().ok_or("no log is open")?;
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for entry in &file.entries {
            let level = file.level(entry);
            let key = if level.trim().is_empty() {
                "(none)".to_string()
            } else {
                level
            };
            *counts.entry(key).or_insert(0) += 1;
        }
        if counts.is_empty() {
            return Ok("no entries".to_string());
        }
        Ok(counts
            .into_iter()
            .map(|(level, n)| format!("{level}: {n}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn ai_add_filter(
        &mut self,
        field: &str,
        op: &str,
        value: &str,
        action: &str,
    ) -> Result<String, String> {
        if field.trim().is_empty() || value.is_empty() {
            return Err("add_filter needs a field and a value".to_string());
        }
        if !matches!(op, "equals" | "contains" | "regex") {
            return Err(format!(
                "op must be equals, contains, or regex (got {op:?})"
            ));
        }
        if !matches!(action, "exclude" | "include") {
            return Err(format!(
                "action must be exclude or include (got {action:?})"
            ));
        }
        if op == "regex" && regex::Regex::new(value).is_err() {
            return Err(format!("invalid regex: {value}"));
        }
        let before = self.ai_visible_count();
        let rule = FilterRule::new(field, op, value, action);
        let describe = rule.describe();
        self.mutate_filters(|filters| filters.add(rule));
        let after = self.ai_visible_count();
        Ok(format!(
            "added filter [{describe}]; {before} -> {after} rows"
        ))
    }

    fn ai_set_time_range(&mut self, start: &str, end: &str) -> Result<String, String> {
        let start = start.trim();
        let end = end.trim();
        if start.is_empty() && end.is_empty() {
            return Err("set_time_range needs a start, an end, or both".to_string());
        }
        if !start.is_empty() && parse_datetime(start).is_none() {
            return Err(format!("start is not a timestamp: {start}"));
        }
        if !end.is_empty() && parse_datetime(end).is_none() {
            return Err(format!("end is not a timestamp: {end}"));
        }
        let before = self.ai_visible_count();
        self.apply_time_range(format!("{start}..{end}"));
        let after = self.ai_visible_count();
        Ok(format!(
            "time range {start}..{end}; {before} -> {after} rows"
        ))
    }

    fn ai_add_source(&mut self, path: &str) -> Result<String, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("add_source needs a path".to_string());
        }
        if !std::path::Path::new(path).is_file() {
            return Err(format!("no file at {path}"));
        }
        let file_id = self.project.add_file(path, None).file_id.clone();
        self.queue_load(&file_id);
        let name = self
            .project
            .get_file(&file_id)
            .map(|f| f.display_name.clone())
            .unwrap_or_default();
        Ok(format!("added source {name}; it is loading now"))
    }

    /// Count the entries the current filters admit in the focused log, computed directly so
    /// it is accurate before the async view recompute catches up.
    fn ai_visible_count(&self) -> usize {
        let Some((file, _)) = self.active_file_view() else {
            return 0;
        };
        if !self.project.filters.has_enabled_rules() {
            return file.entries.len();
        }
        let prepared = self.project.filters.prepare();
        file.entries
            .iter()
            .filter(|entry| prepared.visible(file, entry))
            .count()
    }

    /// Enter in the chat input: run a slash command, or ask the model a question.
    fn submit_ai_input(&mut self) {
        let text = match &self.ai {
            Some(ai) => ai.input.trim().to_string(),
            None => return,
        };
        if text.is_empty() {
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            self.ai_command(rest);
            if let Some(ai) = &mut self.ai {
                ai.input.clear();
            }
            self.input_cursor = 0;
            return;
        }

        if let Some(ai) = &mut self.ai {
            ai.transcript.push(ChatLine::User(text.clone()));
            ai.conversation.push(ChatMsg::user(text));
            ai.turns = 0;
            ai.input.clear();
            // Follow the newest lines as the answer streams in.
            ai.scroll = 0;
        }
        self.input_cursor = 0;
        self.ai_send_turn();
    }

    /// A `/`-prefixed line: set the key, pick the provider or model, or clear the chat.
    fn ai_command(&mut self, rest: &str) {
        let mut words = rest.split_whitespace();
        let command = words.next().unwrap_or("");
        let argument = words.next().unwrap_or("");
        // The rest of the line verbatim, for a value that keeps its spacing (a key).
        let tail = rest[command.len()..].trim();
        let Some(ai) = &mut self.ai else { return };
        match command {
            "key" => {
                if tail.is_empty() {
                    ai.transcript
                        .push(ChatLine::Error("usage: /key <api-key>".to_string()));
                } else {
                    let provider = ai.config.provider;
                    ai.keys.insert(provider, tail.to_string());
                    // Never echo the key back into the transcript.
                    ai.transcript.push(ChatLine::Info(format!(
                        "key set for {} (this session only, not saved to disk)",
                        provider.label()
                    )));
                }
            }
            "provider" => match crate::ai::Provider::from_label(argument) {
                Some(provider) => {
                    ai.config.provider = provider;
                    let _ = ai.config.save();
                    let has_key = ai.resolved_key().is_some();
                    ai.transcript.push(ChatLine::Info(format!(
                        "provider set to {} ({}){}",
                        provider.label(),
                        ai.config.model(),
                        if has_key {
                            String::new()
                        } else {
                            format!(" — set {}", provider.key_var())
                        }
                    )));
                }
                None => ai.transcript.push(ChatLine::Error(
                    "usage: /provider openai|anthropic|deepseek".to_string(),
                )),
            },
            "model" => {
                if argument.is_empty() {
                    ai.transcript
                        .push(ChatLine::Error("usage: /model <name>".to_string()));
                } else {
                    ai.config.model = argument.to_string();
                    let _ = ai.config.save();
                    ai.transcript
                        .push(ChatLine::Info(format!("model set to {argument}")));
                }
            }
            "skills" => Self::list_skills_into(ai),
            "skill" => {
                if argument.is_empty() {
                    Self::list_skills_into(ai);
                } else {
                    let available = crate::ai::skills::list();
                    if !available.iter().any(|skill| skill.name == argument) {
                        ai.transcript.push(ChatLine::Error(format!(
                            "no skill named {argument:?}; /skills lists what is available"
                        )));
                    } else if let Some(pos) =
                        ai.active_skills.iter().position(|name| name == argument)
                    {
                        ai.active_skills.remove(pos);
                        ai.transcript
                            .push(ChatLine::Info(format!("skill {argument:?} off")));
                    } else {
                        ai.active_skills.push(argument.to_string());
                        ai.transcript
                            .push(ChatLine::Info(format!("skill {argument:?} on")));
                    }
                }
            }
            "clear" => {
                ai.conversation.clear();
                ai.transcript.clear();
                ai.transcript
                    .push(ChatLine::Info("conversation cleared".to_string()));
            }
            other => ai.transcript.push(ChatLine::Error(format!(
                "unknown command /{other}; try /key, /provider, /model, /skill, /skills, /clear"
            ))),
        }
    }

    /// List the skills the user has authored, marking any that are switched on. A no-op-ish
    /// helper shared by `/skills` and a bare `/skill`.
    fn list_skills_into(ai: &mut AiChat) {
        let available = crate::ai::skills::list();
        if available.is_empty() {
            ai.transcript.push(ChatLine::Info(
                "no skills found. Create one at ~/.log-scouter/skills/<name>.md".to_string(),
            ));
            return;
        }
        ai.transcript.push(ChatLine::Info(
            "skills (/skill <name> to toggle):".to_string(),
        ));
        for skill in available {
            let mark = if ai.active_skills.contains(&skill.name) {
                "[on] "
            } else {
                ""
            };
            let desc = if skill.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", skill.description)
            };
            ai.transcript
                .push(ChatLine::Info(format!("  {mark}{}{desc}", skill.name)));
        }
    }

    /// Send the conversation (with a fresh system prompt) to the worker for one completion.
    fn ai_send_turn(&mut self) {
        let prompt = self.ai_system_prompt();
        let Some(ai) = &mut self.ai else { return };

        let Some(key) = ai.resolved_key() else {
            ai.transcript.push(ChatLine::Error(format!(
                "No API key. Type /key <your-key>, set {} in the environment, or add \
                 \"api_key\" to ~/.log-scouter/ai.json.",
                ai.config.provider.key_var()
            )));
            ai.pending = false;
            return;
        };

        let mut conversation = Vec::with_capacity(ai.conversation.len() + 1);
        conversation.push(ChatMsg::system(prompt));
        conversation.extend(ai.conversation.iter().cloned());

        ai.generation += 1;
        ai.turns += 1;
        ai.pending = true;

        let request = AgentRequest {
            generation: ai.generation,
            config: ai.config.clone(),
            key,
            conversation,
            tools: ai_tools::specs(),
        };
        if let Err(error) = ai.worker.send(request) {
            ai.transcript.push(ChatLine::Error(error));
            ai.pending = false;
        }
    }

    /// Drain the worker's replies, run any tools the model asked for, and continue the loop
    /// until it stops calling tools. Called once per frame.
    fn drain_ai_events(&mut self) {
        loop {
            let event = match &self.ai {
                Some(ai) if ai.pending => ai.worker.poll(),
                _ => None,
            };
            let Some(event) = event else { break };
            if self.apply_ai_event(event) {
                self.ai_send_turn();
            }
        }
    }

    /// Handle one worker reply. Records the assistant turn, runs any tools it asked for
    /// against the live project, and returns `true` when a follow-up turn should be sent.
    /// Split out from the drain loop so the whole agentic cycle can be tested by feeding
    /// scripted events, with no network.
    fn apply_ai_event(&mut self, event: crate::ai::AgentEvent) -> bool {
        // A reply to a superseded or cancelled question: drop it.
        let generation = self.ai.as_ref().map(|ai| ai.generation).unwrap_or(0);
        if event.generation != generation {
            return false;
        }

        let assistant = match event.result {
            Err(error) => {
                if let Some(ai) = &mut self.ai {
                    ai.transcript.push(ChatLine::Error(error));
                    ai.pending = false;
                }
                return false;
            }
            Ok(assistant) => assistant,
        };

        if let Some(ai) = &mut self.ai {
            ai.conversation.push(ChatMsg::assistant(
                assistant.text.clone(),
                assistant.tool_calls.clone(),
            ));
            if !assistant.text.trim().is_empty() {
                ai.transcript
                    .push(ChatLine::Assistant(assistant.text.clone()));
            }
        }

        if assistant.tool_calls.is_empty() {
            if let Some(ai) = &mut self.ai {
                ai.pending = false;
            }
            return false;
        }

        let turns = self.ai.as_ref().map(|ai| ai.turns).unwrap_or(0);
        if turns >= AI_MAX_TURNS {
            if let Some(ai) = &mut self.ai {
                ai.transcript
                    .push(ChatLine::Error("stopped: too many tool calls".to_string()));
                ai.pending = false;
            }
            return false;
        }

        // Run the tools against the live project (this mutates panels), gather results.
        let mut results = Vec::with_capacity(assistant.tool_calls.len());
        for call in &assistant.tool_calls {
            let (content, line) = match self.dispatch_ai_tool(&call.name, &call.arguments) {
                Ok(text) => {
                    let summary = text.lines().next().unwrap_or("").to_string();
                    (text, format!("ran {}: {summary}", call.name))
                }
                Err(error) => (
                    format!("error: {error}"),
                    format!("ran {}: error: {error}", call.name),
                ),
            };
            if let Some(ai) = &mut self.ai {
                ai.transcript.push(ChatLine::Action(line));
            }
            results.push(ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content,
            });
        }
        if let Some(ai) = &mut self.ai {
            ai.conversation.push(ChatMsg::tool_results(results));
        }
        true
    }

    /// Keystrokes while the chat panel has focus.
    fn handle_chat_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        match key.code {
            KeyCode::Esc => {
                // Cancel an in-flight turn (drop its reply), else leave the panel.
                let pending = self.ai.as_ref().map(|ai| ai.pending).unwrap_or(false);
                if pending {
                    if let Some(ai) = &mut self.ai {
                        ai.generation += 1;
                        ai.pending = false;
                        ai.transcript.push(ChatLine::Info("cancelled".to_string()));
                    }
                } else {
                    self.focus = Focus::Pane;
                }
            }
            KeyCode::Enter => self.submit_ai_input(),
            KeyCode::Backspace => {
                let caret = self.input_cursor;
                if caret > 0 {
                    if let Some(ai) = &mut self.ai {
                        remove_char(&mut ai.input, caret - 1);
                    }
                    self.input_cursor = caret - 1;
                }
            }
            KeyCode::Left => self.input_cursor = self.input_cursor.saturating_sub(1),
            KeyCode::Right => {
                let len = self
                    .ai
                    .as_ref()
                    .map(|ai| ai.input.chars().count())
                    .unwrap_or(0);
                self.input_cursor = (self.input_cursor + 1).min(len);
            }
            KeyCode::Up | KeyCode::PageUp => {
                if let Some(ai) = &mut self.ai {
                    ai.scroll = ai.scroll.saturating_add(1);
                }
            }
            KeyCode::Down | KeyCode::PageDown => {
                if let Some(ai) = &mut self.ai {
                    ai.scroll = ai.scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ai) = &mut self.ai {
                    ai.input.clear();
                }
                self.input_cursor = 0;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let caret = self.input_cursor;
                if let Some(ai) = &mut self.ai {
                    insert_char(&mut ai.input, caret, ch);
                }
                self.input_cursor = caret + 1;
            }
            _ => {}
        }
        Ok(false)
    }

    /// Delete on a sidebar filter row drops that rule. Without it the only way out of a
    /// time range is `F`, which takes every text filter with it.
    fn remove_selected_filter(&mut self) {
        let items = self.sidebar_items();
        let index = match items.get(self.sidebar_selected) {
            Some(SidebarItem::Filter { index, .. } | SidebarItem::TimeFilter { index, .. }) => {
                *index
            }
            _ => return,
        };
        if index >= self.project.filters.rules.len() {
            return;
        }
        let rule = &self.project.filters.rules[index];
        let removed = if rule.is_time_range() {
            format!("time range {}", summarize_time_range(rule))
        } else {
            rule.describe()
        };
        self.mutate_filters(|filters| {
            filters.rules.remove(index);
        });
        // The row under the cursor is gone; do not leave it past the end of the list.
        self.sidebar_selected = self
            .sidebar_selected
            .min(self.sidebar_items().len().saturating_sub(1));
        self.status = format!("filter removed: {removed}");
    }

    fn remove_saved_search(&mut self, index: usize) {
        if index >= self.project.saved_searches.len() {
            return;
        }
        let removed = self.project.saved_searches.remove(index);
        self.sidebar_selected = self
            .sidebar_selected
            .min(self.sidebar_items().len().saturating_sub(1));
        self.autosave_project();
        self.status = format!("search removed: /{removed}");
    }

    /// `d` (and `Delete` in the sidebar): delete whatever the cursor is on. In the sidebar
    /// that is the selected log source, filter (text or time), or saved search; in a pane it
    /// is that pane's log source.
    fn delete_selected(&mut self) {
        if self.focus != Focus::Sidebar {
            self.remove_active_file();
            return;
        }
        enum Target {
            File,
            Filter,
            Search(usize),
            Bookmark(usize),
            None,
        }
        let target = match self.sidebar_items().get(self.sidebar_selected) {
            Some(SidebarItem::File { .. }) => Target::File,
            Some(SidebarItem::Filter { .. } | SidebarItem::TimeFilter { .. }) => Target::Filter,
            Some(SidebarItem::Search { index, .. }) => Target::Search(*index),
            Some(SidebarItem::Bookmark { index, .. }) => Target::Bookmark(*index),
            _ => Target::None,
        };
        match target {
            Target::File => self.remove_active_file(),
            Target::Filter => self.remove_selected_filter(),
            Target::Search(index) => self.remove_saved_search(index),
            Target::Bookmark(index) => self.remove_bookmark(index),
            Target::None => {}
        }
    }

    /// A bad field leaves the popup open on the offending value rather than discarding
    /// everything the user typed.
    fn handle_time_picker_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::TimePicker(mut picker) = self.mode.clone() else {
            return Ok(false);
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            // Enter commits the Start and End fields. Moving onto a preset has already
            // filled them from it, so highlighting one and pressing Enter applies it.
            KeyCode::Enter => match picker.to_range() {
                Ok(value) => {
                    self.mode = Mode::Normal;
                    self.apply_time_range(value);
                    return Ok(false);
                }
                Err(error) => self.status = error,
            },
            KeyCode::Tab | KeyCode::Down => picker.move_row(1),
            KeyCode::BackTab | KeyCode::Up => picker.move_row(-1),
            // Space picks the preset under the cursor, matching Space everywhere else.
            KeyCode::Char(' ') if picker.on_preset() => picker.apply_preset(picker.row),
            // Digits are free on a preset row; on a field they are part of a timestamp.
            KeyCode::Char(ch) if picker.on_preset() && ch.is_ascii_digit() => {
                if let Some(index) = (ch as usize).checked_sub('1' as usize) {
                    if index < TIME_PRESETS.len() {
                        picker.row = index;
                        picker.apply_preset(index);
                    }
                }
            }
            KeyCode::Backspace if picker.cursor > 0 => {
                let caret = picker.cursor;
                if let Some(field) = picker.field_mut() {
                    remove_char(field, caret - 1);
                    picker.cursor = caret - 1;
                }
            }
            KeyCode::Delete => {
                let caret = picker.cursor;
                if let Some(field) = picker.field_mut() {
                    if caret < field.chars().count() {
                        remove_char(field, caret);
                    }
                }
            }
            KeyCode::Left if !picker.on_preset() => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Right if !picker.on_preset() => {
                let len = picker.field().map(|f| f.chars().count()).unwrap_or(0);
                picker.cursor = (picker.cursor + 1).min(len);
            }
            KeyCode::Home => picker.cursor = 0,
            KeyCode::End => {
                picker.cursor = picker.field().map(|f| f.chars().count()).unwrap_or(0);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(field) = picker.field_mut() {
                    field.clear();
                }
                picker.cursor = 0;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let caret = picker.cursor;
                if let Some(field) = picker.field_mut() {
                    insert_char(field, caret, ch);
                    picker.cursor = caret + 1;
                }
            }
            _ => {}
        }

        self.mode = Mode::TimePicker(picker);
        Ok(false)
    }

    /// `o`: browse for a folder, starting where the project already is.
    fn open_folder_browser(&mut self) {
        match FolderBrowser::open(self.browser_start_dir()) {
            Ok(browser) => self.mode = Mode::OpenFolder(browser),
            Err(error) => self.status = format!("could not read folder: {error}"),
        }
    }

    /// `a`: browse for a single file to add as a log source.
    fn open_file_browser(&mut self) {
        match FolderBrowser::open_for_file(self.browser_start_dir()) {
            Ok(browser) => self.mode = Mode::OpenFolder(browser),
            Err(error) => self.status = format!("could not read folder: {error}"),
        }
    }

    /// Where a browser opens: the project folder if it exists, else the current directory.
    fn browser_start_dir(&self) -> PathBuf {
        if self.project.root.is_dir() {
            self.project.root.clone()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    }

    // ---- Command palette --------------------------------------------------------------

    /// `Ctrl+P` / `:`: open the palette on the commands available in the current context.
    fn open_palette(&mut self) {
        self.mode = Mode::Palette(Palette::new(self.palette_commands()));
        self.input_cursor = 0;
    }

    /// The actions offered for what is focused right now, most specific first, then a set of
    /// general actions. Duplicates (e.g. "Ask AI") are dropped, keeping the first position.
    fn palette_commands(&self) -> Vec<Command> {
        let mut commands: Vec<Command> = Vec::new();
        match self.focus {
            Focus::Sidebar => match self.sidebar_items().get(self.sidebar_selected) {
                Some(SidebarItem::File { .. }) => commands.extend([
                    Command::OpenSource,
                    Command::MergeSource,
                    Command::EditItem,
                    Command::DeleteSelected,
                ]),
                Some(SidebarItem::Filter { .. } | SidebarItem::TimeFilter { .. }) => commands
                    .extend([
                        Command::ToggleItem,
                        Command::EditItem,
                        Command::DeleteSelected,
                    ]),
                Some(SidebarItem::Search { .. }) => commands.extend([
                    Command::ToggleItem,
                    Command::EditItem,
                    Command::DeleteSelected,
                ]),
                _ => {}
            },
            Focus::Pane => {
                let has_line = self
                    .active_view()
                    .map(|view| !view.visible.is_empty())
                    .unwrap_or(false);
                if has_line {
                    commands.extend([
                        Command::Copy,
                        Command::BookmarkLine,
                        Command::EditBookmarkNote,
                        Command::PreviousBookmark,
                        Command::NextBookmark,
                        Command::HideSimilar,
                        Command::PrettyPrint,
                        Command::MarkElapsed,
                        Command::ShowDetail,
                        Command::AskAi,
                    ]);
                }
                commands.extend([
                    Command::SplitColumns,
                    Command::SplitRows,
                    Command::ClosePane,
                ]);
            }
            _ => {}
        }
        commands.extend([
            Command::Search,
            Command::AddFilter,
            Command::TimeRange,
            Command::ClearFilters,
            Command::ImportFilters,
            Command::ExportFilters,
            Command::ImportSearches,
            Command::ExportSearches,
            Command::ImportSchemas,
            Command::ExportSchemas,
            Command::ExportIncident,
            Command::AskAi,
            Command::Undo,
            Command::Redo,
            Command::ActionHistory,
            Command::Timeline,
            Command::FocusMode,
            Command::ToggleSidebar,
            Command::ToggleDetail,
            Command::ToggleResults,
            Command::ToggleChat,
            Command::ChooseTheme,
            Command::AddFileBrowse,
            Command::AddLiveSource,
            Command::OpenFolder,
            Command::Help,
        ]);
        let mut unique: Vec<Command> = Vec::with_capacity(commands.len());
        for command in commands {
            if !unique.contains(&command) {
                unique.push(command);
            }
        }
        unique
    }

    /// Run one action. The single place each palette action's behaviour lives; it reuses the
    /// same operations the keys call.
    fn dispatch_command(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Search => {
                let existing = self
                    .active_view()
                    .map(|view| view.query_text.clone())
                    .unwrap_or_default();
                self.open_input(Mode::Search(existing));
            }
            Command::AddFilter => self.open_filter_builder(None),
            Command::ClearFilters => self.clear_filters(),
            Command::TimeRange => self.open_time_picker(),
            Command::ImportFilters => {
                self.open_input(Mode::LoadFilters(self.default_filter_folder_input()))
            }
            Command::ExportFilters => {
                self.open_input(Mode::ExportFilters(self.default_filter_folder_input()))
            }
            Command::ImportSearches => {
                self.open_input(Mode::ImportSearches(self.default_search_folder_input()))
            }
            Command::ExportSearches => {
                self.open_input(Mode::ExportSearches(self.default_search_folder_input()))
            }
            Command::ImportSchemas => {
                self.open_input(Mode::ImportSchemas(self.default_schema_folder_input()))
            }
            Command::ExportSchemas => {
                self.open_input(Mode::ExportSchemas(self.default_schema_folder_input()))
            }
            Command::ExportIncident => {
                self.open_input(Mode::ExportIncident(self.default_incident_file_input()))
            }
            Command::AskAi => self.open_ai_chat(),
            Command::Copy => self.copy_selection(),
            Command::BookmarkLine => self.toggle_bookmark_current(),
            Command::EditBookmarkNote => self.open_bookmark_note(),
            Command::PreviousBookmark => self.jump_bookmark(false, 1),
            Command::NextBookmark => self.jump_bookmark(true, 1),
            Command::HideSimilar => self.begin_hide(),
            Command::PrettyPrint => self.open_pretty_print(),
            Command::MarkElapsed => self.toggle_elapsed_mark(),
            Command::ShowDetail => self.open_entry_detail_popup(),
            Command::OpenSource => self.open_selected_source(),
            Command::MergeSource => self.toggle_active_selection(),
            Command::DeleteSelected => self.delete_selected(),
            Command::EditItem => self.activate_sidebar_item(),
            Command::ToggleItem => self.toggle_active_selection(),
            Command::SplitColumns => self.split_active(SplitMode::Horizontal),
            Command::SplitRows => self.split_active(SplitMode::Vertical),
            Command::ClosePane => self.close_active_pane(),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::ActionHistory => self.mode = Mode::ActionLog,
            Command::FocusMode => self.toggle_focus_mode(),
            Command::Timeline => self.cycle_timeline(),
            Command::ToggleSidebar => self.toggle_sidebar(),
            Command::ToggleDetail => self.toggle_detail(),
            Command::ToggleResults => self.toggle_results_panel(),
            Command::ToggleChat => self.toggle_chat_panel(),
            Command::ChooseTheme => self.open_theme_picker(),
            Command::AddFileBrowse => self.open_file_browser(),
            Command::AddLiveSource => self.open_live_source_editor(),
            Command::OpenFolder => self.open_folder_browser(),
            Command::Help => self.mode = Mode::Help,
        }
        Ok(())
    }

    /// Open the sidebar-selected source into the focused pane.
    fn open_selected_source(&mut self) {
        if let Some(SidebarItem::File { file_id, .. }) =
            self.sidebar_items().get(self.sidebar_selected)
        {
            let file_id = file_id.clone();
            self.open_file_in_focused(&file_id);
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::Palette(mut palette) = self.mode.clone() else {
            return Ok(false);
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            KeyCode::Enter => {
                let chosen = palette.filtered().get(palette.selected).copied();
                self.mode = Mode::Normal;
                if let Some(command) = chosen {
                    self.dispatch_command(command)?;
                }
                return Ok(false);
            }
            KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Down => {
                palette.selected += 1;
                palette.clamp();
            }
            KeyCode::Char('p') if ctrl => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Char('n') if ctrl => {
                palette.selected += 1;
                palette.clamp();
            }
            KeyCode::Backspace => {
                palette.query.pop();
                palette.selected = 0;
            }
            KeyCode::Char(ch) if !ctrl => {
                palette.query.push(ch);
                palette.selected = 0;
            }
            _ => {}
        }
        self.mode = Mode::Palette(palette);
        Ok(false)
    }

    fn open_theme_picker(&mut self) {
        self.mode = Mode::ThemePicker(ThemePicker::new(self.ui_config.theme));
    }

    fn handle_theme_picker_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::ThemePicker(mut picker) = self.mode.clone() else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let theme = picker.theme();
                self.mode = Mode::Normal;
                self.apply_theme(theme);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.pick(-1);
                self.mode = Mode::ThemePicker(picker);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.pick(1);
                self.mode = Mode::ThemePicker(picker);
            }
            _ => self.mode = Mode::ThemePicker(picker),
        }
        Ok(false)
    }

    fn apply_theme(&mut self, theme: ThemeName) {
        self.ui_config.theme = theme;
        match self.ui_config.save() {
            Ok(()) => self.status = format!("theme: {}", theme.label()),
            Err(error) => self.status = format!("theme not saved: {error}"),
        }
    }

    fn open_live_source_editor(&mut self) {
        self.mode = Mode::LiveSourceEditor(LiveSourceEditor::new());
    }

    fn handle_live_source_editor_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::LiveSourceEditor(mut editor) = self.mode.clone() else {
            return Ok(false);
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.submit_live_source_editor(editor);
            }
            KeyCode::Up => {
                editor.pick(-1);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Down | KeyCode::Tab => {
                editor.pick(1);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::BackTab => {
                editor.pick(-1);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Left if editor.field_for_row() == LiveSourceField::Kind => {
                editor.cycle_kind(-1);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Right if editor.field_for_row() == LiveSourceField::Kind => {
                editor.cycle_kind(1);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Right if live_quick_pick_field(editor.field_for_row()) => {
                self.begin_live_quick_pick(editor);
            }
            KeyCode::Backspace => {
                if let Some(field) = editor.field_mut() {
                    field.pop();
                }
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(field) = editor.field_mut() {
                    field.clear();
                }
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Char('i') if editor.field_for_row() == LiveSourceField::Schema && !ctrl => {
                if let Some(file_id) = editor.file_id.clone() {
                    self.save_live_source_editor_fields(&editor);
                    self.mode = Mode::Normal;
                    self.infer_schema_ai_for(file_id);
                } else {
                    editor.focus_field(LiveSourceField::Schema);
                    self.status =
                        "start the live source before asking the LLM to detect its schema"
                            .to_string();
                    self.mode = Mode::LiveSourceEditor(editor);
                }
            }
            KeyCode::Char('e') if editor.field_for_row() == LiveSourceField::Schema && !ctrl => {
                if let Some(file_id) = editor.file_id.clone() {
                    self.save_live_source_editor_fields(&editor);
                    self.open_schema_input_for(&file_id);
                } else {
                    let text = editor.schema.clone();
                    self.open_input(Mode::LiveSchemaEditor { editor, text });
                }
            }
            KeyCode::Char('L') if editor.field_for_row() == LiveSourceField::Schema && !ctrl => {
                if let Some(file_id) = editor.file_id.clone() {
                    self.save_live_source_editor_fields(&editor);
                    self.open_schema_library_picker(file_id);
                } else {
                    self.open_schema_library_picker_for_live_editor(editor);
                }
            }
            KeyCode::Char('X') if editor.field_for_row() == LiveSourceField::Schema && !ctrl => {
                if let Some(file_id) = editor.file_id.clone() {
                    self.save_live_source_editor_fields(&editor);
                    self.open_save_source_schema(file_id);
                } else {
                    editor.focus_field(LiveSourceField::Schema);
                    self.status = "start the live source before saving its schema".to_string();
                    self.mode = Mode::LiveSourceEditor(editor);
                }
            }
            KeyCode::Char('k') if editor.field_for_row() == LiveSourceField::Kind && !ctrl => {
                editor.set_kind(LiveSourceKind::Kubernetes);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Char('d') if editor.field_for_row() == LiveSourceField::Kind && !ctrl => {
                editor.set_kind(LiveSourceKind::Docker);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Char('j') if editor.field_for_row() == LiveSourceField::Kind && !ctrl => {
                editor.set_kind(LiveSourceKind::Journalctl);
                self.mode = Mode::LiveSourceEditor(editor);
            }
            KeyCode::Char(ch) if !ctrl => {
                if let Some(field) = editor.field_mut() {
                    field.push(ch);
                }
                self.mode = Mode::LiveSourceEditor(editor);
            }
            _ => self.mode = Mode::LiveSourceEditor(editor),
        }
        Ok(false)
    }

    fn save_live_source_editor_fields(&mut self, editor: &LiveSourceEditor) {
        let Some(file_id) = editor.file_id.as_ref() else {
            return;
        };
        let config = editor.config();
        let display_name = if editor.short_name.trim().is_empty() {
            config.default_name()
        } else {
            editor.short_name.trim().to_string()
        };
        if let Some(file) = self.project.get_file_mut(file_id) {
            file.display_name = display_name.clone();
            file.label = display_name;
            file.description = editor.description.trim().to_string();
            file.tag = editor.tag.trim().to_string();
            self.autosave_project();
        }
    }

    fn begin_live_quick_pick(&mut self, editor: LiveSourceEditor) {
        let target = editor.field_for_row();
        if !live_quick_pick_field(target) {
            self.mode = Mode::LiveSourceEditor(editor);
            return;
        }
        let request = editor.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match discover_live_quick_pick_options(&request, target) {
                Ok(options) => LivePickResult {
                    target,
                    options,
                    error: None,
                },
                Err(error) => LivePickResult {
                    target,
                    options: Vec::new(),
                    error: Some(error),
                },
            };
            let _ = tx.send(result);
        });
        self.live_pick_rx = Some(rx);
        self.mode = Mode::LiveQuickPick(LiveQuickPick {
            editor,
            target,
            options: Vec::new(),
            selected: 0,
            loading: true,
            message: format!("discovering {}", live_quick_pick_label(target)),
        });
    }

    fn handle_live_quick_pick_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::LiveQuickPick(mut picker) = self.mode.clone() else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Esc => {
                self.live_pick_rx = None;
                picker.editor.focus_field(picker.target);
                self.mode = Mode::LiveSourceEditor(picker.editor);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected = picker.selected.saturating_sub(1);
                self.mode = Mode::LiveQuickPick(picker);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected = (picker.selected + 1).min(picker.options.len().saturating_sub(1));
                self.mode = Mode::LiveQuickPick(picker);
            }
            KeyCode::Enter if !picker.loading => {
                if let Some(value) = picker.options.get(picker.selected).cloned() {
                    apply_live_quick_pick_value(&mut picker.editor, picker.target, &value);
                    self.status =
                        format!("{} selected: {value}", live_quick_pick_label(picker.target));
                }
                picker.editor.focus_field(picker.target);
                self.mode = Mode::LiveSourceEditor(picker.editor);
            }
            _ => self.mode = Mode::LiveQuickPick(picker),
        }
        Ok(false)
    }

    fn submit_live_source_editor(&mut self, editor: LiveSourceEditor) {
        let config = editor.config();
        if let Err(error) = config.validate() {
            self.status = format!("live source not added: {error}");
            return;
        }
        let schema = match self.schema_name_for_new_live_source(&editor.schema) {
            Ok(schema) => schema,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let display_name = if editor.short_name.trim().is_empty() {
            config.default_name()
        } else {
            editor.short_name.trim().to_string()
        };
        if let Some(file_id) = editor.file_id.clone() {
            self.update_live_source(file_id, config, display_name, editor, schema);
            return;
        }
        let file_id = {
            let file = self
                .project
                .add_live_source(config, display_name.clone(), Some(schema));
            file.label = display_name;
            file.description = editor.description.trim().to_string();
            file.tag = editor.tag.trim().to_string();
            file.loaded = true;
            file.file_id.clone()
        };
        self.open_file_in_focused(&file_id);
        self.autosave_project();
        self.start_live_source(&file_id);
    }

    fn update_live_source(
        &mut self,
        file_id: String,
        config: LiveSourceConfig,
        display_name: String,
        editor: LiveSourceEditor,
        schema: String,
    ) {
        let Some(file) = self.project.get_file(&file_id) else {
            self.status = "live source is gone".to_string();
            return;
        };
        let reset_stream = file.live.as_ref() != Some(&config) || file.extractor_name != schema;
        let spool_path = file.path.clone();

        if reset_stream {
            self.live_sources.remove(&file_id);
            if file.live.as_ref() != Some(&config) {
                let _ = fs::remove_file(&spool_path);
            }
            if let Err(error) = self.project.set_file_extractor(&file_id, &schema) {
                self.status = error;
                return;
            }
        }

        if let Some(file) = self.project.get_file_mut(&file_id) {
            file.display_name = display_name.clone();
            file.label = display_name;
            file.description = editor.description.trim().to_string();
            file.tag = editor.tag.trim().to_string();
            file.live = Some(config);
            if reset_stream {
                file.loaded = false;
            }
        }

        self.open_file_in_focused(&file_id);
        self.autosave_project();
        if reset_stream {
            self.queue_load(&file_id);
            self.requeue_all_panes();
        } else {
            self.status = "live source updated".to_string();
        }
    }

    fn schema_name_for_new_live_source(&mut self, text: &str) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(GENERIC_EXTRACTOR_NAME.to_string());
        }
        if !text.contains('|') && !looks_like_schema_json_input(text) {
            if self.project.extractors.contains_key(text) {
                return Ok(text.to_string());
            }
            return Err(format!("unknown log schema: {text}"));
        }

        let extractor = parse_log_schema_input(text)?;
        let name = extractor.name.clone();
        self.project.add_extractor(extractor)?;
        Ok(name)
    }

    fn handle_source_editor_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::SourceEditor(mut editor) = self.mode.clone() else {
            return Ok(false);
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.submit_source_editor(editor);
            }
            KeyCode::Up => {
                editor.pick(-1);
                self.mode = Mode::SourceEditor(editor);
            }
            KeyCode::Down | KeyCode::Tab => {
                editor.pick(1);
                self.mode = Mode::SourceEditor(editor);
            }
            KeyCode::BackTab => {
                editor.pick(-1);
                self.mode = Mode::SourceEditor(editor);
            }
            KeyCode::Backspace => {
                editor.field_mut().pop();
                self.mode = Mode::SourceEditor(editor);
            }
            KeyCode::Char('u') if ctrl => {
                editor.field_mut().clear();
                self.mode = Mode::SourceEditor(editor);
            }
            KeyCode::Char('i') if editor.row == SourceEditor::SCHEMA && !ctrl => {
                self.save_source_editor_fields(&editor);
                self.mode = Mode::Normal;
                self.infer_schema_ai_for(editor.file_id);
            }
            KeyCode::Char('e') if editor.row == SourceEditor::SCHEMA && !ctrl => {
                self.save_source_editor_fields(&editor);
                self.open_schema_input_for(&editor.file_id);
            }
            KeyCode::Char('L') if editor.row == SourceEditor::SCHEMA && !ctrl => {
                self.save_source_editor_fields(&editor);
                self.open_schema_library_picker(editor.file_id);
            }
            KeyCode::Char('X') if editor.row == SourceEditor::SCHEMA && !ctrl => {
                self.save_source_editor_fields(&editor);
                self.open_save_source_schema(editor.file_id);
            }
            KeyCode::Char(ch) if !ctrl => {
                editor.field_mut().push(ch);
                self.mode = Mode::SourceEditor(editor);
            }
            _ => self.mode = Mode::SourceEditor(editor),
        }
        Ok(false)
    }

    fn handle_schema_library_picker_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::SchemaLibraryPicker(mut picker) = self.mode.clone() else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Esc => self.restore_schema_picker_target(picker.target),
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected = picker.selected.saturating_sub(1);
                self.mode = Mode::SchemaLibraryPicker(picker);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected = (picker.selected + 1).min(picker.options.len().saturating_sub(1));
                self.mode = Mode::SchemaLibraryPicker(picker);
            }
            KeyCode::Enter => {
                let selected = picker.options.get(picker.selected).cloned();
                if let Some(schema) = selected {
                    match picker.target {
                        SchemaPickerTarget::File(file_id) => {
                            self.mode = Mode::Normal;
                            self.apply_library_schema(file_id, schema);
                        }
                        SchemaPickerTarget::LiveEditor(editor) => {
                            self.apply_library_schema_to_live_editor(editor, schema);
                        }
                    }
                } else {
                    self.restore_schema_picker_target(picker.target);
                }
            }
            _ => self.mode = Mode::SchemaLibraryPicker(picker),
        }
        Ok(false)
    }

    // ---- Guided filter builder --------------------------------------------------------

    /// `f` (new) or Enter on a text filter (edit): open the guided builder, prefilled from
    /// the active schema and, when editing, the existing rule.
    fn open_filter_builder(&mut self, edit_index: Option<usize>) {
        let builder = match edit_index {
            Some(index) => {
                let Some(rule) = self.project.filters.rules.get(index).cloned() else {
                    self.status = "that filter is gone".to_string();
                    return;
                };
                // The time range is edited with its picker, not the field builder.
                if rule.is_time_range() {
                    self.open_time_picker();
                    return;
                }
                self.builder_from_rule(&rule, Some(index))
            }
            None => self.empty_filter_builder(),
        };
        self.input_cursor = builder.value.chars().count();
        self.mode = Mode::FilterBuilder(builder);
    }

    /// Switch from the raw grammar editor into the builder, prefilled with the same rule.
    /// An unparseable line falls back to a fresh builder rather than losing the popup.
    fn open_filter_builder_from_text(&mut self, text: &str, edit_index: Option<usize>) {
        let builder = match self.parse_filter_rule(text) {
            Ok(rule) if !rule.is_time_range() => self.builder_from_rule(&rule, edit_index),
            _ => self.empty_filter_builder(),
        };
        self.input_cursor = builder.value.chars().count();
        self.mode = Mode::FilterBuilder(builder);
    }

    fn empty_filter_builder(&self) -> FilterBuilder {
        let fields = self.active_field_names();
        let mut schemas: Vec<String> = self.project.extractors.keys().cloned().collect();
        schemas.sort();
        // A sensible starting field: the level (the most common thing to filter on), else
        // the first field.
        let field = fields
            .iter()
            .find(|name| name.as_str() == "level" || name.ends_with("_level"))
            .or_else(|| fields.first())
            .cloned()
            .unwrap_or_else(|| "message".to_string());
        let mut builder = FilterBuilder {
            edit_index: None,
            schema: None,
            field,
            op: 0,
            exclude: true,
            value: String::new(),
            row: FilterBuilder::VALUE,
            schemas,
            fields,
            values: Vec::new(),
            preview: PatternPreview::default(),
            error: None,
        };
        self.refresh_builder(&mut builder);
        builder
    }

    fn builder_from_rule(&self, rule: &FilterRule, edit_index: Option<usize>) -> FilterBuilder {
        let fields = self.active_field_names();
        let mut schemas: Vec<String> = self.project.extractors.keys().cloned().collect();
        schemas.sort();
        let op = crate::core::filters::OPS
            .iter()
            .position(|name| *name == rule.op)
            .unwrap_or(0);
        let mut builder = FilterBuilder {
            edit_index,
            schema: rule.log_schema.clone(),
            field: rule.field.clone(),
            op,
            exclude: rule.action != "include",
            value: rule.value.clone(),
            row: FilterBuilder::VALUE,
            schemas,
            fields,
            values: Vec::new(),
            preview: PatternPreview::default(),
            error: None,
        };
        self.refresh_builder(&mut builder);
        builder
    }

    /// The field names of the focused pane's schema, for the field row's suggestions.
    fn active_field_names(&self) -> Vec<String> {
        let Some((file, _)) = self.active_file_view() else {
            return Vec::new();
        };
        if let Some(extractor) = file.extractor.as_ref() {
            if !extractor.field_names.is_empty() {
                return extractor.field_names.clone();
            }
        }
        file.entries
            .first()
            .and_then(|entry| file.extractor_for(entry))
            .map(|extractor| extractor.field_names.clone())
            .unwrap_or_default()
    }

    /// The most frequent values of `field` in the focused view, for the value row.
    fn field_value_suggestions(&self, field: &str) -> Vec<String> {
        let Some((file, view)) = self.active_file_view() else {
            return Vec::new();
        };
        if field.is_empty() {
            return Vec::new();
        }
        let mut order: Vec<String> = Vec::new();
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        // A sample is enough to surface the common values; no need to scan a million rows.
        for global in view.visible.iter().take(4000) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            let value = file.with_field(entry, field, |text| text.trim().to_string());
            if value.is_empty() || value.chars().count() > 60 {
                continue;
            }
            let count = counts.entry(value.clone()).or_insert(0);
            if *count == 0 {
                order.push(value.clone());
            }
            *count += 1;
        }
        order.sort_by(|a, b| counts[b].cmp(&counts[a]));
        order.truncate(8);
        order
    }

    /// How a rule would narrow the focused view: match count and a couple of sample lines.
    /// The general form of the hide-pattern preview.
    fn filter_preview(&self, rule: &FilterRule) -> PatternPreview {
        let mut preview = PatternPreview::default();
        let Some((file, view)) = self.active_file_view() else {
            return preview;
        };
        preview.total = view.visible.len();
        for global in view.visible.iter().take(PATTERN_PREVIEW_LIMIT) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            preview.scanned += 1;
            if !rule.matches(file, entry) {
                continue;
            }
            preview.matched += 1;
            if preview.samples.len() < PATTERN_PREVIEW_SAMPLES {
                preview
                    .samples
                    .push(file.message(entry).lines().next().unwrap_or("").to_string());
            }
        }
        preview
    }

    /// Recompute the value suggestions, live validation, and preview after a change.
    fn refresh_builder(&self, builder: &mut FilterBuilder) {
        builder.values = self.field_value_suggestions(builder.field.trim());
        match builder.rule() {
            Ok(rule) => {
                builder.error = None;
                builder.preview = self.filter_preview(&rule);
            }
            Err(error) => {
                builder.error = Some(error);
                builder.preview = PatternPreview::default();
            }
        }
    }

    /// Left/Right on the focused row: cycle the dropdown, or step through suggestions.
    fn builder_cycle(&self, builder: &mut FilterBuilder, delta: isize) {
        match builder.row {
            FilterBuilder::SCHEMA => {
                let mut options: Vec<Option<String>> = vec![None];
                options.extend(builder.schemas.iter().cloned().map(Some));
                let current = options
                    .iter()
                    .position(|option| *option == builder.schema)
                    .unwrap_or(0);
                builder.schema = options[cycle_index(current, options.len(), delta)].clone();
            }
            FilterBuilder::FIELD => {
                if builder.fields.is_empty() {
                    return;
                }
                let current = builder
                    .fields
                    .iter()
                    .position(|name| *name == builder.field)
                    .unwrap_or(0);
                builder.field =
                    builder.fields[cycle_index(current, builder.fields.len(), delta)].clone();
            }
            FilterBuilder::OP => {
                builder.op = cycle_index(builder.op, crate::core::filters::OPS.len(), delta);
            }
            FilterBuilder::ACTION => builder.exclude = !builder.exclude,
            FilterBuilder::VALUE => {
                if builder.values.is_empty() {
                    return;
                }
                let next = match builder
                    .values
                    .iter()
                    .position(|value| *value == builder.value)
                {
                    Some(current) => cycle_index(current, builder.values.len(), delta),
                    None if delta >= 0 => 0,
                    None => builder.values.len() - 1,
                };
                builder.value = builder.values[next].clone();
            }
            _ => {}
        }
    }

    fn handle_filter_builder_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::FilterBuilder(mut builder) = self.mode.clone() else {
            return Ok(false);
        };
        let mut changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.submit_filter_builder(builder);
                return Ok(false);
            }
            // Switch to the raw grammar editor, prefilled with the equivalent text.
            KeyCode::Tab => {
                let text = builder.to_input();
                match builder.edit_index {
                    Some(index) => self.open_input(Mode::EditFilter { index, text }),
                    None => self.open_input(Mode::Filter(text)),
                }
                return Ok(false);
            }
            KeyCode::Up => builder.row = builder.row.saturating_sub(1),
            KeyCode::Down => builder.row = (builder.row + 1).min(FilterBuilder::ROWS - 1),
            KeyCode::Left => {
                self.builder_cycle(&mut builder, -1);
                changed = true;
            }
            KeyCode::Right => {
                self.builder_cycle(&mut builder, 1);
                changed = true;
            }
            KeyCode::Char(' ') if builder.row == FilterBuilder::ACTION => {
                builder.exclude = !builder.exclude;
                changed = true;
            }
            KeyCode::Backspace => match builder.row {
                FilterBuilder::FIELD => {
                    builder.field.pop();
                    changed = true;
                }
                FilterBuilder::VALUE => {
                    builder.value.pop();
                    changed = true;
                }
                _ => {}
            },
            KeyCode::Char(ch) => match builder.row {
                FilterBuilder::FIELD => {
                    builder.field.push(ch);
                    changed = true;
                }
                FilterBuilder::VALUE => {
                    builder.value.push(ch);
                    changed = true;
                }
                _ => {}
            },
            _ => {}
        }
        if changed {
            self.refresh_builder(&mut builder);
        }
        self.input_cursor = match builder.row {
            FilterBuilder::FIELD => builder.field.chars().count(),
            FilterBuilder::VALUE => builder.value.chars().count(),
            _ => 0,
        };
        self.mode = Mode::FilterBuilder(builder);
        Ok(false)
    }

    fn submit_filter_builder(&mut self, builder: FilterBuilder) {
        let mut rule = match builder.rule() {
            Ok(rule) => rule,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        match builder.edit_index {
            Some(index) if index < self.project.filters.rules.len() => {
                // Editing the rule must not silently re-enable one the user had off.
                rule.enabled = self.project.filters.rules[index].enabled;
                self.mutate_filters(|filters| filters.rules[index] = rule);
                self.status = "filter updated".to_string();
            }
            Some(_) => self.status = "that filter is gone".to_string(),
            None => {
                self.mutate_filters(|filters| filters.add(rule));
                self.status = "filter added".to_string();
            }
        }
    }

    fn handle_folder_browser_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let Mode::OpenFolder(mut browser) = self.mode.clone() else {
            return Ok(false);
        };
        const HALF_PAGE: isize = 10;

        // Any step that re-reads a folder can fail on permissions. `go_to` and `parent`
        // put the browser back where it was, so all that is left is to say so.
        let mut failure = None;
        let walk = |browser: &mut FolderBrowser, target: Option<PathBuf>| {
            let target = target?;
            browser
                .go_to(target.clone())
                .err()
                .map(|error| format!("could not read {}: {error}", target.display()))
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            KeyCode::Down | KeyCode::Char('j') => browser.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => browser.move_selection(-1),
            KeyCode::PageDown => browser.move_selection(HALF_PAGE),
            KeyCode::PageUp => browser.move_selection(-HALF_PAGE),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                browser.move_selection(HALF_PAGE)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                browser.move_selection(-HALF_PAGE)
            }
            KeyCode::Home | KeyCode::Char('g') => browser.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                browser.selected = browser.rows.len().saturating_sub(1)
            }
            // Going up is always available, even from a folder listing no subfolders.
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                let parent = browser.parent_path();
                failure = walk(&mut browser, parent);
            }
            KeyCode::Char('.') => {
                if let Err(error) = browser.toggle_hidden() {
                    failure = Some(format!("could not read folder: {error}"));
                }
            }
            // In the file picker, fall back to typing a path (e.g. to paste an absolute one).
            KeyCode::Char('p') if matches!(browser.purpose, BrowserPurpose::File) => {
                self.open_input(Mode::AddFile(String::new()));
                return Ok(false);
            }
            // Descend only. Enter does that too, but also opens and goes up, depending on
            // the row; these keys keep their one meaning whatever is selected.
            KeyCode::Right | KeyCode::Char('l') => {
                let child = match browser.selected_row() {
                    Some(BrowserRow::Child(path)) => Some(path.clone()),
                    _ => None,
                };
                failure = walk(&mut browser, child);
            }
            KeyCode::Enter => match browser.selected_row().cloned() {
                Some(BrowserRow::OpenCurrent) => {
                    let folder = browser.current.to_string_lossy().to_string();
                    self.mode = Mode::Normal;
                    self.submit_open_folder(folder)?;
                    return Ok(false);
                }
                Some(BrowserRow::Parent) => {
                    let parent = browser.parent_path();
                    failure = walk(&mut browser, parent);
                }
                Some(BrowserRow::Child(path)) => failure = walk(&mut browser, Some(path)),
                // Picking a file: add it and close.
                Some(BrowserRow::File(path)) => {
                    self.mode = Mode::Normal;
                    self.submit_add_file(path.to_string_lossy().to_string())?;
                    return Ok(false);
                }
                None => {}
            },
            _ => {}
        }

        if let Some(error) = failure {
            self.status = error;
        }
        self.mode = Mode::OpenFolder(browser);
        Ok(false)
    }

    fn submit_export_filters(&mut self, folder: String) {
        let folder = self.filter_folder_from_input(&folder);
        match export_filters_to_folder(&self.project.filters, &folder) {
            Ok(0) => self.status = "no filters to export".to_string(),
            Ok(count) => {
                self.status = format!("exported {count} filter(s) to {}", folder.display());
            }
            Err(error) => {
                self.status = format!("export failed: {error}");
            }
        }
    }

    fn submit_load_filters(&mut self, folder: String) {
        let folder = self.filter_folder_from_input(&folder);
        if !folder.is_dir() {
            self.status = format!("no filter folder: {}", folder.display());
            return;
        }

        let loaded = match load_filters_from_folder(&folder) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.status = format!("load failed: {error}");
                return;
            }
        };
        if loaded.is_empty() {
            self.status = format!("no filter JSON files in {}", folder.display());
            return;
        }

        let count = loaded.len();
        self.mutate_filters(|filters| {
            for filter_file in loaded {
                if !filters.rules.contains(&filter_file.filter) {
                    filters.add(filter_file.filter);
                }
            }
        });
        self.status = format!("loaded {count} filter(s) from {}", folder.display());
    }

    fn submit_export_searches(&mut self, folder: String) {
        let folder = self.search_folder_from_input(&folder);
        match export_searches_to_folder(&self.project.saved_searches, &folder) {
            Ok(0) => self.status = "no saved searches to export".to_string(),
            Ok(count) => {
                self.status = format!("exported {count} saved search(es) to {}", folder.display());
            }
            Err(error) => {
                self.status = format!("export failed: {error}");
            }
        }
    }

    fn submit_import_searches(&mut self, folder: String) {
        let folder = self.search_folder_from_input(&folder);
        match self.install_default_search_library_if_default(&folder) {
            Ok(_) | Err(None) => {}
            Err(Some(error)) => {
                self.status = format!("load failed: {error}");
                return;
            }
        }
        if !folder.is_dir() {
            self.status = format!("no saved-search folder: {}", folder.display());
            return;
        }

        let loaded = match load_searches_from_folder(&folder) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.status = format!("load failed: {error}");
                return;
            }
        };
        if loaded.is_empty() {
            self.status = format!("no saved-search JSON files in {}", folder.display());
            return;
        }

        let mut added = 0;
        let mut skipped = 0;
        for search in loaded {
            let query = search.query.trim().to_string();
            if query.is_empty() {
                skipped += 1;
                continue;
            }
            if self.project.saved_searches.contains(&query) {
                skipped += 1;
                continue;
            }
            self.project.saved_searches.push(query);
            added += 1;
        }

        self.autosave_project();
        self.status = match skipped {
            0 => format!("loaded {added} saved search(es) from {}", folder.display()),
            skipped => format!(
                "loaded {added} saved search(es), skipped {skipped} from {}",
                folder.display()
            ),
        };
    }

    fn submit_export_bookmarks(&mut self, folder: String) {
        let folder = self.bookmark_folder_from_input(&folder);
        if self.project.bookmarks.is_empty() {
            self.status = "no bookmarks to export".to_string();
            return;
        }
        if let Err(error) = fs::create_dir_all(&folder) {
            self.status = format!("export failed: {error}");
            return;
        }

        let mut written = 0usize;
        for (index, bookmark) in self.project.bookmarks.iter().enumerate() {
            let Some(bookmark_file) = self.bookmark_to_file(index + 1, bookmark) else {
                continue;
            };
            let path = folder.join(format!(
                "{:03}-{}.json",
                index + 1,
                sanitize_file_component(&bookmark_file.name)
            ));
            let body = match serde_json::to_string_pretty(&bookmark_file) {
                Ok(body) => body,
                Err(error) => {
                    self.status = format!("export failed: {error}");
                    return;
                }
            };
            if let Err(error) = fs::write(path, body) {
                self.status = format!("export failed: {error}");
                return;
            }
            written += 1;
        }
        self.status = format!("exported {written} bookmark(s) to {}", folder.display());
    }

    fn submit_import_bookmarks(&mut self, folder: String) {
        let folder = self.bookmark_folder_from_input(&folder);
        if !folder.is_dir() {
            self.status = format!("no bookmark folder: {}", folder.display());
            return;
        }
        let mut paths = match json_file_paths(&folder) {
            Ok(paths) => paths,
            Err(error) => {
                self.status = format!("load failed: {error}");
                return;
            }
        };
        paths.sort();
        if paths.is_empty() {
            self.status = format!("no bookmark JSON files in {}", folder.display());
            return;
        }

        let mut added = 0usize;
        let mut skipped = 0usize;
        for path in paths {
            let body = match fs::read_to_string(&path) {
                Ok(body) => body,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let file = match serde_json::from_str::<BookmarkFile>(&body) {
                Ok(file) => file,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let Some(file_id) = self.bookmark_file_id_from_library_path(&file.file_path) else {
                skipped += 1;
                continue;
            };
            let bookmark = Bookmark {
                file_id,
                line_no: file.line_no,
                note: if file.note.trim().is_empty() {
                    file.description
                } else {
                    file.note
                },
            };
            if self.project.bookmarks.contains(&bookmark) {
                skipped += 1;
                continue;
            }
            self.project.bookmarks.push(bookmark);
            added += 1;
        }

        self.autosave_project();
        self.status = match skipped {
            0 => format!("loaded {added} bookmark(s) from {}", folder.display()),
            skipped => format!(
                "loaded {added} bookmark(s), skipped {skipped} from {}",
                folder.display()
            ),
        };
    }

    fn bookmark_to_file(&self, index: usize, bookmark: &Bookmark) -> Option<BookmarkFile> {
        let file = self.project.get_file(&bookmark.file_id)?;
        let source = self.project.rel(&file.path);
        let preview = self
            .bookmark_entry(bookmark)
            .map(|(_, entry)| entry.raw.lines().next().unwrap_or("").to_string())
            .unwrap_or_default();
        let note = bookmark.note.trim();
        let name = if note.is_empty() {
            format!(
                "bookmark-{index:03}-{}-{}",
                file.display_name, bookmark.line_no
            )
        } else {
            format!("bookmark-{index:03}-{}", field_value_preview(note))
        };
        Some(BookmarkFile {
            name,
            description: preview,
            file_path: source,
            line_no: bookmark.line_no,
            note: bookmark.note.clone(),
        })
    }

    fn bookmark_file_id_from_library_path(&self, file_path: &str) -> Option<String> {
        let raw = file_path.trim();
        if raw.is_empty() {
            return None;
        }
        let path = PathBuf::from(raw);
        let path = if path.is_absolute() {
            path
        } else {
            self.project.root.join(path)
        };
        self.project
            .files
            .iter()
            .find(|file| !file.is_merged() && file.path == path)
            .map(|file| file.file_id.clone())
    }

    fn submit_export_schemas(&mut self, folder: String) {
        let folder = self.schema_folder_from_input(&folder);
        // Project order is a HashMap's order, which is not stable across runs. Sort so
        // the same project always exports the same filenames.
        let mut schemas: Vec<Extractor> = self.project.extractors.values().cloned().collect();
        schemas.sort_by(|left, right| left.name.cmp(&right.name));

        match export_schemas_to_folder(&schemas, &folder) {
            Ok(0) => self.status = "no log schemas to export".to_string(),
            Ok(count) => {
                self.status = format!("exported {count} log schema(s) to {}", folder.display());
            }
            Err(error) => self.status = format!("export failed: {error}"),
        }
    }

    fn submit_import_schemas(&mut self, folder: String) {
        let folder = self.schema_folder_from_input(&folder);
        if !folder.is_dir() {
            self.status = format!("no schema folder: {}", folder.display());
            return;
        }

        let loaded = match load_schemas_from_folder(&folder) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.status = format!("import failed: {error}");
                return;
            }
        };
        if loaded.is_empty() {
            self.status = format!("no schema JSON files in {}", folder.display());
            return;
        }

        // A name already in the project is left alone. Overwriting it would silently
        // change how every file using that schema parses, and force a reload of each.
        let (mut added, mut skipped) = (0, 0);
        for schema_file in loaded {
            if self.project.extractors.contains_key(&schema_file.name) {
                skipped += 1;
                continue;
            }
            match self.project.add_extractor(schema_file.schema) {
                Ok(()) => added += 1,
                Err(error) => {
                    self.status = format!("import failed: {error}");
                    return;
                }
            }
        }

        self.autosave_project();
        self.status = match skipped {
            0 => format!("imported {added} log schema(s) from {}", folder.display()),
            skipped => {
                format!("imported {added} log schema(s), skipped {skipped} already in this project")
            }
        };
    }

    fn submit_extractor(&mut self, text: String) {
        let text = text.trim();
        if !text.contains('|') && !looks_like_schema_json_input(text) {
            self.apply_existing_schema_to_target(text);
            return;
        }

        let extractor = match parse_log_schema_input(text) {
            Ok(extractor) => extractor,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let name = extractor.name.clone();
        if let Err(error) = self.project.add_extractor(extractor) {
            self.status = error;
            return;
        }

        // Schemas are per file: apply it to the one being looked at. `autosave_project`
        // rather than `save_project` so the informative status is not overwritten.
        let Some(file_id) = self.schema_target() else {
            self.autosave_project();
            self.status = format!("schema saved: {name}");
            return;
        };
        self.apply_schema_to_file(&file_id, &name);
    }

    fn apply_schema_text_to_file(&mut self, file_id: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.status = "schema name cannot be empty".to_string();
            return;
        }
        if !text.contains('|') && !looks_like_schema_json_input(text) {
            if !self.project.extractors.contains_key(text) {
                self.status = format!("unknown log schema: {text}");
                return;
            }
            self.apply_schema_to_file(file_id, text);
            return;
        }

        let extractor = match parse_log_schema_input(text) {
            Ok(extractor) => extractor,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let name = extractor.name.clone();
        if let Err(error) = self.project.add_extractor(extractor) {
            self.status = error;
            return;
        }
        self.apply_schema_to_file(file_id, &name);
    }

    fn open_schema_library_picker(&mut self, file_id: String) {
        self.open_schema_library_picker_for_target(SchemaPickerTarget::File(file_id));
    }

    fn open_schema_library_picker_for_live_editor(&mut self, editor: LiveSourceEditor) {
        self.open_schema_library_picker_for_target(SchemaPickerTarget::LiveEditor(editor));
    }

    fn open_schema_library_picker_for_target(&mut self, target: SchemaPickerTarget) {
        let folder = self.default_schema_folder_path();
        if !folder.is_dir() {
            self.status = format!("no schema folder: {}", folder.display());
            self.restore_schema_picker_target(target);
            return;
        }
        let loaded = match load_schemas_from_folder(&folder) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.status = format!("load failed: {error}");
                self.restore_schema_picker_target(target);
                return;
            }
        };
        let mut options: Vec<Extractor> = loaded
            .into_iter()
            .map(|schema_file| {
                let mut schema = schema_file.schema;
                if schema.description.trim().is_empty() {
                    schema.description = schema_file.description;
                }
                schema
            })
            .collect();
        options.sort_by(|left, right| left.name.cmp(&right.name));
        if options.is_empty() {
            self.status = format!("no schema JSON files in {}", folder.display());
            self.restore_schema_picker_target(target);
            return;
        }
        self.mode = Mode::SchemaLibraryPicker(SchemaLibraryPicker {
            target,
            options,
            selected: 0,
        });
    }

    fn restore_schema_picker_target(&mut self, target: SchemaPickerTarget) {
        match target {
            SchemaPickerTarget::File(file_id) => {
                let mode = self.project.get_file(&file_id).map(|file| {
                    if file.is_live() {
                        let mut editor = LiveSourceEditor::from_file(file);
                        editor.focus_field(LiveSourceField::Schema);
                        Mode::LiveSourceEditor(editor)
                    } else {
                        let mut editor = SourceEditor::new(file);
                        editor.row = SourceEditor::SCHEMA;
                        Mode::SourceEditor(editor)
                    }
                });
                self.mode = mode.unwrap_or(Mode::Normal);
            }
            SchemaPickerTarget::LiveEditor(mut editor) => {
                editor.focus_field(LiveSourceField::Schema);
                self.mode = Mode::LiveSourceEditor(editor);
            }
        }
    }

    fn apply_library_schema(&mut self, file_id: String, schema: Extractor) {
        let name = schema.name.clone();
        if !self.project.extractors.contains_key(&name) {
            if let Err(error) = self.project.add_extractor(schema) {
                self.status = format!("schema import failed: {error}");
                return;
            }
        }
        self.apply_schema_to_file(&file_id, &name);
    }

    fn apply_library_schema_to_live_editor(
        &mut self,
        mut editor: LiveSourceEditor,
        schema: Extractor,
    ) {
        let name = schema.name.clone();
        if !self.project.extractors.contains_key(&name) {
            if let Err(error) = self.project.add_extractor(schema) {
                self.status = format!("schema import failed: {error}");
                editor.focus_field(LiveSourceField::Schema);
                self.mode = Mode::LiveSourceEditor(editor);
                return;
            }
        }
        editor.schema = name;
        editor.focus_field(LiveSourceField::Schema);
        self.mode = Mode::LiveSourceEditor(editor);
        self.status = "schema selected".to_string();
    }

    fn open_save_source_schema(&mut self, file_id: String) {
        let Some(file) = self.project.get_file(&file_id) else {
            return;
        };
        let Some(schema) = file.extractor.as_ref() else {
            self.status = "this source has no schema to save".to_string();
            return;
        };
        let text = if schema.description.trim().is_empty() {
            schema.name.clone()
        } else {
            format!("{} | {}", schema.name, schema.description)
        };
        self.open_input(Mode::SaveSourceSchema { file_id, text });
    }

    fn submit_save_source_schema(&mut self, file_id: String, text: String) {
        let Some(file) = self.project.get_file(&file_id) else {
            self.status = "source is gone".to_string();
            return;
        };
        let Some(mut schema) = file.extractor.clone() else {
            self.status = "this source has no schema to save".to_string();
            return;
        };
        let (name, description) = match text.split_once('|') {
            Some((name, description)) => (name.trim(), description.trim()),
            None => (text.trim(), schema.description.trim()),
        };
        if name.is_empty() {
            self.status = "schema name cannot be empty".to_string();
            return;
        }
        schema.name = name.to_string();
        schema.description = description.to_string();
        let folder = self.default_schema_folder_path();
        match export_schemas_to_folder(&[schema], &folder) {
            Ok(1) => self.status = format!("saved schema '{name}' to {}", folder.display()),
            Ok(_) => self.status = "no schema saved".to_string(),
            Err(error) => self.status = format!("schema save failed: {error}"),
        }
    }

    fn apply_existing_schema_to_target(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.status = "schema name cannot be empty".to_string();
            return;
        }
        if !self.project.extractors.contains_key(name) {
            self.status = format!("unknown log schema: {name}");
            return;
        }
        let Some(file_id) = self.schema_target() else {
            self.status = "select a log file to apply schema".to_string();
            return;
        };
        self.apply_schema_to_file(&file_id, name);
    }

    fn apply_schema_to_file(&mut self, file_id: &str, name: &str) {
        let display = self
            .project
            .get_file(file_id)
            .map(|file| file.display_name.clone())
            .unwrap_or_default();
        self.live_sources.remove(file_id);
        if let Err(error) = self.project.set_file_extractor(file_id, name) {
            self.autosave_project();
            self.status = error;
            return;
        }

        // Multi-line grouping depends on the schema, so the file must be re-read.
        self.repoint_panes(file_id);
        self.queue_load(file_id);
        self.requeue_all_panes();
        self.autosave_project();
        self.status = format!("schema '{name}' applied to {display}");
    }

    fn hide_like(&mut self, dimension: &str, keyword: &str, exclude: bool) {
        // Honour a single Space-picked line even when the cursor has moved off it.
        let Some((file_id, global_index)) = self
            .active_view()
            .map(|view| view.file_id.clone())
            .zip(self.target_globals().first().copied())
        else {
            self.mode = Mode::Normal;
            return;
        };
        let Some(file) = self.project.get_file(&file_id) else {
            self.mode = Mode::Normal;
            return;
        };
        let Some(entry) = file.entries.get(global_index) else {
            self.mode = Mode::Normal;
            return;
        };
        let rule = hide_like(file, entry, dimension, keyword, exclude);
        self.mutate_filters(|filters| filters.add(rule));
        self.status = if exclude {
            "hide rule added"
        } else {
            "keep rule added"
        }
        .to_string();
        self.mode = Mode::Normal;
    }

    fn extend_active_selection(&mut self, delta: isize) {
        // In the sidebar a range extends over log files, not log lines.
        if self.focus == Focus::Sidebar {
            self.extend_sidebar_selection(delta);
            return;
        }
        if self.focus != Focus::Pane {
            self.move_selection(delta);
            return;
        }
        if let Some(view) = self.active_view_mut() {
            view.extend_selection(delta);
        }
        self.report_selection();
    }

    fn move_keeping_selection(&mut self, delta: isize) {
        if let Some(view) = self.active_view_mut() {
            view.move_keeping_selection(delta);
        }
    }

    /// Space means "select or deselect this thing", whatever the cursor is on: a log line
    /// joins the selection, a log source joins the pane's view, a filter turns on or off,
    /// a saved search runs or stops.
    fn toggle_active_selection(&mut self) {
        if self.focus == Focus::Sidebar {
            let items = self.sidebar_items();
            match items.get(self.sidebar_selected) {
                Some(SidebarItem::File { .. }) => self.toggle_file_in_view(),
                Some(SidebarItem::Filter { index, .. } | SidebarItem::TimeFilter { index, .. }) => {
                    let index = *index;
                    self.toggle_filter_enabled(index);
                }
                Some(SidebarItem::Search { index, .. }) => {
                    let index = *index;
                    self.toggle_saved_search(index);
                }
                Some(SidebarItem::Bookmark { index, .. }) => {
                    let index = *index;
                    self.jump_bookmark_index(index);
                }
                _ => {}
            }
            return;
        }
        if self.focus != Focus::Pane {
            return;
        }
        // Space picks whole lines, so a character run is no longer what is selected.
        self.text_selection = None;
        if let Some(view) = self.active_view_mut() {
            view.toggle_current();
        }
        self.report_selection();
    }

    /// Sidebar item indices of the file rows, in order.
    fn file_item_indices(&self) -> Vec<usize> {
        self.sidebar_items()
            .iter()
            .enumerate()
            .filter_map(|(index, item)| matches!(item, SidebarItem::File { .. }).then_some(index))
            .collect()
    }

    fn file_id_at(&self, item_index: usize) -> Option<String> {
        match self.sidebar_items().get(item_index) {
            Some(SidebarItem::File { file_id, .. }) => Some(file_id.clone()),
            _ => None,
        }
    }

    /// Shift+Up/Down in the sidebar: sweep a contiguous run of logs into the view.
    fn extend_sidebar_selection(&mut self, delta: isize) {
        let files = self.file_item_indices();
        if files.is_empty() {
            return;
        }

        // Anchor on the first Shift press, snapping to a file row if a section header
        // happens to be selected. A plain motion clears the anchor again.
        if self.sidebar_anchor.is_none() {
            let anchor = files
                .iter()
                .copied()
                .min_by_key(|index| index.abs_diff(self.sidebar_selected))
                .expect("files is non-empty");
            self.sidebar_anchor = Some(anchor);
            self.sidebar_selected = anchor;
        }

        // Step to the neighbouring file row rather than through section headers.
        let current = files
            .iter()
            .position(|index| *index == self.sidebar_selected)
            .unwrap_or(0);
        let steps = delta.unsigned_abs().max(1);
        let next = if delta < 0 {
            current.saturating_sub(steps)
        } else {
            current.saturating_add(steps).min(files.len() - 1)
        };
        self.sidebar_selected = files[next];

        let anchor = self.sidebar_anchor.unwrap_or(self.sidebar_selected);
        let (lo, hi) = (
            anchor.min(self.sidebar_selected),
            anchor.max(self.sidebar_selected),
        );
        let wanted: Vec<String> = files
            .iter()
            .filter(|index| **index >= lo && **index <= hi)
            .filter_map(|index| self.file_id_at(*index))
            .collect();
        self.show_files_in_focused(&wanted);
    }

    /// A click landed on a sidebar row. Ctrl toggles that log in the view; a plain
    /// click shows it alone.
    fn click_sidebar(&mut self, row: usize, ctrl: bool) {
        let items = self.sidebar_items();
        if row >= items.len() {
            return;
        }
        self.focus = Focus::Sidebar;
        self.sidebar_selected = row;

        if !matches!(items.get(row), Some(SidebarItem::File { .. })) {
            self.sidebar_anchor = None;
            return;
        }
        if ctrl {
            self.sidebar_anchor = Some(row);
            self.toggle_file_in_view();
            return;
        }
        self.sidebar_anchor = None;
        if let Some(file_id) = self.file_id_at(row) {
            self.show_files_in_focused(&[file_id]);
        }
    }

    /// The real files feeding the focused pane. A merged view reports its sources.
    fn active_view_source_ids(&self) -> Vec<String> {
        let Some(view) = self.active_view() else {
            return Vec::new();
        };
        match self.project.get_file(&view.file_id) {
            Some(file) if file.is_merged() => file.merged_from.clone(),
            Some(file) => vec![file.file_id.clone()],
            None => Vec::new(),
        }
    }

    /// Space on a sidebar file: add it to (or drop it from) the focused pane's view.
    /// One file shows that file; several are interleaved by timestamp. The merge is a
    /// property of the pane, never a new entry in the file list.
    fn toggle_file_in_view(&mut self) {
        let items = self.sidebar_items();
        let Some(SidebarItem::File { file_id, .. }) = items.get(self.sidebar_selected) else {
            return;
        };
        let file_id = file_id.clone();

        let mut wanted = self.active_view_source_ids();
        if let Some(index) = wanted.iter().position(|id| *id == file_id) {
            if wanted.len() == 1 {
                self.status = "a view needs at least one file".to_string();
                return;
            }
            wanted.remove(index);
        } else {
            wanted.push(file_id);
        }
        self.show_files_in_focused(&wanted);
    }

    /// Point the focused pane at exactly `wanted`, merging when there is more than one.
    fn show_files_in_focused(&mut self, wanted: &[String]) {
        // Project order, so the same set always yields the same merge.
        let ordered: Vec<String> = self
            .project
            .files
            .iter()
            .filter(|file| wanted.contains(&file.file_id))
            .map(|file| file.file_id.clone())
            .collect();

        let target = match ordered.len() {
            0 => return,
            1 => ordered[0].clone(),
            _ => match self.project.add_merged(&ordered) {
                Ok(file_id) => file_id,
                Err(error) => {
                    self.status = error;
                    return;
                }
            },
        };

        let focus = self.focus;
        self.open_file_in_focused(&target);
        self.focus = focus; // keep the sidebar so more files can be added
        self.prune_merged_views();

        self.status = match ordered.len() {
            1 => format!(
                "showing {}",
                self.project
                    .get_file(&target)
                    .map(|file| file.display_name.clone())
                    .unwrap_or_default()
            ),
            n => {
                let entries = self
                    .project
                    .get_file(&target)
                    .map(|file| file.entries.len())
                    .unwrap_or(0);
                format!("merged {n} logs by timestamp, {entries} entries")
            }
        };
    }

    /// Merged models are pane-scoped; drop any no pane is showing.
    fn prune_merged_views(&mut self) {
        let in_use: Vec<String> = self
            .panes
            .iter()
            .map(|pane| pane.view.file_id.clone())
            .collect();
        self.project
            .files
            .retain(|file| !file.is_merged() || in_use.contains(&file.file_id));
    }

    /// Which file a schema edit applies to: the sidebar's if it has focus, else the
    /// one in the focused pane.
    fn schema_target(&self) -> Option<String> {
        if self.focus == Focus::Sidebar {
            if let Some(SidebarItem::File { file_id, .. }) =
                self.sidebar_items().get(self.sidebar_selected)
            {
                return Some(file_id.clone());
            }
        }
        self.active_view().map(|view| view.file_id.clone())
    }

    fn open_source_editor_for(&mut self, file_id: &str) {
        let Some(file) = self.project.get_file(file_id) else {
            return;
        };
        if file.is_merged() {
            self.status = "a merged view has no source settings".to_string();
            return;
        }
        if file.is_live() {
            self.mode = Mode::LiveSourceEditor(LiveSourceEditor::from_file(file));
            return;
        }
        self.mode = Mode::SourceEditor(SourceEditor::new(file));
    }

    fn save_source_editor_fields(&mut self, editor: &SourceEditor) {
        if let Some(file) = self.project.get_file_mut(&editor.file_id) {
            file.label = editor.short_name.trim().to_string();
            file.description = editor.description.trim().to_string();
            file.tag = editor.tag.trim().to_string();
            self.autosave_project();
        }
    }

    fn submit_source_editor(&mut self, editor: SourceEditor) {
        let schema = editor.schema.trim().to_string();
        let old_schema = self
            .project
            .get_file(&editor.file_id)
            .map(|file| file.extractor_name.clone())
            .unwrap_or_default();
        self.save_source_editor_fields(&editor);
        if !schema.is_empty() && schema != old_schema {
            self.apply_schema_text_to_file(&editor.file_id, &schema);
            return;
        }
        let name = self
            .project
            .get_file(&editor.file_id)
            .map(|file| source_default_short_name(file))
            .unwrap_or_else(|| editor.short_name.clone());
        self.status = format!("source updated: {name}");
    }

    /// `i` on a log source: ask the configured LLM to infer a schema from its first lines.
    /// The reply is not applied blindly -- `drain_schema_events` opens it in the schema editor
    /// for review. Needs a key configured (`logscout config set`, an env var, or `/key`).
    fn infer_schema_ai_for(&mut self, file_id: String) {
        let Some(file) = self.project.get_file(&file_id) else {
            return;
        };
        if file.is_merged() {
            self.status = "a merged view has no single schema to infer".to_string();
            return;
        }
        let path = file.path.clone();
        let entry_sample: Vec<String> = file
            .entries
            .iter()
            .flat_map(|entry| entry.raw.lines().map(str::to_string))
            .take(SCHEMA_INFER_SAMPLE_LINES)
            .collect();

        let config = AiConfig::load();
        let Some(key) = config.api_key() else {
            self.status =
                "Configure an LLM first: logscout config set --provider <p> --api-key <key>"
                    .to_string();
            return;
        };

        let sample: Vec<String> =
            match crate::core::parser::read_first_lines(&path, SCHEMA_INFER_SAMPLE_LINES) {
                Ok(lines) => lines.into_iter().take(SCHEMA_INFER_SAMPLE_LINES).collect(),
                Err(_error) if !entry_sample.is_empty() => entry_sample,
                Err(error) => {
                    self.status = format!("could not read the file: {error}");
                    return;
                }
            };
        if sample.iter().all(|line| line.trim().is_empty()) {
            self.status = "the file has no lines to sample".to_string();
            return;
        }

        let schema = self.ai_schema.get_or_insert_with(SchemaInfer::new);
        schema.next_gen += 1;
        let generation = schema.next_gen;
        let request = AgentRequest {
            generation,
            config: config.clone(),
            key,
            conversation: vec![
                ChatMsg::system(schema_infer_prompt()),
                ChatMsg::user(format!("Sample log lines:\n{}", sample.join("\n"))),
            ],
            tools: Vec::new(),
        };
        if let Err(error) = schema.worker.send(request) {
            self.status = error;
            return;
        }
        schema.pending = Some((generation, file_id));
        self.status = format!("Inferring a schema with {}…", config.provider.label());
    }

    /// Take a schema-inference reply, if one arrived, build the schema, and apply it to the
    /// file it was inferred from. Applying re-parses the file, so the extracted fields appear
    /// at once -- if the schema is wrong you see it immediately and can `e` to edit or `i` to
    /// try again. Called once per frame.
    fn drain_schema_events(&mut self) {
        let event = match &self.ai_schema {
            Some(schema) if schema.pending.is_some() => schema.worker.poll(),
            _ => None,
        };
        let Some(event) = event else { return };
        let Some((generation, file_id)) = self
            .ai_schema
            .as_ref()
            .and_then(|schema| schema.pending.clone())
        else {
            return;
        };
        if event.generation != generation {
            return;
        }
        if let Some(schema) = &mut self.ai_schema {
            schema.pending = None;
        }

        match event.result {
            Err(error) => self.status = format!("schema inference failed: {error}"),
            Ok(assistant) => match parse_inferred_schema(&assistant.text) {
                Ok(mut extractor) => {
                    let note = self.repair_inferred_schema(&file_id, &mut extractor);
                    let name = extractor.name.clone();
                    if let Err(error) = self.project.add_extractor(extractor) {
                        self.status = format!("the AI's schema did not compile: {error}");
                    } else {
                        self.apply_schema_to_file(&file_id, &name);
                        if let Some(note) = note {
                            self.status = format!("applied '{name}' -- {note}");
                        }
                    }
                }
                Err(error) => self.status = format!("could not use the AI's schema: {error}"),
            },
        }
    }

    /// Repair the most common slip in an inferred schema: an explicit `entry_start` regex
    /// that matches none of the file's header lines. The LLM produced `^\S+ \[HOST:` for a
    /// `2026-06-12 10:17:44.944 [HOST:...]` line, but `\S+` stops at the space inside the
    /// timestamp, so the regex matches nothing -- and a start regex that matches nothing
    /// makes every line a continuation, folding the whole file into a single entry.
    ///
    /// If dropping the bad `entry_start` lets the format's own derived header probe group
    /// the lines (for the case above, `^(.+?) \[HOST:`, whose `.+?` spans the space), drop
    /// it. Returns a note when it changed something, or a warning when the schema still
    /// collapses the sample so the user knows to edit it.
    fn repair_inferred_schema(&self, file_id: &str, extractor: &mut Extractor) -> Option<String> {
        let path = self.project.get_file(file_id).map(|file| file.path.clone())?;
        let lines = crate::core::parser::read_first_lines(&path, SCHEMA_INFER_SAMPLE_LINES).ok()?;
        repair_entry_start(extractor, &lines)
    }

    fn open_schema_input_for(&mut self, file_id: &str) {
        let prefill = self
            .project
            .get_file(file_id)
            .cloned()
            .and_then(|file| {
                file.extractor
                    .map(|extractor| (file.display_name, extractor))
            })
            .map(|(_, extractor)| {
                if extractor.format.contains('\n')
                    || extractor.uses_explicit_entry_boundary()
                    || !extractor.field_patterns.is_empty()
                {
                    return serde_json::to_string_pretty(&extractor).unwrap_or_else(|_| {
                        format!(
                            "{} | {} | {}",
                            extractor.name, extractor.format, extractor.timestamp_format
                        )
                    });
                }
                if extractor.description.is_empty() {
                    format!(
                        "{} | {} | {}",
                        extractor.name, extractor.format, extractor.timestamp_format
                    )
                } else {
                    format!(
                        "{} | {} | {} | {}",
                        extractor.name,
                        extractor.format,
                        extractor.timestamp_format,
                        extractor.description
                    )
                }
            })
            .unwrap_or_else(|| {
                format!(
                    "My schema | {} | {}",
                    BRACKETED_DEFAULT_FORMAT, DEFAULT_TIMESTAMP_FORMAT
                )
            });
        self.open_input(Mode::Extractor(prefill));
    }

    /// Oldest and newest parseable timestamps in the focused log. A log is written in
    /// time order and a merged model is sorted by timestamp, so the bounds sit at the
    /// ends; scanning inward stops at the first entry that has one, instead of parsing
    /// the whole file.
    /// First and last timestamp of one file, cheaply: the extremes of the timestamped
    /// entries at each end. Exact for a time-ordered log, which is the normal case.
    fn file_time_bounds(file: &LogFileModel) -> Option<(NaiveDateTime, NaiveDateTime)> {
        let first = file
            .entries
            .iter()
            .find_map(|entry| file.timestamp(entry))?;
        let last = file
            .entries
            .iter()
            .rev()
            .find_map(|entry| file.timestamp(entry))?;
        Some((first.min(last), first.max(last)))
    }

    /// The span of *every* loaded source, not just the focused pane's.
    ///
    /// The time filter is project-wide, so "Last 15 minutes" has to count back from the
    /// newest entry across all logs. Anchored to one pane, a merge of a source ending at
    /// 10:00 with one ending at 11:00 would take fifteen minutes from whichever pane had
    /// focus -- so the same preset meant a different window depending on where the cursor
    /// happened to be. Merged views are skipped: they are copies of the real files.
    fn project_time_bounds(&self) -> Option<(NaiveDateTime, NaiveDateTime)> {
        self.project
            .files
            .iter()
            .filter(|file| !file.is_merged())
            .filter_map(Self::file_time_bounds)
            .reduce(|(lo, hi), (start, end)| (lo.min(start), hi.max(end)))
    }

    /// Opens on the range already in force, so `t` and Enter on the Time row both mean
    /// "change this" rather than "start over".
    fn open_time_picker(&mut self) {
        let bounds = self.project_time_bounds();
        if bounds.is_none() {
            self.status = "no timestamps in these logs; presets count back from now".to_string();
        }
        let mut picker = TimePicker::new(bounds);
        if let Some(rule) = self.project.filters.time_rule() {
            let (start, end) = rule.time_bounds();
            picker.load_range(start, end);
        }
        self.mode = Mode::TimePicker(picker);
    }

    fn open_entry_detail_popup(&mut self) {
        if self.entry_detail_target().is_none() {
            self.status = "no line selected".to_string();
            return;
        }
        self.mode = Mode::EntryDetail { scroll: 0 };
    }

    fn open_pretty_print(&mut self) {
        let Some((file, entry)) = self.entry_detail_target() else {
            self.status = "no line selected".to_string();
            return;
        };
        let message = file.message(entry);
        match pretty_print_message(&message) {
            Some((kind, body)) => {
                self.mode = Mode::PrettyPrint {
                    title: format!("Pretty {kind}"),
                    body,
                    scroll: 0,
                };
                self.status.clear();
            }
            None => {
                self.status = "no JSON, XML, or SQL found in this message".to_string();
            }
        }
    }

    /// A pane whose file vanished (e.g. a merged view invalidated by a schema change)
    /// would render blank. Point it at `fallback` instead of closing it.
    fn repoint_panes(&mut self, fallback: &str) {
        let ids: Vec<String> = self
            .project
            .files
            .iter()
            .map(|file| file.file_id.clone())
            .collect();
        let Some(index) = self.project.file_index(fallback) else {
            self.panes.retain(|pane| ids.contains(&pane.view.file_id));
            self.focused_pane = self.focused_pane.min(self.panes.len().saturating_sub(1));
            return;
        };

        for pane in &mut self.panes {
            if !ids.contains(&pane.view.file_id) {
                let leaf_id = pane.view.leaf_id.clone();
                pane.view = build_view(leaf_id, &self.project.files[index], &self.project.filters);
            }
        }
        if self.panes.is_empty() {
            self.panes.push(PaneState {
                view: build_view("L1", &self.project.files[index], &self.project.filters),
            });
            self.focused_pane = 0;
        }
    }

    fn clear_active_selection(&mut self) {
        self.sidebar_anchor = None;
        self.detail_selection = None;
        self.text_selection = None;
        self.mouse_drag = None;
        if let Some(view) = self.active_view_mut() {
            view.clear_selection();
        }
    }

    fn move_after_clearing_selection(&mut self, delta: isize) {
        let had_sidebar_range = self.focus == Focus::Sidebar && self.sidebar_anchor.is_some();
        self.clear_active_selection();
        if !had_sidebar_range {
            self.move_selection(delta);
        }
    }

    fn report_selection(&mut self) {
        let count = self
            .active_view()
            .map(ViewModel::selection_count)
            .unwrap_or(0);
        self.status = match count {
            0 => String::new(),
            1 => "1 line selected".to_string(),
            n => format!("{n} lines selected"),
        };
    }

    /// Entries a selection-aware action applies to: the selection if there is one,
    /// otherwise just the cursor line.
    fn target_globals(&self) -> Vec<usize> {
        let Some(view) = self.active_view() else {
            return Vec::new();
        };
        if view.has_selection() {
            view.selected_globals()
        } else {
            view.current_index().into_iter().collect()
        }
    }

    fn copy_selection(&mut self) {
        // A dragged substring is the most specific thing the user asked for, so it wins
        // over the row selection underneath it.
        if let Some(text) = self.selected_substring() {
            self.status = match copy_to_clipboard(&text) {
                Ok(()) => format!("copied {} char(s)", text.chars().count()),
                Err(error) => format!("copy failed: {error}"),
            };
            return;
        }

        let globals = self.target_globals();
        let Some(file) = self
            .active_view()
            .and_then(|view| self.project.get_file(&view.file_id))
        else {
            return;
        };

        let text: String = globals
            .iter()
            .filter_map(|global| file.entries.get(*global))
            .map(|entry| entry.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            self.status = "nothing to copy".to_string();
            return;
        }

        let lines = globals.len();
        self.status = match copy_to_clipboard(&text) {
            Ok(()) => format!("copied {lines} line(s), {} bytes", text.len()),
            Err(error) => format!("copy failed: {error}"),
        };
    }

    fn bookmark_key_for_entry(
        &self,
        file: &LogFileModel,
        entry: &LogEntry,
    ) -> Option<(String, usize)> {
        let file_id = if file.is_merged() {
            file.sources
                .get(entry.source as usize)
                .map(|source| source.file_id.clone())
                .or_else(|| file.merged_from.get(entry.source as usize).cloned())?
        } else {
            file.file_id.clone()
        };
        Some((file_id, entry.line_no))
    }

    fn bookmark_index(&self, file_id: &str, line_no: usize) -> Option<usize> {
        self.project
            .bookmarks
            .iter()
            .position(|bookmark| bookmark.file_id == file_id && bookmark.line_no == line_no)
    }

    fn is_bookmarked_entry(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.bookmark_key_for_entry(file, entry)
            .and_then(|(file_id, line_no)| self.bookmark_index(&file_id, line_no))
            .is_some()
    }

    fn current_bookmark_target(&self) -> Option<(String, usize)> {
        let (file, view) = self.active_file_view()?;
        let entry = view.current_entry(file)?;
        self.bookmark_key_for_entry(file, entry)
    }

    fn toggle_bookmark_current(&mut self) {
        let Some((file_id, line_no)) = self.current_bookmark_target() else {
            self.status = "no line selected".to_string();
            return;
        };
        if let Some(index) = self.bookmark_index(&file_id, line_no) {
            self.project.bookmarks.remove(index);
            self.autosave_project();
            self.status = format!("removed bookmark at line {line_no}");
            return;
        }

        self.project.bookmarks.push(Bookmark {
            file_id,
            line_no,
            note: String::new(),
        });
        self.autosave_project();
        self.status = format!("bookmarked line {line_no}; M adds a note");
    }

    fn open_bookmark_note(&mut self) {
        let Some((file_id, line_no)) = self.current_bookmark_target() else {
            self.status = "no line selected".to_string();
            return;
        };
        let text = self
            .bookmark_index(&file_id, line_no)
            .and_then(|index| self.project.bookmarks.get(index))
            .map(|bookmark| bookmark.note.clone())
            .unwrap_or_default();
        self.open_input(Mode::BookmarkNote {
            file_id,
            line_no,
            text,
        });
    }

    fn submit_bookmark_note(&mut self, file_id: String, line_no: usize, text: String) {
        let note = text.trim().to_string();
        match self.bookmark_index(&file_id, line_no) {
            Some(index) => self.project.bookmarks[index].note = note.clone(),
            None => self.project.bookmarks.push(Bookmark {
                file_id,
                line_no,
                note: note.clone(),
            }),
        }
        self.autosave_project();
        self.status = if note.is_empty() {
            format!("bookmarked line {line_no}")
        } else {
            format!("saved bookmark note for line {line_no}")
        };
    }

    fn remove_bookmark(&mut self, index: usize) {
        if index >= self.project.bookmarks.len() {
            return;
        }
        let removed = self.project.bookmarks.remove(index);
        self.sidebar_selected = self
            .sidebar_selected
            .min(self.sidebar_items().len().saturating_sub(1));
        self.autosave_project();
        self.status = format!("removed bookmark at line {}", removed.line_no);
    }

    fn bookmark_positions_in_active_view(&self) -> Vec<(usize, usize)> {
        let Some((file, view)) = self.active_file_view() else {
            return Vec::new();
        };
        let mut positions = Vec::new();
        for (position, global_index) in view.visible.iter().enumerate() {
            let Some(entry) = file.entries.get(global_index) else {
                continue;
            };
            let Some((file_id, line_no)) = self.bookmark_key_for_entry(file, entry) else {
                continue;
            };
            if let Some(index) = self.bookmark_index(&file_id, line_no) {
                positions.push((position, index));
            }
        }
        positions
    }

    fn jump_bookmark(&mut self, forward: bool, count: usize) {
        let positions = self.bookmark_positions_in_active_view();
        if positions.is_empty() {
            self.status = "no bookmarks in this view".to_string();
            return;
        }
        let cursor = self.active_view().map(|view| view.cursor).unwrap_or(0);
        let mut slot = if forward {
            positions
                .iter()
                .position(|(position, _)| *position > cursor)
                .unwrap_or(0)
        } else {
            positions
                .iter()
                .rposition(|(position, _)| *position < cursor)
                .unwrap_or(positions.len() - 1)
        };
        for _ in 1..count.max(1) {
            slot = if forward {
                (slot + 1) % positions.len()
            } else if slot == 0 {
                positions.len() - 1
            } else {
                slot - 1
            };
        }
        let (position, bookmark_index) = positions[slot];
        if let Some(view) = self.active_view_mut() {
            view.move_cursor_to(position);
        }
        self.focus = Focus::Pane;
        self.status = self
            .project
            .bookmarks
            .get(bookmark_index)
            .map(|bookmark| format!("bookmark line {}", bookmark.line_no))
            .unwrap_or_else(|| "bookmark".to_string());
    }

    fn jump_bookmark_index(&mut self, index: usize) {
        let Some(bookmark) = self.project.bookmarks.get(index).cloned() else {
            return;
        };
        if !self.active_view_source_ids().contains(&bookmark.file_id) {
            self.open_file_in_focused(&bookmark.file_id);
        }
        if let Some(position) = self.bookmark_position_in_active_view(&bookmark) {
            if let Some(view) = self.active_view_mut() {
                view.move_cursor_to(position);
            }
            self.focus = Focus::Pane;
            self.status = format!("bookmark line {}", bookmark.line_no);
        } else {
            self.status = "bookmark is hidden by the current filters".to_string();
        }
    }

    fn bookmark_position_in_active_view(&self, bookmark: &Bookmark) -> Option<usize> {
        let (file, view) = self.active_file_view()?;
        for (position, global_index) in view.visible.iter().enumerate() {
            let Some(entry) = file.entries.get(global_index) else {
                continue;
            };
            let Some((file_id, line_no)) = self.bookmark_key_for_entry(file, entry) else {
                continue;
            };
            if file_id == bookmark.file_id && line_no == bookmark.line_no {
                return Some(position);
            }
        }
        None
    }

    fn bookmark_entry<'a>(
        &'a self,
        bookmark: &Bookmark,
    ) -> Option<(&'a LogFileModel, &'a LogEntry)> {
        let file = self.project.get_file(&bookmark.file_id)?;
        let entry = file
            .entries
            .iter()
            .find(|entry| entry.line_no == bookmark.line_no)?;
        Some((file, entry))
    }

    fn bookmark_sidebar_label(&self, bookmark: &Bookmark) -> String {
        let source = self
            .project
            .get_file(&bookmark.file_id)
            .map(|file| file.display_name.clone())
            .unwrap_or_else(|| bookmark.file_id.clone());
        let preview = if bookmark.note.trim().is_empty() {
            self.bookmark_entry(bookmark)
                .map(|(file, entry)| field_value_preview(&file.message(entry)))
                .unwrap_or_default()
        } else {
            field_value_preview(&bookmark.note)
        };
        if preview.is_empty() {
            format!("{source}:{}", bookmark.line_no)
        } else {
            format!("{source}:{}  {preview}", bookmark.line_no)
        }
    }

    fn bookmark_detail_lines(&self, bookmark: &Bookmark, width: usize) -> Vec<Line<'static>> {
        let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
        let plain = Style::default();
        let mut lines = Vec::new();
        let source = self
            .project
            .get_file(&bookmark.file_id)
            .map(|file| file.display_name.clone())
            .unwrap_or_else(|| bookmark.file_id.clone());
        push_detail(&mut lines, "source", &source, value_width, plain);
        push_detail(
            &mut lines,
            "line",
            &bookmark.line_no.to_string(),
            value_width,
            plain,
        );
        push_detail(&mut lines, "note", &bookmark.note, value_width, plain);
        if let Some((file, entry)) = self.bookmark_entry(bookmark) {
            push_detail(
                &mut lines,
                "message",
                &file.message(entry),
                value_width,
                plain,
            );
            push_detail(&mut lines, "raw", &entry.raw, value_width, plain);
        } else {
            push_detail(&mut lines, "status", "line not loaded", value_width, plain);
        }
        lines
    }

    fn submit_export_incident(&mut self, input: String) {
        let path = self.incident_file_from_input(&input);
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                self.status = format!("export failed: {error}");
                return;
            }
        }
        let body = self.incident_markdown();
        match std::fs::write(&path, body.as_bytes()) {
            Ok(()) => self.status = format!("exported incident notes to {}", path.display()),
            Err(error) => self.status = format!("export failed: {error}"),
        }
    }

    fn incident_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Log Scouter Incident Notes\n\n");
        out.push_str(&format!("- Project: `{}`\n", self.project.root.display()));
        out.push_str(&format!(
            "- Generated: {}\n\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));

        out.push_str("## Selected Lines\n\n");
        let selected = self.incident_selected_lines();
        if selected.is_empty() {
            out.push_str("No selected line.\n\n");
        } else {
            for (source, line_no, raw) in selected {
                out.push_str(&format!("### {source}:{line_no}\n\n"));
                out.push_str(&markdown_code_block(&raw));
                out.push('\n');
            }
        }

        out.push_str("## Bookmarks\n\n");
        if self.project.bookmarks.is_empty() {
            out.push_str("No bookmarks.\n\n");
        } else {
            for bookmark in &self.project.bookmarks {
                let source = self
                    .project
                    .get_file(&bookmark.file_id)
                    .map(|file| file.display_name.clone())
                    .unwrap_or_else(|| bookmark.file_id.clone());
                out.push_str(&format!("### {source}:{}\n\n", bookmark.line_no));
                if !bookmark.note.trim().is_empty() {
                    out.push_str(&format!("Note: {}\n\n", bookmark.note.trim()));
                }
                match self.bookmark_entry(bookmark) {
                    Some((_, entry)) => {
                        out.push_str(&markdown_code_block(&entry.raw));
                        out.push('\n');
                    }
                    None => out.push_str("Line not loaded.\n\n"),
                }
            }
        }

        out.push_str("## Filters\n\n");
        if self.project.filters.rules.is_empty() {
            out.push_str("No filters.\n\n");
        } else {
            for rule in &self.project.filters.rules {
                let state = if rule.enabled { "on" } else { "off" };
                out.push_str(&format!("- [{state}] {}\n", rule.describe()));
            }
            out.push('\n');
        }
        if let Some(view) = self.active_view() {
            if !view.query_text.trim().is_empty() {
                out.push_str(&format!(
                    "- Active search: `{}` with +/-{} context\n\n",
                    view.query_text, view.context
                ));
            }
        }

        out.push_str("## AI Summary\n\n");
        match self.latest_ai_summary() {
            Some(summary) => {
                out.push_str(summary.trim());
                out.push_str("\n");
            }
            None => out.push_str("No AI summary yet.\n"),
        }
        out
    }

    fn incident_selected_lines(&self) -> Vec<(String, usize, String)> {
        let Some((file, _)) = self.active_file_view() else {
            return Vec::new();
        };
        self.target_globals()
            .iter()
            .filter_map(|global| file.entries.get(*global))
            .map(|entry| {
                (
                    self.entry_source_label(file, entry),
                    entry.line_no,
                    entry.raw.clone(),
                )
            })
            .collect()
    }

    fn entry_source_label(&self, file: &LogFileModel, entry: &LogEntry) -> String {
        file.source_name(entry)
            .map(ToString::to_string)
            .unwrap_or_else(|| file.display_name.clone())
    }

    fn latest_ai_summary(&self) -> Option<String> {
        self.ai.as_ref().and_then(|ai| {
            ai.transcript.iter().rev().find_map(|line| match line {
                ChatLine::Assistant(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            })
        })
    }

    /// The first line of the message of every targeted entry.
    fn target_messages(&self) -> Vec<String> {
        let Some(file) = self
            .active_view()
            .and_then(|view| self.project.get_file(&view.file_id))
        else {
            return Vec::new();
        };
        self.target_globals()
            .iter()
            .filter_map(|global| file.entries.get(*global))
            .map(|entry| file.message(entry).lines().next().unwrap_or("").to_string())
            .collect()
    }

    /// One line: choose a schema field. Several: derive the templates they support and let
    /// the user pick before any of it becomes a saved, project-wide filter.
    fn begin_hide(&mut self) {
        let messages = self.target_messages();
        if messages.len() < 2 {
            self.mode = Mode::HideChoice(HideMenu::new(self.hide_choice_entries()));
            return;
        }

        let borrowed: Vec<&str> = messages.iter().map(String::as_str).collect();
        // Only when every selected message is blank; the choice menu still works.
        let Some(default) = common_message_pattern(&borrowed) else {
            self.mode = Mode::HideChoice(HideMenu::new(self.hide_choice_entries()));
            return;
        };
        self.status = format!("pattern from {} lines", messages.len());
        self.open_pattern_prompt(pattern_candidates(&borrowed), default);
    }

    /// `H` again from the choice menu: generalise the one targeted line into a template.
    /// A single line has no second line to diff against, so the value shapes it contains
    /// -- ids, counters, addresses -- are all there is to generalise.
    fn begin_hide_pattern_from_one_line(&mut self) {
        let messages = self.target_messages();
        let template = messages
            .first()
            .and_then(|message| message_template(message));
        let Some(default) = template else {
            self.status = "no message to build a pattern from".to_string();
            return;
        };
        let borrowed: Vec<&str> = messages[..1].iter().map(String::as_str).collect();
        self.status = "pattern from 1 line".to_string();
        self.open_pattern_prompt(pattern_candidates(&borrowed), default);
    }

    fn open_pattern_prompt(&mut self, options: Vec<PatternOption>, default: String) {
        self.open_pattern_prompt_for("message", options, default, true);
    }

    /// Rank the templates by what they actually match here, and open on `default` -- the
    /// one `H` has always produced. The looser rungs sit above it, a keypress away.
    fn open_pattern_prompt_for(
        &mut self,
        field: &str,
        options: Vec<PatternOption>,
        default: String,
        exclude: bool,
    ) {
        let (counts, scanned, total) = self.score_patterns(field, &options);
        let mut candidates: Vec<PatternCandidate> = options
            .into_iter()
            .zip(counts)
            .map(|(option, matched)| PatternCandidate { option, matched })
            .collect();
        // Greediest first. What a template matches *in this log* is the only honest
        // measure of that; the strategy that built it only suggests an order.
        candidates.sort_by(|left, right| {
            right
                .matched
                .cmp(&left.matched)
                .then(left.option.pattern.len().cmp(&right.option.pattern.len()))
        });

        let selected = candidates
            .iter()
            .position(|candidate| candidate.option.pattern == default)
            .unwrap_or(0);
        let text = candidates
            .get(selected)
            .map(|candidate| candidate.option.pattern.clone())
            .unwrap_or(default);

        self.open_input(Mode::HidePattern(PatternPrompt {
            text,
            field: field.to_string(),
            exclude,
            candidates,
            selected,
            scanned,
            total,
        }));
    }

    /// Rows matched by each option, plus how many rows were read and how many there are.
    ///
    /// One pass over the pane, testing every option against each row: pulling the field
    /// out of an entry costs more than the regexes do, so it is done once per row rather
    /// than once per option.
    fn score_patterns(&self, field: &str, options: &[PatternOption]) -> (Vec<usize>, usize, usize) {
        // A prompt with no ladder has nothing to rank, and its header counts for itself.
        if options.is_empty() {
            return (Vec::new(), 0, 0);
        }
        let mut counts = vec![0; options.len()];
        let compiled: Vec<Option<regex::Regex>> = options
            .iter()
            .map(|option| regex::Regex::new(&option.pattern).ok())
            .collect();

        let Some((file, view)) = self.active_file_view() else {
            return (counts, 0, 0);
        };
        let total = view.visible.len();
        let mut scanned = 0;
        for global in view.visible.iter().take(PATTERN_PREVIEW_LIMIT) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            scanned += 1;
            file.with_field(entry, field, |text| {
                for (index, regex) in compiled.iter().enumerate() {
                    if regex
                        .as_ref()
                        .map(|regex| regex.is_match(text))
                        .unwrap_or(false)
                    {
                        counts[index] += 1;
                    }
                }
            });
        }
        (counts, scanned, total)
    }

    fn submit_hide_pattern(&mut self, pattern: String, field: String, exclude: bool) {
        let pattern = pattern.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        if let Err(error) = regex::Regex::new(&pattern) {
            self.status = format!("invalid regex: {error}");
            return;
        }

        let action = if exclude { "exclude" } else { "include" };
        self.mutate_filters(|filters| {
            filters.add(FilterRule::new(
                field.as_str(),
                "regex",
                pattern.as_str(),
                action,
            ))
        });
        self.clear_active_selection();
        self.status = if exclude {
            "hide pattern added"
        } else {
            "keep pattern added"
        }
        .to_string();
    }

    /// How the pattern would land on the rows the pane is showing right now. Recomputed on
    /// every keystroke, so the scan is capped: a filter is committed project-wide and the
    /// user deserves to see its blast radius before pressing Enter, but not to wait for it.
    fn hide_pattern_preview(&self, pattern: &str, field: &str) -> PatternPreview {
        let mut preview = PatternPreview::default();
        if pattern.trim().is_empty() {
            return preview;
        }
        let regex = match regex::Regex::new(pattern) {
            Ok(regex) => regex,
            Err(error) => {
                preview.error = Some(one_line(&error.to_string()));
                return preview;
            }
        };
        let Some((file, view)) = self.active_file_view() else {
            return preview;
        };

        preview.total = view.visible.len();
        for global in view.visible.iter().take(PATTERN_PREVIEW_LIMIT) {
            let Some(entry) = file.entries.get(global) else {
                continue;
            };
            preview.scanned += 1;
            // The same field and the same regex the committed rule will use.
            if !file.with_field(entry, field, |text| regex.is_match(text)) {
                continue;
            }
            preview.matched += 1;
            if preview.samples.len() < PATTERN_PREVIEW_SAMPLES {
                preview
                    .samples
                    .push(file.message(entry).lines().next().unwrap_or("").to_string());
            }
        }
        preview
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Pane => self.move_active_cursor(delta),
            Focus::Sidebar => {
                let len = self.sidebar_items().len();
                if len == 0 {
                    self.sidebar_selected = 0;
                } else {
                    self.sidebar_selected = self
                        .sidebar_selected
                        .saturating_add_signed(delta)
                        .min(len.saturating_sub(1));
                }
            }
            Focus::Results => self.move_result_selection(delta),
            // Chat handles its own keys; arrows there scroll the transcript.
            Focus::Chat => {}
        }
    }

    fn move_active_cursor(&mut self, delta: isize) {
        if let Some(view) = self.active_view_mut() {
            view.move_cursor(delta);
        }
    }

    fn cycle_focus(&mut self, step: isize) {
        if step < 0 {
            self.focus = match self.focus {
                Focus::Sidebar => {
                    if self.search_results_visible() {
                        Focus::Results
                    } else {
                        Focus::Pane
                    }
                }
                Focus::Pane => Focus::Sidebar,
                Focus::Results => Focus::Pane,
                // Tab leaves the chat panel rather than cycling through it.
                Focus::Chat => Focus::Pane,
            };
            return;
        }

        self.focus = match self.focus {
            Focus::Sidebar => Focus::Pane,
            Focus::Pane => {
                if !self.panes.is_empty() && self.focused_pane + 1 < self.panes.len() {
                    self.focused_pane += 1;
                    Focus::Pane
                } else if self.search_results_visible() {
                    Focus::Results
                } else {
                    self.focused_pane = 0;
                    Focus::Sidebar
                }
            }
            Focus::Results => {
                self.focused_pane = 0;
                Focus::Sidebar
            }
            Focus::Chat => Focus::Pane,
        };
    }

    fn move_result_selection(&mut self, delta: isize) {
        let count = self.active_result_positions().len();
        if count == 0 {
            self.results_selected = 0;
            self.results_scroll = 0;
            return;
        }
        self.results_selected = self
            .results_selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
    }

    fn jump_match(&mut self, forward: bool) {
        let next = self
            .active_view()
            .and_then(|view| view.next_match(view.cursor, forward));
        if let Some(next) = next {
            if let Some(view) = self.active_view_mut() {
                view.move_cursor_to(next);
            }
            self.sync_selected_result_to_cursor();
        } else {
            self.status = "no matches".to_string();
        }
    }

    fn goto_first_match(&mut self) {
        let positions = self
            .active_view()
            .map(|view| view.match_positions())
            .unwrap_or_default();
        if let Some(first) = positions.first().copied() {
            if let Some(view) = self.active_view_mut() {
                view.move_cursor_to(first);
            }
            self.results_selected = 0;
            self.results_scroll = 0;
            self.status = format!("{} match(es)", positions.len());
        } else if self
            .active_view()
            .and_then(|view| view.query.as_ref())
            .is_some()
        {
            self.status = "no matches".to_string();
        }
    }

    fn cycle_context(&mut self) {
        if let Some(view) = self.active_view_mut() {
            let index = CONTEXT_CYCLE
                .iter()
                .position(|value| *value == view.context)
                .unwrap_or(0);
            view.context = CONTEXT_CYCLE[(index + 1) % CONTEXT_CYCLE.len()];
            self.status = format!("context +/-{}", view.context);
        }
        self.rebuild_active_view();
    }

    fn clear_search(&mut self) {
        if let Some(view) = self.active_view_mut() {
            view.query = None;
            view.query_text.clear();
            view.context = 0;
        }
        if self.focus == Focus::Results {
            self.focus = Focus::Pane;
        }
        self.results_selected = 0;
        self.results_scroll = 0;
        self.rebuild_active_view();
        self.status.clear();
    }

    fn clear_filters(&mut self) {
        self.mutate_filters(|filters| filters.clear());
        self.status = "filters cleared".to_string();
    }

    fn split_active(&mut self, mode: SplitMode) {
        if let Some(active) = self.active_view().cloned() {
            let leaf_id = format!("L{}", self.panes.len() + 1);
            let mut view = active;
            view.leaf_id = leaf_id;
            self.panes.insert(self.focused_pane + 1, PaneState { view });
            self.focused_pane += 1;
            self.split_mode = mode;
            // A fresh split starts even; a later resize re-weights it.
            self.workspace.pane_weights.clear();
            self.requeue_all_panes();
        }
    }

    // ---- Workspace layout -------------------------------------------------------------

    /// Make sure there is one weight per pane; a mismatch resets to equal.
    fn ensure_pane_weights(&mut self) {
        if self.workspace.pane_weights.len() != self.panes.len() {
            self.workspace.pane_weights = vec![100; self.panes.len()];
        }
    }

    /// `[` / `]`: narrow or widen the sidebar, from its current width.
    fn resize_sidebar(&mut self, delta: isize) {
        let body_width = self.body_area.width;
        let base = self
            .workspace
            .sidebar_width
            .unwrap_or_else(|| sidebar_width(body_width)) as isize;
        let next = base.saturating_add(delta).max(0) as u16;
        self.workspace.sidebar_width = Some(next);
        self.workspace.show_sidebar = true;
        self.workspace.focus_mode = false;
        self.status = format!("sidebar width {}", self.effective_sidebar_width(body_width));
    }

    fn resize_sidebar_or_start_bookmark_nav(&mut self, forward: bool, count: usize) {
        let pending = BookmarkNavPending {
            forward,
            count: count.max(1),
            previous_sidebar_width: self.workspace.sidebar_width,
            previous_show_sidebar: self.workspace.show_sidebar,
            previous_focus_mode: self.workspace.focus_mode,
        };
        self.resize_sidebar(if forward { 4 } else { -4 });
        self.bookmark_nav_pending = Some(pending);
    }

    /// Set the sidebar width from a separator drag to `column`, relative to the body's left.
    fn drag_sidebar_to(&mut self, column: u16) {
        let width = column.saturating_sub(self.body_area.x);
        self.workspace.sidebar_width = Some(width);
        self.workspace.show_sidebar = true;
    }

    /// Whether `(column, row)` is on the draggable sidebar/pane separator.
    fn on_separator(&self, column: u16, row: u16) -> bool {
        let Some(x) = self.separator_x else {
            return false;
        };
        let in_body = row >= self.body_area.y && row < self.body_area.y + self.body_area.height;
        in_body && (column == x || column + 1 == x)
    }

    /// The boundary index (between pane `i` and `i+1`) whose border `(column, row)` lands on,
    /// for dragging pane heights (a rows split) or widths (a columns split).
    fn pane_separator_at(&self, column: u16, row: u16) -> Option<usize> {
        for boundary in 0..self.pane_layout.len().saturating_sub(1) {
            let a = self.pane_layout[boundary];
            let b = self.pane_layout[boundary + 1];
            let hit = match self.split_mode {
                SplitMode::Horizontal => {
                    row >= a.y && row < a.y + a.height && (column == b.x || column + 1 == b.x)
                }
                SplitMode::Vertical => {
                    column >= a.x && column < a.x + a.width && (row == b.y || row + 1 == b.y)
                }
            };
            if hit {
                return Some(boundary);
            }
        }
        None
    }

    /// The stacked panel whose top border `(column, row)` lands on, for dragging its height.
    fn panel_separator_at(&self, column: u16, row: u16) -> Option<PanelSeparator> {
        self.panel_separators.iter().copied().find(|separator| {
            column >= separator.x0
                && column < separator.x1
                && (row == separator.top || row + 1 == separator.top)
        })
    }

    /// Reweight the two panes on either side of `boundary` from a drag to `(column, row)`,
    /// keeping their combined weight fixed so the other panes are undisturbed.
    fn drag_pane_separator(&mut self, boundary: usize, column: u16, row: u16) {
        self.ensure_pane_weights();
        if boundary + 1 >= self.pane_layout.len()
            || boundary + 1 >= self.workspace.pane_weights.len()
        {
            return;
        }
        let a = self.pane_layout[boundary];
        let b = self.pane_layout[boundary + 1];
        let (start, span, pos) = match self.split_mode {
            SplitMode::Horizontal => (a.x, a.width + b.width, column),
            SplitMode::Vertical => (a.y, a.height + b.height, row),
        };
        if span < 4 {
            return;
        }
        let min = 2u16;
        let first_len = pos.saturating_sub(start).clamp(min, span - min) as u32;
        let sum = (self.workspace.pane_weights[boundary]
            + self.workspace.pane_weights[boundary + 1])
            .max(2) as u32;
        let first = (sum * first_len / span as u32).max(1);
        self.workspace.pane_weights[boundary] = first as u16;
        self.workspace.pane_weights[boundary + 1] = (sum - first).max(1) as u16;
    }

    /// `Ctrl+Arrow` (along the split): grow or shrink the focused pane's share.
    fn resize_pane(&mut self, delta: isize) {
        if self.panes.len() < 2 {
            return;
        }
        self.ensure_pane_weights();
        let index = self.focused_pane.min(self.workspace.pane_weights.len() - 1);
        let current = self.workspace.pane_weights[index] as isize;
        self.workspace.pane_weights[index] =
            current.saturating_add(delta * 20).clamp(20, 400) as u16;
        self.status = "pane resized".to_string();
    }

    fn toggle_focus_mode(&mut self) {
        self.workspace.focus_mode = !self.workspace.focus_mode;
        if self.workspace.focus_mode {
            self.focus = Focus::Pane;
            self.status = "focus mode: only the active pane (z to exit)".to_string();
        } else {
            self.status = "focus mode off".to_string();
        }
    }

    fn toggle_sidebar(&mut self) {
        self.workspace.show_sidebar = !self.workspace.show_sidebar;
        if !self.workspace.show_sidebar && self.focus == Focus::Sidebar {
            self.focus = Focus::Pane;
        }
        self.status = format!(
            "sidebar {}",
            if self.workspace.show_sidebar {
                "shown"
            } else {
                "hidden"
            }
        );
    }

    fn toggle_detail(&mut self) {
        self.workspace.show_detail = !self.workspace.show_detail;
        self.status = format!(
            "detail panel {}",
            if self.workspace.show_detail {
                "shown"
            } else {
                "hidden"
            }
        );
    }

    fn toggle_chat_panel(&mut self) {
        self.workspace.show_chat = !self.workspace.show_chat;
        self.status = format!(
            "chat panel {}",
            if self.workspace.show_chat {
                "shown"
            } else {
                "hidden"
            }
        );
    }

    fn toggle_results_panel(&mut self) {
        self.workspace.show_results = !self.workspace.show_results;
        self.status = format!(
            "results panel {}",
            if self.workspace.show_results {
                "shown"
            } else {
                "hidden"
            }
        );
    }

    fn close_active_pane(&mut self) {
        if self.panes.len() <= 1 {
            self.panes.clear();
            self.focused_pane = 0;
            self.requeue_all_panes();
            return;
        }
        self.panes.remove(self.focused_pane);
        self.focused_pane = self.focused_pane.min(self.panes.len().saturating_sub(1));
        self.requeue_all_panes();
    }

    fn remove_active_file(&mut self) {
        let Some(file_id) = self.schema_target() else {
            return;
        };
        // A merged view is not a file; there is nothing to remove from the project.
        if self.project.get_file(&file_id).map(|file| file.is_merged()) == Some(true) {
            self.status = "select a file in the sidebar to remove".to_string();
            return;
        }

        self.live_sources.remove(&file_id);
        self.project.remove_file(&file_id);
        let fallback = self
            .project
            .files
            .iter()
            .find(|file| !file.is_merged())
            .map(|file| file.file_id.clone());
        match fallback {
            Some(fallback) => self.repoint_panes(&fallback),
            None => {
                self.panes.clear();
                self.focused_pane = 0;
            }
        }
        self.prune_merged_views();
        self.requeue_all_panes();
        self.autosave_project();
        self.status = "file removed from project".to_string();
    }

    /// Enter means "open this thing's detail view". For a log source, a filter and a
    /// saved search that view is an editor over the schema, the rule and the query; Space
    /// is what selects. (Showing one file alone is Space on it with the others deselected.)
    fn activate_sidebar_item(&mut self) {
        if self.focus != Focus::Sidebar {
            return;
        }
        let items = self.sidebar_items();
        let Some(item) = items.get(self.sidebar_selected).cloned() else {
            return;
        };
        match item {
            SidebarItem::File { file_id, .. } => self.open_source_editor_for(&file_id),
            SidebarItem::Filter { index, .. } => self.open_filter_builder(Some(index)),
            // The picker, not the filter grammar: a range is edited as two timestamps.
            SidebarItem::TimeFilter { .. } => self.open_time_picker(),
            SidebarItem::Search { index, text, .. } => {
                self.open_input(Mode::EditSearch { index, text });
            }
            SidebarItem::Bookmark { index, .. } => self.jump_bookmark_index(index),
            // `none - t` under Time. Enter on it is the obvious way to make one.
            SidebarItem::Hint(label) if label.trim() == "none - t" => self.open_time_picker(),
            _ => {}
        }
    }

    fn open_file_in_focused(&mut self, file_id: &str) {
        let Some(file_index) = self.project.file_index(file_id) else {
            return;
        };
        if self.panes.is_empty() {
            self.panes.push(PaneState {
                view: build_view("L1", &self.project.files[file_index], &self.project.filters),
            });
            self.focused_pane = 0;
        } else {
            let leaf_id = self.panes[self.focused_pane].view.leaf_id.clone();
            self.panes[self.focused_pane].view = build_view(
                leaf_id,
                &self.project.files[file_index],
                &self.project.filters,
            );
        }
        self.focus = Focus::Pane;
        self.queue_recompute(self.focused_pane, After::Nothing);
    }

    fn rebuild_active_view(&mut self) {
        self.queue_recompute(self.focused_pane, After::Nothing);
    }

    /// Filters are project-global: edit the project's set, push it to every pane,
    /// then persist so the same filters come back on the next run.
    fn mutate_filters(&mut self, edit: impl FnOnce(&mut FilterSet)) {
        edit(&mut self.project.filters);
        for pane in &mut self.panes {
            pane.view.filters = self.project.filters.clone();
            // Visible positions shift under a new filter, so a live selection would
            // highlight the wrong rows.
            pane.view.clear_selection();
        }
        for pane_index in 0..self.panes.len() {
            self.queue_recompute(pane_index, After::Nothing);
        }
        self.autosave_project();
    }

    /// Persist without overwriting the caller's status message on success.
    fn autosave_project(&mut self) {
        self.capture_session();
        if let Err(exc) = self.project.save() {
            self.status = format!("save failed: {exc}");
        }
    }

    fn clamp_scroll(&mut self, pane_index: usize, height: usize) {
        let Some(view) = self.panes.get_mut(pane_index).map(|pane| &mut pane.view) else {
            return;
        };
        if view.cursor < view.scroll_y {
            view.scroll_y = view.cursor;
        }
        if view.cursor >= view.scroll_y + height {
            view.scroll_y = view.cursor.saturating_sub(height.saturating_sub(1));
        }
    }

    fn active_page_size(&self) -> usize {
        20
    }

    fn search_results_visible(&self) -> bool {
        if matches!(self.mode, Mode::Search(_)) {
            return false;
        }
        self.active_view()
            .map(|view| view.query.is_some() && !view.query_text.trim().is_empty())
            .unwrap_or(false)
    }

    fn active_result_positions(&self) -> Vec<usize> {
        self.active_view()
            .map(|view| view.match_positions())
            .unwrap_or_default()
    }

    fn jump_to_selected_result(&mut self) {
        let positions = self.active_result_positions();
        let Some(position) = positions.get(self.results_selected).copied() else {
            self.status = "no matches".to_string();
            return;
        };
        if let Some(view) = self.active_view_mut() {
            view.move_cursor_to(position);
        }
        self.status = format!("jumped to match {}", self.results_selected + 1);
    }

    fn sync_selected_result_to_cursor(&mut self) {
        let Some(cursor) = self.active_view().map(|view| view.cursor) else {
            self.results_selected = 0;
            self.results_scroll = 0;
            return;
        };
        let positions = self.active_result_positions();
        if let Some(index) = positions.iter().position(|position| *position == cursor) {
            self.results_selected = index;
        } else if self.results_selected >= positions.len() {
            self.results_selected = positions.len().saturating_sub(1);
        }
    }

    fn clamp_results_scroll(&mut self, height: usize, count: usize) {
        if count == 0 {
            self.results_selected = 0;
            self.results_scroll = 0;
            return;
        }
        if self.results_selected >= count {
            self.results_selected = count.saturating_sub(1);
        }
        if self.results_selected < self.results_scroll {
            self.results_scroll = self.results_selected;
        }
        if self.results_selected >= self.results_scroll + height {
            self.results_scroll = self
                .results_selected
                .saturating_sub(height.saturating_sub(1));
        }
    }

    fn hide_choice_fields(&self) -> Vec<String> {
        let Some((file, _)) = self.active_file_view() else {
            return vec!["message".to_string()];
        };
        let Some(global) = self.target_globals().first().copied() else {
            return vec!["message".to_string()];
        };
        let Some(entry) = file.entries.get(global) else {
            return vec!["message".to_string()];
        };
        file.extractor_for(entry)
            .map(|extractor| extractor.field_names.clone())
            .filter(|fields| !fields.is_empty())
            .unwrap_or_else(|| vec!["message".to_string()])
    }

    /// Those fields paired with what the targeted line holds in each, so the menu can be
    /// read as "hide lines whose level is Trace" rather than "hide by level".
    fn hide_choice_entries(&self) -> Vec<(String, String)> {
        let fields = self.hide_choice_fields();
        let Some((file, _)) = self.active_file_view() else {
            return fields
                .into_iter()
                .map(|field| (field, String::new()))
                .collect();
        };
        let entry = self
            .target_globals()
            .first()
            .copied()
            .and_then(|global| file.entries.get(global));
        fields
            .into_iter()
            .map(|field| {
                let value = entry
                    .map(|entry| file.get_field(entry, &field))
                    .unwrap_or_default();
                (field, value)
            })
            .collect()
    }

    fn active_file_index(&self) -> Option<usize> {
        let view = self.active_view()?;
        self.project.file_index(&view.file_id)
    }

    fn project_filter_folder_path(&self) -> PathBuf {
        self.project.root.join(CONFIG_DIR).join("filters")
    }

    /// Prefer the shared user-level library; fall back to the project when `$HOME` is unset.
    fn default_filter_folder_path(&self) -> PathBuf {
        user_filter_dir().unwrap_or_else(|| self.project_filter_folder_path())
    }

    /// Prefilled popup text. Keep the `~` form when we can: it is shorter to read and
    /// `filter_folder_from_input` expands it back.
    fn default_filter_folder_input(&self) -> String {
        if user_filter_dir().is_some() {
            format!("~/{}/{}", USER_DIR, USER_FILTERS_SUBDIR)
        } else {
            self.default_filter_folder_path()
                .to_string_lossy()
                .to_string()
        }
    }

    fn project_search_folder_path(&self) -> PathBuf {
        self.project.root.join(CONFIG_DIR).join("searches")
    }

    fn default_search_folder_path(&self) -> PathBuf {
        user_search_dir().unwrap_or_else(|| self.project_search_folder_path())
    }

    fn default_search_folder_input(&self) -> String {
        if user_search_dir().is_some() {
            format!("~/{}/{}", USER_DIR, USER_SEARCHES_SUBDIR)
        } else {
            self.default_search_folder_path()
                .to_string_lossy()
                .to_string()
        }
    }

    fn project_schema_folder_path(&self) -> PathBuf {
        self.project.root.join(CONFIG_DIR).join("schemas")
    }

    fn default_schema_folder_path(&self) -> PathBuf {
        user_schema_dir().unwrap_or_else(|| self.project_schema_folder_path())
    }

    fn default_schema_folder_input(&self) -> String {
        if user_schema_dir().is_some() {
            format!("~/{}/{}", USER_DIR, USER_SCHEMAS_SUBDIR)
        } else {
            self.default_schema_folder_path()
                .to_string_lossy()
                .to_string()
        }
    }

    fn project_bookmark_folder_path(&self) -> PathBuf {
        self.project
            .root
            .join(CONFIG_DIR)
            .join(USER_BOOKMARKS_SUBDIR)
    }

    fn default_bookmark_folder_path(&self) -> PathBuf {
        home_dir()
            .map(|home| home.join(USER_DIR).join(USER_BOOKMARKS_SUBDIR))
            .unwrap_or_else(|| self.project_bookmark_folder_path())
    }

    fn default_bookmark_folder_input(&self) -> String {
        if home_dir().is_some() {
            format!("~/{}/{}", USER_DIR, USER_BOOKMARKS_SUBDIR)
        } else {
            self.default_bookmark_folder_path()
                .to_string_lossy()
                .to_string()
        }
    }

    fn default_incident_file_input(&self) -> String {
        format!("{CONFIG_DIR}/incident.md")
    }

    fn incident_file_from_input(&self, input: &str) -> PathBuf {
        let trimmed = input.trim();
        let path = if trimmed.is_empty() {
            PathBuf::from(self.default_incident_file_input())
        } else {
            expand_tilde(trimmed)
        };
        if path.is_absolute() {
            path
        } else {
            self.project.root.join(path)
        }
    }

    fn schema_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_schema_folder_path)
    }

    fn filter_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_filter_folder_path)
    }

    fn search_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_search_folder_path)
    }

    fn bookmark_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_bookmark_folder_path)
    }

    fn install_default_search_library_if_default(
        &self,
        folder: &Path,
    ) -> Result<usize, Option<io::Error>> {
        if folder != self.default_search_folder_path() {
            return Err(None);
        }
        install_default_search_library(folder).map_err(Some)
    }

    /// `~` expands, an absolute path is taken as-is, and a relative one resolves inside
    /// the project so `.logscouter/filters` reaches the project-local folder.
    fn folder_from_input(&self, input: &str, fallback: impl Fn(&Self) -> PathBuf) -> PathBuf {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return fallback(self);
        }
        let path = expand_tilde(trimmed);
        if path.is_absolute() {
            path
        } else {
            self.project.root.join(path)
        }
    }

    fn active_file_view(&self) -> Option<(&LogFileModel, &ViewModel)> {
        let view = self.active_view()?;
        let file = self.project.get_file(&view.file_id)?;
        Some((file, view))
    }

    fn entry_detail_target(&self) -> Option<(&LogFileModel, &LogEntry)> {
        let (file, view) = self.active_file_view()?;
        let global = if view.selection_count() == 1 {
            view.selected_globals().first().copied()
        } else {
            view.current_index()
        }?;
        file.entries.get(global).map(|entry| (file, entry))
    }

    fn active_view(&self) -> Option<&ViewModel> {
        self.panes.get(self.focused_pane).map(|pane| &pane.view)
    }

    fn active_view_mut(&mut self) -> Option<&mut ViewModel> {
        self.panes
            .get_mut(self.focused_pane)
            .map(|pane| &mut pane.view)
    }

    fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        items.push(SidebarItem::Section("Files".to_string()));
        // A merged view is a property of a pane, not a file, so it is not listed.
        let shown = self.active_view_source_ids();
        let real: Vec<&LogFileModel> = self
            .project
            .files
            .iter()
            .filter(|file| !file.is_merged())
            .collect();
        if real.is_empty() {
            items.push(SidebarItem::Hint(
                "none - press a or add live source".to_string(),
            ));
        } else {
            for file in real {
                let suffix = if self.live_sources.contains_key(&file.file_id) {
                    format!(" live ({})", file.entries.len())
                } else if !file.error.is_empty() {
                    " !".to_string()
                } else if file.loaded {
                    format!(" ({})", file.entries.len())
                } else {
                    " loading".to_string()
                };
                // A star marks each file feeding the focused pane's view.
                let star = if shown.contains(&file.file_id) {
                    '*'
                } else {
                    ' '
                };
                items.push(SidebarItem::File {
                    file_id: file.file_id.clone(),
                    label: format!("{star} {}{}", file.display_name, suffix),
                });
            }
        }

        items.push(SidebarItem::Section("Filters".to_string()));
        items.push(SidebarItem::SubSection("Text".to_string()));
        let text_rules: Vec<(usize, &FilterRule)> = self.project.filters.text_rules().collect();
        if text_rules.is_empty() {
            // Indented by hand: a hint under a sub-section lines up with its rows, and a
            // hint under a section lines up with those.
            items.push(SidebarItem::Hint("  none - f or H".to_string()));
        } else {
            for (index, rule) in text_rules {
                let mark = if rule.enabled { "*" } else { "o" };
                items.push(SidebarItem::Filter {
                    index,
                    label: format!("{mark} {}", rule.describe()),
                });
            }
        }

        items.push(SidebarItem::SubSection("Time".to_string()));
        match self.project.filters.time_index() {
            Some(index) => {
                let rule = &self.project.filters.rules[index];
                let mark = if rule.enabled { "*" } else { "o" };
                items.push(SidebarItem::TimeFilter {
                    index,
                    label: format!("{mark} {}", describe_time_range(rule)),
                });
            }
            None => items.push(SidebarItem::Hint("  none - t".to_string())),
        }

        items.push(SidebarItem::Section("Saved Searches".to_string()));
        if self.project.saved_searches.is_empty() {
            items.push(SidebarItem::Hint("none".to_string()));
        } else {
            // A star marks the search the focused pane is running, mirroring the file and
            // filter rows, so Space reads the same everywhere.
            let active = self
                .active_view()
                .map(|view| view.query_text.clone())
                .unwrap_or_default();
            for (index, search) in self.project.saved_searches.iter().enumerate() {
                let mark = if !search.is_empty() && *search == active {
                    '*'
                } else {
                    ' '
                };
                items.push(SidebarItem::Search {
                    index,
                    text: search.clone(),
                    label: format!("{mark} /{search}"),
                });
            }
        }

        items.push(SidebarItem::Section("Bookmarks".to_string()));
        if self.project.bookmarks.is_empty() {
            items.push(SidebarItem::Hint("none - m".to_string()));
        } else {
            for (index, bookmark) in self.project.bookmarks.iter().enumerate() {
                items.push(SidebarItem::Bookmark {
                    index,
                    label: self.bookmark_sidebar_label(bookmark),
                });
            }
        }

        items
    }

    fn save_project(&mut self) {
        self.capture_session();
        match self.project.save() {
            Ok(()) => self.status = "saved".to_string(),
            Err(exc) => self.status = format!("save failed: {exc}"),
        }
    }
}

/// Copy via OSC 52, which asks the *terminal* to set the clipboard. Unlike a native
/// clipboard binding this survives SSH, which is where these logs are usually read.
fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    stdout.flush()
}

fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let bits = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(bits >> 18) as usize & 63] as char);
        out.push(ALPHABET[(bits >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[bits as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn markdown_code_block(text: &str) -> String {
    let mut fence = "```".to_string();
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}text\n{text}\n{fence}\n")
}

/// Attach the project filters but do not apply them: the caller queues a recompute so
/// the (potentially multi-second) filter pass runs behind a progress bar.
fn build_view(leaf_id: impl Into<String>, file: &LogFileModel, filters: &FilterSet) -> ViewModel {
    let mut view = ViewModel::new(leaf_id, file);
    view.filters = filters.clone();
    view
}

/// Filter rules are long ("exclude message contains '...'") and now sit two levels deep
/// under `Filters > Text`, so let a roomy terminal spend a quarter of its width on the
/// sidebar. Never starve the log panes.
fn sidebar_width(body_width: u16) -> u16 {
    (body_width / 4)
        .clamp(36, 56)
        .min(body_width.saturating_sub(24))
}

/// A hand-resized sidebar keeps at least this many columns, and leaves the panes at least
/// `MIN_PANE_WIDTH`.
const MIN_SIDEBAR_WIDTH: u16 = 16;
const MIN_PANE_WIDTH: u16 = 20;

/// Split into fixed-width character chunks. Unlike word wrapping this never reflows,
/// so the caret's (row, column) is a plain division.
fn chunk_chars(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn insert_char(text: &mut String, char_index: usize, ch: char) {
    let byte = byte_offset(text, char_index);
    text.insert(byte, ch);
}

fn remove_char(text: &mut String, char_index: usize) {
    let byte = byte_offset(text, char_index);
    if byte < text.len() {
        text.remove(byte);
    }
}

fn byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len())
}

fn log_schema_scope_from_token(token: &str) -> Option<String> {
    ["schema=", "log_schema=", "schema:", "log_schema:"]
        .iter()
        .find_map(|prefix| token.strip_prefix(prefix))
        .map(str::trim)
        .filter(|schema| !schema.is_empty())
        .map(ToString::to_string)
}

/// The instructions handed to the LLM for schema inference: teach it log-scouter's format
/// language and pin the output to a JSON object the app can parse.
fn schema_infer_prompt() -> String {
    "You infer a log-parsing schema for log-scouter from sample log lines.\n\
     A schema is a `format` string made of `<field>` placeholders separated by the literal \
     text that appears between fields in the log (brackets, colons, spaces). Use `<field?>` \
     for a field only some lines carry; it also consumes the literal separator right before \
     it. Exactly one field is the timestamp and it MUST be named <timestamp>.\n\
     The format may contain literal newline characters when one logical log entry spans \
     multiple physical lines. If records span multiple lines, include `entry_start` as a \
     regex matching the first physical line of each record. Include `entry_end` when there \
     is a reliable closing line. For mostly single-line logs with stack traces or exception \
     chains, keep a one-line format and set `entry_start` to the regex for ordinary log \
     header lines; non-matching lines will be merged into the previous entry. Use \
     `field_patterns` only when a field needs a tighter regex than the default.\n\n\
     Reply with ONLY a JSON object -- no prose, no code fence:\n\
     {\"name\": \"<short name>\", \"format\": \"<the format string>\", \
     \"timestamp_format\": \"<chrono strftime for the timestamp, e.g. %Y-%m-%d %H:%M:%S%.3f>\", \
     \"entry_start\": \"<optional regex>\", \"entry_end\": \"<optional regex>\", \
     \"field_patterns\": {\"field\": \"<optional regex>\"}, \"description\": \"<one line>\"}\n\n\
     With each <field> replaced by that line's value, the format must reproduce the sample \
     lines exactly. Prefer specific literals so it does not over-match. Example: for the line\n\
     [2026-06-16 10:09:43.288][Kernel][Info] service started\n\
     use format `[<timestamp>][<module>][<level>] <message>` and timestamp_format \
     `%Y-%m-%d %H:%M:%S%.3f`. For a block beginning with `{` and ending with `}`, use \
     entry_start `^\\s*\\{\\s*$` and entry_end `^\\s*\\}\\s*$`."
        .to_string()
}

/// Repair a schema whose `entry_start` matches none of `lines`, given the sample lines.
/// A start regex that matches nothing makes every line a continuation, so the whole file
/// folds into a single entry -- the "it matched all of the file as one record" symptom.
/// When clearing the bad `entry_start` lets the format's own derived header probe group the
/// lines, clear it and say so; otherwise warn. Pure, so the decision is unit-tested apart
/// from the file I/O in `repair_inferred_schema`.
fn repair_entry_start(extractor: &mut Extractor, lines: &[String]) -> Option<String> {
    let non_empty: Vec<&String> = lines.iter().filter(|line| !line.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return None;
    }

    let groups_the_sample =
        |candidate: &Extractor| non_empty.iter().any(|line| candidate.is_start(line));

    if groups_the_sample(extractor) {
        return None; // grouping already works
    }

    if !extractor.entry_start.trim().is_empty() {
        let mut derived = extractor.clone();
        derived.entry_start = String::new();
        if derived.compile_relaxed().is_ok() && groups_the_sample(&derived) {
            *extractor = derived;
            return Some("adjusted its entry_start so records group correctly".to_string());
        }
    }

    Some(
        "warning: its entry_start matches no lines, so the file stays one entry -- press e to fix it"
            .to_string(),
    )
}

/// Build an `Extractor` from the LLM's JSON reply. Tolerates surrounding prose or a code
/// fence around the object. Built directly rather than through the `name | format | …` text
/// grammar, because a format may itself contain `|` (pipe-delimited logs), which that grammar
/// splits on.
fn parse_inferred_schema(text: &str) -> Result<Extractor, String> {
    let json = extract_json_object(text).ok_or("no JSON object in the reply")?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|error| format!("bad JSON: {error}"))?;
    extractor_from_schema_json(&value, "AI schema")
}

fn extractor_from_schema_json(
    value: &serde_json::Value,
    fallback_name: &str,
) -> Result<Extractor, String> {
    let schema_value = value.get("schema").unwrap_or(value);
    if schema_value.get("name").is_some() {
        if let Ok(mut extractor) = serde_json::from_value::<Extractor>(schema_value.clone()) {
            if extractor.description.trim().is_empty() {
                extractor.description = value
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            }
            if extractor.name.trim().is_empty() {
                extractor.name = value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(fallback_name)
                    .trim()
                    .to_string();
            }
            extractor.compile()?;
            return Ok(extractor);
        }
    }

    let str_field = |key: &str| {
        schema_value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .or_else(|| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
            })
    };

    let format = str_field("format")
        .filter(|value| !value.is_empty())
        .ok_or("the reply has no \"format\"")?;
    let name = str_field("name")
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name);
    let timestamp = str_field("timestamp_format")
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_TIMESTAMP_FORMAT);
    let mut extractor = Extractor::with_timestamp_format(name, format, timestamp)?;
    extractor.entry_start = [
        "entry_start",
        "entry_start_regex",
        "start_regex",
        "start_pattern",
    ]
    .into_iter()
    .find_map(|key| str_field(key).filter(|value| !value.is_empty()))
    .unwrap_or("")
    .to_string();
    extractor.entry_end = ["entry_end", "entry_end_regex", "end_regex", "end_pattern"]
        .into_iter()
        .find_map(|key| str_field(key).filter(|value| !value.is_empty()))
        .unwrap_or("")
        .to_string();
    if let Some(patterns) = schema_value
        .get("field_patterns")
        .and_then(serde_json::Value::as_object)
    {
        extractor.field_patterns.clear();
        for (name, pattern) in patterns {
            if let Some(pattern) = pattern
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                extractor
                    .field_patterns
                    .insert(name.trim().to_string(), pattern.to_string());
            }
        }
    }
    extractor.description = str_field("description").unwrap_or("").to_string();
    extractor.compile()?;
    Ok(extractor)
}

/// Extract the first balanced `{...}` object from `text`, ignoring braces inside strings, so
/// a JSON reply wrapped in prose or ```json fences still parses.
fn extract_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + offset + ch.len_utf8()].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_log_schema_input(input: &str) -> Result<Extractor, String> {
    if looks_like_schema_json_input(input) {
        let json = extract_json_object(input).ok_or("no JSON object in the schema input")?;
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| format!("bad JSON: {error}"))?;
        return extractor_from_schema_json(&value, "Custom schema");
    }

    let parts: Vec<&str> = input.splitn(6, '|').map(str::trim).collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(
            "schema needs: name | format | timestamp format | description | entry start regex | entry end regex"
                .to_string(),
        );
    }

    let (timestamp_format, description) = match parts.as_slice() {
        [_, _, timestamp_format, description, ..] => (
            if timestamp_format.is_empty() {
                DEFAULT_TIMESTAMP_FORMAT
            } else {
                timestamp_format
            },
            *description,
        ),
        [_, _, third] if looks_like_timestamp_format(third) => (*third, ""),
        [_, _, description] => (DEFAULT_TIMESTAMP_FORMAT, *description),
        _ => (DEFAULT_TIMESTAMP_FORMAT, ""),
    };
    let mut extractor = Extractor::with_timestamp_format(parts[0], parts[1], timestamp_format)?;
    extractor.description = description.to_string();
    if let Some(entry_start) = parts.get(4).filter(|value| !value.trim().is_empty()) {
        extractor.entry_start = (*entry_start).to_string();
    }
    if let Some(entry_end) = parts.get(5).filter(|value| !value.trim().is_empty()) {
        extractor.entry_end = (*entry_end).to_string();
    }
    extractor.compile()?;
    Ok(extractor)
}

fn looks_like_schema_json_input(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with("```")
}

fn format_filter_datetime(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// The time range as it fits a sidebar row: `10:09:03 → 10:09:05  (2s)`. The date is
/// dropped when both ends share one and the detail panel can say it, and dropped to
/// `06-16` otherwise -- a sidebar is 36 columns wide, and a year is never the surprise.
fn describe_time_range(rule: &FilterRule) -> String {
    let state = if rule.enabled { "" } else { " (off)" };
    let (start, end) = rule.time_bounds();
    let body = match (parse_datetime(start), parse_datetime(end)) {
        (Some(low), Some(high)) => {
            let dated = low.date() != high.date();
            format!(
                "{} → {}  ({})",
                format_range_bound(low, dated),
                format_range_bound(high, dated),
                format_range_span(high - low)
            )
        }
        (Some(low), None) => format!("from {}", format_range_bound(low, true)),
        (None, Some(high)) => format!("until {}", format_range_bound(high, true)),
        // Neither end parses: show what is stored rather than pretend.
        (None, None) => rule.value.clone(),
    };
    format!("{body}{state}")
}

/// Full timestamps, for the status line and anywhere else with room to spell them out.
fn summarize_time_range(rule: &FilterRule) -> String {
    let (start, end) = rule.time_bounds();
    match (start.is_empty(), end.is_empty()) {
        (false, false) => format!("{start} → {end}"),
        (false, true) => format!("from {start}"),
        (true, false) => format!("until {end}"),
        (true, true) => rule.value.clone(),
    }
}

/// A short, human-readable description of what changed between two snapshots, for the action
/// log. Best-effort: it reports the single most salient difference.
fn describe_change(before: &Snapshot, after: &Snapshot) -> String {
    if before.filters != after.filters {
        if before.filters.time_rule() != after.filters.time_rule() {
            return match after.filters.time_rule() {
                Some(rule) => format!("changed time range: {}", summarize_time_range(rule)),
                None => "cleared time range".to_string(),
            };
        }
        let (b, a) = (&before.filters.rules, &after.filters.rules);
        if a.len() > b.len() {
            return match a.iter().find(|rule| !b.contains(rule)) {
                Some(rule) => format!("added filter: {}", rule.describe()),
                None => "added filter".to_string(),
            };
        }
        if a.len() < b.len() {
            return match b.iter().find(|rule| !a.contains(rule)) {
                Some(rule) => format!("removed filter: {}", rule.describe()),
                None => "removed filter".to_string(),
            };
        }
        return "edited a filter".to_string();
    }
    if before.saved_searches != after.saved_searches {
        return "changed saved searches".to_string();
    }
    if before.bookmarks != after.bookmarks {
        let (b, a) = (&before.bookmarks, &after.bookmarks);
        if a.len() > b.len() {
            return match a.iter().find(|bookmark| !b.contains(bookmark)) {
                Some(bookmark) => format!("bookmarked line {}", bookmark.line_no),
                None => "added bookmark".to_string(),
            };
        }
        if a.len() < b.len() {
            return "removed bookmark".to_string();
        }
        return "edited bookmark note".to_string();
    }

    let queries = |snapshot: &Snapshot| -> Vec<String> {
        snapshot
            .session
            .panes
            .iter()
            .map(|pane| pane.query.clone())
            .collect()
    };
    if queries(before) != queries(after) {
        return match queries(after).into_iter().find(|query| !query.is_empty()) {
            Some(query) => format!("searched: {query:?}"),
            None => "cleared search".to_string(),
        };
    }

    let sources = |snapshot: &Snapshot| -> Vec<Vec<String>> {
        snapshot
            .session
            .panes
            .iter()
            .map(|pane| pane.file_ids.clone())
            .collect()
    };
    if sources(before) != sources(after) {
        return "changed the pane's sources".to_string();
    }

    "changed the layout".to_string()
}

fn format_range_bound(value: NaiveDateTime, with_date: bool) -> String {
    let time = if value.and_utc().timestamp_subsec_millis() == 0 {
        value.format("%H:%M:%S").to_string()
    } else {
        value.format("%H:%M:%S%.3f").to_string()
    };
    if with_date {
        format!("{} {time}", value.format("%m-%d"))
    } else {
        time
    }
}

/// How long a range is. `format_elapsed` writes an offset from a mark, so it leads with a
/// sign and always spells out the milliseconds; the length of a range wants neither.
fn format_range_span(delta: ChronoDuration) -> String {
    let total = delta.num_milliseconds().max(0);
    let (seconds, millis) = (total / 1000, total % 1000);
    if seconds < 60 {
        return if millis == 0 {
            format!("{seconds}s")
        } else {
            format!("{seconds}.{millis:03}s")
        };
    }
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if minutes < 60 {
        return if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{seconds:02}s")
        };
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if hours < 24 {
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes:02}m")
        };
    }
    let (days, hours) = (hours / 24, hours % 24);
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{hours:02}h")
    }
}

/// "hides 412 of 8,201 shown lines", or the same in the keep direction. The count is what
/// happens to the pane the user is looking at, which is the question they are asking.
fn preview_summary(preview: &PatternPreview, exclude: bool) -> String {
    if preview.total == 0 {
        return "nothing on screen to match".to_string();
    }
    let verb = if exclude { "hides" } else { "keeps" };
    if preview.capped() {
        return format!(
            "{verb} {} of the first {} rows scanned ({} shown)",
            thousands(preview.matched),
            thousands(preview.scanned),
            thousands(preview.total)
        );
    }
    if preview.matched == 0 {
        return format!(
            "matches none of the {} shown rows",
            thousands(preview.total)
        );
    }
    format!(
        "{verb} {} of {} shown rows",
        thousands(preview.matched),
        thousands(preview.total)
    )
}

/// `8201` -> `8,201`. A blast radius is easier to judge when it is grouped.
fn thousands(value: usize) -> String {
    let digits: Vec<char> = value.to_string().chars().collect();
    let lead = match digits.len() % 3 {
        0 => 3,
        remainder => remainder,
    };
    let mut out: String = digits[..lead].iter().collect();
    for group in digits[lead..].chunks(3) {
        out.push(',');
        out.extend(group);
    }
    out
}

/// A regex compile error spans several lines with a caret diagram; the popup has one.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Step `current` by `delta`, wrapping around a list of `len`. Used by the filter builder's
/// dropdown rows.
fn cycle_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as isize;
    (((current as isize + delta) % len + len) % len) as usize
}

/// A field's value, short enough to sit in the menu: its first three words. A message
/// runs to hundreds of characters, and the first few are what identify it.
fn field_value_preview(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "(empty)".to_string();
    }
    let mut words = value.split_whitespace();
    let head: Vec<&str> = words.by_ref().take(3).collect();
    let more = words.next().is_some();
    format!("{}{}", head.join(" "), if more { " …" } else { "" })
}

fn hide_choice_index(ch: char) -> Option<usize> {
    match ch {
        '1'..='9' => Some(ch as usize - '1' as usize),
        '0' => Some(9),
        'a'..='z' => Some(10 + (ch as usize - 'a' as usize)),
        _ => None,
    }
}

/// Ten digits do not cover a schema with more fields than that, and a field with no key
/// cannot be picked at all. Letters carry the rest.
fn hide_choice_key(index: usize) -> Option<char> {
    match index {
        0..=8 => char::from_digit((index + 1) as u32, 10),
        9 => Some('0'),
        10..=35 => char::from_u32('a' as u32 + (index - 10) as u32),
        _ => None,
    }
}

fn looks_like_timestamp_format(input: &str) -> bool {
    input.contains('%')
}

/// End-truncate with an ellipsis so a clipped label reads as clipped, rather than
/// silently losing the tail of a filter value.
fn truncate_label(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Give the detail panel about half the sidebar, but only once the sidebar list
/// still has room to be useful above it.
fn detail_panel_height(sidebar_height: u16) -> u16 {
    if sidebar_height < 14 {
        0
    } else {
        (sidebar_height / 2).min(18)
    }
}

fn detail_lines(file: &LogFileModel, entry: &LogEntry, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let mut lines = Vec::new();
    let plain = Style::default();

    if let Some(source) = file.source_name(entry) {
        push_detail(&mut lines, "from", source, value_width, plain);
    }
    push_detail(
        &mut lines,
        "schema",
        file.log_schema_name_for(entry),
        value_width,
        plain,
    );
    push_detail(
        &mut lines,
        "line",
        &entry.line_no.to_string(),
        value_width,
        plain,
    );

    match file.extractor_for(entry) {
        Some(extractor) => {
            let mut fields = extractor.field_names.clone();
            if let Some(index) = fields.iter().position(|field| field == "message") {
                let message = fields.remove(index);
                fields.insert(0, message);
            }
            for field in &fields {
                let value = file.get_field(entry, field);
                let style = if matches!(field.as_str(), "level" | "log_level") {
                    level_style(&value)
                } else {
                    plain
                };
                push_detail(&mut lines, field, &value, value_width, style);
            }
        }
        None => push_detail(&mut lines, "message", &entry.raw, value_width, plain),
    }
    lines
}

fn full_detail_lines(file: &LogFileModel, entry: &LogEntry, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "parsed fields",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.extend(detail_lines(file, entry, width));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "raw",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    push_detail(&mut lines, "raw", &entry.raw, value_width, Style::default());
    lines
}

fn pretty_print_message(message: &str) -> Option<(&'static str, String)> {
    pretty_json(message)
        .map(|body| ("JSON", body))
        .or_else(|| pretty_xml(message).map(|body| ("XML", body)))
        .or_else(|| pretty_sql(message).map(|body| ("SQL", body)))
}

fn pretty_json(message: &str) -> Option<String> {
    let trimmed = message.trim();
    for candidate in json_candidates(trimmed) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) {
            if let Ok(body) = serde_json::to_string_pretty(&value) {
                return Some(body);
            }
        }
    }
    None
}

fn json_candidates(text: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    if matches!(text.chars().next(), Some('{') | Some('[')) {
        candidates.push(text);
    }
    for (start, ch) in text.char_indices() {
        if !matches!(ch, '{' | '[') {
            continue;
        }
        if let Some(len) = balanced_json_len(&text[start..]) {
            let candidate = &text[start..start + len];
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn balanced_json_len(text: &str) -> Option<usize> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for (offset, ch) in text.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(ch) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(offset + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

fn pretty_xml(message: &str) -> Option<String> {
    let trimmed = message.trim();
    let start = trimmed.find('<')?;
    let mut rest = &trimmed[start..];
    if !rest.contains('>') {
        return None;
    }

    let mut indent = 0usize;
    let mut lines = Vec::new();
    while let Some(tag_start) = rest.find('<') {
        let text = rest[..tag_start].trim();
        if !text.is_empty() {
            lines.push(format!("{}{}", "  ".repeat(indent), text));
        }

        rest = &rest[tag_start..];
        let tag_end = rest.find('>')?;
        let tag = rest[..=tag_end].trim();
        if tag.starts_with("</") {
            indent = indent.saturating_sub(1);
        }
        lines.push(format!("{}{}", "  ".repeat(indent), tag));
        if xml_opens_child(tag) {
            indent += 1;
        }
        rest = &rest[tag_end + 1..];
    }

    let tail = rest.trim();
    if !tail.is_empty() {
        lines.push(format!("{}{}", "  ".repeat(indent), tail));
    }

    (lines.len() > 1).then(|| lines.join("\n"))
}

fn xml_opens_child(tag: &str) -> bool {
    tag.starts_with('<')
        && !tag.starts_with("</")
        && !tag.starts_with("<?")
        && !tag.starts_with("<!")
        && !tag.ends_with("/>")
}

fn pretty_sql(message: &str) -> Option<String> {
    let trimmed = message.trim();
    let start = sql_start(trimmed)?;
    let sql = trimmed[start..].trim();
    if !looks_like_sql(sql) {
        return None;
    }

    let mut formatted = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    for keyword in [
        r"UNION(?:\s+ALL)?",
        r"(?:(?:LEFT|RIGHT|INNER|OUTER)\s+)?JOIN",
        r"GROUP\s+BY",
        r"ORDER\s+BY",
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "VALUES",
        "RETURNING",
        "FROM",
        "WHERE",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "SET",
        "AND",
        "OR",
    ] {
        let pattern = format!(r"(?i)\b{keyword}\b");
        let regex = regex::Regex::new(&pattern).ok()?;
        formatted = regex
            .replace_all(&formatted, |captures: &regex::Captures| {
                format!("\n{}", captures.get(0).unwrap().as_str())
            })
            .to_string();
    }
    formatted = formatted.replace(',', ",\n  ");

    let lines: Vec<String> = formatted
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            if sql_line_starts_clause(line) {
                line.to_string()
            } else {
                format!("  {line}")
            }
        })
        .collect();

    (lines.len() > 1).then(|| lines.join("\n"))
}

fn sql_start(text: &str) -> Option<usize> {
    ["SELECT", "WITH", "INSERT", "UPDATE", "DELETE"]
        .iter()
        .filter_map(|keyword| {
            let regex = regex::Regex::new(&format!(r"(?i)\b{}\b", keyword)).ok()?;
            regex.find(text).map(|hit| hit.start())
        })
        .min()
}

fn looks_like_sql(sql: &str) -> bool {
    let upper = format!(" {} ", sql.to_ascii_uppercase());
    if upper.contains(" SELECT ") || upper.contains(" WITH ") {
        return true;
    }
    if upper.contains(" INSERT ") {
        return upper.contains(" INTO ") || upper.contains(" VALUES ");
    }
    if upper.contains(" UPDATE ") {
        return upper.contains(" SET ");
    }
    if upper.contains(" DELETE ") {
        return upper.contains(" FROM ");
    }
    false
}

fn sql_line_starts_clause(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    [
        "SELECT",
        "WITH",
        "INSERT",
        "UPDATE",
        "DELETE",
        "VALUES",
        "RETURNING",
        "FROM",
        "WHERE",
        "GROUP BY",
        "ORDER BY",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "JOIN",
        "LEFT JOIN",
        "RIGHT JOIN",
        "INNER JOIN",
        "OUTER JOIN",
        "UNION",
        "UNION ALL",
        "SET",
        "AND",
        "OR",
    ]
    .iter()
    .any(|keyword| upper.starts_with(keyword))
}

fn pretty_body_lines(body: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in body.lines() {
        if raw.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        for chunk in chunk_chars(raw, width) {
            lines.push(Line::from(Span::raw(chunk)));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn file_detail_lines(file: &LogFileModel, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let plain = Style::default();
    let mut lines = Vec::new();
    push_detail(&mut lines, "file", &file.display_name, value_width, plain);
    if !file.label.is_empty() {
        push_detail(&mut lines, "label", &file.label, value_width, plain);
    }
    if !file.description.is_empty() {
        push_detail(&mut lines, "note", &file.description, value_width, plain);
    }
    if !file.tag.is_empty() {
        push_detail(&mut lines, "tag", &file.tag, value_width, plain);
    }
    push_detail(
        &mut lines,
        "path",
        &file.path.display().to_string(),
        value_width,
        plain,
    );
    push_detail(
        &mut lines,
        "schema",
        &file.extractor_name,
        value_width,
        plain,
    );
    if let Some(extractor) = file.extractor.as_ref() {
        push_detail(
            &mut lines,
            "schema_desc",
            &extractor.description,
            value_width,
            plain,
        );
        push_detail(&mut lines, "format", &extractor.format, value_width, plain);
    }
    push_detail(
        &mut lines,
        "entries",
        &file.entries.len().to_string(),
        value_width,
        plain,
    );
    push_detail(
        &mut lines,
        "total_lines",
        &total_line_count(file).to_string(),
        value_width,
        plain,
    );
    let status = if !file.error.is_empty() {
        format!("error: {}", file.error)
    } else if file.loaded {
        "loaded".to_string()
    } else {
        "loading".to_string()
    };
    push_detail(&mut lines, "status", &status, value_width, plain);
    lines
}

/// The time range broken into the two things a person edits, plus how long it spans.
fn time_filter_detail_lines(rule: &FilterRule, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let plain = Style::default();
    let (start, end) = rule.time_bounds();
    let mut lines = Vec::new();

    push_detail(&mut lines, "start", open_end(start), value_width, plain);
    push_detail(&mut lines, "end", open_end(end), value_width, plain);
    if let (Some(low), Some(high)) = (parse_datetime(start), parse_datetime(end)) {
        push_detail(
            &mut lines,
            "span",
            &format_range_span(high - low),
            value_width,
            plain,
        );
    }
    push_detail(
        &mut lines,
        "enabled",
        if rule.enabled { "true" } else { "false" },
        value_width,
        plain,
    );
    push_detail(
        &mut lines,
        "edit",
        "Enter reopens the picker",
        value_width,
        plain,
    );
    lines
}

/// How an omitted end of a range reads in the detail panel.
fn open_end(bound: &str) -> &str {
    if bound.is_empty() {
        "(open)"
    } else {
        bound
    }
}

fn filter_detail_lines(rule: &FilterRule, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let plain = Style::default();
    let mut lines = Vec::new();
    push_detail(&mut lines, "action", &rule.action, value_width, plain);
    push_detail(&mut lines, "field", &rule.field, value_width, plain);
    push_detail(&mut lines, "op", &rule.op, value_width, plain);
    push_detail(&mut lines, "value", &rule.value, value_width, plain);
    push_detail(
        &mut lines,
        "schema",
        rule.log_schema.as_deref().unwrap_or("all"),
        value_width,
        plain,
    );
    push_detail(
        &mut lines,
        "enabled",
        if rule.enabled { "true" } else { "false" },
        value_width,
        plain,
    );
    push_detail(&mut lines, "summary", &rule.describe(), value_width, plain);
    lines
}

fn search_detail_lines(search: &str, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let mut lines = Vec::new();
    push_detail(&mut lines, "search", search, value_width, Style::default());
    lines
}

fn label_detail_lines(kind: &str, label: &str, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let mut lines = Vec::new();
    push_detail(&mut lines, kind, label, value_width, Style::default());
    lines
}

fn source_default_short_name(file: &LogFileModel) -> String {
    if !file.label.trim().is_empty() {
        return file.label.clone();
    }
    if file.is_live() && !file.display_name.trim().is_empty() {
        return file.display_name.clone();
    }
    file.path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or(&file.display_name)
        .to_string()
}

fn live_quick_pick_field(field: LiveSourceField) -> bool {
    matches!(
        field,
        LiveSourceField::Namespace
            | LiveSourceField::Pod
            | LiveSourceField::Container
            | LiveSourceField::DockerContainer
    )
}

fn live_quick_pick_label(field: LiveSourceField) -> &'static str {
    match field {
        LiveSourceField::Namespace => "namespace",
        LiveSourceField::Pod => "pod",
        LiveSourceField::Container => "container",
        LiveSourceField::DockerContainer => "docker container",
        _ => "value",
    }
}

fn apply_live_quick_pick_value(
    editor: &mut LiveSourceEditor,
    target: LiveSourceField,
    value: &str,
) {
    match target {
        LiveSourceField::Namespace => {
            if editor.namespace != value {
                editor.pod.clear();
                editor.container.clear();
            }
            editor.namespace = value.to_string();
        }
        LiveSourceField::Pod => {
            if let Some((namespace, pod)) = value.split_once('/') {
                if editor.namespace != namespace {
                    editor.namespace = namespace.to_string();
                }
                editor.pod = pod.to_string();
            } else {
                editor.pod = value.to_string();
            }
            editor.container.clear();
        }
        LiveSourceField::Container => editor.container = value.to_string(),
        LiveSourceField::DockerContainer => editor.docker_container = value.to_string(),
        _ => {}
    }
}

fn discover_live_quick_pick_options(
    editor: &LiveSourceEditor,
    target: LiveSourceField,
) -> Result<Vec<String>, String> {
    match target {
        LiveSourceField::DockerContainer => command_lines(
            "docker",
            &["ps", "-a", "--format", "{{.Names}}"],
            "docker containers",
        ),
        LiveSourceField::Namespace => command_lines(
            "kubectl",
            &[
                "get",
                "namespaces",
                "-o",
                r#"jsonpath={range .items[*]}{.metadata.name}{"\n"}{end}"#,
            ],
            "kubernetes namespaces",
        ),
        LiveSourceField::Pod => {
            if editor.namespace.trim().is_empty() {
                command_lines(
                    "kubectl",
                    &[
                        "get",
                        "pods",
                        "--all-namespaces",
                        "-o",
                        r#"jsonpath={range .items[*]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}"#,
                    ],
                    "kubernetes pods",
                )
            } else {
                command_lines(
                    "kubectl",
                    &[
                        "get",
                        "pods",
                        "-n",
                        editor.namespace.trim(),
                        "-o",
                        r#"jsonpath={range .items[*]}{.metadata.name}{"\n"}{end}"#,
                    ],
                    "kubernetes pods",
                )
            }
        }
        LiveSourceField::Container => {
            let pod = editor.pod.trim();
            if pod.is_empty() {
                return Err("select or type a pod before picking a container".to_string());
            }
            let (namespace, pod) = pod
                .split_once('/')
                .map(|(namespace, pod)| (namespace, pod))
                .unwrap_or((editor.namespace.trim(), pod));
            let mut args = vec!["get", "pod", pod];
            if !namespace.is_empty() {
                args.extend(["-n", namespace]);
            }
            args.extend([
                "-o",
                r#"jsonpath={range .spec.containers[*]}{.name}{"\n"}{end}"#,
            ]);
            command_lines("kubectl", &args, "pod containers")
        }
        _ => Err("this field has no quick pick".to_string()),
    }
}

fn command_lines(program: &str, args: &[&str], label: &str) -> Result<Vec<String>, String> {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return if message.is_empty() {
            Err(format!("{program} exited with {}", output.status))
        } else {
            Err(format!("{program}: {message}"))
        };
    }

    let mut options: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    options.sort();
    options.dedup();
    if options.is_empty() {
        Err(format!("no {label} found"))
    } else {
        Ok(options)
    }
}

fn push_detail(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    value_width: usize,
    style: Style,
) {
    if value.trim().is_empty() {
        return;
    }
    let label = truncate_label(label, DETAIL_LABEL_WIDTH);
    for (index, chunk) in wrap_value(value, value_width).into_iter().enumerate() {
        let prefix = if index == 0 {
            format!("{label:<width$} ", width = DETAIL_LABEL_WIDTH)
        } else {
            " ".repeat(DETAIL_LABEL_WIDTH + 1)
        };
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(chunk, style),
        ]));
    }
}

fn selected_detail_line(line: Line<'static>) -> Line<'static> {
    let text = line_to_plain(&line);
    Line::from(Span::styled(
        text,
        Style::default().bg(Color::LightBlue).fg(Color::Black),
    ))
}

fn detail_surface_label(surface: DetailSurface) -> &'static str {
    match surface {
        DetailSurface::Inline | DetailSurface::Popup => "detail",
        DetailSurface::PrettyPrint => "pretty",
        DetailSurface::Help => "help",
    }
}

fn line_to_plain(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

/// Word-wrap to `width`, hard-splitting tokens too long to ever fit (paths, GUIDs,
/// SQL blobs) so no content is lost off the right edge.
fn wrap_value(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();

    for raw_line in text.split('\n') {
        let source = raw_line.replace('\t', "    ");
        let source = source.trim_end();
        if source.is_empty() {
            out.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0usize;
        for word in source.split_whitespace() {
            let word_len = word.chars().count();
            if current_len > 0 && current_len + 1 + word_len > width {
                out.push(std::mem::take(&mut current));
                current_len = 0;
            }

            if word_len > width {
                if current_len > 0 {
                    out.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                let mut chars = word.chars().peekable();
                while chars.peek().is_some() {
                    let chunk: String = chars.by_ref().take(width).collect();
                    let chunk_len = chunk.chars().count();
                    if chunk_len == width {
                        out.push(chunk);
                    } else {
                        current = chunk;
                        current_len = chunk_len;
                    }
                }
                continue;
            }

            if current_len > 0 {
                current.push(' ');
                current_len += 1;
            }
            current.push_str(word);
            current_len += word_len;
        }
        if !current.is_empty() {
            out.push(current);
        }
    }

    out
}

fn pad(text: &str, width: usize) -> String {
    let mut text = text.replace('\t', " ");
    if text.chars().count() > width {
        text = text
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>();
        text.push('~');
    }
    format!("{text:<width$}")
}

fn crop(text: &str, start: usize, width: usize) -> String {
    text.chars().skip(start).take(width).collect()
}

fn input_window(text: &str, caret: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let len = text.chars().count();
    let caret = caret.min(len);
    let start = if caret < width { 0 } else { caret + 1 - width };
    let shown = text.chars().skip(start).take(width).collect();
    (shown, caret.saturating_sub(start).min(width - 1))
}

fn pad_to_width(text: &str, width: usize) -> String {
    let mut out = crop(text, 0, width);
    let len = out.chars().count();
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

/// Clip a path from the *left*. `…/microstrategy/Tech/log-scouter` says which folder you
/// are in; the first 60 characters of `/var/lib/jenkins/...` say only where it lives.
fn truncate_head(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if width == 0 || length <= width {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(length + 1 - width));
    out
}

/// A signed, human-scaled time offset. Padded to the timestamp column's width by the
/// caller, so turning elapsed mode on never shifts the columns behind it.
fn format_elapsed(delta: ChronoDuration) -> String {
    let negative = delta < ChronoDuration::zero();
    let sign = if negative { '-' } else { '+' };
    let delta = if negative { -delta } else { delta };

    let milliseconds = delta.num_milliseconds();
    let (seconds, millis) = (milliseconds / 1000, milliseconds % 1000);
    if seconds == 0 {
        return format!("{sign}{milliseconds}ms");
    }
    if seconds < 60 {
        return format!("{sign}{seconds}.{millis:03}s");
    }
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if minutes < 60 {
        return format!("{sign}{minutes}m{seconds:02}.{millis:03}s");
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if hours < 24 {
        return format!("{sign}{hours}h{minutes:02}m{seconds:02}s");
    }
    format!("{sign}{}d{:02}h{minutes:02}m", hours / 24, hours % 24)
}

/// The timestamp column's contents: an absolute time, or an offset from the mark.
fn timestamp_column(
    file: &LogFileModel,
    entry: &LogEntry,
    elapsed_from: Option<NaiveDateTime>,
) -> String {
    let Some(origin) = elapsed_from else {
        let field = file.get_field(entry, "timestamp");
        if !field.trim().is_empty() {
            return field;
        }
        // A schema with no timestamp field still shows the time sniffed off the line.
        return file
            .timestamp(entry)
            .map(|stamp| stamp.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_default();
    };
    match file.timestamp(entry) {
        Some(stamp) => format_elapsed(stamp - origin),
        // A banner or continuation line has no time of its own to measure.
        None => "-".to_string(),
    }
}

/// One pane row: three marker columns, then the fixed-width fields, then the message.
fn row_line(
    file: &LogFileModel,
    entry: &LogEntry,
    at_cursor: bool,
    picked: bool,
    matched: bool,
    bookmarked: bool,
    elapsed_from: Option<NaiveDateTime>,
) -> String {
    let cursor = if at_cursor { ">" } else { " " };
    let pick_mark = if picked { "+" } else { " " };
    let match_mark = if matched {
        "*"
    } else if bookmarked {
        "m"
    } else {
        " "
    };
    let timestamp = pad(&timestamp_column(file, entry, elapsed_from), 23);
    let module = pad(&file.get_field(entry, "module"), 14);
    let level = pad(&file.get_field(entry, "level"), 8);
    let mut message = file.message(entry).lines().next().unwrap_or("").to_string();
    if entry.raw.contains('\n') {
        message.push_str("  <+>");
    }
    format!("{cursor}{pick_mark}{match_mark}{timestamp} {module} {level} {message}")
}

/// Repaint `[lo, hi]` of the *uncropped* row inside the already-cropped `line`.
/// `scroll_x` maps one to the other; a run scrolled off-screen simply yields no spans.
fn highlighted_row(
    line: &str,
    lo: usize,
    hi: usize,
    scroll_x: usize,
    base: Style,
) -> Line<'static> {
    highlighted_ranges(
        line,
        &[(lo, hi)],
        scroll_x,
        base,
        base.bg(Color::White).fg(Color::Black),
    )
}

fn highlighted_ranges(
    line: &str,
    ranges: &[(usize, usize)],
    scroll_x: usize,
    base: Style,
    highlight: Style,
) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let ranges = visible_ranges(ranges, scroll_x, chars.len());
    if ranges.is_empty() {
        return Line::from(Span::styled(line.to_string(), base));
    }

    let take = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (lo, hi) in ranges {
        if cursor < lo {
            spans.push(Span::styled(take(cursor..lo), base));
        }
        spans.push(Span::styled(take(lo..hi), highlight));
        cursor = hi;
    }
    if cursor < chars.len() {
        spans.push(Span::styled(take(cursor..chars.len()), base));
    }
    Line::from(spans)
}

fn visible_ranges(
    ranges: &[(usize, usize)],
    scroll_x: usize,
    visible_len: usize,
) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = ranges
        .iter()
        .filter_map(|(lo, hi)| {
            let visible_lo = lo.saturating_sub(scroll_x).min(visible_len);
            // `hi` is inclusive; convert to an exclusive end inside the cropped line.
            let visible_hi = hi
                .saturating_add(1)
                .saturating_sub(scroll_x)
                .min(visible_len);
            (visible_lo < visible_hi).then_some((visible_lo, visible_hi))
        })
        .collect();
    if out.is_empty() {
        return out;
    }

    out.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(out.len());
    for (lo, hi) in out {
        match merged.last_mut() {
            Some((_, last_hi)) if lo <= *last_hi => *last_hi = (*last_hi).max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

fn query_highlight_ranges(query: Option<&Query>, line: &str) -> Vec<(usize, usize)> {
    let Some(query) = query else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    for predicate in &query.predicates {
        match predicate {
            Predicate::Substring(needle) => push_substring_ranges(line, needle, &mut ranges),
            Predicate::Regex(regex) => push_regex_ranges(line, regex, &mut ranges),
            Predicate::FieldEq { value, .. } => push_substring_ranges(line, value, &mut ranges),
            Predicate::FieldContains { value, .. } => {
                push_substring_ranges(line, value, &mut ranges)
            }
            Predicate::FieldRegex { regex, .. } => push_regex_ranges(line, regex, &mut ranges),
            Predicate::After(_) | Predicate::Before(_) | Predicate::DateRange { .. } => {}
        }
    }
    normalize_ranges(&mut ranges);
    ranges
}

fn push_substring_ranges(line: &str, needle: &str, ranges: &mut Vec<(usize, usize)>) {
    if needle.is_empty() {
        return;
    }
    let haystack = line.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(found) = haystack[offset..].find(&needle) {
        let start = offset + found;
        let end = start + needle.len();
        if start < end {
            let start_char = byte_to_char(line, start);
            let end_char = byte_to_char(line, end);
            if start_char < end_char {
                ranges.push((start_char, end_char - 1));
            }
        }
        offset = end;
    }
}

fn push_regex_ranges(line: &str, regex: &regex::Regex, ranges: &mut Vec<(usize, usize)>) {
    for hit in regex.find_iter(line) {
        if hit.start() == hit.end() {
            continue;
        }
        let start_char = byte_to_char(line, hit.start());
        let end_char = byte_to_char(line, hit.end());
        if start_char < end_char {
            ranges.push((start_char, end_char - 1));
        }
    }
}

fn normalize_ranges(ranges: &mut Vec<(usize, usize)>) {
    if ranges.is_empty() {
        return;
    }
    ranges.sort_unstable();
    let mut write = 0;
    for read in 1..ranges.len() {
        if ranges[read].0 <= ranges[write].1.saturating_add(1) {
            ranges[write].1 = ranges[write].1.max(ranges[read].1);
        } else {
            write += 1;
            ranges[write] = ranges[read];
        }
    }
    ranges.truncate(write + 1);
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < byte)
        .count()
}

fn level_style(level: &str) -> Style {
    match level.trim().to_lowercase().as_str() {
        "trace" => Style::default().fg(Color::DarkGray),
        "debug" => Style::default().fg(Color::Cyan),
        "info" | "information" => Style::default().fg(Color::Green),
        "notice" => Style::default().fg(Color::Blue),
        "warn" | "warning" => Style::default().fg(Color::Yellow),
        "error" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "fatal" | "critical" | "severe" => Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
        _ => Style::default(),
    }
}

fn total_line_count(file: &LogFileModel) -> usize {
    // A merged view's line numbers come from different files, so the last one says
    // nothing about the total.
    if file.is_merged() {
        return file.entries.len();
    }
    file.entries
        .last()
        .map(|entry| entry.line_no + entry.raw.lines().count().saturating_sub(1))
        .unwrap_or(0)
}

fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn centered_rect(width: u16, height: u16, root: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(root.height.saturating_sub(height) / 2),
            Constraint::Length(height.min(root.height)),
            Constraint::Min(0),
        ])
        .split(root);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(root.width.saturating_sub(width) / 2),
            Constraint::Length(width.min(root.width)),
            Constraint::Min(0),
        ])
        .split(popup_layout[1])[1]
}

fn help_text() -> &'static str {
    "Undo / history
  u undo   Ctrl+r redo   (filters, time range, searches, merges, layout, AI-applied ops)
  U shows the action history: recent User and AI actions with timestamps

Command palette
  Ctrl+P or : opens a searchable, context-aware action list; type to filter, Enter runs it

Project/files
  a browse for a file  o browse for a folder      d delete selected item      Ctrl+s save
  Ctrl+P Add live source; Right quick-picks Docker/Kubernetes names; r refreshes sources
  d/Delete delete what the cursor is on: a log source, filter, or saved search
  In the browser: j/k or arrows move, Enter opens './' or a folder (or adds the file),
                  Right enters, Left/Backspace goes up, '.' shows hidden, Esc cancels
                  a's file picker also lists files; Enter adds one, or 'p' types a path
  Enter on a log source edits its short name, description, tag, and schema
  In source/live schema rows: i infer with LLM, e edit, L load library, X save library
  Space selects/deselects whatever the cursor is on; Enter opens its detail view.
  In the sidebar: Space on a log adds/removes it, merging the logs by timestamp
                  Space on a filter enables/disables it; d/Delete removes it
                  Space on a saved search runs it, or clears it if running; d removes it
                  Space/Enter on a bookmark jumps to it; d removes it
                  Enter edits that source, that filter, or that search
                  Enter on the Time row (or its 'none - t') reopens the time picker
  Enter also jumps to the selected search result

Navigate
  j/k or arrows     move cursor                 gg/G top/bottom
  [count]j/k        move by count               [count]G go to row
  Ctrl+d/u          half page                    Ctrl+f/b page
  h/l or arrows     horizontal scroll            0/$ line start/end
  Tab/Shift+Tab     focus sidebar, panes, and search results

Select/copy
  Shift+Up/Down     extend a run of lines        Shift+PgUp/PgDn by page
  Ctrl+Up/Down      move without losing the selection
  Space (in a pane) add/remove the current line (build non-adjacent picks)
  Drag in one row   select a substring of that row
  Drag across rows  select whole rows; Ctrl+click toggles one row
  Mouse drag        also selects lines in the detail panel
  y                 copy the substring, else the selected raw lines, else the row
  m                 bookmark/unbookmark the current line
  M                 add or edit the current line's bookmark note
  ]m / [m           next / previous bookmark in the current view
  Right-click       copy the substring, the clicked/selected rows, or detail text
  P                 pretty-print the current message as JSON, XML, or SQL
  Esc               clear the selection, then the search

Filter/search
  / inline search   Enter opens matches panel    n/N next/previous match
                    c context 0/3/10
  f guided filter builder   t time range picker
  In the builder: ↑↓ pick a row, ←→ change a dropdown or step suggestions, type to edit
                  the field/value, Tab switches to the raw grammar, Enter applies
  Filters split into Text (as many as you like) and Time (at most one, replaced
  by each new range rather than intersected with the old)
  T elapsed time from this line (again to restore absolute timestamps)
  H hide like current row
  b timeline histogram: cycle off / by level / by module / by source; drag across its
    bars to build a time-range filter (a click zooms to one bucket)
  L loads the selected source schema, filter pack, bookmark pack, or saved-search library
  X saves the selected source schema, filter pack, bookmark pack, or saved-search library
  Schema pack import/export is still available from the command palette (Ctrl+P or :)
  E export selected lines, bookmarks, filters, and latest AI summary as Markdown
  Time range picker presets count back from the newest entry, not from now
  Moving onto a preset fills Start/End from it; Enter applies what they show
  Raw filter syntax (Tab from the builder): [schema=\"name\"] field op [include|exclude] value
  In the hide menu: every field shows this line's value; a field's own key hides by it
                    at once, Space picks several, Enter ANDs the picks into one regex;
                    Tab switches the whole menu between hide and keep-only
  H with several lines selected derives a regex shared by them all
  H again in the hide menu derives one from the single current line
  In the pattern popup: Up/Down pick a template, greediest first, each with the
                        rows it matches; Tab flips hide/keep; the header counts both
  Filters apply to the whole project and are saved automatically
  L/X default to the matching ~/.log-scouter library folder (any path works)
  While typing /, the pane jumps to the first live match; Enter submits the search.
  In the matches panel, click a match or focus it and press Enter

Layout
  | split columns   - split rows                 w close pane
  [ / ]             narrow / widen the sidebar (or drag its separator with the mouse)
  Ctrl+←/→ or ↑/↓   resize the focused pane along the split (or drag a border between panes)
  Drag a panel's top border to resize its height: results, detail, or chat
  z                 focus mode: show only the active pane (again to restore)
  Toggle the sidebar, detail, results, and chat panels from the palette (Ctrl+P or :)
  Choose theme from the palette; saved in ~/.log-scouter/ui.json
  Enter on a log row opens a larger detail popup with parsed fields and raw text
  Detail panel (left, bottom) shows the selected line or project item details
  A star marks files open in a pane, enabled filters, and the running search
  Quitting records the panes, sizes, panel visibility, their logs and searches; reopening
  restores them

AI assistant
  A open the AI chat panel (bottom left); type a question and Enter to ask
  It can inspect the logs and apply filters, searches, and time ranges for you;
  the panels update as it works, and each action is noted in the transcript
  Esc cancels a reply in flight, then leaves the panel; Up/Down scroll the transcript
  The panel title shows the provider and model, and 'no key' until one is configured
  A key comes from OPENAI_API_KEY / ANTHROPIC_API_KEY / DEEPSEEK_API_KEY, from the
  api_key field in ~/.log-scouter/ai.json, or from /key <api-key> (this session only)
  /provider openai|anthropic|deepseek and /model <name> pick the model (saved to ai.json)
  /skills lists skills in ~/.log-scouter/skills/*.md; /skill <name> toggles one on/off
  /clear resets the conversation

Log schema
  <field> is required, <field?> is optional and may be missing from a line
  An optional field takes the separator in front of it: [<level>][<code?>][UID:<id>]
  matches both [Error][0x800424FB][UID:1] and [Error][UID:1]
  Adding a file detects its schema from the first lines, most specific first
  A schema may carry sample lines with their expected level; a schema that matches
  a sample but parses it wrongly is rejected when defined or imported

Input popups
  Left/Right/Home/End move the caret   Delete/Backspace edit   Ctrl+u clears
  Long values wrap instead of being clipped

Long operations
  Loading, filtering and searching show a progress bar; Esc cancels

Press y to copy help text, or any other key to close."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extractor::{DEFAULT_EXTRACTOR_NAME, GENERIC_EXTRACTOR_NAME};
    use ratatui::backend::TestBackend;

    #[test]
    fn inferred_schema_builds_a_working_extractor() {
        let json = r#"{"name":"svc","format":"[<timestamp>][<level>] <message>",
            "timestamp_format":"%Y-%m-%d %H:%M:%S","description":"a service log"}"#;
        let extractor = parse_inferred_schema(json).expect("usable schema");
        assert_eq!(extractor.name, "svc");
        assert_eq!(extractor.timestamp_format, "%Y-%m-%d %H:%M:%S");
        assert_eq!(extractor.description, "a service log");
        let fields = extractor.extract("[2026-07-13 10:00:01][INFO] hi").unwrap();
        assert_eq!(fields.get("level").map(String::as_str), Some("INFO"));
    }

    fn postgres_live_extractor() -> Extractor {
        let mut extractor = Extractor::with_timestamp_format(
            "postgresql_log",
            "[logscout stderr] <timestamp> UTC [<pid>] <level>:  <message>",
            "%Y-%m-%d %H:%M:%S%.3f",
        )
        .unwrap();
        extractor.field_patterns.insert("pid".into(), r"\d+".into());
        extractor.field_patterns.insert("level".into(), "LOG".into());
        extractor.entry_start =
            r"^\[logscout stderr\] \d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} UTC \[\d+\] \w+:"
                .into();
        extractor.compile().unwrap();
        extractor
    }

    #[test]
    fn live_prefix_tags_only_entry_starts() {
        let extractor = postgres_live_extractor();
        let prefix = "[logscout stderr] ";

        // A header line keeps the tag, so `entry_start` still recognises it.
        let header = apply_live_prefix(
            "2025-10-18 08:43:44.296 UTC [35] LOG:  statement: ".into(),
            prefix,
            Some(&extractor),
        );
        assert_eq!(
            header,
            "[logscout stderr] 2025-10-18 08:43:44.296 UTC [35] LOG:  statement: "
        );
        assert!(extractor.is_start(&header));

        // A continuation line (the wrapped SQL) is left clean and is not a new entry.
        let continuation =
            apply_live_prefix("            SELECT".into(), prefix, Some(&extractor));
        assert_eq!(continuation, "            SELECT");
        assert!(!extractor.is_start(&continuation));
    }

    #[test]
    fn live_prefix_leaves_stdout_untouched_and_falls_back_without_a_schema() {
        // stdout carries no prefix: returned verbatim.
        assert_eq!(
            apply_live_prefix("plain stdout line".into(), "", Some(&postgres_live_extractor())),
            "plain stdout line"
        );
        // No schema: every line counts as a start, so the tag is always applied (the
        // original behaviour).
        assert_eq!(
            apply_live_prefix("anything".into(), "[logscout stderr] ", None),
            "[logscout stderr] anything"
        );
    }

    #[test]
    fn repair_clears_an_entry_start_that_matches_nothing() {
        // The exact SearchService slip: `\S+` stops at the space inside the timestamp, so
        // `^\S+ \[HOST:` matches no header line and the whole file folds into one entry.
        let format = "<timestamp> [HOST:<host>][POD:<pod>][PID:<pid>][THR:<thread>][<service>][<level>][Tenant:<tenant?>] <message>";
        let mut extractor =
            Extractor::with_timestamp_format("SearchService", format, "%Y-%m-%d %H:%M:%S%.3f")
                .unwrap();
        extractor.entry_start = r"^\S+ \[HOST:".to_string();
        extractor.compile().unwrap();

        let lines = vec![
            "2026-06-12 10:17:44.944 [HOST:WIN-91TDYP3][POD:unknown][PID:44612][THR:main][SearchService][INFO][Tenant:] Starting".to_string(),
            "2026-06-12 10:17:44.996 [HOST:WIN-91TDYP3][POD:unknown][PID:44612][THR:main][SearchService][INFO][Tenant:] Active profile".to_string(),
        ];

        // Before: the bad entry_start matches nothing.
        assert!(!lines.iter().any(|l| extractor.is_start(l)));

        let note = repair_entry_start(&mut extractor, &lines);
        assert!(note.unwrap().contains("adjusted"));
        // After: entry_start cleared, and the derived probe now groups every header line.
        assert_eq!(extractor.entry_start, "");
        assert!(lines.iter().all(|l| extractor.is_start(l)));
    }

    #[test]
    fn repair_leaves_a_working_entry_start_alone() {
        let format = "<timestamp> [<level>] <message>";
        let mut extractor =
            Extractor::with_timestamp_format("ok", format, "%Y-%m-%d %H:%M:%S").unwrap();
        extractor.entry_start = r"^\d{4}-\d{2}-\d{2} ".to_string();
        extractor.compile().unwrap();
        let lines = vec!["2026-06-12 10:17:44 [INFO] hello".to_string()];
        assert_eq!(repair_entry_start(&mut extractor, &lines), None);
        assert_eq!(extractor.entry_start, r"^\d{4}-\d{2}-\d{2} ");
    }

    #[test]
    fn inferred_schema_handles_pipe_formats_and_fences() {
        // A pipe-delimited format — which the `name | format | …` text grammar could not
        // round-trip — built straight from JSON wrapped in a ```json fence.
        let reply = "```json\n{\"format\": \"<timestamp> | <level> | <message>\"}\n```";
        let extractor = parse_inferred_schema(reply).expect("usable schema");
        let fields = extractor
            .extract("2026-07-13T10:00:01Z | INFO | hi")
            .unwrap();
        assert_eq!(fields.get("level").map(String::as_str), Some("INFO"));
        assert_eq!(fields.get("message").map(String::as_str), Some("hi"));
    }

    #[test]
    fn inferred_schema_accepts_multiline_entry_boundaries() {
        let reply = r#"{
            "name": "py-block",
            "format": "{\n  'timestamp':'<timestamp>',\n  'level': '<level>',\n  'message': '<message>'\n}",
            "timestamp_format": "%Y-%m-%d %H:%M:%S,%f",
            "entry_start": "^\\s*\\{\\s*$",
            "entry_end": "^\\s*\\}\\s*$",
            "field_patterns": {"level": "[A-Z]+"},
            "description": "python dict blocks"
        }"#;
        let extractor = parse_inferred_schema(reply).expect("usable schema");
        assert!(extractor.is_start("{"));
        assert!(extractor.is_end("}"));
        assert_eq!(
            extractor.field_patterns.get("level").map(String::as_str),
            Some("[A-Z]+")
        );
        let fields = extractor
            .extract("{\n  'timestamp':'2026-07-14 07:14:40,530',\n  'level': 'INFO',\n  'message': 'hello'\n}")
            .unwrap();
        assert_eq!(fields.get("message").map(String::as_str), Some("hello"));
    }

    #[test]
    fn inferred_schema_rejects_junk() {
        assert!(parse_inferred_schema("no json here").is_err());
        assert!(parse_inferred_schema(r#"{"name":"x"}"#).is_err()); // no format
    }

    const LONG_MESSAGE: &str =
        "UserSession::TimeOut() failed to resolve inbox message for session 4A2F99BC";

    fn app_with_log(root: &std::path::Path) -> AppState {
        let log = root.join("a.log");
        std::fs::write(
            &log,
            format!(
                "2026-06-16 10:09:43.288 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Error][UID:0][SID:0][OID:0][Disp.cpp:394] {LONG_MESSAGE}\n\
                 2026-06-16 10:09:44.100 [HOST:h][SERVER:S][PID:5][THR:9][SQL][Trace][UID:0][SID:0][OID:0][Q.cpp:12] cache miss\n"
            ),
        )
        .unwrap();

        boot(Project::load(root), &log)
    }

    /// Quit and reopen the folder, the way `run()` does on exit and entry.
    fn reopen(mut app: AppState) -> AppState {
        let root = app.project.root.clone();
        app.capture_session();
        app.project.save().unwrap();

        let mut reopened = AppState::new(Project::load(&root));
        reopened.queue_initial_loads();
        reopened.finish_work();
        reopened
    }

    /// Build the app the way `run()` does: queue loads, then let them finish.
    fn boot(mut project: Project, log: &std::path::Path) -> AppState {
        project.add_file(log, None);
        let mut app = AppState::new(project);
        app.ui_config = UiConfig::default();
        app.queue_initial_loads();
        app.finish_work();
        app
    }

    fn render(app: &mut AppState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// Mirrors the real loop: handle the key, then drain the work it queued.
    fn press(app: &mut AppState, code: KeyCode) {
        press_mod(app, code, KeyModifiers::NONE);
    }

    fn press_mod(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
        app.handle_key(KeyEvent::new(code, modifiers)).unwrap();
        app.finish_work();
    }

    fn mouse(
        app: &mut AppState,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) {
        app.handle_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        });
        app.finish_work();
    }

    /// `n` single-line entries whose messages share a prefix but differ in one token.
    fn app_with_lines(root: &std::path::Path, n: usize) -> AppState {
        let log = root.join("many.log");
        let body: String = (0..n)
            .map(|i| {
                format!(
                    "2026-06-16 10:09:{:02}.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:{i}] Distribution Service Trigger: {i} queued\n",
                    i % 60
                )
            })
            .collect();
        std::fs::write(&log, body).unwrap();
        boot(Project::load(root), &log)
    }

    fn app_with_message(root: &std::path::Path, message: &str) -> AppState {
        let log = root.join("structured.log");
        std::fs::write(
            &log,
            format!(
                "2026-06-16 10:09:00.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Info][UID:0][SID:0][OID:0][D.cpp:0] {message}\n"
            ),
        )
        .unwrap();
        boot(Project::load(root), &log)
    }

    // ---- AI assistant ----------------------------------------------------------------

    use crate::ai::message::{Assistant, ToolCall};
    use crate::ai::AgentEvent;

    /// An assistant reply carrying one tool call.
    fn tool_call_event(generation: u64, name: &str, args: serde_json::Value) -> AgentEvent {
        AgentEvent {
            generation,
            result: Ok(Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: name.into(),
                    arguments: args,
                }],
            }),
        }
    }

    fn text_event(generation: u64, text: &str) -> AgentEvent {
        AgentEvent {
            generation,
            result: Ok(Assistant {
                text: text.into(),
                tool_calls: Vec::new(),
            }),
        }
    }

    #[test]
    fn ai_read_tools_report_the_focused_log() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        // count_matches uses the same query language as the search box.
        let count = app
            .dispatch_ai_tool("count_matches", &serde_json::json!({"query": "queued"}))
            .unwrap();
        assert!(count.starts_with("5 of 5"), "{count}");
        let none = app
            .dispatch_ai_tool("count_matches", &serde_json::json!({"query": "nope"}))
            .unwrap();
        assert!(none.starts_with("0 of 5"), "{none}");

        // level_breakdown sums to the entry count.
        let levels = app
            .dispatch_ai_tool("level_breakdown", &serde_json::json!({}))
            .unwrap();
        assert!(levels.contains("Trace: 5"), "{levels}");

        // sample_lines returns real log text.
        let sample = app
            .dispatch_ai_tool("sample_lines", &serde_json::json!({"count": 2}))
            .unwrap();
        assert_eq!(sample.lines().count(), 2);
        assert!(sample.contains("Distribution Service Trigger"), "{sample}");
    }

    #[test]
    fn ai_add_filter_tool_narrows_the_view_and_reports_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        assert_eq!(app.active_view().unwrap().visible.len(), 10);

        let result = app
            .dispatch_ai_tool(
                "add_filter",
                &serde_json::json!({
                    "field": "level", "op": "equals", "value": "Trace", "action": "exclude"
                }),
            )
            .unwrap();
        assert!(result.contains("10 -> 0 rows"), "{result}");
        assert_eq!(app.project.filters.rules.len(), 1);
        app.finish_work();
        assert_eq!(app.active_view().unwrap().visible.len(), 0);

        // A bad regex is rejected rather than added.
        let err = app
            .dispatch_ai_tool(
                "add_filter",
                &serde_json::json!({"field": "message", "op": "regex", "value": "(", "action": "exclude"}),
            )
            .unwrap_err();
        assert!(err.contains("invalid regex"), "{err}");
        assert_eq!(app.project.filters.rules.len(), 1);
    }

    /// The whole agentic cycle, driven with scripted replies instead of a live model:
    /// a tool call runs and asks for a follow-up; a text reply ends the turn.
    #[test]
    fn ai_event_loop_runs_tools_then_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.open_ai_chat();
        // Stand in for a turn already sent: a matching generation, mid-flight.
        {
            let ai = app.ai.as_mut().unwrap();
            ai.generation = 7;
            ai.turns = 1;
            ai.pending = true;
        }

        // A stale reply (wrong generation) is ignored entirely.
        assert!(!app.apply_ai_event(tool_call_event(1, "level_breakdown", serde_json::json!({}))));
        assert!(app.ai.as_ref().unwrap().conversation.is_empty());

        // The model asks to hide Trace: the tool runs and a follow-up is requested.
        let needs_more = app.apply_ai_event(tool_call_event(
            7,
            "add_filter",
            serde_json::json!({"field": "level", "op": "equals", "value": "Trace", "action": "exclude"}),
        ));
        assert!(needs_more, "a tool call should ask for another turn");
        assert_eq!(app.project.filters.rules.len(), 1);
        app.finish_work();
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
        // The conversation now has the assistant turn and the tool result turn.
        let ai = app.ai.as_ref().unwrap();
        assert_eq!(ai.conversation.len(), 2);
        assert!(matches!(ai.conversation[1].role, crate::ai::Role::Tool));
        assert!(ai
            .transcript
            .iter()
            .any(|line| matches!(line, ChatLine::Action(_))));

        // A plain text reply ends the turn.
        let needs_more = app.apply_ai_event(text_event(7, "Hidden the Trace noise."));
        assert!(!needs_more);
        let ai = app.ai.as_ref().unwrap();
        assert!(!ai.pending);
        assert!(ai
            .transcript
            .iter()
            .any(|line| matches!(line, ChatLine::Assistant(text) if text.contains("Trace"))));
    }

    #[test]
    fn ai_error_reply_stops_the_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 3);
        app.open_ai_chat();
        {
            let ai = app.ai.as_mut().unwrap();
            ai.generation = 2;
            ai.pending = true;
        }
        let needs_more = app.apply_ai_event(AgentEvent {
            generation: 2,
            result: Err("provider error 401: bad key".into()),
        });
        assert!(!needs_more);
        let ai = app.ai.as_ref().unwrap();
        assert!(!ai.pending);
        assert!(ai
            .transcript
            .iter()
            .any(|line| matches!(line, ChatLine::Error(text) if text.contains("401"))));
    }

    fn selected_line_numbers(app: &AppState) -> Vec<usize> {
        let view = app.active_view().unwrap();
        let file = app.project.get_file(&view.file_id).unwrap();
        view.selected_globals()
            .iter()
            .map(|global| file.entries[*global].line_no)
            .collect()
    }

    fn selected_visible_line_numbers(app: &AppState) -> Vec<usize> {
        let view = app.active_view().unwrap();
        let file = app.project.get_file(&view.file_id).unwrap();
        view.visible
            .iter()
            .filter_map(|global| file.entries.get(global).map(|entry| entry.line_no))
            .collect()
    }

    #[test]
    fn shift_arrows_extend_a_contiguous_run_and_shrink_back() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        press(&mut app, KeyCode::Char('j')); // cursor on row 2

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(selected_line_numbers(&app), vec![2, 3, 4]);
        assert_eq!(app.status, "3 lines selected");

        // Reversing direction shrinks the run rather than growing it downward.
        press_mod(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(selected_line_numbers(&app), vec![2, 3]);

        // Crossing back past the anchor selects upward.
        press_mod(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(selected_line_numbers(&app), vec![1, 2]);

        // A plain motion drops the selection.
        press(&mut app, KeyCode::Char('j'));
        assert!(selected_line_numbers(&app).is_empty());
    }

    #[test]
    fn shift_selection_spans_pages_and_scrolls_the_viewport() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 200);
        // Render first so the pane height is known and scroll clamping applies.
        render(&mut app, 100, 30);

        press_mod(&mut app, KeyCode::PageDown, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::PageDown, KeyModifiers::SHIFT);
        let selected = selected_line_numbers(&app);
        assert_eq!(selected.len(), 41); // two 20-line pages, inclusive of the anchor
        assert_eq!(selected.first().copied(), Some(1));
        assert_eq!(selected.last().copied(), Some(41));

        // The cursor left the first screenful, so the view must have scrolled with it.
        render(&mut app, 100, 30);
        let view = app.active_view().unwrap();
        assert_eq!(view.cursor, 40);
        assert!(view.scroll_y > 0, "viewport did not follow the selection");
        assert!(view.cursor >= view.scroll_y);
    }

    #[test]
    fn ctrl_arrows_keep_the_selection_so_space_can_pick_non_adjacent_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        // Pick lines 1-2 with Shift.
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(selected_line_numbers(&app), vec![1, 2]);

        // Travel three lines away without disturbing them.
        for _ in 0..3 {
            press_mod(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        }
        assert_eq!(selected_line_numbers(&app), vec![1, 2]);
        assert_eq!(app.active_view().unwrap().cursor, 4);

        // Space folds line 5 in, leaving a gap at 3-4.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(selected_line_numbers(&app), vec![1, 2, 5]);
        assert_eq!(app.status, "3 lines selected");

        // Space again toggles it back off.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(selected_line_numbers(&app), vec![1, 2]);

        // Esc clears the selection but leaves the cursor put.
        press(&mut app, KeyCode::Esc);
        assert!(selected_line_numbers(&app).is_empty());
        assert_eq!(app.active_view().unwrap().cursor, 4);
    }

    #[test]
    fn selection_count_is_exact_when_marks_sit_outside_the_shift_range() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 20);

        // Mark line 1, travel away, then Shift-select 5..7.
        press(&mut app, KeyCode::Char(' '));
        for _ in 0..4 {
            press_mod(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        }
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);

        assert_eq!(selected_line_numbers(&app), vec![1, 5, 6, 7]);
        // The cheap count must agree with the expensive enumeration.
        assert_eq!(app.active_view().unwrap().selection_count(), 4);

        // A mark *inside* the live range must not be counted twice.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        press_mod(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char(' ')); // mark line 2
        press_mod(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(selected_line_numbers(&app), vec![1, 2, 3]);
        assert_eq!(app.active_view().unwrap().selection_count(), 3);
    }

    #[test]
    fn selected_rows_are_marked_and_highlighted() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);

        // Wide enough that the message column is not clipped; the markers are the point.
        let screen = render(&mut app, 102, 30);
        // Pane rows only -- the Detail panel echoes the message too.
        let rows: Vec<&str> = screen
            .lines()
            .filter(|line| line.contains("Trace    Distribution"))
            .collect();
        assert_eq!(rows.len(), 5);
        // Row 1 is picked but not the cursor; row 2 is picked and the cursor.
        assert!(rows[0].contains(" + "), "row 1 not marked: {:?}", rows[0]);
        assert!(rows[1].contains(">+ "), "row 2 not marked: {:?}", rows[1]);
        assert!(
            !rows[2].contains('+'),
            "row 3 wrongly marked: {:?}",
            rows[2]
        );
    }

    #[test]
    fn copy_falls_back_to_the_cursor_line_and_targets_the_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        // No selection: the cursor line alone.
        assert_eq!(app.target_globals(), vec![0]);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(app.target_globals(), vec![0, 1, 2]);
    }

    #[test]
    fn right_click_copies_and_reports_the_byte_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 50,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        app.finish_work();
        assert!(
            app.status.starts_with("copied 2 line(s),"),
            "{}",
            app.status
        );
    }

    /// A press alone cannot tell a row drag from a substring drag, so it only moves the
    /// cursor. Crossing a row boundary is what commits the gesture to rows.
    #[test]
    fn mouse_drag_across_rows_selects_log_rows_in_the_view_panel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 100, 30);
        let pane = app.pane_areas[0];

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + 4,
            pane.y + 2,
            KeyModifiers::NONE,
        );
        assert_eq!(selected_line_numbers(&app), Vec::<usize>::new());
        assert_eq!(
            app.active_view().unwrap().cursor,
            2,
            "the cursor moved there"
        );

        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 4,
            pane.y + 4,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            pane.x + 4,
            pane.y + 4,
            KeyModifiers::NONE,
        );
        assert_eq!(selected_line_numbers(&app), vec![3, 4, 5]);
        assert_eq!(app.status, "3 lines selected");
        assert!(app.text_selection.is_none(), "row drag leaves no substring");
    }

    #[test]
    fn right_click_in_the_view_panel_copies_selected_or_clicked_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);
        render(&mut app, 100, 30);
        let pane = app.pane_areas[0];

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + 4,
            pane.y + 1,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 4,
            pane.y + 2,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            pane.x + 4,
            pane.y + 1,
            KeyModifiers::NONE,
        );
        assert!(
            app.status.starts_with("copied 2 line(s),"),
            "{}",
            app.status
        );

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            pane.x + 4,
            pane.y + 5,
            KeyModifiers::NONE,
        );
        assert_eq!(selected_line_numbers(&app), Vec::<usize>::new());
        assert_eq!(app.active_view().unwrap().cursor, 5);
        assert!(
            app.status.starts_with("copied 1 line(s),"),
            "{}",
            app.status
        );
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // RFC 4648 test vectors: padding is what most terminals reject if wrong.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn hide_with_a_multi_line_selection_prefills_a_derived_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        let total = app.active_view().unwrap().visible.len();
        assert_eq!(total, 10);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));

        // The popup is prefilled, not applied: the counter token generalises to its shape,
        // not to a blanket wildcard.
        let Mode::HidePattern(prompt) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        let pattern = prompt.text;
        assert_eq!(pattern, r"Distribution\s+Service\s+Trigger:\s+\d+\s+queued");
        assert_eq!(
            app.project.filters.rules.len(),
            0,
            "applied without consent"
        );

        // Enter commits it; it generalises to all 10 lines, not just the 3 selected.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, "hide pattern added");
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
        assert_eq!(app.project.filters.rules[0].op, "regex");
        assert!(!app.active_view().unwrap().has_selection());

        // And it autosaved, like any other filter.
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        assert!(saved.contains("regex"), "{saved}");
    }

    #[test]
    fn hide_with_one_selected_line_still_opens_the_choice_menu() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        press(&mut app, KeyCode::Char(' ')); // select exactly one
        press(&mut app, KeyCode::Char('H'));
        assert!(matches!(app.mode, Mode::HideChoice(_)));
    }

    #[test]
    fn hide_choice_uses_schema_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('H'));
        assert_eq!(
            app.hide_choice_fields(),
            vec![
                "timestamp",
                "host",
                "server",
                "process_id",
                "thread_id",
                "log_module",
                "log_level",
                "error_code",
                "user_id",
                "session_id",
                "object_id",
                "file_name",
                "line_number",
                "message",
            ]
        );

        press(&mut app, KeyCode::Char('7')); // log_level
        assert_eq!(app.status, "hide rule added");
        let rule = &app.project.filters.rules[0];
        assert_eq!(rule.field, "log_level");
        assert_eq!(rule.value, "Trace");
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
    }

    /// The bracketed schema has fourteen fields but only ten digit keys, so the tail of the
    /// list -- `file_name`, `line_number`, `message` -- is reachable only via letters.
    #[test]
    fn hide_choice_reaches_fields_past_the_tenth() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('H'));
        let fields = app.hide_choice_fields();
        assert_eq!(fields[13], "message");
        assert_eq!(hide_choice_index('d'), Some(13));

        press(&mut app, KeyCode::Char('d')); // index 13 -> message
        assert_eq!(app.status, "hide rule added");
        assert_eq!(app.project.filters.rules[0].field, "message");
    }

    fn hide_menu(app: &AppState) -> HideMenu {
        let Mode::HideChoice(menu) = app.mode.clone() else {
            panic!("expected the hide menu, got {:?}", app.mode);
        };
        menu
    }

    #[test]
    fn field_value_preview_shows_the_first_three_words() {
        assert_eq!(field_value_preview(""), "(empty)");
        assert_eq!(field_value_preview("   "), "(empty)");
        assert_eq!(field_value_preview("Trace"), "Trace");
        assert_eq!(field_value_preview("a b c"), "a b c");
        assert_eq!(field_value_preview("a b c d"), "a b c …");
    }

    /// The menu names each field *and* what the targeted line holds in it.
    #[test]
    fn the_hide_menu_shows_the_value_of_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('H'));
        let menu = hide_menu(&app);
        assert_eq!(menu.fields[6], ("log_level".into(), "Trace".into()));
        assert_eq!(menu.fields[7], ("error_code".into(), String::new()));
        assert_eq!(
            menu.fields[13],
            (
                "message".into(),
                "Distribution Service Trigger: 0 queued".into()
            )
        );

        let screen = render(&mut app, 110, 34);
        assert!(screen.contains("7  log_level      Trace"), "{screen}");
        assert!(screen.contains("8  error_code     (empty)"), "{screen}");
        // A long value is cut to its first three words.
        assert!(
            screen.contains("d  message        Distribution Service Trigger: …"),
            "{screen}"
        );
    }

    /// Space picks fields; Enter turns two or more of them into one positional regex.
    #[test]
    fn picking_two_fields_ands_them_into_one_regex_over_the_raw_line() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("mixed.log");
        std::fs::write(
            &log,
            "2026-06-16 10:09:01.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:1] kernel trace\n\
             2026-06-16 10:09:02.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Error][UID:0][SID:0][OID:0][D.cpp:2] kernel error\n\
             2026-06-16 10:09:03.000 [HOST:h][SERVER:S][PID:5][THR:9][Net][Trace][UID:0][SID:0][OID:0][D.cpp:3] net trace\n",
        )
        .unwrap();
        let mut app = boot(Project::load(tmp.path()), &log);
        assert_eq!(app.active_view().unwrap().visible.len(), 3);

        press(&mut app, KeyCode::Char('H'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Down); // log_module
        }
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down); // log_level
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(hide_menu(&app).chosen().len(), 2);

        let screen = render(&mut app, 110, 34);
        assert!(screen.contains("all 2 picked fields"), "{screen}");

        press(&mut app, KeyCode::Enter);
        let prompt = hide_prompt(&app);
        assert_eq!(prompt.field, "raw");
        assert!(
            prompt.candidates.is_empty(),
            "a combined regex has no ladder"
        );
        assert_eq!(
            app.status,
            "hide pattern from 2 fields: log_module and log_level"
        );
        // Only the line that is Kernel *and* Trace: neither field alone would do.
        assert_eq!(app.hide_pattern_preview(&prompt.text, "raw").matched, 1);

        press(&mut app, KeyCode::Enter);
        let rule = &app.project.filters.rules[0];
        assert_eq!((rule.field.as_str(), rule.op.as_str()), ("raw", "regex"));
        assert_eq!(app.active_view().unwrap().visible.len(), 2);
    }

    /// One pick stays an `equals` rule: it reads better, and it holds on a line the
    /// schema cannot fully parse.
    #[test]
    fn picking_one_field_stays_an_equals_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('H'));
        for _ in 0..6 {
            press(&mut app, KeyCode::Down); // log_level
        }
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.status, "hide rule added");
        let rule = &app.project.filters.rules[0];
        assert_eq!(rule.field, "log_level");
        assert_eq!(rule.op, "equals");
        assert_eq!(rule.value, "Trace");
    }

    /// Enter with nothing picked acts on the row under the cursor.
    #[test]
    fn enter_with_no_picks_hides_by_the_cursor_field() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Down); // host
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.project.filters.rules[0].field, "host");
        assert_eq!(app.project.filters.rules[0].value, "h");
    }

    /// Tab flips the menu to keep-only, so a single field becomes an `include` rule that
    /// shows only the matching lines.
    #[test]
    fn tab_makes_the_hide_menu_keep_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);

        press(&mut app, KeyCode::Char('H'));
        assert!(matches!(&app.mode, Mode::HideChoice(menu) if menu.exclude));
        press(&mut app, KeyCode::Tab);
        assert!(matches!(&app.mode, Mode::HideChoice(menu) if !menu.exclude));

        let screen = render(&mut app, 110, 34);
        assert!(screen.contains("Keep only logs where"), "{screen}");
        assert!(screen.contains("Tab  switch to hide"), "{screen}");

        // A field's own key now keeps by it.
        for _ in 0..6 {
            press(&mut app, KeyCode::Down); // log_level
        }
        press(&mut app, KeyCode::Char('7'));
        assert_eq!(app.status, "keep rule added");
        let rule = &app.project.filters.rules[0];
        assert_eq!(
            (rule.field.as_str(), rule.op.as_str(), rule.action.as_str()),
            ("log_level", "equals", "include")
        );
        // Every line here is Trace, so keeping Trace shows them all -- and none vanish.
        assert_eq!(app.active_view().unwrap().visible.len(), 6);
    }

    /// The keep direction carries through the combined-field regex too.
    #[test]
    fn tab_then_two_fields_builds_an_include_regex() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("mixed.log");
        std::fs::write(
            &log,
            "2026-06-16 10:09:01.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:1] kernel trace\n\
             2026-06-16 10:09:02.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Error][UID:0][SID:0][OID:0][D.cpp:2] kernel error\n\
             2026-06-16 10:09:03.000 [HOST:h][SERVER:S][PID:5][THR:9][Net][Trace][UID:0][SID:0][OID:0][D.cpp:3] net trace\n",
        )
        .unwrap();
        let mut app = boot(Project::load(tmp.path()), &log);

        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Tab); // keep only
        for _ in 0..5 {
            press(&mut app, KeyCode::Down); // log_module
        }
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down); // log_level
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);

        let prompt = hide_prompt(&app);
        assert!(!prompt.exclude, "the keep direction was lost");
        assert!(
            app.status.starts_with("keep pattern from 2 fields"),
            "{}",
            app.status
        );

        press(&mut app, KeyCode::Enter);
        let rule = &app.project.filters.rules[0];
        assert_eq!(
            (rule.field.as_str(), rule.action.as_str()),
            ("raw", "include")
        );
        // Only Kernel *and* Trace survives.
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
    }

    /// The bug behind "select multiple lines, press H does not work": dissimilar
    /// messages produced no pattern and the popup never opened.
    #[test]
    fn hide_on_dissimilar_lines_still_opens_a_popup() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("mixed.log");
        std::fs::write(
            &log,
            "2026-06-16 10:09:01.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:1] alpha beta\n\
             2026-06-16 10:09:02.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:2] gamma delta\n",
        )
        .unwrap();
        let mut app = boot(Project::load(tmp.path()), &log);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));

        let Mode::HidePattern(prompt) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        let pattern = prompt.text;
        // No shared template, so it matches the two lines literally rather than
        // generalising into something that would hide the file.
        assert_eq!(pattern, "(?:alpha beta|gamma delta)");
        assert!(
            app.project.filters.rules.is_empty(),
            "applied without consent"
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
    }

    #[test]
    fn an_invalid_edited_hide_pattern_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.open_input(Mode::HidePattern(PatternPrompt::new("(unclosed", true)));
        press(&mut app, KeyCode::Enter);
        assert!(app.status.starts_with("invalid regex:"), "{}", app.status);
        assert!(app.project.filters.rules.is_empty());
    }

    fn hide_prompt(app: &AppState) -> PatternPrompt {
        let Mode::HidePattern(prompt) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        prompt
    }

    /// `H` offers the whole ladder, ranked by what each rung actually takes out here.
    #[test]
    fn the_hide_popup_ranks_its_templates_by_the_rows_they_match() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));
        let prompt = hide_prompt(&app);

        // Greediest first. Every rung but `exact` generalises the counter, so all ten
        // rows fall to them; ties break towards the shorter regex.
        let ladder: Vec<(&str, usize)> = prompt
            .candidates
            .iter()
            .map(|candidate| (candidate.option.name, candidate.matched))
            .collect();
        assert_eq!(
            ladder,
            [
                ("prefix", 10),
                ("loose", 10),
                ("wildcard", 10),
                ("typed", 10),
                ("exact", 2),
            ]
        );
        assert_eq!((prompt.scanned, prompt.total), (10, 10));
        assert!(!prompt.capped());

        // It opens on `typed` -- the template `H` produced before there was a ladder.
        assert_eq!(prompt.selected, 3);
        assert_eq!(
            prompt.text,
            r"Distribution\s+Service\s+Trigger:\s+\d+\s+queued"
        );
        assert!(!prompt.edited());

        let screen = render(&mut app, 100, 34);
        assert!(
            screen.contains("prefix    leading words, then .*"),
            "{screen}"
        );
        assert!(screen.contains("matches 10"), "{screen}");
        assert!(screen.contains("matches 2"), "{screen}");
        assert!(screen.contains("Up/Down pick a template"), "{screen}");
    }

    /// Up and Down walk the ladder, loading each rung into the editable field.
    #[test]
    fn up_and_down_step_the_template_ladder() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));

        // Down from `typed` reaches the strictest rung: only the two selected lines.
        press(&mut app, KeyCode::Down);
        let prompt = hide_prompt(&app);
        assert_eq!(prompt.selected, 4);
        assert!(prompt.text.starts_with("(?:"), "{}", prompt.text);
        assert_eq!(app.input_cursor, prompt.text.chars().count());
        assert_eq!(app.hide_pattern_preview(&prompt.text, "message").matched, 2);

        // Up three rungs reaches the greediest, and the preview follows.
        for _ in 0..3 {
            press(&mut app, KeyCode::Up);
        }
        let prompt = hide_prompt(&app);
        assert_eq!(prompt.selected, 1);
        assert_eq!(prompt.text, r"Distribution.*Service.*Trigger:.*queued");
        press(&mut app, KeyCode::Up);
        let prompt = hide_prompt(&app);
        assert_eq!(prompt.selected, 0);
        assert_eq!(prompt.text, r"Distribution\s+Service\s+Trigger:.*");

        let screen = render(&mut app, 100, 34);
        assert!(screen.contains("hides 10 of 10 shown rows"), "{screen}");

        // Up at the top stays put, and Enter commits whatever rung is showing.
        press(&mut app, KeyCode::Up);
        assert_eq!(hide_prompt(&app).selected, 0);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.project.filters.rules[0].value,
            r"Distribution\s+Service\s+Trigger:.*"
        );
    }

    /// An edit detaches the field from its rung, and the popup says so.
    #[test]
    fn editing_the_pattern_marks_the_rung_as_edited() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));
        assert!(!hide_prompt(&app).edited());

        press(&mut app, KeyCode::Backspace);
        assert!(hide_prompt(&app).edited());
        let screen = render(&mut app, 100, 34);
        assert!(screen.contains("(edited)"), "{screen}");

        // Stepping the ladder discards the edit rather than half-keeping it.
        press(&mut app, KeyCode::Up);
        assert!(!hide_prompt(&app).edited());
    }

    /// The single-line ladder has no `wildcard` rung, but it is still a ladder.
    #[test]
    fn a_single_line_pattern_also_offers_a_ladder() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Char('H'));
        let prompt = hide_prompt(&app);

        // With one line there is nothing to diff, so `typed` is the only rung that
        // generalises at all -- and ranking by rows matched puts it on top, where the
        // strategy names alone would have buried it under `loose` and `prefix`.
        let ladder: Vec<(&str, usize)> = prompt
            .candidates
            .iter()
            .map(|candidate| (candidate.option.name, candidate.matched))
            .collect();
        assert_eq!(
            ladder,
            [("typed", 10), ("loose", 1), ("exact", 1), ("prefix", 1)]
        );
        assert_eq!(prompt.selected, 0);
        assert_eq!(
            prompt.text,
            r"Distribution\s+Service\s+Trigger:\s+\d+\s+queued"
        );
    }

    /// The popup says what the pattern will do before it is allowed to do it.
    #[test]
    fn the_hide_pattern_popup_counts_the_rows_it_would_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));

        // The derived pattern generalises the counter, so it covers all ten rows.
        let preview = app.hide_pattern_preview(
            r"Distribution\s+Service\s+Trigger:\s+\d+\s+queued",
            "message",
        );
        assert_eq!((preview.matched, preview.total), (10, 10));
        assert!(!preview.capped());
        assert_eq!(preview.samples.len(), PATTERN_PREVIEW_SAMPLES);
        assert_eq!(preview_summary(&preview, true), "hides 10 of 10 shown rows");
        assert_eq!(
            preview_summary(&preview, false),
            "keeps 10 of 10 shown rows"
        );

        // Narrowing it to one counter narrows the blast radius the popup reports.
        let preview = app.hide_pattern_preview(r"Trigger:\s+7\s+queued", "message");
        assert_eq!(preview.matched, 1);
        assert_eq!(preview.samples, ["Distribution Service Trigger: 7 queued"]);

        let preview = app.hide_pattern_preview("nothing matches this", "message");
        assert_eq!(preview.matched, 0);
        assert_eq!(
            preview_summary(&preview, true),
            "matches none of the 10 shown rows"
        );

        // A half-typed regex reports the error instead of a count.
        let preview = app.hide_pattern_preview("Trigger: (", "message");
        assert!(preview.error.is_some(), "{preview:?}");
        assert_eq!(preview.matched, 0);

        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("hides 10 of 10 shown rows"), "{screen}");
        assert!(screen.contains("Tab hide/keep"), "{screen}");
    }

    /// Tab flips the rule's direction without re-deriving the pattern.
    #[test]
    fn tab_turns_a_hide_pattern_into_a_keep_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        // Select two lines, but tighten the pattern so it keeps only those two.
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));
        clear_input(&mut app);
        type_text(&mut app, r"Trigger:\s+[01]\s+queued");

        press(&mut app, KeyCode::Tab);
        let Mode::HidePattern(prompt) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        let exclude = prompt.exclude;
        assert!(!exclude, "Tab did not flip the direction");
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Keep Pattern"), "{screen}");
        assert!(screen.contains("keeps 2 of 10 shown rows"), "{screen}");

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, "keep pattern added");
        assert_eq!(app.project.filters.rules[0].action, "include");
        assert_eq!(app.active_view().unwrap().visible.len(), 2);
    }

    /// `H` twice: the choice menu, then a template derived from the one current line.
    #[test]
    fn hide_pattern_can_be_derived_from_a_single_line() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('H'));
        assert!(matches!(app.mode, Mode::HideChoice(_)));
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("H  message pattern"), "{screen}");

        press(&mut app, KeyCode::Char('H'));
        let Mode::HidePattern(prompt) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        let (text, exclude) = (prompt.text, prompt.exclude);
        assert!(exclude);
        assert_eq!(app.status, "pattern from 1 line");
        // The counter is the only volatile token, so the template covers all ten lines.
        assert_eq!(text, r"Distribution\s+Service\s+Trigger:\s+\d+\s+queued");

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, "hide pattern added");
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
    }

    fn type_text(app: &mut AppState, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    /// Add a filter through the raw grammar input (the guided builder now owns `f`).
    fn add_filter_text(app: &mut AppState, text: &str) {
        app.open_input(Mode::Filter(String::new()));
        type_text(app, text);
        press(app, KeyCode::Enter);
    }

    /// Wipe an input popup's prefilled text before typing a replacement.
    fn clear_input(app: &mut AppState) {
        press_mod(app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    }

    #[test]
    fn detail_panel_shows_the_long_selected_line_wrapped_in_the_left_column() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        let screen = render(&mut app, 100, 30);

        assert!(screen.contains("Detail"), "no Detail panel:\n{screen}");
        // The full message does not fit the 34-wide sidebar, so it must appear across rows.
        assert!(!screen.contains(LONG_MESSAGE));
        assert!(screen.contains("UserSession"));
        assert!(
            screen.contains("4A2F99BC"),
            "tail of message lost:\n{screen}"
        );
        assert!(screen.contains("message"));
        assert!(screen.contains("schema"));

        // Detail tracks the cursor: move down and the second entry's fields show up.
        press(&mut app, KeyCode::Char('j'));
        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("cache miss"),
            "detail did not follow cursor:\n{screen}"
        );
        assert!(!screen.contains("4A2F99BC"));
    }

    #[test]
    fn enter_on_a_log_row_opens_a_full_detail_popup_with_raw_text() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::EntryDetail { scroll: 0 }));

        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("Log Detail"), "{screen}");
        assert!(screen.contains("parsed fields"), "{screen}");
        assert!(screen.contains("raw"), "{screen}");
        assert!(screen.contains("[HOST:h]"), "{screen}");
        assert!(screen.contains("UserSession"), "{screen}");
        assert!(
            screen.contains("4A2F99BC"),
            "tail of raw entry lost:\n{screen}"
        );

        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn p_pretty_prints_the_current_message_from_pane_or_detail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_message(
            tmp.path(),
            r#"payload={"user":"ada","roles":["admin","ops"]}"#,
        );

        press(&mut app, KeyCode::Char('P'));
        match &app.mode {
            Mode::PrettyPrint {
                title,
                body,
                scroll,
            } => {
                assert_eq!(title, "Pretty JSON");
                assert_eq!(*scroll, 0);
                assert!(body.contains("\"roles\": ["), "{body}");
                assert!(body.contains("\"admin\""), "{body}");
            }
            mode => panic!("expected pretty-print mode, got {mode:?}"),
        }
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("Pretty JSON"), "{screen}");

        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::EntryDetail { .. }));
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.mode, Mode::PrettyPrint { ref title, .. } if title == "Pretty JSON"));
    }

    #[test]
    fn p_reports_when_the_current_message_is_not_structured() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_message(tmp.path(), "plain text only");

        press(&mut app, KeyCode::Char('P'));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status, "no JSON, XML, or SQL found in this message");
    }

    #[test]
    fn help_popup_uses_the_available_width() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('?'));
        let screen = render(&mut app, 160, 80);

        assert!(screen.contains("Keys"), "{screen}");
        assert!(
            screen.contains(
                r#"Raw filter syntax (Tab from the builder): [schema="name"] field op [include|exclude] value"#
            ),
            "help text wrapped too early:\n{screen}"
        );
    }

    #[test]
    fn mouse_selects_and_copies_help_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        press(&mut app, KeyCode::Char('?'));
        render(&mut app, 160, 80);
        let help = app.help_area;
        assert!(help.height > 3, "help area was not rendered");

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            help.x + 2,
            help.y + 1,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            help.x + 2,
            help.y + 3,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.detail_selection,
            Some(DetailSelection {
                surface: DetailSurface::Help,
                anchor: 1,
                cursor: 3,
            })
        );
        assert_eq!(app.status, "3 help lines selected");

        press(&mut app, KeyCode::Char('y'));
        assert!(
            app.status.starts_with("copied 3 help line(s),"),
            "{}",
            app.status
        );
    }

    #[test]
    fn mouse_selects_and_copies_inline_detail_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        render(&mut app, 100, 30);
        let detail = app.detail_area;
        assert!(detail.height > 2, "detail area was not rendered");

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            detail.x + 2,
            detail.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            detail.x + 2,
            detail.y + 1,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.detail_selection,
            Some(DetailSelection {
                surface: DetailSurface::Inline,
                anchor: 0,
                cursor: 1,
            })
        );
        assert_eq!(app.status, "2 detail lines selected");

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            detail.x + 2,
            detail.y,
            KeyModifiers::NONE,
        );
        assert!(
            app.status.starts_with("copied 2 detail line(s),"),
            "{}",
            app.status
        );
    }

    #[test]
    fn mouse_selects_and_copies_full_detail_popup_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        press(&mut app, KeyCode::Enter);
        render(&mut app, 120, 30);
        let detail = app.entry_detail_area;
        assert!(detail.height > 3, "popup detail area was not rendered");

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            detail.x + 2,
            detail.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            detail.x + 2,
            detail.y + 2,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.detail_selection,
            Some(DetailSelection {
                surface: DetailSurface::Popup,
                anchor: 0,
                cursor: 2,
            })
        );

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            detail.x + 2,
            detail.y,
            KeyModifiers::NONE,
        );
        assert!(
            app.status.starts_with("copied 3 detail line(s),"),
            "{}",
            app.status
        );
    }

    #[test]
    fn mouse_selects_and_copies_pretty_print_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_message(
            tmp.path(),
            r#"payload={"user":"ada","roles":["admin","ops"]}"#,
        );

        press(&mut app, KeyCode::Char('P'));
        render(&mut app, 120, 30);
        let pretty = app.pretty_print_area;
        assert!(pretty.height > 3, "pretty area was not rendered");

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            pretty.x + 2,
            pretty.y + 1,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pretty.x + 2,
            pretty.y + 3,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.detail_selection,
            Some(DetailSelection {
                surface: DetailSurface::PrettyPrint,
                anchor: 1,
                cursor: 3,
            })
        );
        assert_eq!(app.status, "3 pretty lines selected");

        press(&mut app, KeyCode::Char('y'));
        assert!(
            app.status.starts_with("copied 3 pretty line(s),"),
            "{}",
            app.status
        );
    }

    #[test]
    fn full_detail_popup_uses_one_selected_line_over_the_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' ')); // select the second row
        press_mod(&mut app, KeyCode::Up, KeyModifiers::CONTROL); // cursor back to first row
        press(&mut app, KeyCode::Enter);

        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("cache miss"), "{screen}");
        assert!(!screen.contains("4A2F99BC"), "{screen}");
    }

    #[test]
    fn sidebar_width_grows_with_the_terminal_but_never_starves_the_panes() {
        assert_eq!(sidebar_width(100), 36); // narrow: enough for a nested filter row
        assert_eq!(sidebar_width(160), 40);
        assert_eq!(sidebar_width(400), 56); // capped
        assert_eq!(sidebar_width(50), 26); // tiny: panes keep 24 columns
        assert_eq!(sidebar_width(20), 0);
    }

    #[test]
    fn truncate_label_marks_clipped_text() {
        assert_eq!(truncate_label("short", 10), "short");
        assert_eq!(truncate_label("exactly-10", 10), "exactly-10");
        assert_eq!(truncate_label("far too long here", 10), "far too l…");
        assert_eq!(truncate_label("abc", 0), "");
        assert_eq!(truncate_label("abc", 1), "…");
        // Multi-byte input must not panic or split a char.
        assert_eq!(truncate_label("héllo wörld", 6), "héllo…");
    }

    #[test]
    fn a_clipped_filter_rule_is_marked_with_an_ellipsis() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        app.mutate_filters(|filters| {
            filters.add(FilterRule::new(
                "message",
                "contains",
                "UserSession::TimeOut() and then some more text",
                "exclude",
            ))
        });
        app.finish_work();

        // Narrow terminal: the rule cannot fit, so it must be visibly clipped.
        let narrow = render(&mut app, 100, 30);
        assert!(narrow.contains('…'), "clipped silently:\n{narrow}");
        assert!(!narrow.contains("'UserSession"));

        // Wide terminal: the sidebar grows, so more of the rule survives.
        let wide = render(&mut app, 220, 30);
        assert!(
            wide.contains("exclude message contains 'UserSession::TimeOu"),
            "wide sidebar showed no more of the rule:\n{wide}"
        );
    }

    #[test]
    fn a_filter_rule_that_fits_is_shown_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("level", "equals", "Trace", "exclude"))
        });

        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("* exclude level equals 'Trace'"),
            "{screen}"
        );
        assert!(!screen.contains('…'), "unnecessary ellipsis:\n{screen}");
    }

    #[test]
    fn detail_panel_is_dropped_when_the_sidebar_is_too_short() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        let screen = render(&mut app, 100, 12);
        assert!(!screen.contains("Detail"), "Detail should yield:\n{screen}");
        assert!(screen.contains("Project"));
    }

    #[test]
    fn sidebar_file_selection_shows_file_details() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1;

        let lines = app.sidebar_detail_lines(48).unwrap();
        let rendered = line_texts(&lines);

        assert!(rendered.iter().any(|line| line.starts_with("file")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("schema         Bracketed default")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("entries        2")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("total_lines    2")));
    }

    #[test]
    fn sidebar_filter_selection_shows_filter_details() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        app.mutate_filters(|filters| {
            filters.add(
                FilterRule::new("log_level", "equals", "Trace", "exclude")
                    .for_log_schema("Bracketed default"),
            )
        });
        app.finish_work();
        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Filter { .. }))
            .unwrap();

        let lines = app.sidebar_detail_lines(56).unwrap();
        let rendered = line_texts(&lines);

        assert!(rendered
            .iter()
            .any(|line| line.starts_with("field          log_level")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("schema         Bracketed default")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("value          Trace")));
    }

    #[test]
    fn adding_a_filter_autosaves_it_and_it_returns_on_the_next_run() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        let before = app.active_view().unwrap().visible.len();
        assert_eq!(before, 2);

        add_filter_text(&mut app, "level equals exclude Trace");

        assert_eq!(app.status, "filter added");
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
        assert_eq!(app.project.filters.rules.len(), 1);

        // Autosaved without any explicit Ctrl+s.
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        assert!(
            saved.contains("\"filters\""),
            "filters missing from {saved}"
        );
        assert!(saved.contains("Trace"));

        // A fresh launch restores the filter and applies it to the opened pane.
        let mut relaunched = app_with_log(tmp.path());
        assert_eq!(relaunched.project.filters.rules.len(), 1);
        assert_eq!(relaunched.active_view().unwrap().visible.len(), 1);
        let screen = render(&mut relaunched, 100, 30);
        assert!(
            screen.contains("exclude level equals"),
            "sidebar lost the filter:\n{screen}"
        );
    }

    #[test]
    fn filter_input_accepts_a_log_schema_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        add_filter_text(
            &mut app,
            "schema=\"Bracketed default\" level equals exclude Trace",
        );

        assert_eq!(app.status, "filter added");
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
        assert_eq!(
            app.project.filters.rules[0].log_schema.as_deref(),
            Some("Bracketed default")
        );
        assert!(
            app.sidebar_items()
                .iter()
                .any(|item| matches!(item, SidebarItem::Filter { label, .. } if label.contains("on schema 'Bracketed default'")))
        );
    }

    #[test]
    fn filter_input_rejects_an_unknown_log_schema_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        app.submit_filter("schema=missing level equals exclude Trace".to_string());

        assert_eq!(app.status, "unknown log schema: missing");
        assert!(app.project.filters.rules.is_empty());
    }

    /// Presets count back from the newest entry in the log. Anchoring them to wall-clock
    /// now would select nothing at all on a log written weeks ago.
    #[test]
    fn t_opens_a_picker_anchored_to_the_newest_log_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };

        assert_eq!(
            picker.row,
            TimePicker::DEFAULT_PRESET,
            "opens on Last 1 hour"
        );
        assert_eq!(
            picker.earliest.map(format_filter_datetime).as_deref(),
            Some("2026-06-16 10:09:00.000")
        );
        assert_eq!(
            picker.latest.map(format_filter_datetime).as_deref(),
            Some("2026-06-16 10:09:09.000")
        );
        // One hour back from the newest entry, not from today.
        assert_eq!(picker.end, "2026-06-16 10:09:09.000");
        assert_eq!(picker.start, "2026-06-16 09:09:09.000");
    }

    #[test]
    fn space_on_a_preset_fills_the_start_and_end_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        for _ in 0..3 {
            press(&mut app, KeyCode::Down); // Last 1 hour -> All time
        }
        press(&mut app, KeyCode::Char(' '));

        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(TIME_PRESETS[picker.row].0, "All time");
        assert_eq!(picker.start, "2026-06-16 10:09:00.000");
        assert_eq!(picker.end, "2026-06-16 10:09:09.000");
    }

    /// Digits are a shortcut on a preset row, where they cannot be part of a timestamp.
    #[test]
    fn a_digit_picks_a_preset_by_number() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('5')); // All time
        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(picker.row, 4);
        assert_eq!(picker.start, "2026-06-16 10:09:00.000");
    }

    #[test]
    fn the_picker_adds_an_include_timestamp_range_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.start = "2026-06-16 10:09:03".to_string();
            picker.end = "2026-06-16 10:09:05".to_string();
        }
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.project.filters.rules.len(), 1);
        let rule = &app.project.filters.rules[0];
        assert_eq!(rule.field, "timestamp");
        assert_eq!(rule.op, "range");
        assert_eq!(rule.action, "include");
        assert_eq!(rule.value, "2026-06-16 10:09:03..2026-06-16 10:09:05");
        assert_eq!(selected_visible_line_numbers(&app), vec![4, 5, 6]);
        assert!(
            app.status.starts_with("time range: 2026-06-16 10:09:03"),
            "{}",
            app.status
        );
    }

    /// A log spanning two hours, one entry a minute, so the presets differ.
    /// A bracketed line at `HH:MM` on 2026-06-16.
    fn line_at(hour: u32, minute: u32, tag: &str) -> String {
        format!(
            "2026-06-16 {hour:02}:{minute:02}:00.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][x.cpp:1] {tag}\n"
        )
    }

    /// "Last 15 minutes" counts back from the newest entry across *all* loaded sources,
    /// not from the focused pane's. Two files an hour apart, focused on the earlier one:
    /// the anchor must still be the later file's end.
    #[test]
    fn the_time_preset_anchors_to_the_newest_source_not_the_focused_one() {
        let tmp = tempfile::tempdir().unwrap();
        let early = tmp.path().join("early.log");
        let late = tmp.path().join("late.log");
        // early ends 10:00, late ends 11:00.
        std::fs::write(
            &early,
            format!("{}{}", line_at(9, 50, "early a"), line_at(10, 0, "early b")),
        )
        .unwrap();
        std::fs::write(
            &late,
            format!("{}{}", line_at(10, 50, "late a"), line_at(11, 0, "late b")),
        )
        .unwrap();

        let mut project = Project::load(tmp.path());
        project.add_file(&early, None);
        project.add_file(&late, None);
        let mut app = AppState::new(project);
        app.queue_initial_loads();
        app.finish_work();

        // Focus the pane on the earlier file, whose own latest is 10:00.
        let early_id = app.project.files[0].file_id.clone();
        app.open_file_in_focused(&early_id);
        app.finish_work();
        assert_eq!(
            AppState::file_time_bounds(&app.project.files[0]).unwrap().1,
            parse_datetime("2026-06-16 10:00:00").unwrap(),
        );

        // Open the picker and take "Last 15 minutes" (the top preset).
        press(&mut app, KeyCode::Char('t'));
        while !matches!(&app.mode, Mode::TimePicker(picker) if picker.row == 0) {
            press(&mut app, KeyCode::Up);
        }
        press(&mut app, KeyCode::Enter);

        // 10:45..11:00, anchored to the later file -- not 09:45..10:00.
        assert_eq!(
            app.project.filters.time_rule().unwrap().value,
            "2026-06-16 10:45:00.000..2026-06-16 11:00:00.000"
        );
    }

    fn app_spanning_two_hours(root: &std::path::Path) -> AppState {
        let log = root.join("span.log");
        let body: String = (0..120)
            .map(|i| {
                format!(
                    "2026-06-16 {:02}:{:02}:00.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:{i}] entry {i}\n",
                    9 + i / 60,
                    i % 60
                )
            })
            .collect();
        std::fs::write(&log, body).unwrap();
        boot(Project::load(root), &log)
    }

    /// The reported bug: switching from `Last 1 hour` to `Last 15 minutes` left the view
    /// showing the hour. Highlighting a preset did not fill Start and End, and Enter
    /// commits those fields -- so it silently reapplied the range already in force.
    #[test]
    fn highlighting_a_preset_is_what_enter_applies() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_spanning_two_hours(tmp.path());
        assert_eq!(app.active_view().unwrap().visible.len(), 120);

        // `t` opens on "Last 1 hour" (the default preset); Enter applies it.
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.active_view().unwrap().visible.len(), 61);

        // Reopen on that range, walk up to "Last 15 minutes", and press Enter. No Space.
        press(&mut app, KeyCode::Char('t'));
        while !matches!(&app.mode, Mode::TimePicker(picker) if picker.row == 0) {
            press(&mut app, KeyCode::Up);
        }
        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(TIME_PRESETS[picker.row].0, "Last 15 minutes");
        // The fields already show the highlighted preset, so they cannot lie about it.
        assert_eq!(picker.start, "2026-06-16 10:44:00.000");
        assert_eq!(picker.end, "2026-06-16 10:59:00.000");

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.active_view().unwrap().visible.len(), 16);
        assert_eq!(
            app.project.filters.time_rule().unwrap().value,
            "2026-06-16 10:44:00.000..2026-06-16 10:59:00.000"
        );
        assert_eq!(
            app.project.filters.rules.len(),
            1,
            "a second range was added"
        );
    }

    /// Moving off the End field must not wrap round onto a preset and overwrite the
    /// range that was just typed there.
    #[test]
    fn the_picker_rows_clamp_rather_than_wrap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_spanning_two_hours(tmp.path());

        press(&mut app, KeyCode::Char('t'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Down); // onto End, then try to fall off it
        }
        press_mod(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        for ch in "2026-06-16 09:30:00".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Down);

        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(picker.row, TimePicker::END_ROW, "Down wrapped off the end");
        assert_eq!(picker.end, "2026-06-16 09:30:00", "the typed end was lost");

        // And Up off the first preset stays on it.
        for _ in 0..10 {
            press(&mut app, KeyCode::Up);
        }
        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker");
        };
        assert_eq!(picker.row, 0);
    }

    /// Position of the first sidebar row of a given kind.
    fn sidebar_row(app: &AppState, want: &str) -> usize {
        app.sidebar_items()
            .iter()
            .position(|item| match (item, want) {
                (SidebarItem::File { .. }, "file") => true,
                (SidebarItem::TimeFilter { .. }, "time") => true,
                (SidebarItem::Filter { .. }, "text") => true,
                (SidebarItem::Search { .. }, "search") => true,
                (SidebarItem::Bookmark { .. }, "bookmark") => true,
                (SidebarItem::Hint(label), "no-time") => label.trim() == "none - t",
                _ => false,
            })
            .unwrap_or_else(|| panic!("no {want} row in {:?}", app.sidebar_items()))
    }

    fn set_time_range(app: &mut AppState, start: &str, end: &str) {
        press(app, KeyCode::Char('t'));
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.start = start.to_string();
            picker.end = end.to_string();
        }
        press(app, KeyCode::Enter);
    }

    #[test]
    fn format_range_span_drops_the_units_it_does_not_need() {
        let span = |ms: i64| format_range_span(ChronoDuration::milliseconds(ms));
        assert_eq!(span(2_000), "2s");
        assert_eq!(span(2_500), "2.500s");
        assert_eq!(span(900_000), "15m");
        assert_eq!(span(905_000), "15m05s");
        assert_eq!(span(3_600_000), "1h");
        assert_eq!(span(5_400_000), "1h30m");
        assert_eq!(span(2 * 86_400_000), "2d");
        assert_eq!(span(2 * 86_400_000 + 3_600_000), "2d01h");
        assert_eq!(span(-5_000), "0s");
    }

    #[test]
    fn a_time_range_reads_as_two_clock_times_when_it_stays_inside_a_day() {
        let rule = |value: &str| FilterRule::new("timestamp", "range", value, "include");
        assert_eq!(
            describe_time_range(&rule("2026-06-16 10:09:03..2026-06-16 10:09:05")),
            "10:09:03 → 10:09:05  (2s)"
        );
        // Crossing midnight brings the date back, on both ends.
        assert_eq!(
            describe_time_range(&rule("2026-06-16 23:00:00..2026-06-17 01:00:00")),
            "06-16 23:00:00 → 06-17 01:00:00  (2h)"
        );
        // An open end is spelled out rather than left blank.
        assert_eq!(
            describe_time_range(&rule("2026-06-16 10:09:03..")),
            "from 06-16 10:09:03"
        );
        assert_eq!(
            describe_time_range(&rule("..2026-06-16 10:09:05")),
            "until 06-16 10:09:05"
        );
        // A disabled range says so, and an unparseable one shows what is stored.
        let mut off = rule("2026-06-16 10:09:03..2026-06-16 10:09:05");
        off.enabled = false;
        assert!(describe_time_range(&off).ends_with(" (off)"));
        assert_eq!(describe_time_range(&rule("junk..junk")), "junk..junk");
    }

    /// `Filters` holds a `Text` list and a `Time` slot, each with its own rows.
    #[test]
    fn the_sidebar_groups_filters_into_text_and_time() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        // Empty: both sub-sections are present, each with its own hint.
        let screen = render(&mut app, 110, 30);
        assert!(screen.contains("Filters"), "{screen}");
        assert!(screen.contains("  Text"), "{screen}");
        assert!(screen.contains("    none - f or H"), "{screen}");
        assert!(screen.contains("  Time"), "{screen}");
        assert!(screen.contains("    none - t"), "{screen}");

        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("level", "equals", "Trace", "exclude"))
        });
        set_time_range(&mut app, "2026-06-16 10:09:03", "2026-06-16 10:09:05");

        // The time range is not listed among the text filters.
        let text_rows = app
            .sidebar_items()
            .iter()
            .filter(|item| matches!(item, SidebarItem::Filter { .. }))
            .count();
        assert_eq!(text_rows, 1);
        let screen = render(&mut app, 110, 30);
        assert!(
            screen.contains("* exclude level equals 'Trace'"),
            "{screen}"
        );
        assert!(screen.contains("* 10:09:03 → 10:09:05  (2s)"), "{screen}");
        assert!(
            !screen.contains("timestamp range"),
            "raw grammar:\n{screen}"
        );
    }

    /// Space on the Time row disables it, like any other filter.
    #[test]
    fn space_on_the_time_row_toggles_it_off() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        set_time_range(&mut app, "2026-06-16 10:09:03", "2026-06-16 10:09:05");
        assert_eq!(app.active_view().unwrap().visible.len(), 3);

        app.focus = Focus::Sidebar;
        app.sidebar_selected = sidebar_row(&app, "time");
        press(&mut app, KeyCode::Char(' '));

        assert!(!app.project.filters.rules[0].enabled);
        assert_eq!(app.active_view().unwrap().visible.len(), 10);
        // The row's mark flips to `o`; the ` (off)` tail is clipped at this width, and
        // the label itself carries it.
        let screen = render(&mut app, 110, 30);
        assert!(screen.contains("o 10:09:03 → 10:09:05  (2s)"), "{screen}");
        let SidebarItem::TimeFilter { label, .. } = &app.sidebar_items()[sidebar_row(&app, "time")]
        else {
            panic!("expected the time row");
        };
        assert!(label.ends_with(" (off)"), "{label}");
    }

    /// Enter reopens the picker on the range in force, not on a fresh preset.
    #[test]
    fn enter_on_the_time_row_reopens_the_picker_on_that_range() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        set_time_range(&mut app, "2026-06-16 10:09:03", "2026-06-16 10:09:05");

        app.focus = Focus::Sidebar;
        app.sidebar_selected = sidebar_row(&app, "time");
        press(&mut app, KeyCode::Enter);

        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(picker.start, "2026-06-16 10:09:03");
        assert_eq!(picker.end, "2026-06-16 10:09:05");
        // On Start, so Space does not overwrite the range with a preset.
        assert_eq!(picker.row, TimePicker::START_ROW);

        // Narrowing it replaces the rule rather than adding a second one.
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.end = "2026-06-16 10:09:04".to_string();
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project.filters.rules.len(), 1);
        assert_eq!(app.active_view().unwrap().visible.len(), 2);
    }

    /// Enter on the `none - t` hint is the obvious way to make a range.
    #[test]
    fn enter_on_the_empty_time_row_opens_the_picker() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.focus = Focus::Sidebar;
        app.sidebar_selected = sidebar_row(&app, "no-time");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::TimePicker(_)), "{:?}", app.mode);
    }

    /// Delete drops the row under the cursor, whichever kind of filter it is.
    #[test]
    fn delete_removes_the_selected_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("level", "equals", "Trace", "exclude"))
        });
        set_time_range(&mut app, "2026-06-16 10:09:03", "2026-06-16 10:09:05");
        assert_eq!(app.project.filters.rules.len(), 2);

        app.focus = Focus::Sidebar;
        app.sidebar_selected = sidebar_row(&app, "time");
        press(&mut app, KeyCode::Delete);
        assert!(
            app.status.starts_with("filter removed: time range"),
            "{}",
            app.status
        );
        assert_eq!(app.project.filters.time_rule(), None);
        assert_eq!(app.project.filters.rules.len(), 1);

        app.sidebar_selected = sidebar_row(&app, "text");
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.project.filters.rules.len(), 0);
        assert_eq!(app.active_view().unwrap().visible.len(), 10);

        // The cursor does not run off the end of the shortened list.
        assert!(app.sidebar_selected < app.sidebar_items().len());
        // Delete on a section header is a no-op.
        app.sidebar_selected = 0;
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.project.filters.rules.len(), 0);
    }

    /// `d` deletes whatever the sidebar cursor is on, unified across the kinds of rows.
    #[test]
    fn d_deletes_the_selected_filter_or_search() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("level", "equals", "Trace", "exclude"))
        });
        app.project.saved_searches = vec!["alpha".to_string(), "beta".to_string()];
        app.focus = Focus::Sidebar;

        // `d` on a text filter removes it, like Delete does.
        app.sidebar_selected = sidebar_row(&app, "text");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.project.filters.rules.len(), 0);
        assert!(app.status.starts_with("filter removed"), "{}", app.status);

        // `d` on a saved search removes that search.
        let row = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Search { text, .. } if text == "alpha"))
            .unwrap();
        app.sidebar_selected = row;
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.project.saved_searches, vec!["beta".to_string()]);
        assert!(app.status.contains("search removed"), "{}", app.status);
    }

    /// The palette offers context actions and runs them through the shared dispatcher.
    #[test]
    fn command_palette_is_context_aware_and_dispatches() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        // On a pane: line and pane actions are offered.
        app.focus = Focus::Pane;
        let pane = app.palette_commands();
        assert!(pane.contains(&Command::Copy));
        assert!(pane.contains(&Command::HideSimilar));
        assert!(pane.contains(&Command::PrettyPrint));
        assert!(pane.contains(&Command::SplitColumns));

        // `:` opens it; typing narrows the list; Enter runs the top match.
        press(&mut app, KeyCode::Char(':'));
        assert!(matches!(app.mode, Mode::Palette(_)));
        for ch in "add text".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        if let Mode::Palette(palette) = &app.mode {
            assert_eq!(
                palette.filtered().first().copied(),
                Some(Command::AddFilter)
            );
        } else {
            panic!("palette closed early");
        }
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.mode, Mode::FilterBuilder(_)),
            "Enter ran Add text filter (opens the guided builder)"
        );
    }

    #[test]
    fn command_palette_opens_the_live_source_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        assert!(app.palette_commands().contains(&Command::AddLiveSource));
        app.dispatch_command(Command::AddLiveSource).unwrap();
        let Mode::LiveSourceEditor(editor) = &app.mode else {
            panic!("expected live source editor, got {:?}", app.mode);
        };
        assert_eq!(editor.kind, LiveSourceKind::Kubernetes);
        assert_eq!(editor.schema, GENERIC_EXTRACTOR_NAME);
        assert!(editor
            .rows()
            .iter()
            .any(|(field, _, _)| *field == LiveSourceField::Pod));
    }

    #[test]
    fn live_source_quick_pick_values_update_dependent_fields() {
        let mut editor = LiveSourceEditor::new();
        editor.namespace = "old".to_string();
        editor.pod = "old-pod".to_string();
        editor.container = "old-container".to_string();

        apply_live_quick_pick_value(&mut editor, LiveSourceField::Namespace, "prod");
        assert_eq!(editor.namespace, "prod");
        assert_eq!(editor.pod, "");
        assert_eq!(editor.container, "");

        apply_live_quick_pick_value(&mut editor, LiveSourceField::Pod, "prod/api-7d9");
        assert_eq!(editor.namespace, "prod");
        assert_eq!(editor.pod, "api-7d9");
        assert_eq!(editor.container, "");

        apply_live_quick_pick_value(&mut editor, LiveSourceField::DockerContainer, "worker");
        assert_eq!(editor.docker_container, "worker");
    }

    #[test]
    fn library_schema_can_be_selected_before_live_source_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = AppState::new(Project::new(tmp.path()));
        let editor = LiveSourceEditor::new();
        let schema = Extractor::new("picked", "<message>").unwrap();

        app.apply_library_schema_to_live_editor(editor, schema);

        let Mode::LiveSourceEditor(editor) = &app.mode else {
            panic!("expected live source editor, got {:?}", app.mode);
        };
        assert_eq!(editor.schema, "picked");
        assert!(app.project.extractors.contains_key("picked"));
    }

    #[test]
    fn ui_config_saves_and_loads_the_selected_theme() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config/ui.json");
        let config = UiConfig {
            theme: ThemeName::Amber,
        };

        config.save_to(&path).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(r#""theme": "amber""#), "{body}");
        assert_eq!(UiConfig::load_from(&path).unwrap(), config);
    }

    #[test]
    fn ui_config_defaults_to_classic_when_theme_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ui.json");
        std::fs::write(&path, "{}").unwrap();

        assert_eq!(
            UiConfig::load_from(&path).unwrap().theme,
            ThemeName::Classic
        );
    }

    #[test]
    fn command_palette_opens_the_theme_picker() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.ui_config.theme = ThemeName::Amber;

        assert!(app.palette_commands().contains(&Command::ChooseTheme));
        app.dispatch_command(Command::ChooseTheme).unwrap();

        let Mode::ThemePicker(picker) = app.mode.clone() else {
            panic!("expected the theme picker, got {:?}", app.mode);
        };
        assert_eq!(picker.theme(), ThemeName::Amber);
    }

    #[test]
    fn bookmarks_toggle_notes_render_in_sidebar_and_navigate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);

        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.project.bookmarks.len(), 1);
        assert_eq!(app.project.bookmarks[0].line_no, 1);

        press(&mut app, KeyCode::Char('M'));
        assert!(matches!(app.mode, Mode::BookmarkNote { .. }));
        type_text(&mut app, "first failure");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project.bookmarks[0].note, "first failure");

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.project.bookmarks.len(), 2);
        assert_eq!(app.project.bookmarks[1].line_no, 3);

        let items = app.sidebar_items();
        assert!(items
            .iter()
            .any(|item| matches!(item, SidebarItem::Section(label) if label == "Bookmarks")));
        assert!(items.iter().any(
            |item| matches!(item, SidebarItem::Bookmark { label, .. } if label.contains("first failure"))
        ));

        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.active_view().unwrap().cursor, 0);

        let sidebar_width = app.workspace.sidebar_width;
        press(&mut app, KeyCode::Char(']'));
        assert!(app.bookmark_nav_pending.is_some());
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.active_view().unwrap().cursor, 2);
        assert_eq!(app.workspace.sidebar_width, sidebar_width);

        press(&mut app, KeyCode::Char('['));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.active_view().unwrap().cursor, 0);
    }

    #[test]
    fn incident_markdown_exports_selection_bookmarks_filters_and_ai_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 4);

        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('M'));
        type_text(&mut app, "suspect queue buildup");
        press(&mut app, KeyCode::Enter);

        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("message", "contains", "queued", "include"))
        });
        app.finish_work();

        app.open_ai_chat();
        app.ai
            .as_mut()
            .unwrap()
            .transcript
            .push(ChatLine::Assistant(
                "Queue buildup is the likely symptom.".to_string(),
            ));

        let body = app.incident_markdown();
        assert!(body.contains("## Selected Lines"), "{body}");
        assert!(body.contains("Distribution Service Trigger"), "{body}");
        assert!(body.contains("## Bookmarks"), "{body}");
        assert!(body.contains("suspect queue buildup"), "{body}");
        assert!(body.contains("## Filters"), "{body}");
        assert!(body.contains("queued"), "{body}");
        assert!(
            body.contains("Queue buildup is the likely symptom."),
            "{body}"
        );

        app.submit_export_incident("incident.md".to_string());
        let saved = std::fs::read_to_string(tmp.path().join("incident.md")).unwrap();
        assert!(saved.contains("# Log Scouter Incident Notes"));
        assert!(
            app.status.starts_with("exported incident notes"),
            "{}",
            app.status
        );
    }

    #[test]
    fn workspace_resizes_toggles_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        // `[`/`]` set an explicit sidebar width; `z` toggles focus mode.
        press(&mut app, KeyCode::Char(']'));
        assert!(app.workspace.sidebar_width.is_some());
        let widened = app.workspace.sidebar_width.unwrap();
        press(&mut app, KeyCode::Char('['));
        assert!(app.workspace.sidebar_width.unwrap() < widened);

        press(&mut app, KeyCode::Char('z'));
        assert!(app.workspace.focus_mode);
        assert_eq!(app.focus, Focus::Pane, "focus mode moves focus to the pane");

        // Panel toggles via the palette dispatcher.
        app.dispatch_command(Command::ToggleDetail).unwrap();
        assert!(!app.workspace.show_detail);
        app.dispatch_command(Command::ToggleSidebar).unwrap();
        assert!(!app.workspace.show_sidebar);

        // The layout survives a save/reopen.
        let reopened = reopen(app);
        assert!(reopened.workspace.focus_mode);
        assert!(!reopened.workspace.show_detail);
        assert!(!reopened.workspace.show_sidebar);
        assert!(reopened.workspace.sidebar_width.is_some());
    }

    #[test]
    fn undo_and_redo_restore_filters_and_log_actions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        assert_eq!(app.project.filters.rules.len(), 0);

        // Apply a filter through the same capture/commit path the run loop uses.
        let before = app.undo_snapshot();
        app.submit_filter("log_level equals exclude Trace".to_string());
        app.commit_change(before, Actor::User);
        app.finish_work();
        assert_eq!(app.project.filters.rules.len(), 1);
        assert_eq!(app.action_log.len(), 1);
        assert!(
            app.action_log[0].description.contains("added filter"),
            "{}",
            app.action_log[0].description
        );

        // Undo removes it; redo brings it back.
        app.undo();
        app.finish_work();
        assert_eq!(app.project.filters.rules.len(), 0);
        app.redo();
        app.finish_work();
        assert_eq!(app.project.filters.rules.len(), 1);

        // An unchanged action records nothing.
        let before = app.undo_snapshot();
        app.status = "just a status message".to_string();
        app.commit_change(before, Actor::User);
        assert_eq!(app.action_log.len(), 1, "no-op did not log");
    }

    #[test]
    fn timeline_buckets_entries_and_drag_sets_a_time_range() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 20);

        // `b` turns the timeline on, aggregating by the level field.
        press(&mut app, KeyCode::Char('b'));
        assert_eq!(app.workspace.timeline_field.as_deref(), Some("log_level"));

        let data = app
            .compute_timeline("log_level", 80)
            .expect("timeline data");
        // app_with_lines writes Trace lines; every entry is counted into a bucket.
        assert!(data.rows.iter().any(|(value, _)| value == "Trace"));
        let counted: u32 = data.rows.iter().flat_map(|(_, c)| c.iter()).sum();
        assert_eq!(counted, 20);

        // A drag across buckets builds the project's time-range filter.
        app.timeline_geom = Some(TimelineGeom {
            x0: 10,
            buckets: 60,
            y0: 2,
            y1: 5,
            min: data.min,
            max: data.max,
        });
        app.apply_timeline_range(20, 45);
        app.finish_work();
        assert!(
            app.project.filters.time_rule().is_some(),
            "drag set a time range"
        );
    }

    #[test]
    fn dragging_a_panel_border_resizes_its_height() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        // Stand in for the drawn results separator: top border at row 20, bottom (status) 28.
        app.panel_separators = vec![PanelSeparator {
            panel: PanelEdge::Results,
            top: 20,
            bottom: 28,
            x0: 0,
            x1: 100,
        }];
        assert_eq!(
            app.panel_separator_at(10, 20).map(|s| s.panel),
            Some(PanelEdge::Results)
        );

        let mouse = |kind, row| MouseEvent {
            kind,
            column: 10,
            row,
            modifiers: KeyModifiers::empty(),
        };
        app.begin_mouse_selection(mouse(MouseEventKind::Down(MouseButton::Left), 20));
        app.drag_mouse_selection(mouse(MouseEventKind::Drag(MouseButton::Left), 14));
        // Dragging the top border up to row 14 makes the panel 28 - 14 = 14 rows tall.
        assert_eq!(app.workspace.results_height, Some(14));
    }

    #[test]
    fn dragging_a_pane_border_reweights_heights() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.split_active(SplitMode::Vertical); // two stacked panes
        app.ensure_pane_weights();

        // Stand in for the drawn layout: two 10-row panes stacked from y=1.
        app.pane_layout = vec![
            Rect {
                x: 0,
                y: 1,
                width: 40,
                height: 10,
            },
            Rect {
                x: 0,
                y: 11,
                width: 40,
                height: 10,
            },
        ];
        // The border sits at y=11; a press there grabs boundary 0.
        assert_eq!(app.pane_separator_at(5, 11), Some(0));

        // Drag it up to y=4: the top pane shrinks below the bottom one.
        app.drag_pane_separator(0, 5, 4);
        assert!(
            app.workspace.pane_weights[0] < app.workspace.pane_weights[1],
            "top pane shrank: {:?}",
            app.workspace.pane_weights
        );
    }

    #[test]
    fn ctrl_arrow_reweights_panes_along_the_split() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.split_active(SplitMode::Horizontal); // two side-by-side panes, focus on the 2nd
        app.finish_work();
        assert_eq!(app.panes.len(), 2);

        // Along a horizontal split, Ctrl+Right grows the focused pane's weight.
        press_mod(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        app.ensure_pane_weights();
        assert!(
            app.workspace.pane_weights[app.focused_pane] > app.workspace.pane_weights[0],
            "focused pane got a larger share: {:?}",
            app.workspace.pane_weights
        );
    }

    /// The guided builder previews live and applies a working rule; `f` opens it.
    #[test]
    fn filter_builder_previews_and_applies() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('f'));
        let Mode::FilterBuilder(_) = app.mode else {
            panic!("f opens the guided builder, got {:?}", app.mode);
        };
        // Value row is focused; type a value the sample lines carry.
        type_text(&mut app, "Trace");
        if let Mode::FilterBuilder(builder) = &app.mode {
            // The live preview counts matches without applying anything yet.
            assert!(builder.error.is_none(), "valid rule");
            assert!(builder.preview.matched > 0, "preview found matches");
            assert_eq!(app.project.filters.rules.len(), 0, "not applied yet");
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, "filter added");
        assert_eq!(app.project.filters.rules.len(), 1);
        let rule = &app.project.filters.rules[0];
        assert_eq!(rule.field, "log_level");
        assert_eq!(rule.op, "equals");
        assert_eq!(rule.value, "Trace");
        assert_eq!(rule.action, "exclude");
    }

    /// Tab moves a rule from the guided builder to the raw editor and back, losslessly.
    #[test]
    fn filter_builder_switches_to_raw_and_back() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('f'));
        type_text(&mut app, "Trace");
        press(&mut app, KeyCode::Tab); // to raw
        let Mode::Filter(text) = app.mode.clone() else {
            panic!("Tab switches to the raw editor, got {:?}", app.mode);
        };
        assert!(text.contains("log_level"), "raw text: {text}");
        assert!(text.contains("Trace"), "raw text: {text}");
        press(&mut app, KeyCode::Tab); // back to builder
        let Mode::FilterBuilder(builder) = app.mode.clone() else {
            panic!("Tab switches back to the builder, got {:?}", app.mode);
        };
        assert_eq!(builder.field, "log_level");
        assert_eq!(builder.value, "Trace");
    }

    #[test]
    fn command_palette_on_a_source_lists_source_actions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::File { .. }))
            .unwrap();

        let commands = app.palette_commands();
        assert!(commands.contains(&Command::EditItem));
        assert!(commands.contains(&Command::DeleteSelected));
        // Pane-only actions are not offered on a source.
        assert!(!commands.contains(&Command::ClosePane));
    }

    /// Typing into Start must reach the field, not the preset list.
    #[test]
    fn the_start_field_is_editable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        for _ in 0..4 {
            press(&mut app, KeyCode::Down); // onto Start
        }
        press_mod(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        for ch in "2026-06-16 10:09:07".chars() {
            press(&mut app, KeyCode::Char(ch));
        }

        let Mode::TimePicker(picker) = app.mode.clone() else {
            panic!("expected the time picker, got {:?}", app.mode);
        };
        assert_eq!(picker.row, TimePicker::START_ROW);
        assert_eq!(picker.start, "2026-06-16 10:09:07");

        press(&mut app, KeyCode::Enter);
        assert_eq!(selected_visible_line_numbers(&app), vec![8, 9, 10]);
    }

    /// A typo must not throw away everything else the user typed.
    #[test]
    fn an_unparseable_field_keeps_the_picker_open() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.start = "yesterday".to_string();
        }
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, Mode::TimePicker(_)), "stays open");
        assert_eq!(app.status, "start is not a timestamp: yesterday");
        assert!(app.project.filters.rules.is_empty());
    }

    #[test]
    fn a_backwards_range_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.start = "2026-06-16 10:09:05".to_string();
            picker.end = "2026-06-16 10:09:03".to_string();
        }
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, Mode::TimePicker(_)));
        assert_eq!(app.status, "start is after end");
        assert!(app.project.filters.rules.is_empty());
    }

    /// An open-ended range is legal: blank End means "up to the last line".
    #[test]
    fn a_blank_end_gives_an_open_ended_range() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        if let Mode::TimePicker(picker) = &mut app.mode {
            picker.start = "2026-06-16 10:09:08".to_string();
            picker.end = String::new();
        }
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.project.filters.rules[0].value, "2026-06-16 10:09:08..");
        assert_eq!(selected_visible_line_numbers(&app), vec![9, 10]);
    }

    #[test]
    fn esc_cancels_the_picker_without_adding_a_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.project.filters.rules.is_empty());
    }

    #[test]
    fn filters_are_global_across_panes_and_survive_clearing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        press(&mut app, KeyCode::Char('|')); // split
        assert_eq!(app.panes.len(), 2);

        add_filter_text(&mut app, "level equals exclude Trace");

        // Both panes see the project-global filter, not just the focused one.
        for pane in &app.panes {
            assert_eq!(pane.view.filters.rules.len(), 1);
            assert_eq!(pane.view.visible.len(), 1);
        }

        app.dispatch_command(Command::ClearFilters).unwrap();
        app.finish_work();
        assert!(app.project.filters.rules.is_empty());
        for pane in &app.panes {
            assert_eq!(pane.view.visible.len(), 2);
        }
        // Assert on the filters themselves: the schema's sample lines mention "Trace"
        // too, so scanning the whole file for that word proves nothing.
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            saved["filters"]["rules"].as_array().map(Vec::len),
            Some(0),
            "cleared filter still saved: {saved}"
        );
    }

    #[test]
    fn export_and_import_default_to_the_user_level_library() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        let expected = user_filter_dir().unwrap();
        assert_eq!(app.default_filter_folder_path(), expected);
        assert_eq!(app.default_filter_folder_input(), "~/.log-scouter/filters");
        // The prefilled `~` text round-trips back to the same absolute folder.
        assert_eq!(
            app.filter_folder_from_input(&app.default_filter_folder_input()),
            expected
        );

        // Relative input still resolves inside the project.
        assert_eq!(
            app.filter_folder_from_input("mine"),
            app.project.root.join("mine")
        );

        // Export to an explicit folder, then import it back into a second project.
        let library = tmp.path().join("library");
        app.mutate_filters(|filters| {
            filters.add(FilterRule::new("level", "equals", "Trace", "exclude"))
        });
        app.finish_work();
        app.submit_export_filters(library.to_string_lossy().to_string());
        assert!(
            app.status.starts_with("exported 1 filter(s)"),
            "{}",
            app.status
        );

        let other = tempfile::tempdir().unwrap();
        let mut target = app_with_log(other.path());
        target.submit_load_filters(library.to_string_lossy().to_string());
        target.finish_work();
        assert!(
            target.status.starts_with("loaded 1 filter(s)"),
            "{}",
            target.status
        );
        assert_eq!(target.project.filters.rules.len(), 1);
        assert_eq!(target.active_view().unwrap().visible.len(), 1);

        // Importing the same folder twice does not duplicate rules.
        target.submit_load_filters(library.to_string_lossy().to_string());
        target.finish_work();
        assert_eq!(target.project.filters.rules.len(), 1);
    }

    #[test]
    fn importing_from_a_missing_folder_reports_instead_of_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());
        app.submit_load_filters(tmp.path().join("nope").to_string_lossy().to_string());
        assert!(
            app.status.starts_with("no filter folder:"),
            "{}",
            app.status
        );
        assert!(app.project.filters.rules.is_empty());
    }

    #[test]
    fn a_long_hide_pattern_can_be_edited_and_stays_visible() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('H'));

        let Mode::HidePattern(PatternPrompt { text: original, .. }) = app.mode.clone() else {
            panic!("expected HidePattern");
        };
        // Caret starts at the end of the prefilled text.
        assert_eq!(app.input_cursor, original.chars().count());

        // Typing appends; Backspace removes; Home/Delete edit the front.
        type_text(&mut app, "XY");
        let Mode::HidePattern(PatternPrompt { text: edited, .. }) = app.mode.clone() else {
            panic!("expected HidePattern");
        };
        assert_eq!(edited, format!("{original}XY"));

        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.input_cursor, 0);
        press(&mut app, KeyCode::Delete);
        let Mode::HidePattern(PatternPrompt { text: edited, .. }) = app.mode.clone() else {
            panic!("expected HidePattern");
        };
        assert_eq!(edited, format!("{}X", &original[1..]));

        // Crucially, the whole value is rendered: it wraps instead of clipping.
        let screen = render(&mut app, 100, 30);
        let tail: String = edited
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(
            screen.contains(&tail),
            "edited tail {tail:?} not on screen:\n{screen}"
        );
    }

    #[test]
    fn input_editing_helpers_handle_multibyte_text() {
        let mut text = String::from("héllo");
        insert_char(&mut text, 1, 'X');
        assert_eq!(text, "hXéllo");
        remove_char(&mut text, 2); // the 'é'
        assert_eq!(text, "hXllo");
        remove_char(&mut text, 99); // out of range is a no-op
        assert_eq!(text, "hXllo");

        assert_eq!(chunk_chars("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(chunk_chars("", 4), vec![""]);
        assert_eq!(chunk_chars("héllo", 2), vec!["hé", "ll", "o"]);
    }

    #[test]
    fn a_search_after_filtering_reuses_the_cached_filter_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 50);

        add_filter_text(&mut app, "message contains exclude queued");
        assert_eq!(app.active_view().unwrap().visible.len(), 0);

        // Clear that filter, then filter to a subset and search within it.
        app.dispatch_command(Command::ClearFilters).unwrap();
        app.finish_work();
        press(&mut app, KeyCode::Char('f'));
        type_text(&mut app, "message contains include Trigger:");
        press(&mut app, KeyCode::Enter);
        let filtered = app.active_view().unwrap().visible.len();
        assert_eq!(filtered, 50);

        // A queued search must reuse the base rather than re-running the filter stage.
        app.submit_search("queued".to_string());
        let reused = matches!(app.work.front(), Some(Job::Recompute(job)) if matches!(job.stage, Stage::Search { .. }));
        assert!(reused, "search re-ran the filter pass");
        app.finish_work();
        assert_eq!(app.active_view().unwrap().match_set.len(), 50);

        // Changing the filters invalidates the cache, so the filter stage runs again.
        app.mutate_filters(|filters| filters.clear());
        let refiltered = matches!(app.work.front(), Some(Job::Recompute(job)) if matches!(job.stage, Stage::Filter { .. }));
        assert!(refiltered, "filter change did not invalidate the base");
        app.finish_work();
    }

    #[test]
    fn a_search_jumps_to_the_first_match_only_after_the_scan_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 40);

        app.submit_search("Trigger: 7 ".to_string());
        // Still queued: the cursor has not moved yet.
        assert!(app.work_pending());
        assert_eq!(app.active_view().unwrap().cursor, 0);

        app.finish_work();
        assert!(!app.work_pending());
        assert_eq!(app.active_view().unwrap().cursor, 7);
        assert!(app.status.contains("match"), "{}", app.status);
    }

    #[test]
    fn slash_search_is_inline_live_and_submits_to_results() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);

        press(&mut app, KeyCode::Char('/'));
        assert!(matches!(app.mode, Mode::Search(_)));
        let screen = render(&mut app, 120, 30);
        assert!(
            !screen.contains("text | \"phrase\""),
            "search should not open the old popup:\n{screen}"
        );
        assert!(
            !screen.contains("Matches"),
            "live search should not open the results panel yet:\n{screen}"
        );

        type_text(&mut app, "Trigger: 7");
        assert!(matches!(app.mode, Mode::Search(_)));
        assert_eq!(app.active_view().unwrap().query_text, "Trigger: 7");
        assert_eq!(app.active_view().unwrap().cursor, 7);
        assert!(
            !app.search_results_visible(),
            "results panel stays hidden while editing"
        );
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("/Trigger: 7"), "{screen}");
        assert!(!screen.contains("Matches"), "{screen}");

        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.search_results_visible());
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("Matches 1/1"), "{screen}");
    }

    #[test]
    fn n_and_shift_n_navigate_submitted_search_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);

        app.submit_search("queued".to_string());
        app.finish_work();
        assert_eq!(app.active_view().unwrap().cursor, 0);

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.active_view().unwrap().cursor, 1);

        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.active_view().unwrap().cursor, 0);
    }

    #[test]
    fn loading_reports_progress_and_populates_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("big.log");
        let body: String = (0..30_000)
            .map(|i| {
                format!("2026-06-16 10:09:00.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:{i}] line {i}\n")
            })
            .collect();
        std::fs::write(&log, &body).unwrap();

        let mut project = Project::load(tmp.path());
        project.add_file(&log, None);
        let mut app = AppState::new(project);
        app.queue_initial_loads();
        assert!(app.work_pending());
        assert!(!app.project.files[0].loaded);

        // One slice: partway through, with a bar to show for it.
        app.step_work(Duration::from_millis(1));
        let progress = app.progress.as_ref().expect("a progress bar while loading");
        assert!(progress.label.starts_with("Loading big.log"));
        assert!(progress.done > 0 && progress.done <= progress.total);
        assert_eq!(progress.total, body.len() as u64);

        app.finish_work();
        assert!(app.project.files[0].loaded);
        assert_eq!(app.project.files[0].entries.len(), 30_000);
        assert_eq!(app.active_view().unwrap().visible.len(), 30_000);
        assert!(app.progress.is_none(), "bar should clear when work drains");
    }

    #[test]
    fn opening_a_folder_from_an_empty_project_loads_direct_text_files() {
        let start = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        std::fs::write(
            logs.path().join("a.log"),
            "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] alpha\n",
        )
        .unwrap();
        std::fs::write(
            logs.path().join("b.txt"),
            "2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:1] beta\n",
        )
        .unwrap();
        std::fs::write(logs.path().join("bin.dat"), b"text\0binary").unwrap();
        std::fs::create_dir(logs.path().join("nested")).unwrap();
        std::fs::write(logs.path().join("nested").join("c.log"), "nested\n").unwrap();

        let mut app = AppState::new(Project::new(start.path()));
        assert!(app.active_view().is_none());

        app.submit_open_folder(logs.path().to_string_lossy().to_string())
            .unwrap();
        assert!(app.work_pending());
        assert!(app.status.contains("with 2 text files"), "{}", app.status);
        app.finish_work();

        assert_eq!(
            app.project.root,
            std::fs::canonicalize(logs.path()).unwrap()
        );
        let names: Vec<&str> = app
            .project
            .files
            .iter()
            .map(|file| file.display_name.as_str())
            .collect();
        assert_eq!(names, ["a.log", "b.txt"]);
        assert!(app.project.files.iter().all(|file| file.loaded));
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
    }

    /// The rows of the `o` browser, as rendered.
    fn browser_rows(app: &AppState) -> Vec<String> {
        let Mode::OpenFolder(browser) = &app.mode else {
            panic!("expected the folder browser, got {:?}", app.mode);
        };
        browser.rows.iter().map(|row| browser.label(row)).collect()
    }

    fn browser(app: &AppState) -> &FolderBrowser {
        let Mode::OpenFolder(browser) = &app.mode else {
            panic!("expected the folder browser, got {:?}", app.mode);
        };
        browser
    }

    #[test]
    fn o_browses_from_the_project_folder_and_lists_its_subfolders() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("archive")).unwrap();
        std::fs::create_dir(root.path().join("Nested")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        std::fs::write(root.path().join("a.log"), "one\n").unwrap();
        std::fs::write(root.path().join("b.log"), "two\n").unwrap();

        let mut app = AppState::new(Project::new(root.path()));
        press(&mut app, KeyCode::Char('o'));

        // Sorted case-insensitively, dot-folders hidden, and the current folder counted.
        assert_eq!(
            browser_rows(&app),
            [
                "./     open this folder",
                "../    go up",
                "archive/",
                "Nested/",
            ]
        );
        assert_eq!(browser(&app).file_count, 2);
        assert_eq!(browser(&app).selected, 0);

        // `.` reveals what was hidden, and puts the selection back on a known row.
        press(&mut app, KeyCode::Char('.'));
        assert!(browser_rows(&app).contains(&".hidden/".to_string()));
        assert_eq!(browser(&app).selected, 0);
        press(&mut app, KeyCode::Char('.'));
        assert_eq!(browser_rows(&app).len(), 4);
    }

    #[test]
    fn the_browser_walks_down_into_a_subfolder_and_back_up() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("archive").join("2026-06");
        std::fs::create_dir_all(&deep).unwrap();

        let mut app = AppState::new(Project::new(root.path()));
        // `Project::new` canonicalises, so compare against what it settled on.
        let base = app.project.root.clone();
        let archive = base.join("archive");
        press(&mut app, KeyCode::Char('o'));

        // Down to `archive/`, then Enter descends rather than opening.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(browser(&app).selected, 2);
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.mode, Mode::OpenFolder(_)),
            "Enter opened a project"
        );
        assert_eq!(browser(&app).current, archive);
        assert_eq!(browser(&app).selected, 0, "a new listing starts at the top");
        assert_eq!(browser_rows(&app)[2], "2026-06/");

        // Right descends too; Left comes back.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Right);
        assert_eq!(browser(&app).current, archive.join("2026-06"));
        assert!(deep.is_dir());
        press(&mut app, KeyCode::Left);
        assert_eq!(browser(&app).current, archive);

        // `..` goes up as well, and the browser is still open.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(browser(&app).current, base);
    }

    /// Enter on the `./` row is the one row that leaves the browser.
    #[test]
    fn the_browser_opens_the_folder_it_is_showing() {
        let start = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::write(logs.join("a.log"), "2026-06-16 10:00:01.000 INFO alpha\n").unwrap();

        let mut app = AppState::new(Project::new(start.path()));
        press(&mut app, KeyCode::Char('o'));
        // The browser starts at the project's own folder.
        assert_eq!(browser(&app).current, app.project.root);
        app.mode = Mode::OpenFolder(FolderBrowser::open(root.path().to_path_buf()).unwrap());

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j')); // logs/
        press(&mut app, KeyCode::Enter); // descend
        press(&mut app, KeyCode::Enter); // ./ -> open it
        app.finish_work();

        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.status.contains("with 1 text file"), "{}", app.status);
        assert_eq!(app.project.root, std::fs::canonicalize(&logs).unwrap());
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
    }

    #[test]
    fn the_file_browser_lists_and_adds_a_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("app.log"),
            "2026-06-16 10:00:01.000 INFO alpha\n",
        )
        .unwrap();

        let mut app = AppState::new(Project::new(root.path()));
        press(&mut app, KeyCode::Char('a'));
        assert!(matches!(app.mode, Mode::OpenFolder(_)), "a opens a browser");

        // The file is offered as a row, and the file picker has no "open this folder" row.
        let rows = browser_rows(&app);
        assert!(rows.iter().any(|row| row == "app.log"), "rows: {rows:?}");
        assert!(
            !rows.iter().any(|row| row.contains("open this folder")),
            "rows: {rows:?}"
        );

        // Select the file and add it.
        let index = rows.iter().position(|row| row == "app.log").unwrap();
        for _ in 0..index {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Enter);
        app.finish_work();

        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.project.files.iter().filter(|f| !f.is_merged()).count(),
            1
        );
        assert_eq!(app.active_view().unwrap().visible.len(), 1);
    }

    #[test]
    fn the_file_browser_can_fall_back_to_typing_a_path() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AppState::new(Project::new(root.path()));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('p'));
        assert!(
            matches!(app.mode, Mode::AddFile(_)),
            "p opens the path input"
        );
    }

    #[test]
    fn esc_closes_the_browser_without_changing_the_project() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("elsewhere")).unwrap();
        let mut app = AppState::new(Project::new(root.path()));
        let before = app.project.root.clone();

        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Esc);

        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.project.root, before);
    }

    /// A folder the browser cannot read leaves it exactly where it was. Vanishing stands
    /// in for the permission error, which `root` would not hit.
    #[test]
    fn an_unreadable_folder_is_reported_and_does_not_move_the_browser() {
        let root = tempfile::tempdir().unwrap();
        let doomed = root.path().join("doomed");
        std::fs::create_dir(&doomed).unwrap();

        let mut app = AppState::new(Project::new(root.path()));
        let base = app.project.root.clone();
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(browser_rows(&app)[2], "doomed/");

        // It goes away after the listing was taken, as a folder on a network share might.
        std::fs::remove_dir(&doomed).unwrap();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);

        assert!(app.status.starts_with("could not read"), "{}", app.status);
        assert!(app.status.contains("doomed"), "{}", app.status);
        assert_eq!(
            browser(&app).current,
            base,
            "browser moved into a dead folder"
        );
    }

    #[test]
    fn truncate_head_keeps_the_tail_of_a_long_path() {
        assert_eq!(truncate_head("/a/b/c", 10), "/a/b/c");
        assert_eq!(truncate_head("/very/long/path", 6), "…/path");
        assert_eq!(truncate_head("/very/long/path", 0), "/very/long/path");
    }

    #[test]
    fn esc_cancels_in_flight_work() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        app.submit_search("Trigger".to_string());
        assert!(app.work_pending());

        app.cancel_work();
        assert!(!app.work_pending());
        assert!(app.progress.is_none());
        assert_eq!(app.status, "cancelled");
    }

    #[test]
    fn the_sidebar_stars_files_that_are_open_in_a_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 3);

        // Add a second file; it becomes the open one, so the star moves to it.
        let other = tmp.path().join("other.log");
        std::fs::write(&other, "2026-06-16 10:09:00.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:1] hello\n").unwrap();
        app.submit_add_file(other.to_string_lossy().to_string())
            .unwrap();
        app.finish_work();

        let labels: Vec<String> = app
            .sidebar_items()
            .iter()
            .filter_map(|item| match item {
                SidebarItem::File { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(labels.len(), 2);
        assert!(labels[0].starts_with("  many.log"), "{:?}", labels[0]);
        assert!(labels[1].starts_with("* other.log"), "{:?}", labels[1]);

        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("* other.log"), "{screen}");
    }

    /// Two files with interleaved timestamps, plus a helper to open both in a project.
    fn app_with_two_logs(root: &std::path::Path) -> (AppState, std::path::PathBuf) {
        let a = root.join("a.log");
        let b = root.join("b.log");
        std::fs::write(&a, "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] alpha one\n\
                            2026-06-16 10:00:03.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:2] alpha two\n").unwrap();
        std::fs::write(&b, "2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:1] beta one\n").unwrap();

        let mut project = Project::load(root);
        project.add_file(&a, None);
        project.add_file(&b, None);
        let mut app = AppState::new(project);
        app.queue_initial_loads();
        app.finish_work();
        (app, b)
    }

    /// Sidebar file rows, in order, as rendered.
    fn file_labels(app: &AppState) -> Vec<String> {
        app.sidebar_items()
            .iter()
            .filter_map(|item| match item {
                SidebarItem::File { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn space_in_the_sidebar_adds_a_log_to_the_pane_and_merges_by_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1; // a.log, already shown

        // Space on the second log adds it to the current view.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert!(
            app.status
                .starts_with("merged 2 logs by timestamp, 3 entries"),
            "{}",
            app.status
        );
        // Focus stays in the sidebar so more logs can be added.
        assert_eq!(app.focus, Focus::Sidebar);

        let view_id = app.active_view().unwrap().file_id.clone();
        let file = app.project.get_file(&view_id).unwrap();
        assert!(file.is_merged());
        let messages: Vec<String> = file.entries.iter().map(|e| file.message(e)).collect();
        assert_eq!(messages, ["alpha one", "beta one", "alpha two"]);

        // The merge is NOT a new item in the file list; both sources are starred.
        let labels = file_labels(&app);
        assert_eq!(
            labels.len(),
            2,
            "merge leaked into the file list: {labels:?}"
        );
        assert!(labels[0].starts_with("* a.log"), "{:?}", labels[0]);
        assert!(labels[1].starts_with("* b.log"), "{:?}", labels[1]);
        let screen = render(&mut app, 100, 30);
        assert!(
            !screen.contains("a.log + b.log ("),
            "merge shown as a file:\n{screen}"
        );

        // Space again removes it, collapsing back to the single log.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.status, "showing a.log");
        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert!(!file.is_merged());
        assert_eq!(file.display_name, "a.log");
        // The discarded merge is collected, not left lying around.
        assert!(app.project.files.iter().all(|file| !file.is_merged()));

        let labels = file_labels(&app);
        assert!(labels[0].starts_with("* a.log"));
        assert!(labels[1].starts_with("  b.log"), "{:?}", labels[1]);
    }

    /// End to end, the way the bug was reported: open a folder whose logs are not all in
    /// the same format, add both to the pane, and read the merged view top to bottom.
    #[test]
    fn merging_logs_of_different_formats_orders_them_by_timestamp() {
        let start = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        std::fs::write(
            logs.path().join("a.log"),
            "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] alpha one\n\
             2026-06-16 10:00:03.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:2] alpha two\n",
        )
        .unwrap();
        std::fs::write(
            logs.path().join("b.log"),
            "2026-06-16 10:00:02.000 INFO beta one\n2026-06-16 10:00:04.000 INFO beta two\n",
        )
        .unwrap();

        let mut app = AppState::new(Project::new(start.path()));
        app.submit_open_folder(logs.path().to_string_lossy().to_string())
            .unwrap();
        app.finish_work();

        // Each log is read under a schema that can actually parse it, so neither arrives
        // as one folded, timestamp-less entry.
        assert_eq!(app.project.files[0].extractor_name, DEFAULT_EXTRACTOR_NAME);
        assert_eq!(app.project.files[1].extractor_name, GENERIC_EXTRACTOR_NAME);
        assert_eq!(app.project.files[1].entries.len(), 2);

        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1; // a.log, already shown
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.status, "merged 2 logs by timestamp, 4 entries");

        let view_id = app.active_view().unwrap().file_id.clone();
        let merged = app.project.get_file(&view_id).unwrap();
        let expected = ["alpha one", "beta one", "alpha two", "beta two"];
        for (entry, want) in merged.entries.iter().zip(expected) {
            assert!(
                entry.raw.ends_with(want),
                "{:?} should end with {want}",
                entry.raw
            );
        }

        // And the pane shows a timestamp for the plain log, sniffed off its own line.
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("2026-06-16 10:00:02.000"), "{screen}");
    }

    /// Click the `n`th sidebar row (0 = the "Files" header). Requires a prior render
    /// so the sidebar's rect is known.
    fn click_sidebar_row(app: &mut AppState, row: u16, ctrl: bool) {
        let area = app.sidebar_area;
        assert!(area.width > 0, "render before clicking");
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y + row,
            modifiers: if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        });
        app.finish_work();
    }

    fn shown_display_name(app: &AppState) -> String {
        let view = app.active_view().unwrap();
        app.project
            .get_file(&view.file_id)
            .unwrap()
            .display_name
            .clone()
    }

    /// Three logs, so a Shift sweep has something in the middle to pick up.
    fn app_with_three_logs(root: &std::path::Path) -> AppState {
        let (mut app, _) = app_with_two_logs(root);
        let c = root.join("c.log");
        std::fs::write(&c, "2026-06-16 10:00:04.000 [HOST:h][SERVER:S][PID:1][THR:2][SQL][Warn][UID:0][SID:0][OID:0][c.cpp:1] gamma one\n").unwrap();
        app.submit_add_file(c.to_string_lossy().to_string())
            .unwrap();
        app.finish_work();
        app
    }

    #[test]
    fn ctrl_click_adds_a_log_to_the_view_and_a_plain_click_replaces_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        render(&mut app, 100, 30);

        // Row 0 is the "Files" header; rows 1 and 2 are the logs.
        click_sidebar_row(&mut app, 1, false);
        assert_eq!(shown_display_name(&app), "a.log");
        assert_eq!(app.focus, Focus::Sidebar);

        // Ctrl+click merges the second log in, without a new file row appearing.
        click_sidebar_row(&mut app, 2, true);
        assert_eq!(shown_display_name(&app), "a.log + b.log");
        assert_eq!(file_labels(&app).len(), 2);
        assert!(
            app.status.starts_with("merged 2 logs by timestamp"),
            "{}",
            app.status
        );

        // Ctrl+click again toggles it back out.
        click_sidebar_row(&mut app, 2, true);
        assert_eq!(shown_display_name(&app), "a.log");

        // A plain click replaces the view rather than adding to it.
        click_sidebar_row(&mut app, 2, true);
        assert_eq!(shown_display_name(&app), "a.log + b.log");
        click_sidebar_row(&mut app, 2, false);
        assert_eq!(shown_display_name(&app), "b.log");
        assert!(app.project.files.iter().all(|file| !file.is_merged()));
    }

    #[test]
    fn clicking_a_non_file_row_only_moves_the_sidebar_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        render(&mut app, 100, 30);

        click_sidebar_row(&mut app, 0, false); // the "Files" header
        assert_eq!(app.sidebar_selected, 0);
        assert_eq!(shown_display_name(&app), "a.log");

        // Out of range is a no-op, not a panic.
        click_sidebar_row(&mut app, 200, false);
        assert_eq!(app.sidebar_selected, 0);
    }

    #[test]
    fn shift_arrows_sweep_a_range_of_logs_into_the_view() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_three_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1; // a.log

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(shown_display_name(&app), "a.log + b.log");

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(shown_display_name(&app), "a.log + b.log + c.log");
        assert_eq!(app.focus, Focus::Sidebar, "sweeping keeps sidebar focus");

        // All three sources are starred, and the merge is still not a file row.
        let labels = file_labels(&app);
        assert_eq!(labels.len(), 3);
        assert!(
            labels.iter().all(|label| label.starts_with('*')),
            "{labels:?}"
        );

        // Merged by timestamp across all three.
        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        let messages: Vec<String> = file.entries.iter().map(|e| file.message(e)).collect();
        assert_eq!(
            messages,
            ["alpha one", "beta one", "alpha two", "gamma one"]
        );

        // Shrinking the range back drops logs from the view.
        press_mod(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(shown_display_name(&app), "a.log + b.log");
        press_mod(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(shown_display_name(&app), "a.log");
        assert!(app.project.files.iter().all(|file| !file.is_merged()));
    }

    #[test]
    fn a_plain_motion_clears_the_sidebar_range_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_three_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1;

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert!(app.sidebar_anchor.is_some());

        press(&mut app, KeyCode::Down); // plain motion
        assert!(app.sidebar_anchor.is_none());

        // The next Shift press starts a fresh range from where the cursor now is.
        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(shown_display_name(&app), "b.log + c.log");
    }

    #[test]
    fn shift_arrows_still_select_log_lines_when_a_pane_has_focus() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        assert_eq!(app.focus, Focus::Pane);

        press_mod(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(app.active_view().unwrap().selection_count(), 2);
        assert!(app.sidebar_anchor.is_none());
    }

    /// Enter on a log source opens the source settings panel; Space is what selects.
    /// Showing one log alone is Space on the others to deselect them.
    #[test]
    fn enter_on_a_log_source_opens_the_source_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2; // b.log

        press(&mut app, KeyCode::Enter);
        let Mode::SourceEditor(editor) = &app.mode else {
            panic!("expected the source editor, got {:?}", app.mode);
        };
        assert_eq!(editor.short_name, "b");
        assert_eq!(editor.description, "");
        assert_eq!(editor.tag, "");
        assert_eq!(editor.schema, "Bracketed default");
    }

    #[test]
    fn enter_on_a_live_log_source_opens_the_live_source_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut project = Project::new(tmp.path());
        project.add_live_source(
            LiveSourceConfig {
                kind: LiveSourceKind::Journalctl,
                unit: "nginx.service".to_string(),
                tail: "100".to_string(),
                ..LiveSourceConfig::default()
            },
            "nginx journal",
            None,
        );
        let mut app = AppState::new(project);
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1;

        press(&mut app, KeyCode::Enter);
        let Mode::LiveSourceEditor(editor) = &app.mode else {
            panic!("expected the live source editor, got {:?}", app.mode);
        };
        assert_eq!(editor.kind, LiveSourceKind::Journalctl);
        assert_eq!(editor.short_name, "nginx journal");
        assert_eq!(editor.unit, "nginx.service");
        assert_eq!(editor.tail, "100");
    }

    #[test]
    fn changed_source_files_notify_and_refresh_with_r() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 2);
        let file_id = app.active_view().unwrap().file_id.clone();
        let log = tmp.path().join("many.log");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"2026-06-16 10:09:02.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:2] Distribution Service Trigger: 2 queued\n")
            .unwrap();

        app.last_source_check = Instant::now() - SOURCE_POLL_INTERVAL;
        app.check_source_modifications();

        assert!(app.dirty_sources.contains(&file_id));
        assert!(
            app.status.contains("press r to refresh"),
            "status was {}",
            app.status
        );

        press(&mut app, KeyCode::Char('r'));
        let file = app.project.get_file(&file_id).unwrap();
        assert_eq!(file.entries.len(), 3);
        assert!(!app.dirty_sources.contains(&file_id));
    }

    #[test]
    fn live_source_schema_row_routes_schema_shortcuts() {
        let tmp = tempfile::tempdir().unwrap();
        let mut project = Project::new(tmp.path());
        project.add_live_source(
            LiveSourceConfig {
                kind: LiveSourceKind::Journalctl,
                unit: "nginx.service".to_string(),
                ..LiveSourceConfig::default()
            },
            "nginx journal",
            None,
        );
        let mut app = AppState::new(project);
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1;

        press(&mut app, KeyCode::Enter);
        let Mode::LiveSourceEditor(mut editor) = app.mode.clone() else {
            panic!("expected live source editor, got {:?}", app.mode);
        };
        editor.focus_field(LiveSourceField::Schema);
        app.mode = Mode::LiveSourceEditor(editor.clone());
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.mode, Mode::Extractor(_)));

        app.mode = Mode::LiveSourceEditor(editor);
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::SaveSourceSchema { .. }));
    }

    #[test]
    fn source_editor_saves_name_description_tag_and_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1; // a.log
        let file_id = app.file_id_at(app.sidebar_selected).unwrap();

        press(&mut app, KeyCode::Enter);
        let Mode::SourceEditor(mut editor) = app.mode.clone() else {
            panic!("expected the source editor, got {:?}", app.mode);
        };
        editor.short_name = "access".to_string();
        editor.description = "front door requests".to_string();
        editor.tag = "access_log".to_string();
        editor.schema = "custom | <timestamp> <level>: <message> | %H:%M:%S | custom".to_string();
        app.mode = Mode::SourceEditor(editor);
        press(&mut app, KeyCode::Enter);
        app.finish_work();

        let file = app.project.get_file(&file_id).unwrap();
        assert_eq!(file.label, "access");
        assert_eq!(file.description, "front door requests");
        assert_eq!(file.tag, "access_log");
        assert_eq!(file.extractor_name, "custom");
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        assert!(saved.contains("\"tag\": \"access_log\""), "{saved}");
    }

    #[test]
    fn library_shortcuts_follow_the_selected_sidebar_item() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.focus = Focus::Sidebar;

        app.sidebar_selected = sidebar_row(&app, "file");
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::SaveSourceSchema { .. }));
        app.mode = Mode::Normal;

        app.submit_filter("message contains include queued".to_string());
        app.finish_work();
        app.sidebar_selected = sidebar_row(&app, "text");
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::ExportFilters(_)));
        app.mode = Mode::Normal;
        press(&mut app, KeyCode::Char('L'));
        assert!(matches!(app.mode, Mode::LoadFilters(_)));
        app.mode = Mode::Normal;

        app.submit_search("queued 3".to_string());
        app.finish_work();
        app.sidebar_selected = sidebar_row(&app, "search");
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::ExportSearches(_)));
        app.mode = Mode::Normal;
        press(&mut app, KeyCode::Char('L'));
        assert!(matches!(app.mode, Mode::ImportSearches(_)));
        app.mode = Mode::Normal;

        app.focus = Focus::Pane;
        press(&mut app, KeyCode::Char('m'));
        app.focus = Focus::Sidebar;
        app.sidebar_selected = sidebar_row(&app, "bookmark");
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::ExportBookmarks(_)));
        app.mode = Mode::Normal;
        press(&mut app, KeyCode::Char('L'));
        assert!(matches!(app.mode, Mode::ImportBookmarks(_)));
    }

    #[test]
    fn space_deselecting_a_merged_log_collapses_the_pane_to_the_other() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2; // b.log

        press(&mut app, KeyCode::Char(' ')); // merge a.log + b.log
        assert!(app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap()
            .is_merged());

        app.sidebar_selected = 1; // a.log
        press(&mut app, KeyCode::Char(' ')); // deselect it, leaving b.log alone

        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert!(!file.is_merged());
        assert_eq!(file.display_name, "b.log");
        assert!(app.project.files.iter().all(|file| !file.is_merged()));
    }

    #[test]
    fn a_view_always_keeps_at_least_one_log() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1; // a.log, the only log in the view

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.status, "a view needs at least one file");
        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert_eq!(file.display_name, "a.log");
    }

    #[test]
    fn removing_a_source_log_discards_the_merge_and_keeps_a_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2;
        press(&mut app, KeyCode::Char(' ')); // merge a.log + b.log
        assert!(app.project.files.iter().any(|file| file.is_merged()));

        // `d` on a merged pane has no file to remove.
        app.focus = Focus::Pane;
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.status, "select a file in the sidebar to remove");
        assert_eq!(
            app.project.files.iter().filter(|f| !f.is_merged()).count(),
            2
        );

        // From the sidebar it removes that log and drops the stale merge.
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2;
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.status, "file removed from project");
        assert!(app.project.files.iter().all(|file| !file.is_merged()));
        assert_eq!(app.panes.len(), 1);
        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert_eq!(file.display_name, "a.log");
    }

    #[test]
    fn a_schema_applies_to_one_file_and_leaves_the_other_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let simple_log = tmp.path().join("simple.log");
        std::fs::write(&simple_log, "10:00:01 WARN: disk almost full\n").unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.submit_add_file(simple_log.to_string_lossy().to_string())
            .unwrap();
        app.finish_work();

        // No schema explains the new file, so it lands on the catch-all: one entry per
        // line, and an empty level, because that format names no level field.
        let view_file = app.active_view().unwrap().file_id.clone();
        let file = app.project.get_file(&view_file).unwrap();
        assert_eq!(file.display_name, "simple.log");
        assert_eq!(file.extractor_name, GENERIC_EXTRACTOR_NAME);
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.level(&file.entries[0]), "");

        app.submit_extractor("simple | <timestamp> <level>: <message> | %H:%M:%S".to_string());
        app.finish_work();

        assert!(
            app.status
                .starts_with("schema 'simple' applied to simple.log"),
            "{}",
            app.status
        );
        let file = app.project.get_file(&view_file).unwrap();
        assert_eq!(file.extractor_name, "simple");
        assert_eq!(file.level(&file.entries[0]), "WARN");
        assert_eq!(file.message(&file.entries[0]), "disk almost full");

        // The other files keep the bracketed schema.
        let other = app
            .project
            .files
            .iter()
            .find(|f| f.display_name == "a.log")
            .unwrap();
        assert_eq!(other.extractor_name, "Bracketed default");
        assert_eq!(other.level(&other.entries[0]), "Trace");

        // And the per-file schema is persisted.
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        assert!(saved.contains("\"extractor_name\": \"simple\""), "{saved}");
    }

    #[test]
    fn source_schema_field_can_define_and_apply_a_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let simple_log = tmp.path().join("simple.log");
        std::fs::write(&simple_log, "10:00:01 WARN: disk almost full\n").unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.submit_add_file(simple_log.to_string_lossy().to_string())
            .unwrap();
        app.finish_work();
        let file_id = app.active_view().unwrap().file_id.clone();

        app.apply_schema_text_to_file(
            &file_id,
            "simple | <timestamp> <level>: <message> | %H:%M:%S | compact service log",
        );
        app.finish_work();

        assert!(
            app.status
                .starts_with("schema 'simple' applied to simple.log"),
            "{}",
            app.status
        );
        assert_eq!(
            app.project.extractors["simple"].description,
            "compact service log"
        );

        let file = app.project.get_file(&file_id).unwrap();
        assert_eq!(file.extractor_name, "simple");
        assert_eq!(file.level(&file.entries[0]), "WARN");
        assert_eq!(file.message(&file.entries[0]), "disk almost full");
        let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
        assert!(
            saved.contains("\"description\": \"compact service log\""),
            "{saved}"
        );
    }

    #[test]
    fn changing_a_schema_drops_merged_views_built_on_that_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());

        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2;
        press(&mut app, KeyCode::Char(' ')); // merge b.log into the a.log view
        assert!(app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap()
            .is_merged());

        // Re-parsing a.log makes the merge stale, so it must not survive.
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 1;
        app.submit_extractor("simple | <timestamp> <level>: <message> | %H:%M:%S".to_string());
        app.finish_work();

        assert!(app.project.files.iter().all(|file| !file.is_merged()));

        // The pane that showed the merge is repointed at the re-parsed file, not blank.
        assert_eq!(app.panes.len(), 1);
        let file = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert_eq!(file.display_name, "a.log");
        assert_eq!(file.extractor_name, "simple");
        assert!(!app.active_view().unwrap().visible.is_empty());
    }

    #[test]
    fn wrap_value_breaks_on_words() {
        assert_eq!(
            wrap_value("Failed to resolve inbox message", 12),
            vec!["Failed to", "resolve", "inbox", "message"]
        );
        assert!(wrap_value("Failed to resolve inbox message", 12)
            .iter()
            .all(|line| line.chars().count() <= 12));
    }

    #[test]
    fn wrap_value_hard_splits_tokens_longer_than_the_width() {
        let token = "A".repeat(25);
        assert_eq!(
            wrap_value(&token, 10),
            vec!["AAAAAAAAAA", "AAAAAAAAAA", "AAAAA"]
        );

        // A long token after a short word flushes the short word first.
        let wrapped = wrap_value(&format!("sid {}", "B".repeat(12)), 10);
        assert_eq!(wrapped, vec!["sid", "BBBBBBBBBB", "BB"]);
    }

    #[test]
    fn wrap_value_preserves_multiline_continuations() {
        let wrapped = wrap_value("header\n    at frame_one()\n    at frame_two()", 30);
        assert_eq!(wrapped, vec!["header", "at frame_one()", "at frame_two()"]);
    }

    #[test]
    fn wrap_value_keeps_every_character() {
        let text = "session 4A2F-99 failed /very/long/path/to/a/file/that/never/fits.cpp";
        let joined: String = wrap_value(text, 14).join(" ");
        for word in text.split_whitespace() {
            if word.chars().count() <= 14 {
                assert!(joined.contains(word), "lost word {word}");
            }
        }
        let stripped: String = wrap_value(text, 14).concat();
        assert_eq!(
            stripped.chars().filter(|c| !c.is_whitespace()).count(),
            text.chars().filter(|c| !c.is_whitespace()).count()
        );
    }

    #[test]
    fn detail_panel_height_yields_to_a_short_sidebar() {
        assert_eq!(detail_panel_height(13), 0);
        assert_eq!(detail_panel_height(14), 7);
        assert_eq!(detail_panel_height(40), 18);
    }

    #[test]
    fn detail_lines_wrap_a_long_message_under_its_label() {
        let extractor = crate::core::extractor::default_extractor();
        let mut file =
            LogFileModel::new("f1", "t.log", extractor.name.clone(), "", Some(extractor));
        file.load_from_lines([format!(
            "2026-06-16 10:09:43.288 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Error][UID:0][SID:0][OID:0][Disp.cpp:394] {}",
            "the quick brown fox jumps over the lazy dog again and again"
        )]);

        let lines = detail_lines(&file, &file.entries[0], 32);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        assert!(rendered
            .iter()
            .any(|line| line.starts_with("log_level      Error")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("file_name      Disp.cpp")));
        assert!(rendered
            .iter()
            .any(|line| line.starts_with("line_number    394")));

        let message_start = rendered
            .iter()
            .position(|l| l.starts_with("message"))
            .unwrap();
        // The message wrapped onto continuation rows indented under the label.
        assert!(rendered.len() > message_start + 1);
        assert!(rendered[message_start + 1].starts_with(&" ".repeat(DETAIL_LABEL_WIDTH + 1)));
        let message_text = rendered[message_start..].join(" ");
        let normalized = message_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(normalized.contains("the quick brown fox"));
    }

    #[test]
    fn pretty_print_message_formats_json_xml_and_sql() {
        let (kind, body) = pretty_print_message(r#"payload={"ok":true,"items":[1,2]}"#).unwrap();
        assert_eq!(kind, "JSON");
        assert!(body.contains("\"items\": ["), "{body}");

        let (kind, body) = pretty_print_message("<root><item id=\"1\">x</item></root>").unwrap();
        assert_eq!(kind, "XML");
        assert!(body.contains("<root>"), "{body}");
        assert!(body.contains("  <item id=\"1\">"), "{body}");

        let (kind, body) =
            pretty_print_message("query=select a,b from users where id = 1 order by a").unwrap();
        assert_eq!(kind, "SQL");
        assert!(body.contains("select a,"), "{body}");
        assert!(body.contains("from users"), "{body}");
        assert!(body.contains("where id = 1"), "{body}");

        assert!(pretty_print_message("plain text only").is_none());
    }

    // ---- session restore -------------------------------------------------------

    fn two_log_app(root: &std::path::Path) -> AppState {
        for (name, module) in [("a.log", "Kernel"), ("b.log", "SQL")] {
            let body: String = (0..4)
                .map(|i| {
                    format!(
                        "2026-06-16 10:09:{:02}.000 [HOST:h][SERVER:S][PID:5][THR:9][{module}][Trace][UID:0][SID:0][OID:0][D.cpp:{i}] queued {i}\n",
                        i * 2
                    )
                })
                .collect();
            std::fs::write(root.join(name), body).unwrap();
        }
        let mut project = Project::load(root);
        project.add_file(root.join("a.log"), None);
        project.add_file(root.join("b.log"), None);
        let mut app = AppState::new(project);
        app.queue_initial_loads();
        app.finish_work();
        app
    }

    #[test]
    fn session_restores_the_open_file_and_its_search() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_log_app(tmp.path());

        // Look at b.log, not the first file, and search inside it.
        let b = app.project.files[1].file_id.clone();
        app.open_file_in_focused(&b);
        app.finish_work();
        app.submit_search("queued 2".to_string());
        app.finish_work();

        let app = reopen(app);

        assert_eq!(app.panes.len(), 1);
        let view = app.active_view().unwrap();
        assert_eq!(view.query_text, "queued 2");
        assert!(
            view.query.is_some(),
            "the query must be recompiled, not just stored"
        );
        let file = app.project.get_file(&view.file_id).unwrap();
        assert_eq!(
            file.display_name, "b.log",
            "reopened on the last file, not the first"
        );
    }

    #[test]
    fn session_restores_the_split_and_focused_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_log_app(tmp.path());

        press(&mut app, KeyCode::Char('-')); // vertical split
        assert_eq!(app.panes.len(), 2);
        app.focused_pane = 1;

        let app = reopen(app);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.focused_pane, 1);
        assert_eq!(app.split_mode, SplitMode::Vertical);
    }

    /// A merged pane cannot be rebuilt until its files load, so it is restored late.
    #[test]
    fn session_restores_a_merged_pane_after_its_files_load() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_log_app(tmp.path());

        // Space on the second sidebar file merges it into the pane.
        app.focus = Focus::Sidebar;
        app.sidebar_selected = app.file_item_indices()[1];
        press(&mut app, KeyCode::Char(' '));
        let merged = app
            .project
            .get_file(&app.active_view().unwrap().file_id)
            .unwrap();
        assert!(merged.is_merged());
        assert_eq!(merged.entries.len(), 8);

        let app = reopen(app);

        let view = app.active_view().unwrap();
        let file = app.project.get_file(&view.file_id).unwrap();
        assert!(
            file.is_merged(),
            "the merge must come back, not just its first file"
        );
        assert_eq!(file.merged_from.len(), 2);
        assert_eq!(file.entries.len(), 8);
        assert!(
            app.pending_merges.is_empty(),
            "the deferred merge must have been drained"
        );
    }

    /// A file deleted between sessions must not strand the pane or panic.
    #[test]
    fn session_skips_files_that_vanished() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_log_app(tmp.path());
        let b = app.project.files[1].file_id.clone();
        app.open_file_in_focused(&b);
        app.finish_work();
        app.capture_session();
        app.project.save().unwrap();

        // b.log disappears from the project.json before the next open.
        let mut project = Project::load(tmp.path());
        project.remove_file(&b);
        project.save().unwrap();

        let mut app = AppState::new(Project::load(tmp.path()));
        app.queue_initial_loads();
        app.finish_work();

        assert_eq!(app.panes.len(), 1, "falls back to the remaining file");
        let view = app.active_view().unwrap();
        let file = app.project.get_file(&view.file_id).unwrap();
        assert_eq!(file.display_name, "a.log");
    }

    // ---- unified Space / Enter -------------------------------------------------

    /// Space on a sidebar filter enables/disables it rather than deleting it, and the
    /// panes refilter immediately.
    #[test]
    fn space_on_a_filter_toggles_it_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_filter("message contains exclude queued".to_string());
        app.finish_work();
        assert_eq!(app.active_view().unwrap().visible.len(), 0);

        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Filter { .. }))
            .unwrap();

        press(&mut app, KeyCode::Char(' '));
        assert!(!app.project.filters.rules[0].enabled);
        assert_eq!(app.status, "filter disabled");
        assert_eq!(
            app.active_view().unwrap().visible.len(),
            5,
            "rows come back"
        );

        press(&mut app, KeyCode::Char(' '));
        assert!(app.project.filters.rules[0].enabled);
        assert_eq!(app.active_view().unwrap().visible.len(), 0);
    }

    /// Enter on a filter opens an editor prefilled with text that parses back to the same
    /// rule, and the edit lands in place instead of appending a second rule.
    #[test]
    fn enter_on_a_filter_edits_it_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_filter("message contains exclude queued".to_string());
        app.finish_work();

        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Filter { .. }))
            .unwrap();
        press(&mut app, KeyCode::Enter);

        // Enter opens the guided builder, prefilled from the rule and targeting its slot.
        let Mode::FilterBuilder(builder) = app.mode.clone() else {
            panic!("expected the filter builder, got {:?}", app.mode);
        };
        assert_eq!(builder.edit_index, Some(0));
        assert_eq!(builder.field, "message");
        assert_eq!(builder.op_name(), "contains");
        assert_eq!(builder.value, "queued");
        assert!(builder.exclude);

        // Retarget the rule at a value no line has.
        app.submit_edit_filter(0, "message contains exclude nomatch".to_string());
        app.finish_work();
        assert_eq!(app.project.filters.rules.len(), 1, "edited, not appended");
        assert_eq!(app.project.filters.rules[0].value, "nomatch");
        assert_eq!(app.active_view().unwrap().visible.len(), 5);
    }

    /// Editing a disabled rule's text must not quietly switch it back on.
    #[test]
    fn editing_a_disabled_filter_keeps_it_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_filter("message contains exclude queued".to_string());
        app.toggle_filter_enabled(0);
        app.finish_work();
        assert!(!app.project.filters.rules[0].enabled);

        app.submit_edit_filter(0, "message contains exclude other".to_string());
        app.finish_work();
        assert_eq!(app.project.filters.rules[0].value, "other");
        assert!(!app.project.filters.rules[0].enabled);
    }

    /// A rule with spaces and a schema scope must survive describe -> edit -> parse.
    #[test]
    fn filter_edit_text_round_trips_through_the_parser() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with_lines(tmp.path(), 1);
        let rule = FilterRule::new("message", "contains", "two words", "include")
            .for_log_schema("Bracketed default");

        let parsed = app.parse_filter_rule(&rule.to_input()).unwrap();
        assert_eq!(parsed, rule, "to_input must parse back to an equal rule");
    }

    #[test]
    fn space_on_a_saved_search_applies_then_clears_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_search("queued 3".to_string());
        app.finish_work();
        app.clear_search();
        app.finish_work();
        assert_eq!(app.active_view().unwrap().query_text, "");

        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Search { .. }))
            .unwrap();

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.active_view().unwrap().query_text, "queued 3");

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.active_view().unwrap().query_text,
            "",
            "space again clears it"
        );
    }

    /// The applied search is starred, the way a shown file and an enabled filter are.
    #[test]
    fn the_applied_saved_search_is_starred() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_search("queued 3".to_string());
        app.finish_work();

        let starred = app.sidebar_items().into_iter().find_map(|item| match item {
            SidebarItem::Search { label, .. } => Some(label),
            _ => None,
        });
        assert_eq!(starred.as_deref(), Some("* /queued 3"));
    }

    #[test]
    fn enter_on_a_saved_search_edits_and_runs_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.submit_search("queued 3".to_string());
        app.finish_work();

        app.focus = Focus::Sidebar;
        app.sidebar_selected = app
            .sidebar_items()
            .iter()
            .position(|item| matches!(item, SidebarItem::Search { .. }))
            .unwrap();
        press(&mut app, KeyCode::Enter);

        let Mode::EditSearch { index, text } = app.mode.clone() else {
            panic!("expected the search editor, got {:?}", app.mode);
        };
        assert_eq!((index, text.as_str()), (0, "queued 3"));

        app.submit_edit_search(0, "queued 4".to_string());
        app.finish_work();
        assert_eq!(app.active_view().unwrap().query_text, "queued 4");
        assert!(
            app.project.saved_searches.contains(&"queued 4".to_string()),
            "{:?}",
            app.project.saved_searches
        );
    }

    #[test]
    fn saved_search_library_exports_imports_and_is_in_the_palette() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        app.project.saved_searches = vec!["level=Error".to_string(), "queued 4".to_string()];

        assert_eq!(app.default_search_folder_input(), "~/.log-scouter/searches");
        assert!(app.palette_commands().contains(&Command::ImportSearches));
        assert!(app.palette_commands().contains(&Command::ExportSearches));

        let folder = tmp.path().join("search-library");
        app.submit_export_searches(folder.to_string_lossy().to_string());
        assert_eq!(
            app.status,
            format!("exported 2 saved search(es) to {}", folder.display())
        );

        let other = tempfile::tempdir().unwrap();
        let mut target = app_with_lines(other.path(), 5);
        target.submit_import_searches(folder.to_string_lossy().to_string());
        assert_eq!(
            target.project.saved_searches,
            vec!["level=Error".to_string(), "queued 4".to_string()]
        );
        assert!(
            target.status.starts_with("loaded 2 saved search(es)"),
            "{}",
            target.status
        );
    }

    /// Space in a pane still means "add this log line to the selection".
    #[test]
    fn space_in_a_pane_still_selects_the_log_line() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 5);
        assert_eq!(app.focus, Focus::Pane);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.active_view().unwrap().selection_count(), 1);
    }

    // ---- schema packs ----------------------------------------------------------

    #[test]
    fn schema_export_and_import_default_to_the_user_level_library() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with_log(tmp.path());

        let expected = user_schema_dir().unwrap();
        assert_eq!(app.default_schema_folder_path(), expected);
        assert_eq!(app.default_schema_folder_input(), "~/.log-scouter/schemas");
        assert_eq!(
            app.schema_folder_from_input(&app.default_schema_folder_input()),
            expected
        );
        // Relative input resolves inside the project, like the filter packs.
        assert_eq!(
            app.schema_folder_from_input(".logscouter/schemas"),
            tmp.path().join(".logscouter/schemas")
        );
    }

    /// `X` writes one JSON per schema; `I` merges a folder back into another project.
    #[test]
    fn x_exports_schemas_and_i_imports_them_into_another_project() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        // Define a second schema so the export is not just the built-in default.
        app.project
            .add_extractor(
                Extractor::with_timestamp_format(
                    "compact",
                    "<timestamp> <level>: <message>",
                    "%H:%M:%S",
                )
                .unwrap()
                .with_description("small"),
            )
            .unwrap();
        // The two built-ins, plus `compact`.
        assert_eq!(app.project.extractors.len(), 3);

        let folder = tmp.path().join("pack");
        app.dispatch_command(Command::ExportSchemas).unwrap();
        assert!(matches!(app.mode, Mode::ExportSchemas(_)));
        clear_input(&mut app);
        type_text(&mut app, folder.to_str().unwrap());
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.status,
            format!("exported 3 log schema(s) to {}", folder.display())
        );

        // A fresh project starts with only the built-in schemas.
        let other = tempfile::tempdir().unwrap();
        let mut app2 = app_with_log(other.path());
        assert_eq!(app2.project.extractors.len(), 2);

        app2.dispatch_command(Command::ImportSchemas).unwrap();
        assert!(matches!(app2.mode, Mode::ImportSchemas(_)));
        clear_input(&mut app2);
        type_text(&mut app2, folder.to_str().unwrap());
        press(&mut app2, KeyCode::Enter);

        // Both built-ins already existed, so only `compact` is new.
        assert_eq!(
            app2.status,
            "imported 1 log schema(s), skipped 2 already in this project"
        );
        assert!(app2.project.extractors.contains_key("compact"));

        // And it is usable straight away: assign it to the open file.
        let saved = std::fs::read_to_string(other.path().join(".logscouter/project.json")).unwrap();
        assert!(saved.contains("compact"), "{saved}");
    }

    /// Importing must never silently repoint files onto a different parse.
    #[test]
    fn importing_an_existing_schema_name_leaves_the_project_one_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("pack");
        let hostile = Extractor::new("Bracketed default", "<message>").unwrap();
        export_schemas_to_folder(&[hostile], &folder).unwrap();

        let mut app = app_with_log(tmp.path());
        let before = app.project.extractors["Bracketed default"].format.clone();

        app.dispatch_command(Command::ImportSchemas).unwrap();
        clear_input(&mut app);
        type_text(&mut app, folder.to_str().unwrap());
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            app.status,
            "imported 0 log schema(s), skipped 1 already in this project"
        );
        assert_eq!(app.project.extractors["Bracketed default"].format, before);
    }

    #[test]
    fn importing_schemas_from_a_missing_folder_reports_instead_of_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_log(tmp.path());

        app.dispatch_command(Command::ImportSchemas).unwrap();
        clear_input(&mut app);
        type_text(&mut app, tmp.path().join("nope").to_str().unwrap());
        press(&mut app, KeyCode::Enter);

        assert!(
            app.status.starts_with("no schema folder:"),
            "{}",
            app.status
        );
    }

    // ---- substring selection ---------------------------------------------------

    /// Column offsets into a rendered row, from `row_line`:
    /// `   ` markers(3) + timestamp(23) + ` ` + module(14) + ` ` + level(8) + ` ` + message
    const COL_TIMESTAMP: u16 = 3;
    const COL_MODULE: u16 = 27;
    const COL_MESSAGE: u16 = 51;

    fn drag_within_row(app: &mut AppState, row: u16, from: u16, to: u16) {
        let pane = app.pane_areas[0];
        mouse(
            app,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + from,
            pane.y + row,
            KeyModifiers::NONE,
        );
        mouse(
            app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + to,
            pane.y + row,
            KeyModifiers::NONE,
        );
        mouse(
            app,
            MouseEventKind::Up(MouseButton::Left),
            pane.x + to,
            pane.y + row,
            KeyModifiers::NONE,
        );
    }

    #[test]
    fn dragging_inside_one_row_selects_a_substring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        // "Kernel" spans columns 27..=32.
        drag_within_row(&mut app, 3, COL_MODULE, COL_MODULE + 5);

        assert_eq!(app.selected_substring().as_deref(), Some("Kernel"));
        assert_eq!(app.status, "6 char(s) selected");
        // No rows were selected: this was a text gesture, not a row gesture.
        assert_eq!(selected_line_numbers(&app), Vec::<usize>::new());
    }

    #[test]
    fn a_substring_can_be_dragged_right_to_left() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        drag_within_row(&mut app, 3, COL_MODULE + 5, COL_MODULE);
        assert_eq!(app.selected_substring().as_deref(), Some("Kernel"));
    }

    #[test]
    fn y_copies_the_substring_rather_than_the_whole_line() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        drag_within_row(&mut app, 3, COL_MESSAGE, COL_MESSAGE + 11); // "Distribution"
        assert_eq!(app.selected_substring().as_deref(), Some("Distribution"));

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.status, "copied 12 char(s)");
    }

    #[test]
    fn right_click_copies_the_substring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);
        let pane = app.pane_areas[0];

        drag_within_row(&mut app, 3, COL_MODULE, COL_MODULE + 5);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            pane.x + 1,
            pane.y + 7, // a different row entirely
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.status, "copied 6 char(s)",
            "the substring wins over the clicked row"
        );
    }

    /// The marker columns are not text; a drag from the left edge starts at the timestamp.
    #[test]
    fn the_cursor_and_mark_columns_are_not_selectable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        drag_within_row(&mut app, 3, 0, COL_TIMESTAMP + 3);
        assert_eq!(app.selected_substring().as_deref(), Some("2026"));
    }

    /// Dragging past the right edge saturates at the last *visible* character instead of
    /// dropping the selection. It deliberately does not reach text scrolled off-screen:
    /// the user would be selecting something they cannot see.
    #[test]
    fn dragging_past_the_right_edge_clamps_to_the_last_visible_character() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);
        let pane = app.pane_areas[0];

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + COL_MESSAGE,
            pane.y + 3,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.right() + 40, // far outside the pane
            pane.y + 3,
            KeyModifiers::NONE,
        );
        let selected = app.selected_substring().unwrap();
        assert!(selected.starts_with("Distribution"), "{selected:?}");

        let last_visible = app.active_view().unwrap().scroll_x + pane.width as usize - 1;
        assert_eq!(app.text_selection.unwrap().cursor, last_visible);
        assert!(
            !selected.ends_with("queued"),
            "must not reach past the pane edge: {selected:?}"
        );
    }

    /// Leaving the row mid-drag abandons the substring and selects rows instead.
    #[test]
    fn a_drag_that_leaves_the_row_becomes_a_row_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);
        let pane = app.pane_areas[0];

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + COL_MODULE,
            pane.y + 2,
            KeyModifiers::NONE,
        );
        // Move within the row first: a substring appears.
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + COL_MODULE + 5,
            pane.y + 2,
            KeyModifiers::NONE,
        );
        assert!(app.text_selection.is_some());

        // Then wander down a row: it turns into a row selection anchored where we pressed.
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + COL_MODULE + 5,
            pane.y + 4,
            KeyModifiers::NONE,
        );
        assert!(app.text_selection.is_none(), "substring abandoned");
        assert_eq!(selected_line_numbers(&app), vec![3, 4, 5]);
    }

    #[test]
    fn esc_and_space_clear_a_substring_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        drag_within_row(&mut app, 3, COL_MODULE, COL_MODULE + 5);
        assert!(app.text_selection.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.text_selection.is_none());
        assert_eq!(app.status, "selection cleared");

        drag_within_row(&mut app, 3, COL_MODULE, COL_MODULE + 5);
        press(&mut app, KeyCode::Char(' '));
        assert!(app.text_selection.is_none(), "Space selects whole lines");
        assert_eq!(app.active_view().unwrap().selection_count(), 1);
    }

    /// A horizontally scrolled row keeps its selection: offsets are absolute, and the
    /// highlight is mapped through `scroll_x` at draw time.
    #[test]
    fn a_substring_survives_horizontal_scrolling() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 10);
        render(&mut app, 120, 30);

        drag_within_row(&mut app, 3, COL_MESSAGE, COL_MESSAGE + 11);
        press(&mut app, KeyCode::Char('l')); // scroll right by 8
        assert_eq!(app.active_view().unwrap().scroll_x, 8);
        assert_eq!(app.selected_substring().as_deref(), Some("Distribution"));
    }

    #[test]
    fn highlighted_row_maps_offsets_through_the_horizontal_scroll() {
        let base = Style::default();
        // "abcdefgh" scrolled by 2 renders "cdefgh"; selecting absolute 4..=5 ("ef")
        // must land on visible offsets 2..=3.
        let line = highlighted_row("cdefgh", 4, 5, 2, base);
        let texts: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["cd", "ef", "gh"]);

        // A run entirely scrolled off leaves the row unstyled.
        let line = highlighted_row("cdefgh", 0, 1, 2, base);
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn query_highlight_ranges_cover_every_visible_occurrence() {
        let query = compile_query("queued");
        let line = "queued then QUEUED";
        let ranges = query_highlight_ranges(Some(&query), line);
        assert_eq!(ranges, vec![(0, 5), (12, 17)]);

        let highlighted = highlighted_ranges(
            line,
            &ranges,
            0,
            Style::default(),
            Theme::new(ThemeName::Classic).search_hit(),
        );
        let texts: Vec<String> = highlighted
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(texts, vec!["queued", " then ", "QUEUED"]);
    }

    // ---- elapsed time from a mark ----------------------------------------------

    fn ms(n: i64) -> ChronoDuration {
        ChronoDuration::milliseconds(n)
    }

    #[test]
    fn elapsed_is_formatted_at_a_human_scale() {
        assert_eq!(format_elapsed(ms(0)), "+0ms");
        assert_eq!(format_elapsed(ms(7)), "+7ms");
        assert_eq!(format_elapsed(ms(-250)), "-250ms");
        assert_eq!(format_elapsed(ms(1234)), "+1.234s");
        assert_eq!(format_elapsed(ms(-59_999)), "-59.999s");
        assert_eq!(format_elapsed(ms(62_500)), "+1m02.500s");
        assert_eq!(format_elapsed(ms(3_723_000)), "+1h02m03s");
        assert_eq!(format_elapsed(ms(-3_723_000)), "-1h02m03s");
        // 93,784,000ms = 1d 2h 3m 4s; sub-minute precision is dropped at this scale.
        assert_eq!(format_elapsed(ms(93_784_000)), "+1d02h03m");
    }

    /// The timestamp column occupies chars 3..26 of a rendered row.
    fn time_column(app: &AppState, position: usize) -> String {
        app.pane_row_line(0, position)
            .unwrap()
            .chars()
            .skip(ROW_MARKER_WIDTH)
            .take(23)
            .collect::<String>()
            .trim()
            .to_string()
    }

    /// `app_with_lines` writes one entry per second, so offsets are whole seconds.
    #[test]
    fn t_marks_the_cursor_line_and_shows_offsets_from_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);
        render(&mut app, 120, 20);

        press(&mut app, KeyCode::Char('j')); // cursor -> row 1 (10:09:01)
        press(&mut app, KeyCode::Char('j')); // cursor -> row 2 (10:09:02)
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.status, "elapsed time from line 3");

        assert_eq!(time_column(&app, 0), "-2.000s");
        assert_eq!(time_column(&app, 1), "-1.000s");
        assert_eq!(time_column(&app, 2), "+0ms", "the mark itself");
        assert_eq!(time_column(&app, 3), "+1.000s");
        assert_eq!(time_column(&app, 5), "+3.000s");

        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("elapsed from line 3"), "{screen}");
    }

    #[test]
    fn t_again_restores_absolute_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 4);
        render(&mut app, 120, 20);

        press(&mut app, KeyCode::Char('T'));
        assert!(app.elapsed_mark.is_some());
        assert_eq!(time_column(&app, 1), "+1.000s");

        press(&mut app, KeyCode::Char('T'));
        assert!(app.elapsed_mark.is_none());
        assert_eq!(app.status, "elapsed time off");
        assert_eq!(time_column(&app, 1), "2026-06-16 10:09:01.000");
    }

    /// The elapsed column is padded to the timestamp column's width, so the fields behind
    /// it never shift. Substring selection offsets depend on this.
    #[test]
    fn elapsed_mode_does_not_move_the_other_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_lines(tmp.path(), 6);
        render(&mut app, 120, 20);

        let before = app.pane_row_line(0, 3).unwrap();
        press(&mut app, KeyCode::Char('T'));
        let after = app.pane_row_line(0, 3).unwrap();

        assert_eq!(
            before.chars().count(),
            after.chars().count(),
            "row width changed:\n{before}\n{after}"
        );
        for column in [COL_MODULE as usize, COL_MESSAGE as usize] {
            assert_eq!(
                before.chars().nth(column),
                after.chars().nth(column),
                "column {column} moved"
            );
        }
        // And a substring drag still lands on the module name.
        drag_within_row(&mut app, 3, COL_MODULE, COL_MODULE + 5);
        assert_eq!(app.selected_substring().as_deref(), Some("Kernel"));
    }

    /// An entry whose first line carries no timestamp has no offset to show.
    #[test]
    fn an_entry_without_a_timestamp_shows_a_dash() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("banner.log");
        std::fs::write(
            &log,
            "==== server starting ====\n\
             2026-06-16 10:09:02.000 [HOST:h][SERVER:S][PID:5][THR:9][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:2] second\n",
        )
        .unwrap();
        let mut app = boot(Project::load(tmp.path()), &log);
        render(&mut app, 120, 20);
        assert_eq!(app.active_view().unwrap().visible.len(), 2);

        press(&mut app, KeyCode::Char('j')); // onto the line that has a timestamp
        press(&mut app, KeyCode::Char('T'));

        assert_eq!(
            time_column(&app, 0),
            "-",
            "the banner has no time of its own"
        );
        assert_eq!(time_column(&app, 1), "+0ms");
    }

    /// The mark belongs to one file: a pane on another log keeps real timestamps.
    #[test]
    fn the_mark_only_applies_to_its_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        render(&mut app, 140, 20);

        press(&mut app, KeyCode::Char('T'));
        let marked_file = app.elapsed_mark.as_ref().unwrap().file_id.clone();
        assert!(app.elapsed_from(&marked_file).is_some());
        assert!(app.elapsed_from("no-such-file").is_none());
        assert_eq!(time_column(&app, 1), "+2.000s");

        // Point the pane at the other log; timestamps come back.
        let other = app
            .project
            .files
            .iter()
            .map(|file| file.file_id.clone())
            .find(|id| *id != marked_file)
            .unwrap();
        app.open_file_in_focused(&other);
        app.finish_work();
        assert!(app.elapsed_from(&other).is_none());
        assert_eq!(time_column(&app, 0), "2026-06-16 10:00:02.000");
    }

    #[test]
    fn t_on_a_line_with_no_timestamp_reports_instead_of_marking() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("banner.log");
        std::fs::write(&log, "not a log line at all\n").unwrap();
        let mut app = boot(Project::load(tmp.path()), &log);
        render(&mut app, 120, 20);

        press(&mut app, KeyCode::Char('T'));
        assert!(app.elapsed_mark.is_none());
        assert_eq!(app.status, "this line has no timestamp to measure from");
    }
}
