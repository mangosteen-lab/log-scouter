use crate::core::extractor::Extractor;
use crate::core::filters::FilterSet;
use crate::core::parser;
use crate::core::search::Query;
use chrono::NaiveDateTime;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub index: usize,
    pub line_no: usize,
    pub raw: String,
    /// Index into `LogFileModel::sources` for a merged model; always 0 otherwise.
    pub source: u16,
}

/// One contributing file inside a merged model, with the schema it was parsed under.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub display_name: String,
    pub extractor_name: String,
    pub extractor: Option<Extractor>,
}

#[derive(Debug, Clone)]
pub struct LogFileModel {
    pub file_id: String,
    pub path: PathBuf,
    pub extractor_name: String,
    pub display_name: String,
    /// An optional short name and free-text note the user gives a source so the assistant
    /// (and the sidebar) can tell what it is. Both empty by default; persisted per project.
    pub label: String,
    pub description: String,
    pub extractor: Option<Extractor>,
    pub entries: Vec<LogEntry>,
    pub loaded: bool,
    pub error: String,
    /// Non-empty only for a merged model. Each entry then resolves its fields through
    /// `sources[entry.source]`, so files with different schemas can be interleaved.
    pub sources: Vec<SourceInfo>,
    /// File ids this model was merged from; empty for a real file on disk.
    pub merged_from: Vec<String>,
    concrete: HashMap<String, Option<String>>,
}

impl LogFileModel {
    pub fn new(
        file_id: impl Into<String>,
        path: impl Into<PathBuf>,
        extractor_name: impl Into<String>,
        display_name: impl Into<String>,
        extractor: Option<Extractor>,
    ) -> Self {
        let path = path.into();
        let display_name = {
            let requested = display_name.into();
            if requested.is_empty() {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or(""))
                    .to_string()
            } else {
                requested
            }
        };

        Self {
            file_id: file_id.into(),
            path,
            extractor_name: extractor_name.into(),
            display_name,
            label: String::new(),
            description: String::new(),
            extractor,
            entries: Vec::new(),
            loaded: false,
            error: String::new(),
            sources: Vec::new(),
            merged_from: Vec::new(),
            concrete: HashMap::new(),
        }
    }

    pub fn is_merged(&self) -> bool {
        !self.merged_from.is_empty()
    }

    /// The schema that parsed this entry. Merged models keep one per source file.
    pub fn extractor_for(&self, entry: &LogEntry) -> Option<&Extractor> {
        if self.sources.is_empty() {
            return self.extractor.as_ref();
        }
        self.sources
            .get(entry.source as usize)
            .and_then(|source| source.extractor.as_ref())
    }

    pub fn source_name(&self, entry: &LogEntry) -> Option<&str> {
        self.sources
            .get(entry.source as usize)
            .map(|source| source.display_name.as_str())
    }

    /// The log schema assigned to this entry. Merged models resolve it per source.
    pub fn log_schema_name_for(&self, entry: &LogEntry) -> &str {
        if self.sources.is_empty() {
            return &self.extractor_name;
        }
        self.sources
            .get(entry.source as usize)
            .map(|source| source.extractor_name.as_str())
            .unwrap_or("")
    }

    pub fn load(&mut self) -> std::io::Result<()> {
        self.entries = parser::read_entries(&self.path, self.extractor.as_ref(), None)?;
        self.loaded = true;
        self.error.clear();
        Ok(())
    }

    pub fn load_from_lines<I>(&mut self, lines: I)
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.entries = parser::build_entries(lines, self.extractor.as_ref());
        self.loaded = true;
    }

    pub fn get_field(&self, entry: &LogEntry, name: &str) -> String {
        self.with_field(entry, name, |value| value.to_string())
    }

    /// Borrowing field access. Avoids cloning multi-KB messages on the filter/search
    /// hot path; only a multi-line message has to be materialised.
    pub fn with_field<R>(&self, entry: &LogEntry, name: &str, visit: impl FnOnce(&str) -> R) -> R {
        if matches!(name, "raw" | "text" | "any") {
            return visit(&entry.raw);
        }

        if matches!(name, "source" | "file_source") {
            return visit(self.source_name(entry).unwrap_or(""));
        }

        let Some(extractor) = self.extractor_for(entry) else {
            return if matches!(name, "message" | "msg") {
                visit(&entry.raw)
            } else {
                visit("")
            };
        };

        let Some(concrete) = concrete_name(extractor, name) else {
            return visit("");
        };
        let message_group = message_group(extractor);
        let is_message = concrete == message_group;

        // The trailing field runs to end-of-line, so only the header needs capturing.
        let first_line = entry.raw.split_once('\n').map(|(head, _)| head);
        let head_line = first_line.unwrap_or(&entry.raw);

        let Some((captures, tail_start)) = extractor.head_captures(head_line) else {
            return if is_message {
                visit(&entry.raw)
            } else {
                visit("")
            };
        };

        if is_message && extractor.tail_field() == Some(concrete.as_str()) {
            let tail = &head_line[tail_start..];
            return match first_line {
                None => visit(tail),
                Some(_) => {
                    // Multi-line entry: continuation lines belong to the message.
                    let continuation = &entry.raw[head_line.len()..];
                    let mut owned = String::with_capacity(tail.len() + continuation.len());
                    owned.push_str(tail);
                    owned.push_str(continuation);
                    visit(&owned)
                }
            };
        }

        let value = captures.name(&concrete).map(|m| m.as_str()).unwrap_or("");
        if is_message {
            if let Some((_, continuation)) = entry.raw.split_once('\n') {
                let mut owned = String::with_capacity(value.len() + continuation.len() + 1);
                owned.push_str(value);
                owned.push('\n');
                owned.push_str(continuation);
                return visit(&owned);
            }
        }
        visit(value)
    }

    pub fn fields_for(&self, entry: &LogEntry) -> HashMap<String, String> {
        let Some(extractor) = self.extractor_for(entry) else {
            return HashMap::from([("message".to_string(), entry.raw.clone())]);
        };
        if extractor.captures(&entry.raw).is_none() {
            return HashMap::from([("message".to_string(), entry.raw.clone())]);
        }

        let names = extractor.field_names.clone();
        let mut parsed = HashMap::with_capacity(names.len());
        for name in &names {
            parsed.insert(name.clone(), self.get_field(entry, name));
        }
        parsed
    }

    /// The entry's time, from its schema's timestamp field when that field exists and
    /// parses. Otherwise the line itself is sniffed: a schema with no timestamp field
    /// (`Generic line`) or a `timestamp_format` that does not fit would leave the entry
    /// timeless, and a timeless entry cannot take its place in a merge.
    pub fn timestamp(&self, entry: &LogEntry) -> Option<NaiveDateTime> {
        if let Some(extractor) = self.extractor_for(entry) {
            let raw = self.get_field(entry, "timestamp");
            if !raw.trim().is_empty() {
                if let Some(stamp) = crate::core::extractor::parse_timestamp_with_format(
                    &raw,
                    &extractor.timestamp_format,
                ) {
                    return Some(stamp);
                }
            }
        }
        crate::core::extractor::sniff_timestamp(head_line(&entry.raw))
    }

    pub fn message(&self, entry: &LogEntry) -> String {
        let message = self.get_field(entry, "message");
        if message.is_empty() {
            entry.raw.clone()
        } else {
            message
        }
    }

    pub fn level(&self, entry: &LogEntry) -> String {
        self.get_field(entry, "level")
    }

    pub fn module(&self, entry: &LogEntry) -> String {
        self.get_field(entry, "module")
    }

    pub fn refresh_extractor(&mut self, extractor: Option<Extractor>) {
        self.extractor = extractor;
        self.concrete.clear();
    }
}

/// The first physical line of an entry. Continuation lines carry no timestamp of their own.
fn head_line(raw: &str) -> &str {
    raw.split_once('\n').map(|(head, _)| head).unwrap_or(raw)
}

/// Resolve a logical name (`level`) to the extractor's actual group (`log_level`).
fn concrete_name(extractor: &Extractor, name: &str) -> Option<String> {
    let names = &extractor.field_names;
    if names.iter().any(|candidate| candidate == name) {
        return Some(name.to_string());
    }
    for alias in field_aliases(name) {
        if names.iter().any(|candidate| candidate == alias) {
            return Some(alias.to_string());
        }
    }
    None
}

fn message_group(extractor: &Extractor) -> String {
    extractor
        .field_names
        .last()
        .cloned()
        .unwrap_or_else(|| "message".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleIndices {
    Range(usize),
    List(Vec<usize>),
}

impl VisibleIndices {
    pub fn len(&self) -> usize {
        match self {
            Self::Range(end) => *end,
            Self::List(indices) => indices.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<usize> {
        match self {
            Self::Range(end) => (index < *end).then_some(index),
            Self::List(indices) => indices.get(index).copied(),
        }
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = usize> + '_> {
        match self {
            Self::Range(end) => Box::new(0..*end),
            Self::List(indices) => Box::new(indices.iter().copied()),
        }
    }

    pub fn as_slice_prefix(&self, max: usize) -> Vec<usize> {
        self.iter().take(max).collect()
    }
}

#[derive(Debug, Clone)]
pub struct ViewModel {
    pub leaf_id: String,
    pub file_id: String,
    pub filters: FilterSet,
    pub query: Option<Query>,
    pub query_text: String,
    pub context: usize,
    pub visible: VisibleIndices,
    pub match_set: HashSet<usize>,
    pub cursor: usize,
    pub scroll_y: usize,
    pub scroll_x: usize,
    /// Entries toggled individually (Space), as global entry indices.
    pub marked: HashSet<usize>,
    /// Visible position where an in-progress Shift range started, if any. The live
    /// selection is `marked` union the positions between `anchor` and `cursor`.
    pub anchor: Option<usize>,
    /// Entries surviving the filters, before any search. Cached because filtering is
    /// the expensive pass; see `base_signature` for when it is stale.
    base: VisibleIndices,
    base_signature: Option<(FilterSet, usize)>,
}

impl ViewModel {
    pub fn new(leaf_id: impl Into<String>, file: &LogFileModel) -> Self {
        let mut view = Self {
            leaf_id: leaf_id.into(),
            file_id: file.file_id.clone(),
            filters: FilterSet::default(),
            query: None,
            query_text: String::new(),
            context: 0,
            visible: VisibleIndices::Range(0),
            match_set: HashSet::new(),
            cursor: 0,
            scroll_y: 0,
            scroll_x: 0,
            marked: HashSet::new(),
            anchor: None,
            base: VisibleIndices::Range(0),
            base_signature: None,
        };
        view.rebuild(file);
        view
    }

    /// True when `base` still reflects the current filters and file.
    pub fn base_is_current(&self, file: &LogFileModel) -> bool {
        self.base_signature
            .as_ref()
            .map(|(filters, entries)| *filters == self.filters && *entries == file.entries.len())
            .unwrap_or(false)
    }

    pub fn base(&self) -> &VisibleIndices {
        &self.base
    }

    /// Adopt a filtered index set computed elsewhere (e.g. incrementally, with progress).
    pub fn install_base(&mut self, base: VisibleIndices, file: &LogFileModel) {
        self.base = base;
        self.base_signature = Some((self.filters.clone(), file.entries.len()));
    }

    fn ensure_base(&mut self, file: &LogFileModel) {
        if self.base_is_current(file) {
            return;
        }
        let base = if self.filters.has_enabled_rules() {
            let filters = self.filters.prepare();
            VisibleIndices::List(
                file.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| filters.visible(file, entry))
                    .map(|(index, _)| index)
                    .collect(),
            )
        } else {
            VisibleIndices::Range(file.entries.len())
        };
        self.install_base(base, file);
    }

    /// Filtering is the expensive pass, so its result is cached against the filter set.
    /// A new search then only walks the already-filtered lines, not the whole file.
    pub fn compute(&mut self, file: &LogFileModel) -> (VisibleIndices, HashSet<usize>) {
        self.ensure_base(file);

        let Some(query) = self.query.as_ref() else {
            return (self.base.clone(), HashSet::new());
        };

        let mut match_positions: Vec<usize> = Vec::new();
        let mut match_set: HashSet<usize> = HashSet::new();
        for (position, global_index) in self.base.iter().enumerate() {
            let Some(entry) = file.entries.get(global_index) else {
                continue;
            };
            if query.matches(file, entry) {
                match_positions.push(position);
                match_set.insert(global_index);
            }
        }

        apply_context(&self.base, &match_positions, match_set, self.context)
    }

    pub fn apply(&mut self, result: (VisibleIndices, HashSet<usize>)) {
        self.visible = result.0;
        self.match_set = result.1;
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
        if self.scroll_y >= self.visible.len() {
            self.scroll_y = self.visible.len().saturating_sub(1);
        }
    }

    pub fn rebuild(&mut self, file: &LogFileModel) {
        let result = self.compute(file);
        self.apply(result);
    }

    pub fn is_large(&self, file: &LogFileModel) -> bool {
        file.entries.len() > 150_000
    }

    pub fn current_index(&self) -> Option<usize> {
        self.visible.get(self.cursor)
    }

    pub fn current_entry<'a>(&self, file: &'a LogFileModel) -> Option<&'a LogEntry> {
        self.current_index()
            .and_then(|index| file.entries.get(index))
    }

    pub fn match_positions(&self) -> Vec<usize> {
        if self.match_set.is_empty() {
            return Vec::new();
        }
        self.visible
            .iter()
            .enumerate()
            .filter_map(|(position, index)| self.match_set.contains(&index).then_some(position))
            .collect()
    }

    pub fn next_match(&self, from_pos: usize, forward: bool) -> Option<usize> {
        let positions = self.match_positions();
        if positions.is_empty() {
            return None;
        }

        if forward {
            positions
                .iter()
                .copied()
                .find(|position| *position > from_pos)
                .or_else(|| positions.first().copied())
        } else {
            positions
                .iter()
                .rev()
                .copied()
                .find(|position| *position < from_pos)
                .or_else(|| positions.last().copied())
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.visible.len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
    }

    pub fn move_cursor_to(&mut self, position: usize) {
        self.cursor = position.min(self.visible.len().saturating_sub(1));
    }

    /// Inclusive visible-position bounds of the in-progress Shift range.
    fn range_bounds(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// O(1) per-row check used while rendering, so a huge Shift range costs nothing.
    pub fn is_selected(&self, position: usize, global_index: usize) -> bool {
        if self.marked.contains(&global_index) {
            return true;
        }
        self.range_bounds()
            .map(|(lo, hi)| position >= lo && position <= hi)
            .unwrap_or(false)
    }

    pub fn has_selection(&self) -> bool {
        !self.marked.is_empty() || self.anchor.is_some()
    }

    pub fn selection_count(&self) -> usize {
        let Some((lo, hi)) = self.range_bounds() else {
            return self.marked.len();
        };
        // `visible` is ascending, so the range covers exactly the global indices
        // between its endpoints. Count marked entries outside that window.
        let (low_global, high_global) = (self.visible.get(lo), self.visible.get(hi));
        let outside = match (low_global, high_global) {
            (Some(low), Some(high)) => self
                .marked
                .iter()
                .filter(|global| **global < low || **global > high)
                .count(),
            _ => self.marked.len(),
        };
        (hi - lo + 1) + outside
    }

    /// Selected entries as sorted global indices. Walks the range, so call on demand.
    pub fn selected_globals(&self) -> Vec<usize> {
        let mut set = self.marked.clone();
        if let Some((lo, hi)) = self.range_bounds() {
            for position in lo..=hi {
                if let Some(global_index) = self.visible.get(position) {
                    set.insert(global_index);
                }
            }
        }
        let mut out: Vec<usize> = set.into_iter().collect();
        out.sort_unstable();
        out
    }

    pub fn clear_selection(&mut self) {
        self.marked.clear();
        self.anchor = None;
    }

    /// Fold an in-progress Shift range into `marked` so later moves do not resize it.
    pub fn commit_range(&mut self) {
        if let Some((lo, hi)) = self.range_bounds() {
            for position in lo..=hi {
                if let Some(global_index) = self.visible.get(position) {
                    self.marked.insert(global_index);
                }
            }
        }
        self.anchor = None;
    }

    /// Shift+move: anchor on first use, then grow or shrink as the cursor moves.
    pub fn extend_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.move_cursor(delta);
    }

    /// Ctrl+move: travel without disturbing the selection, so the next toggle can add
    /// a line that is not adjacent to the others.
    pub fn move_keeping_selection(&mut self, delta: isize) {
        self.commit_range();
        self.move_cursor(delta);
    }

    pub fn toggle_current(&mut self) {
        self.commit_range();
        if let Some(global_index) = self.current_index() {
            if !self.marked.remove(&global_index) {
                self.marked.insert(global_index);
            }
        }
    }
}

/// Interleave several loaded files into one model ordered by timestamp.
///
/// Entries without a parseable timestamp (banner lines, stack-trace continuations that
/// began a new record) inherit the previous timestamp *from their own file*, so they
/// stay next to the line they belong with instead of sinking to the top. A file's
/// *leading* entries have no previous line to inherit from, so they borrow the file's
/// first known timestamp and sit just above it.
///
/// A file with no readable timestamp anywhere still sorts ahead of everything else --
/// there is nothing to interleave it on. `sniff_timestamp` exists to make that rare.
pub fn merge_files(file_id: impl Into<String>, files: &[&LogFileModel]) -> LogFileModel {
    let display_name = files
        .iter()
        .map(|file| file.display_name.as_str())
        .collect::<Vec<_>>()
        .join(" + ");

    let mut merged = LogFileModel::new(file_id, PathBuf::new(), "", display_name, None);
    merged.sources = files
        .iter()
        .map(|file| SourceInfo {
            display_name: file.display_name.clone(),
            extractor_name: file.extractor_name.clone(),
            extractor: file.extractor.clone(),
        })
        .collect();
    merged.merged_from = files.iter().map(|file| file.file_id.clone()).collect();

    // (sort key, source, original position, entry)
    let mut ordered: Vec<(NaiveDateTime, u16, usize, &LogEntry)> = Vec::new();
    for (source, file) in files.iter().enumerate() {
        let stamps: Vec<Option<NaiveDateTime>> = file
            .entries
            .iter()
            .map(|entry| file.timestamp(entry))
            .collect();
        // Seeded with the file's first known time so a leading banner does not sort
        // ahead of every other file's first record.
        let mut last = stamps
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(NaiveDateTime::MIN);
        for (position, (entry, stamp)) in file.entries.iter().zip(stamps).enumerate() {
            let stamp = stamp.unwrap_or(last);
            last = stamp;
            ordered.push((stamp, source as u16, position, entry));
        }
    }
    // Stable, and ties break by source then original order, so equal timestamps keep
    // each file's internal sequence.
    ordered.sort_by_key(|(stamp, source, position, _)| (*stamp, *source, *position));

    merged.entries = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (_, source, _, entry))| LogEntry {
            index,
            line_no: entry.line_no,
            raw: entry.raw.clone(),
            source,
        })
        .collect();
    merged.loaded = true;
    merged
}

/// Turn match positions within `base` into the visible set, widening by `context`
/// rows on each side. `match_set` holds global indices of the matches themselves.
pub fn apply_context(
    base: &VisibleIndices,
    match_positions: &[usize],
    match_set: HashSet<usize>,
    context: usize,
) -> (VisibleIndices, HashSet<usize>) {
    if context == 0 {
        return (base.clone(), match_set);
    }

    let len = base.len();
    let mut keep = vec![false; len];
    for position in match_positions {
        let start = position.saturating_sub(context);
        let end = (position + context + 1).min(len);
        for slot in &mut keep[start..end] {
            *slot = true;
        }
    }

    let visible = base
        .iter()
        .enumerate()
        .filter(|(position, _)| keep[*position])
        .map(|(_, global_index)| global_index)
        .collect();
    (VisibleIndices::List(visible), match_set)
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or(""))
        .to_string()
}

fn field_aliases(name: &str) -> &'static [&'static str] {
    match name {
        "level" => &["log_level", "level", "severity"],
        "module" => &["log_module", "module", "component"],
        "message" => &["message", "msg"],
        "host" => &["host"],
        "server" => &["server"],
        "pid" => &["process_id", "pid"],
        "thread" => &["thread_id", "thr", "thread"],
        "file" => &["file_name", "file"],
        "line" => &["line_number", "line"],
        "timestamp" => &["timestamp", "time", "ts"],
        _ => &[],
    }
}
