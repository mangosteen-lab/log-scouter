use crate::core::models::{LogEntry, LogFileModel};
use crate::core::search::parse_datetime;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const OPS: &[&str] = &["equals", "contains", "regex", "range"];
pub const ACTIONS: &[&str] = &["include", "exclude"];

/// User-level filter library, shared across every project.
pub const USER_DIR: &str = ".log-scouter";
pub const USER_FILTERS_SUBDIR: &str = "filters";

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `~/.log-scouter/filters`, or `None` when `$HOME` is unset.
pub fn user_filter_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join(USER_FILTERS_SUBDIR))
}

/// Expand a leading `~` so typed paths like `~/.log-scouter/filters` resolve.
pub fn expand_tilde(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if trimmed == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_schema: Option<String>,
    pub field: String,
    #[serde(default = "default_op")]
    pub op: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterSet {
    #[serde(default)]
    pub rules: Vec<FilterRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterFile {
    pub name: String,
    pub description: String,
    pub filter: FilterRule,
}

impl FilterRule {
    pub fn new(
        field: impl Into<String>,
        op: impl Into<String>,
        value: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            log_schema: None,
            field: field.into(),
            op: op.into(),
            value: value.into(),
            action: action.into(),
            enabled: true,
        }
    }

    pub fn for_log_schema(mut self, log_schema: impl Into<String>) -> Self {
        let log_schema = log_schema.into();
        self.log_schema = (!log_schema.trim().is_empty()).then_some(log_schema);
        self
    }

    pub fn matches(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.applies_to_log_schema(file, entry)
            && self.matches_with(file, entry, self.compile_regex().as_ref())
    }

    pub fn applies_to_log_schema(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.log_schema
            .as_deref()
            .map(str::trim)
            .filter(|schema| !schema.is_empty())
            .map(|schema| file.log_schema_name_for(entry) == schema)
            .unwrap_or(true)
    }

    /// `regex` must be the compiled form of `self.value`, hoisted out of the entry loop.
    /// Compiling per entry costs more than the match itself on million-line files.
    fn matches_with(
        &self,
        file: &LogFileModel,
        entry: &LogEntry,
        regex: Option<&regex::Regex>,
    ) -> bool {
        if self.op == "range" {
            return self.range_match(file, entry);
        }

        file.with_field(entry, &self.field, |val| match self.op.as_str() {
            "equals" => val == self.value,
            "contains" => contains_ignore_case(val, &self.value),
            "regex" => regex.map(|regex| regex.is_match(val)).unwrap_or(false),
            _ => false,
        })
    }

    fn compile_regex(&self) -> Option<regex::Regex> {
        (self.op == "regex")
            .then(|| regex::Regex::new(&self.value).ok())
            .flatten()
    }

    /// The rule written in the `f` popup's own syntax, so Enter on a filter can prefill
    /// an editor with something that parses back to an equal rule. `describe` is prose
    /// for the sidebar and does not round-trip.
    pub fn to_input(&self) -> String {
        let mut out = String::new();
        if let Some(schema) = self
            .log_schema
            .as_deref()
            .map(str::trim)
            .filter(|schema| !schema.is_empty())
        {
            // Quoted whole, since a schema name may contain spaces and the parser reads
            // `schema=` off a single token.
            out.push_str(&shell_words::quote(&format!("schema={schema}")));
            out.push(' ');
        }
        out.push_str(&format!(
            "{} {} {} {}",
            self.field,
            self.op,
            self.action,
            shell_words::quote(&self.value)
        ));
        out
    }

    pub fn describe(&self) -> String {
        let state = if self.enabled { "" } else { " (off)" };
        let schema = self
            .log_schema
            .as_deref()
            .filter(|schema| !schema.trim().is_empty())
            .map(|schema| format!(" on schema '{schema}'"))
            .unwrap_or_default();
        format!(
            "{} {} {} '{}'{}{}",
            self.action, self.field, self.op, self.value, schema, state
        )
    }

    fn range_match(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        let (start_s, end_s) = self.value.split_once("..").unwrap_or((&self.value, ""));
        if matches!(self.field.as_str(), "timestamp" | "time" | "ts") {
            let Some(ts) = file.timestamp(entry) else {
                return false;
            };
            let start = if start_s.trim().is_empty() {
                None
            } else {
                parse_datetime(start_s)
            };
            let end = if end_s.trim().is_empty() {
                None
            } else {
                parse_datetime(end_s)
            };
            if start.map(|start| ts < start).unwrap_or(false) {
                return false;
            }
            if end.map(|end| ts > end).unwrap_or(false) {
                return false;
            }
            return true;
        }

        let val = file.get_field(entry, &self.field);
        if !start_s.is_empty() && val.as_str() < start_s {
            return false;
        }
        if !end_s.is_empty() && val.as_str() > end_s {
            return false;
        }
        true
    }
}

/// A `FilterSet` with its regexes compiled and its rules split by action. Build once,
/// then call `visible` for every entry.
pub struct PreparedFilters<'a> {
    includes: Vec<PreparedRule<'a>>,
    excludes: Vec<PreparedRule<'a>>,
}

struct PreparedRule<'a> {
    rule: &'a FilterRule,
    regex: Option<regex::Regex>,
}

impl PreparedRule<'_> {
    fn applies_to(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.rule.applies_to_log_schema(file, entry)
    }

    fn matches(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.rule.matches_with(file, entry, self.regex.as_ref())
    }
}

impl PreparedFilters<'_> {
    pub fn visible(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        let mut has_applicable_include = false;
        let mut matched_include = false;
        for rule in self
            .includes
            .iter()
            .filter(|rule| rule.applies_to(file, entry))
        {
            has_applicable_include = true;
            if rule.matches(file, entry) {
                matched_include = true;
                break;
            }
        }
        if has_applicable_include && !matched_include {
            return false;
        }
        !self
            .excludes
            .iter()
            .filter(|rule| rule.applies_to(file, entry))
            .any(|rule| rule.matches(file, entry))
    }
}

impl FilterSet {
    pub fn prepare(&self) -> PreparedFilters<'_> {
        let enabled = |action: &'static str| {
            self.rules
                .iter()
                .filter(|rule| rule.enabled && rule.action == action)
                .map(|rule| PreparedRule {
                    rule,
                    regex: rule.compile_regex(),
                })
                .collect()
        };
        PreparedFilters {
            includes: enabled("include"),
            excludes: enabled("exclude"),
        }
    }

    pub fn visible(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.prepare().visible(file, entry)
    }

    pub fn add(&mut self, rule: FilterRule) {
        self.rules.push(rule);
    }

    pub fn clear(&mut self) {
        self.rules.clear();
    }

    pub fn has_enabled_rules(&self) -> bool {
        self.rules.iter().any(|rule| rule.enabled)
    }
}

pub fn hide_like(
    file: &LogFileModel,
    entry: &LogEntry,
    dimension: &str,
    keyword: &str,
) -> FilterRule {
    if dimension == "keyword" {
        return FilterRule::new("message", "contains", keyword, "exclude");
    }
    let value = file.get_field(entry, dimension);
    FilterRule::new(dimension, "equals", value, "exclude")
}

/// Derive a regex matching every one of `messages`, generalising the parts where they
/// differ. Always yields a pattern that matches exactly the lines it was given (and
/// their variants), so pressing `H` on any selection is never a dead end.
///
/// Three strategies, most general first:
/// 1. Same token count: differing tokens become `\S+`, keeping positions aligned.
/// 2. Enough shared tokens: join the tokens common to all with `.*`.
/// 3. Otherwise the lines are not variants of one template, so match them literally.
///    A near-empty token subsequence like `the` would otherwise hide half the file.
pub fn common_message_pattern(messages: &[&str]) -> Option<String> {
    if messages.len() < 2 {
        return None;
    }

    let token_lists: Vec<Vec<&str>> = messages
        .iter()
        .map(|message| message.split_whitespace().collect())
        .collect();
    if token_lists.iter().all(|tokens| tokens.is_empty()) {
        return None;
    }

    let width = token_lists[0].len();
    if width > 0 && token_lists.iter().all(|tokens| tokens.len() == width) {
        let parts: Vec<String> = (0..width)
            .map(|index| {
                let first = token_lists[0][index];
                if token_lists.iter().all(|tokens| tokens[index] == first) {
                    regex::escape(first)
                } else {
                    r"\S+".to_string()
                }
            })
            .collect();
        if parts.iter().any(|part| part != r"\S+") {
            return Some(parts.join(r"\s+"));
        }
    }

    let shortest = token_lists
        .iter()
        .map(|tokens| tokens.len())
        .min()
        .unwrap_or(0);
    if shortest > 0 {
        let common = token_lists[1..]
            .iter()
            .fold(token_lists[0].clone(), |acc, tokens| {
                common_subsequence(&acc, tokens)
            });
        // Require the shared tokens to carry at least half the shortest message,
        // otherwise the "template" is mostly wildcard.
        if common.len() * 2 >= shortest {
            return Some(
                common
                    .iter()
                    .map(|token| regex::escape(token))
                    .collect::<Vec<_>>()
                    .join(".*"),
            );
        }
    }

    Some(literal_alternation(messages))
}

/// `(?:one|two)` over the distinct messages, in first-seen order.
fn literal_alternation(messages: &[&str]) -> String {
    let mut seen = Vec::new();
    for message in messages {
        let trimmed = message.trim();
        if !trimmed.is_empty() && !seen.contains(&trimmed) {
            seen.push(trimmed);
        }
    }
    let body = seen
        .iter()
        .map(|message| regex::escape(message))
        .collect::<Vec<_>>()
        .join("|");
    format!("(?:{body})")
}

/// Longest common subsequence of two token slices, preserving order.
fn common_subsequence<'a>(left: &[&'a str], right: &[&'a str]) -> Vec<&'a str> {
    let mut table = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            table[i][j] = if left[i] == right[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            out.push(left[i]);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

pub fn export_filters_to_folder(filter_set: &FilterSet, folder: &Path) -> io::Result<usize> {
    fs::create_dir_all(folder)?;
    for (index, rule) in filter_set.rules.iter().enumerate() {
        let name = filter_file_name(index + 1, rule);
        let filter_file = FilterFile {
            name: name.clone(),
            description: rule.describe(),
            filter: rule.clone(),
        };
        let path = folder.join(format!(
            "{:03}-{}.json",
            index + 1,
            sanitize_file_component(&name)
        ));
        let body = serde_json::to_string_pretty(&filter_file).map_err(io::Error::other)?;
        fs::write(path, body)?;
    }
    Ok(filter_set.rules.len())
}

pub fn load_filters_from_folder(folder: &Path) -> io::Result<Vec<FilterFile>> {
    let mut paths = filter_file_paths(folder)?;
    paths.sort();

    let mut filters = Vec::new();
    for path in paths {
        let body = fs::read_to_string(&path)?;
        let filter_file = parse_filter_file(&body).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
        filters.push(filter_file);
    }
    Ok(filters)
}

fn filter_file_paths(folder: &Path) -> io::Result<Vec<PathBuf>> {
    json_file_paths(folder)
}

/// Every `.json` directly inside `folder`. Shared by the filter and schema packs.
pub(crate) fn json_file_paths(folder: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn parse_filter_file(body: &str) -> Result<FilterFile, serde_json::Error> {
    serde_json::from_str::<FilterFile>(body).or_else(|_| {
        let filter = serde_json::from_str::<FilterRule>(body)?;
        Ok(FilterFile {
            name: filter_file_name(1, &filter),
            description: filter.describe(),
            filter,
        })
    })
}

fn filter_file_name(index: usize, rule: &FilterRule) -> String {
    let value = if rule.value.chars().count() > 48 {
        format!("{}...", rule.value.chars().take(48).collect::<String>())
    } else {
        rule.value.clone()
    };
    let schema = rule
        .log_schema
        .as_deref()
        .filter(|schema| !schema.trim().is_empty())
        .map(|schema| format!("schema-{schema}-"))
        .unwrap_or_default();
    format!(
        "filter-{index:03}-{schema}{}-{}-{}-{}",
        rule.action, rule.field, rule.op, value
    )
}

/// Shared with the schema pack writer, which names files the same way.
pub(crate) fn sanitize_file_component(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else if ch.is_whitespace() || matches!(ch, ':' | '/' | '\\' | '|' | '\'' | '"') {
            out.push('-');
        }
    }

    let compact = out
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if compact.is_empty() {
        "filter".to_string()
    } else {
        compact
    }
}

/// Case-insensitive substring test that does not allocate. An ASCII needle can only
/// match ASCII bytes, and UTF-8 continuation bytes are all >= 0x80, so a byte-wise
/// search can never land inside a multi-byte character.
pub fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if !needle.is_ascii() {
        return haystack.to_lowercase().contains(&needle.to_lowercase());
    }

    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn default_op() -> String {
    "contains".to_string()
}

fn default_action() -> String {
    "exclude".to_string()
}

fn default_enabled() -> bool {
    true
}
