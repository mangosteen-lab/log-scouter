use crate::core::extractor::{
    export_schemas_to_folder, load_schemas_from_folder, user_schema_dir, Extractor,
    DEFAULT_TIMESTAMP_FORMAT, BRACKETED_DEFAULT_FORMAT, USER_SCHEMAS_SUBDIR,
};
use crate::core::filters::{
    common_message_pattern, expand_tilde, export_filters_to_folder, hide_like,
    load_filters_from_folder, user_filter_dir, FilterRule, FilterSet, USER_DIR,
    USER_FILTERS_SUBDIR,
};
use crate::core::models::{apply_context, LogEntry, LogFileModel, ViewModel, VisibleIndices};
use crate::core::parser::{self, EntryBuilder};
use crate::core::project::{PaneSession, Project, Session, CONFIG_DIR};
use crate::core::search::{compile_query, parse_datetime, Query};
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
use std::collections::{HashSet, VecDeque};
use std::io::{self, BufRead, Stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CONTEXT_CYCLE: &[usize] = &[0, 3, 10];
const DETAIL_LABEL_WIDTH: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Pane,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone)]
struct PaneState {
    view: ViewModel,
}

#[derive(Debug, Clone)]
enum Mode {
    Normal,
    Search(String),
    AddFile(String),
    Filter(String),
    TimePicker(TimePicker),
    ExportFilters(String),
    LoadFilters(String),
    ExportSchemas(String),
    ImportSchemas(String),
    Extractor(String),
    LogSchema(String),
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
    HideChoice,
    HidePattern(String),
    EntryDetail {
        scroll: usize,
    },
    Help,
}

#[derive(Debug, Clone)]
enum SidebarItem {
    Section(String),
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
    /// `index` addresses `project.saved_searches`.
    Search {
        index: usize,
        text: String,
        label: String,
    },
    Hint(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailSurface {
    Inline,
    Popup,
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

    /// "Last 1 hour" counts back from the newest entry in the log, not from wall-clock
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

    fn move_row(&mut self, delta: isize) {
        self.row = (self.row as isize + delta).rem_euclid(Self::ROWS as isize) as usize;
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

struct RecomputeJob {
    pane: usize,
    after: After,
    stage: Stage,
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
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if app.handle_key(key)? {
                        break;
                    }
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse),
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
    /// Char index of the edit caret within the active input popup.
    input_cursor: usize,
    /// Panes of the restored session that want a merge. A merge interleaves entries by
    /// timestamp, so it cannot be rebuilt until those files have finished loading;
    /// `finish_load` drains this once they have.
    pending_merges: Vec<(usize, Vec<String>)>,
}

impl AppState {
    fn new(project: Project) -> Self {
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
            results_selected: 0,
            results_scroll: 0,
            results_area: Rect::default(),
            sidebar_area: Rect::default(),
            pane_areas: Vec::new(),
            detail_area: Rect::default(),
            entry_detail_area: Rect::default(),
            mouse_drag: None,
            detail_selection: None,
            text_selection: None,
            elapsed_mark: None,
            sidebar_anchor: None,
            work: VecDeque::new(),
            progress: None,
            input_cursor: 0,
            pending_merges: Vec::new(),
        };
        app.restore_session();
        app
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
        });
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

        // Views built before the entries arrived hold an empty, stale result.
        for pane in 0..self.panes.len() {
            if self.panes[pane].view.file_id == file_id {
                self.queue_recompute(pane, After::Nothing);
            }
        }
        // A restored merged pane has been waiting for exactly this.
        self.apply_pending_merges();
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
    }

    fn queue_load(&mut self, file_id: &str) {
        let Some(file) = self.project.get_file(file_id) else {
            return;
        };
        let path = file.path.clone();
        let display_name = file.display_name.clone();
        let extractor = file.extractor.clone();
        let total = parser::file_size(&path);

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
                if let Some(file) = self.project.get_file_mut(file_id) {
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
        let show_results = self.search_results_visible();
        let result_height = root.height.saturating_sub(4).clamp(3, 8);
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
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width(rows[1].width)),
                Constraint::Min(1),
            ])
            .split(rows[1]);
        let detail_height = detail_panel_height(body[0].height);
        if detail_height > 0 {
            let column = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(detail_height)])
                .split(body[0]);
            self.draw_sidebar(frame, column[0]);
            self.draw_detail(frame, column[1]);
        } else {
            self.detail_area = Rect::default();
            self.draw_sidebar(frame, body[0]);
        }
        self.draw_panes(frame, body[1]);
        if show_results {
            self.draw_search_results(frame, rows[2]);
            self.draw_status(frame, rows[3]);
        } else {
            self.results_area = Rect::default();
            self.draw_status(frame, rows[2]);
        }
        self.draw_mode(frame, root);
        self.draw_progress(frame, root);
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
                .gauge_style(Style::default().fg(Color::Cyan))
                .ratio(ratio)
                .label(format!("{percent}%")),
            rows[0],
        );
        if rows[1].height > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Esc to cancel",
                    Style::default().fg(Color::DarkGray),
                )),
                rows[1],
            );
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let text = format!(
            " Log Scouter  {}  q quit  / search  f filter  t time  H hide  y copy  ? help ",
            self.project.root.display()
        );
        frame.render_widget(
            Paragraph::new(text).style(Style::default().bg(Color::Blue).fg(Color::White)),
            area,
        );
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
            Paragraph::new(format!(" {status}"))
                .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
            area,
        );
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let items = self.sidebar_items();
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
                    SidebarItem::Section(label) => (
                        label.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    SidebarItem::File { label, .. } => (format!("  {label}"), Style::default()),
                    SidebarItem::Filter { label, .. } => {
                        (format!("  {label}"), Style::default().fg(Color::Yellow))
                    }
                    SidebarItem::Search { label, .. } => {
                        (format!("  {label}"), Style::default().fg(Color::Green))
                    }
                    SidebarItem::Hint(label) => {
                        (format!("  {label}"), Style::default().fg(Color::DarkGray))
                    }
                };
                let style = if selected {
                    style.bg(Color::Gray).fg(Color::Black)
                } else {
                    style
                };
                ListItem::new(Line::from(Span::styled(
                    truncate_label(&label, label_width),
                    style,
                )))
            })
            .collect();

        let border_style = if self.focus == Focus::Sidebar {
            Style::default().fg(Color::Cyan)
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
    fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title("Detail")
            .borders(Borders::ALL)
            .border_style(Style::default());
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
            SidebarItem::Search { text, .. } => Some(search_detail_lines(text, width)),
            SidebarItem::Section(label) => Some(label_detail_lines("section", label, width)),
            SidebarItem::Hint(label) => Some(label_detail_lines("hint", label, width)),
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
        let direction = match self.split_mode {
            SplitMode::Horizontal => Direction::Horizontal,
            SplitMode::Vertical => Direction::Vertical,
        };
        let constraints: Vec<Constraint> = (0..pane_count)
            .map(|_| Constraint::Ratio(1, pane_count as u32))
            .collect();
        let areas = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(area);
        self.pane_areas.clear();

        for index in 0..pane_count {
            self.draw_pane(frame, areas[index], index);
        }
    }

    fn draw_pane(&mut self, frame: &mut Frame, area: Rect, pane_index: usize) {
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
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        if self.pane_areas.len() <= pane_index {
            self.pane_areas.resize(pane_index + 1, Rect::default());
        }
        self.pane_areas[pane_index] = inner;
        frame.render_widget(block, area);

        let row_height = inner.height as usize;
        if row_height == 0 {
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
            let raw_line = row_line(file, entry, at_cursor, picked, matched, elapsed_from);
            let line = crop(&raw_line, view.scroll_x, inner.width as usize);
            let style = match (picked, at_cursor) {
                (true, true) => Style::default().bg(Color::LightBlue).fg(Color::Black),
                (true, false) => Style::default().bg(Color::Blue).fg(Color::White),
                (false, true) => Style::default().bg(Color::DarkGray),
                (false, false) if matched => Style::default().fg(Color::Yellow),
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
                None => Line::from(Span::styled(line, style)),
            }));
        }

        frame.render_widget(List::new(rows), inner);
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
                Style::default().fg(Color::Cyan)
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
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Yellow)
            };
            rows.push(ListItem::new(Line::from(Span::styled(
                crop(&raw_line, 0, inner.width as usize),
                style,
            ))));
        }

        frame.render_widget(List::new(rows), inner);
    }

    fn draw_mode(&mut self, frame: &mut Frame, root: Rect) {
        self.entry_detail_area = Rect::default();
        match self.mode.clone() {
            Mode::Normal => {}
            Mode::Search(text) => self.draw_input_popup(
                frame,
                root,
                "Search",
                "text | \"phrase\" | /regex/ | field=value | after:<ts>",
                &text,
            ),
            Mode::AddFile(text) => {
                self.draw_input_popup(frame, root, "Add File", "Type a path and press Enter", &text)
            }
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
            Mode::Extractor(text) => self.draw_input_popup(
                frame,
                root,
                "Apply Log Schema",
                "schema name OR name | format template | [timestamp strptime format] | [description]",
                &text,
            ),
            Mode::LogSchema(text) => self.draw_input_popup(
                frame,
                root,
                "New Log Schema",
                "name | format template | [timestamp strptime format] | [description]",
                &text,
            ),
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
            Mode::HideChoice => {
                let choices = self.hide_choice_fields();
                let height = (choices.len() as u16 + 5).clamp(7, root.height.max(7));
                let area = centered_rect(64, height, root);
                frame.render_widget(Clear, area);
                let mut lines = vec![
                    Line::from("Hide logs where this field has the current value"),
                    Line::from(""),
                ];
                for (index, field) in choices.iter().enumerate() {
                    let Some(key) = hide_choice_key(index) else {
                        break;
                    };
                    lines.push(Line::from(format!("{key}  {field}")));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Esc cancel",
                    Style::default().fg(Color::DarkGray),
                )));
                frame.render_widget(
                    Paragraph::new(Text::from(lines))
                        .block(Block::default().title("Hide").borders(Borders::ALL))
                        .wrap(Wrap { trim: false }),
                    area,
                );
            }
            Mode::HidePattern(text) => self.draw_input_popup(
                frame,
                root,
                "Hide Pattern",
                "Regex shared by the selected lines - edit, then Enter to exclude",
                &text,
            ),
            Mode::EntryDetail { scroll } => self.draw_entry_detail_popup(frame, root, scroll),
            Mode::Help => {
                let area = centered_rect(84, 46, root);
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new(help_text())
                        .block(Block::default().title("Keys").borders(Borders::ALL))
                        .wrap(Wrap { trim: false }),
                    area,
                );
            }
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
            "Up/Down move  Space pick preset  Enter apply  Esc cancel",
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
            .title("Log Detail  Enter/Esc close  j/k scroll")
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

    fn handle_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let mode_kind = match &self.mode {
            Mode::Normal => 0,
            Mode::Search(_)
            | Mode::AddFile(_)
            | Mode::Filter(_)
            | Mode::ExportFilters(_)
            | Mode::LoadFilters(_)
            | Mode::ExportSchemas(_)
            | Mode::ImportSchemas(_)
            | Mode::Extractor(_)
            | Mode::LogSchema(_)
            | Mode::EditFilter { .. }
            | Mode::EditSearch { .. }
            | Mode::HidePattern(_) => 1,
            Mode::HideChoice => 2,
            Mode::EntryDetail { .. } => 3,
            Mode::Help => 4,
            Mode::TimePicker(_) => 5,
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
            _ => Ok(false),
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(self.mode, Mode::EntryDetail { .. }) {
            self.handle_entry_detail_mouse(mouse);
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => self.copy_from_mouse(mouse),
            MouseEventKind::Down(MouseButton::Left) => self.begin_mouse_selection(mouse),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_mouse_selection(mouse),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_drag = None,
            _ => {}
        }
    }

    fn handle_entry_detail_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if rect_contains(self.entry_detail_area, mouse.column, mouse.row) {
                    self.copy_detail_text(DetailSurface::Popup);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.begin_detail_mouse_selection(DetailSurface::Popup, mouse);
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
        self.status = match copy_to_clipboard(&text) {
            Ok(()) => format!("copied {count} detail line(s), {} bytes", text.len()),
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
        let line = match surface {
            DetailSurface::Inline => row,
            DetailSurface::Popup => self.entry_detail_scroll().saturating_add(row),
        };
        let lines = self.detail_surface_lines(surface, area.width as usize);
        (line < lines.len()).then_some(line)
    }

    fn detail_surface_area(&self, surface: DetailSurface) -> Rect {
        match surface {
            DetailSurface::Inline => self.detail_area,
            DetailSurface::Popup => self.entry_detail_area,
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
        }
    }

    fn entry_detail_scroll(&self) -> usize {
        match self.mode {
            Mode::EntryDetail { scroll } => scroll,
            _ => 0,
        }
    }

    fn report_detail_selection(&mut self) {
        let Some(selection) = self.detail_selection else {
            return;
        };
        let count = selection.anchor.abs_diff(selection.cursor) + 1;
        self.status = match count {
            1 => "1 detail line selected".to_string(),
            n => format!("{n} detail lines selected"),
        };
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        // Ctrl+<motion> never disturbs the selection, so you can travel to a distant
        // line and Space it in without losing what you already picked.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => {
                    self.save_project();
                    return Ok(false);
                }
                KeyCode::Up => {
                    self.move_keeping_selection(-1);
                    return Ok(false);
                }
                KeyCode::Down => {
                    self.move_keeping_selection(1);
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
                KeyCode::Right => {
                    if let Some(view) = self.active_view_mut() {
                        view.scroll_x += 8;
                    }
                    return Ok(false);
                }
                KeyCode::Left => {
                    if let Some(view) = self.active_view_mut() {
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
            KeyCode::Char('f') => self.open_input(Mode::Filter(String::new())),
            KeyCode::Char('t') => self.open_time_picker(),
            KeyCode::Char('T') => self.toggle_elapsed_mark(),
            KeyCode::Char('F') => self.clear_filters(),
            KeyCode::Char('x') => {
                self.open_input(Mode::ExportFilters(self.default_filter_folder_input()))
            }
            KeyCode::Char('L') => {
                self.open_input(Mode::LoadFilters(self.default_filter_folder_input()))
            }
            KeyCode::Char('X') => {
                self.open_input(Mode::ExportSchemas(self.default_schema_folder_input()))
            }
            KeyCode::Char('I') => {
                self.open_input(Mode::ImportSchemas(self.default_schema_folder_input()))
            }
            KeyCode::Char('H') => self.begin_hide(),
            KeyCode::Char('a') => self.open_input(Mode::AddFile(String::new())),
            KeyCode::Char('d') => self.remove_active_file(),
            KeyCode::Char('S') => self.open_new_schema_input(),
            KeyCode::Char('|') | KeyCode::Char('\\') => self.split_active(SplitMode::Horizontal),
            KeyCode::Char('-') => self.split_active(SplitMode::Vertical),
            KeyCode::Char('w') => self.close_active_pane(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('e') => self.open_schema_input(),
            KeyCode::Enter => match self.focus {
                Focus::Sidebar => self.activate_sidebar_item(),
                Focus::Results => self.jump_to_selected_result(),
                Focus::Pane => self.open_entry_detail_popup(),
            },
            _ => {}
        }

        Ok(false)
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        let caret = self.input_cursor;
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
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

    /// Every path into an input popup routes through here so the caret starts at the
    /// end of the prefilled text instead of a stale offset.
    fn open_input(&mut self, mode: Mode) {
        self.input_cursor = match &mode {
            Mode::Search(text)
            | Mode::AddFile(text)
            | Mode::Filter(text)
            | Mode::ExportFilters(text)
            | Mode::LoadFilters(text)
            | Mode::ExportSchemas(text)
            | Mode::ImportSchemas(text)
            | Mode::Extractor(text)
            | Mode::LogSchema(text)
            | Mode::EditFilter { text, .. }
            | Mode::EditSearch { text, .. }
            | Mode::HidePattern(text) => text.chars().count(),
            _ => 0,
        };
        self.mode = mode;
    }

    fn handle_hide_choice_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Char(ch) => {
                if let Some(index) = hide_choice_index(ch) {
                    if let Some(field) = self.hide_choice_fields().get(index).cloned() {
                        self.hide_like(&field, "");
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_entry_detail_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Char('q') => self.mode = Mode::Normal,
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

    fn scroll_entry_detail(&mut self, delta: isize) {
        if let Mode::EntryDetail { scroll } = &mut self.mode {
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
            | Mode::ExportSchemas(input)
            | Mode::ImportSchemas(input)
            | Mode::Extractor(input)
            | Mode::LogSchema(input)
            | Mode::EditFilter { text: input, .. }
            | Mode::EditSearch { text: input, .. }
            | Mode::HidePattern(input) => Some(input),
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
            Mode::ExportSchemas(folder) => self.submit_export_schemas(folder),
            Mode::ImportSchemas(folder) => self.submit_import_schemas(folder),
            Mode::Extractor(text) => self.submit_extractor(text),
            Mode::LogSchema(text) => self.submit_schema_definition(text),
            Mode::EditFilter { index, text } => self.submit_edit_filter(index, text),
            Mode::EditSearch { index, text } => self.submit_edit_search(index, text),
            Mode::HidePattern(pattern) => self.submit_hide_pattern(pattern),
            _ => {}
        }
        Ok(())
    }

    fn submit_search(&mut self, text: String) {
        let query_text = text.trim().to_string();
        if let Some(view) = self.active_view_mut() {
            view.query_text = query_text.clone();
            view.query = if query_text.is_empty() {
                None
            } else {
                Some(compile_query(&query_text))
            };
        }
        if !query_text.is_empty() && !self.project.saved_searches.contains(&query_text) {
            self.project.saved_searches.insert(0, query_text.clone());
            self.project.saved_searches.truncate(8);
        }
        self.results_selected = 0;
        self.results_scroll = 0;
        // The scan spans frames, so jumping to the first match has to wait for it.
        self.queue_recompute(self.focused_pane, After::GotoFirstMatch);

        if let Some(query) = self.active_view().and_then(|view| view.query.as_ref()) {
            if !query.error.is_empty() {
                self.status = format!("search: {}", query.error);
            }
        }
        self.save_project();
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

    fn apply_time_range(&mut self, value: String) {
        self.mutate_filters(|filters| {
            filters.add(FilterRule::new(
                "timestamp",
                "range",
                value.as_str(),
                "include",
            ))
        });
        self.status = format!("time range filter added: {value}");
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
        if !text.contains('|') {
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

    fn submit_schema_definition(&mut self, text: String) {
        let extractor = match parse_log_schema_input(&text) {
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
        self.autosave_project();
        self.status = format!("schema saved: {name}");
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

    fn hide_like(&mut self, dimension: &str, keyword: &str) {
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
        let rule = hide_like(file, entry, dimension, keyword);
        self.mutate_filters(|filters| filters.add(rule));
        self.status = "hide rule added".to_string();
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
                Some(SidebarItem::Filter { index, .. }) => {
                    let index = *index;
                    self.toggle_filter_enabled(index);
                }
                Some(SidebarItem::Search { index, .. }) => {
                    let index = *index;
                    self.toggle_saved_search(index);
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

    fn open_schema_input(&mut self) {
        let prefill = self
            .schema_target()
            .and_then(|file_id| self.project.get_file(&file_id).cloned())
            .and_then(|file| {
                file.extractor
                    .map(|extractor| (file.display_name, extractor))
            })
            .map(|(_, extractor)| {
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

    fn open_new_schema_input(&mut self) {
        self.open_input(Mode::LogSchema(format!(
            "My schema | <timestamp> <level>: <message> | {} | description",
            DEFAULT_TIMESTAMP_FORMAT
        )));
    }

    /// Oldest and newest parseable timestamps in the focused log. A log is written in
    /// time order and a merged model is sorted by timestamp, so the bounds sit at the
    /// ends; scanning inward stops at the first entry that has one, instead of parsing
    /// the whole file.
    fn active_time_bounds(&self) -> Option<(NaiveDateTime, NaiveDateTime)> {
        let (file, _) = self.active_file_view()?;
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

    fn open_time_picker(&mut self) {
        let bounds = self.active_time_bounds();
        if bounds.is_none() {
            self.status = "no timestamps in this log; presets count back from now".to_string();
        }
        self.mode = Mode::TimePicker(TimePicker::new(bounds));
    }

    fn open_entry_detail_popup(&mut self) {
        if self.entry_detail_target().is_none() {
            self.status = "no line selected".to_string();
            return;
        }
        self.mode = Mode::EntryDetail { scroll: 0 };
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

    /// One line: choose a schema field. Several: derive a shared regex and let the
    /// user vet it before it becomes a saved, project-wide filter.
    fn begin_hide(&mut self) {
        let globals = self.target_globals();
        if globals.len() < 2 {
            self.mode = Mode::HideChoice;
            return;
        }

        let Some(file) = self
            .active_view()
            .and_then(|view| self.project.get_file(&view.file_id))
        else {
            return;
        };
        let messages: Vec<String> = globals
            .iter()
            .filter_map(|global| file.entries.get(*global))
            .map(|entry| file.message(entry).lines().next().unwrap_or("").to_string())
            .collect();
        let borrowed: Vec<&str> = messages.iter().map(String::as_str).collect();

        match common_message_pattern(&borrowed) {
            Some(pattern) => {
                self.status = format!("pattern from {} lines", globals.len());
                self.open_input(Mode::HidePattern(pattern));
            }
            // Only when every selected message is blank; the choice menu still works.
            None => self.mode = Mode::HideChoice,
        }
    }

    fn submit_hide_pattern(&mut self, pattern: String) {
        let pattern = pattern.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        if let Err(error) = regex::Regex::new(&pattern) {
            self.status = format!("invalid regex: {error}");
            return;
        }

        self.mutate_filters(|filters| {
            filters.add(FilterRule::new(
                "message",
                "regex",
                pattern.as_str(),
                "exclude",
            ))
        });
        self.clear_active_selection();
        self.status = "hide pattern added".to_string();
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
            self.requeue_all_panes();
        }
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
            SidebarItem::File { .. } => self.open_schema_input(),
            SidebarItem::Filter { index, .. } => {
                let Some(rule) = self.project.filters.rules.get(index) else {
                    return;
                };
                let text = rule.to_input();
                self.open_input(Mode::EditFilter { index, text });
            }
            SidebarItem::Search { index, text, .. } => {
                self.open_input(Mode::EditSearch { index, text });
            }
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

    fn schema_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_schema_folder_path)
    }

    fn filter_folder_from_input(&self, input: &str) -> PathBuf {
        self.folder_from_input(input, Self::default_filter_folder_path)
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
            items.push(SidebarItem::Hint("none - press a".to_string()));
        } else {
            for file in real {
                let suffix = if !file.error.is_empty() {
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
        if self.project.filters.rules.is_empty() {
            items.push(SidebarItem::Hint("none - f or H".to_string()));
        } else {
            for (index, rule) in self.project.filters.rules.iter().enumerate() {
                let mark = if rule.enabled { "*" } else { "o" };
                items.push(SidebarItem::Filter {
                    index,
                    label: format!("{mark} {}", rule.describe()),
                });
            }
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

/// Attach the project filters but do not apply them: the caller queues a recompute so
/// the (potentially multi-second) filter pass runs behind a progress bar.
fn build_view(leaf_id: impl Into<String>, file: &LogFileModel, filters: &FilterSet) -> ViewModel {
    let mut view = ViewModel::new(leaf_id, file);
    view.filters = filters.clone();
    view
}

/// Filter rules are long ("exclude message contains '...'"), so let a roomy terminal
/// spend a quarter of its width on the sidebar. Never starve the log panes.
fn sidebar_width(body_width: u16) -> u16 {
    (body_width / 4)
        .clamp(34, 56)
        .min(body_width.saturating_sub(24))
}

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

fn parse_log_schema_input(input: &str) -> Result<Extractor, String> {
    let parts: Vec<&str> = input.splitn(4, '|').map(str::trim).collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err("schema needs: name | format | timestamp format | description".to_string());
    }

    let (timestamp_format, description) = match parts.as_slice() {
        [_, _, timestamp_format, description] => (
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
    Ok(extractor)
}

fn format_filter_datetime(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
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

fn file_detail_lines(file: &LogFileModel, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH + 1).max(8);
    let plain = Style::default();
    let mut lines = Vec::new();
    push_detail(&mut lines, "file", &file.display_name, value_width, plain);
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
        return file.get_field(entry, "timestamp");
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
    elapsed_from: Option<NaiveDateTime>,
) -> String {
    let cursor = if at_cursor { ">" } else { " " };
    let pick_mark = if picked { "+" } else { " " };
    let match_mark = if matched { "*" } else { " " };
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
    let chars: Vec<char> = line.chars().collect();
    let visible_lo = lo.saturating_sub(scroll_x).min(chars.len());
    // `hi` is inclusive, so the exclusive end is one past it.
    let visible_hi = (hi + 1).saturating_sub(scroll_x).min(chars.len());
    if visible_lo >= visible_hi {
        return Line::from(Span::styled(line.to_string(), base));
    }

    let take = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    let selected = base.bg(Color::White).fg(Color::Black);
    let mut spans = Vec::with_capacity(3);
    if visible_lo > 0 {
        spans.push(Span::styled(take(0..visible_lo), base));
    }
    spans.push(Span::styled(take(visible_lo..visible_hi), selected));
    if visible_hi < chars.len() {
        spans.push(Span::styled(take(visible_hi..chars.len()), base));
    }
    Line::from(spans)
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
    "Project/files
  a add file        d remove focused file       Ctrl+s save
  S define reusable log schema
  e apply/edit this file's schema (schema name, or full format template)
  Space selects/deselects whatever the cursor is on; Enter opens its detail view.
  In the sidebar: Space on a log adds/removes it, merging the logs by timestamp
                  Space on a filter enables/disables it
                  Space on a saved search runs it, or clears it if running
                  Enter edits that log's schema, that filter, or that search
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
  Right-click       copy the substring, the clicked/selected rows, or detail text
  Esc               clear the selection, then the search

Filter/search
  / search          n/N next/previous match      c context 0/3/10
  f add filter      t time range picker          F clear filters
  T elapsed time from this line (again to restore absolute timestamps)
  H hide like current row
  x export filters  L import filters
  X export schemas  I import schemas (merges; existing names are kept)
  Time range picker presets count back from the newest entry, not from now
  Filter syntax     [schema=\"name\"] field op [include|exclude] value
  H with several lines selected derives a regex shared by them all
  Filters apply to the whole project and are saved automatically
  x/L default to ~/.log-scouter/filters (any path works)
  Search opens a bottom matches panel; click a match or focus it and press Enter

Layout
  | split columns   - split rows                 w close pane
  Enter on a log row opens a larger detail popup with parsed fields and raw text
  Detail panel (left, bottom) shows the selected line or project item details
  A star marks files open in a pane, enabled filters, and the running search
  Quitting records the panes, their logs and their searches; reopening restores them

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

Press any key to close."
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

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

        let screen = render(&mut app, 100, 30);
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

        // The popup is prefilled, not applied: the counter token is generalised.
        let Mode::HidePattern(pattern) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
        assert_eq!(pattern, r"Distribution\s+Service\s+Trigger:\s+\S+\s+queued");
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
        assert!(matches!(app.mode, Mode::HideChoice));
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

        let Mode::HidePattern(pattern) = app.mode.clone() else {
            panic!("expected a HidePattern popup, got {:?}", app.mode);
        };
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
        app.open_input(Mode::HidePattern("(unclosed".to_string()));
        press(&mut app, KeyCode::Enter);
        assert!(app.status.starts_with("invalid regex:"), "{}", app.status);
        assert!(app.project.filters.rules.is_empty());
    }

    fn type_text(app: &mut AppState, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
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
        assert_eq!(sidebar_width(100), 34); // narrow: the old fixed width
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

        // f, then the filter expression, then Enter -- exactly what a user types.
        press(&mut app, KeyCode::Char('f'));
        type_text(&mut app, "level equals exclude Trace");
        press(&mut app, KeyCode::Enter);

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

        press(&mut app, KeyCode::Char('f'));
        type_text(
            &mut app,
            "schema=\"Bracketed default\" level equals exclude Trace",
        );
        press(&mut app, KeyCode::Enter);

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
        assert!(app.status.starts_with("time range filter added:"));
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

        press(&mut app, KeyCode::Char('f'));
        type_text(&mut app, "level equals exclude Trace");
        press(&mut app, KeyCode::Enter);

        // Both panes see the project-global filter, not just the focused one.
        for pane in &app.panes {
            assert_eq!(pane.view.filters.rules.len(), 1);
            assert_eq!(pane.view.visible.len(), 1);
        }

        press(&mut app, KeyCode::Char('F')); // clear
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
        assert_eq!(
            app.default_filter_folder_input(),
            "~/.log-scouter/filters"
        );
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

        let Mode::HidePattern(original) = app.mode.clone() else {
            panic!("expected HidePattern");
        };
        // Caret starts at the end of the prefilled text.
        assert_eq!(app.input_cursor, original.chars().count());

        // Typing appends; Backspace removes; Home/Delete edit the front.
        type_text(&mut app, "XY");
        let Mode::HidePattern(edited) = app.mode.clone() else {
            panic!("expected HidePattern");
        };
        assert_eq!(edited, format!("{original}XY"));

        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.input_cursor, 0);
        press(&mut app, KeyCode::Delete);
        let Mode::HidePattern(edited) = app.mode.clone() else {
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

        press(&mut app, KeyCode::Char('f'));
        type_text(&mut app, "message contains exclude queued");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.active_view().unwrap().visible.len(), 0);

        // Clear that filter, then filter to a subset and search within it.
        press(&mut app, KeyCode::Char('F'));
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

    /// Enter on a log source opens its schema for editing; Space is what selects. Showing
    /// one log alone is Space on the others to deselect them.
    #[test]
    fn enter_on_a_log_source_opens_its_schema_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.focus = Focus::Sidebar;
        app.sidebar_selected = 2; // b.log

        press(&mut app, KeyCode::Enter);
        let Mode::Extractor(text) = &app.mode else {
            panic!("expected the schema editor, got {:?}", app.mode);
        };
        assert!(
            text.contains("Bracketed default"),
            "prefilled with the file's current schema: {text}"
        );
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

        // The new file is open in the pane and parsed with the bracketed schema, so the
        // level is empty: the line does not match that format at all.
        let view_file = app.active_view().unwrap().file_id.clone();
        let file = app.project.get_file(&view_file).unwrap();
        assert_eq!(file.display_name, "simple.log");
        assert_eq!(file.level(&file.entries[0]), "");

        // `e` prefills the focused file's current schema.
        press(&mut app, KeyCode::Char('e'));
        let Mode::Extractor(prefill) = app.mode.clone() else {
            panic!("expected the schema popup, got {:?}", app.mode);
        };
        assert!(
            prefill.starts_with("Bracketed default | <timestamp> [HOST:"),
            "{prefill}"
        );

        app.mode = Mode::Normal;
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
    fn user_defined_schema_can_be_saved_then_assigned_to_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let simple_log = tmp.path().join("simple.log");
        std::fs::write(&simple_log, "10:00:01 WARN: disk almost full\n").unwrap();
        let (mut app, _) = app_with_two_logs(tmp.path());
        app.submit_add_file(simple_log.to_string_lossy().to_string())
            .unwrap();
        app.finish_work();
        let file_id = app.active_view().unwrap().file_id.clone();

        press(&mut app, KeyCode::Char('S'));
        let Mode::LogSchema(prefill) = app.mode.clone() else {
            panic!("expected schema definition popup, got {:?}", app.mode);
        };
        assert!(
            prefill.starts_with("My schema | <timestamp> <level>: <message>"),
            "{prefill}"
        );

        app.mode = Mode::Normal;
        app.submit_schema_definition(
            "simple | <timestamp> <level>: <message> | %H:%M:%S | compact service log".to_string(),
        );
        assert_eq!(app.status, "schema saved: simple");
        assert_eq!(
            app.project.extractors["simple"].description,
            "compact service log"
        );

        // Defining a schema does not implicitly re-parse the focused file.
        let file = app.project.get_file(&file_id).unwrap();
        assert_eq!(file.extractor_name, "Bracketed default");
        assert_eq!(file.level(&file.entries[0]), "");

        // Typing only the schema name in the file schema popup assigns an existing one.
        app.submit_extractor("simple".to_string());
        app.finish_work();

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

        let Mode::EditFilter { index, text } = app.mode.clone() else {
            panic!("expected the filter editor, got {:?}", app.mode);
        };
        assert_eq!(index, 0);
        assert_eq!(text, "message contains exclude queued");

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
        assert_eq!(
            app.default_schema_folder_input(),
            "~/.log-scouter/schemas"
        );
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
        press(&mut app, KeyCode::Char('S'));
        clear_input(&mut app); // `S` prefills a "My schema | ..." template
        type_text(
            &mut app,
            "compact | <timestamp> <level>: <message> | %H:%M:%S | small",
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project.extractors.len(), 2);

        let folder = tmp.path().join("pack");
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(app.mode, Mode::ExportSchemas(_)));
        clear_input(&mut app);
        type_text(&mut app, folder.to_str().unwrap());
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.status,
            format!("exported 2 log schema(s) to {}", folder.display())
        );

        // A fresh project starts with only the built-in schema.
        let other = tempfile::tempdir().unwrap();
        let mut app2 = app_with_log(other.path());
        assert_eq!(app2.project.extractors.len(), 1);

        press(&mut app2, KeyCode::Char('I'));
        assert!(matches!(app2.mode, Mode::ImportSchemas(_)));
        clear_input(&mut app2);
        type_text(&mut app2, folder.to_str().unwrap());
        press(&mut app2, KeyCode::Enter);

        // "Bracketed default" already existed, so only `compact` is new.
        assert_eq!(
            app2.status,
            "imported 1 log schema(s), skipped 1 already in this project"
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

        press(&mut app, KeyCode::Char('I'));
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

        press(&mut app, KeyCode::Char('I'));
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
