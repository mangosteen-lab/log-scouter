use crate::core::library::Origin;
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
    resolve_home(|key| std::env::var_os(key))
}

/// Resolve the user's home directory across platforms: `HOME` first (Unix, and set by many
/// Windows shells), then `USERPROFILE`, then `HOMEDRIVE` + `HOMEPATH` (Windows, where `HOME`
/// is usually unset). Split from `home_dir` so the fallback order is testable without
/// touching the process environment.
fn resolve_home(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let non_empty = |key: &str| get(key).filter(|value| !value.is_empty());
    if let Some(home) = non_empty("HOME") {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = non_empty("USERPROFILE") {
        return Some(PathBuf::from(profile));
    }
    match (non_empty("HOMEDRIVE"), non_empty("HOMEPATH")) {
        // e.g. `C:` + `\Users\name`. Concatenate the strings rather than `join`, which would
        // treat the drive-relative `\Users\name` as replacing the drive.
        (Some(drive), Some(path)) => {
            let mut home = drive.to_string_lossy().into_owned();
            home.push_str(&path.to_string_lossy());
            Some(PathBuf::from(home))
        }
        _ => None,
    }
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
    /// Which library this rule was picked from, kept so the sidebar can say whose rule it
    /// is. `None` for one typed by hand, and for every project saved before this existed --
    /// which is why it is optional rather than defaulted to `Project`: "no idea" and "the
    /// project's own" are different answers, and only one of them is true.
    ///
    /// Provenance, not identity: `same_rule` ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
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
            origin: None,
        }
    }

    /// Tag this rule with the library it came from.
    pub fn from_library(mut self, origin: Origin) -> Self {
        self.origin = Some(origin);
        self
    }

    /// Whether two rules filter the same way. Ignores `origin`: the same rule picked from
    /// two different tiers is one rule, and adding it twice would be a duplicate, not a
    /// second opinion.
    pub fn same_rule(&self, other: &Self) -> bool {
        self.log_schema == other.log_schema
            && self.field == other.field
            && self.op == other.op
            && self.value == other.value
            && self.action == other.action
            && self.enabled == other.enabled
    }

    pub fn for_log_schema(mut self, log_schema: impl Into<String>) -> Self {
        let log_schema = log_schema.into();
        self.log_schema = (!log_schema.trim().is_empty()).then_some(log_schema);
        self
    }

    /// A range over the timestamp: the "when" filter, which the sidebar keeps in a slot of
    /// its own because a project only ever wants one answer to that question.
    pub fn is_time_range(&self) -> bool {
        self.op == "range" && matches!(self.field.as_str(), "timestamp" | "time" | "ts")
    }

    /// The two ends of a time range, either of which may be open.
    pub fn time_bounds(&self) -> (&str, &str) {
        let (start, end) = self.value.split_once("..").unwrap_or((&self.value, ""));
        (start.trim(), end.trim())
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

    /// Add a rule, except that a time range replaces the one already there. Two ranges
    /// over the same field can only ever intersect, and the second one is what the user
    /// just asked for -- so `set_time` is what "add another time filter" has to mean.
    pub fn add(&mut self, rule: FilterRule) {
        if rule.is_time_range() {
            self.set_time(rule);
            return;
        }
        self.rules.push(rule);
    }

    pub fn clear(&mut self) {
        self.rules.clear();
    }

    /// Where the single time range lives inside `rules`, if the project has one.
    pub fn time_index(&self) -> Option<usize> {
        self.rules.iter().position(FilterRule::is_time_range)
    }

    pub fn time_rule(&self) -> Option<&FilterRule> {
        self.time_index().map(|index| &self.rules[index])
    }

    /// Install the project's one time range, in place of *every* earlier one.
    ///
    /// Removing all of them, not just the first, matters because two `include` ranges are
    /// OR'd -- a stale `10:24..11:24` left beside a new `11:09..11:24` widens the window
    /// back out, so the user still sees 10:24 lines. It also heals a `project.json` that a
    /// previous build wrote with several ranges.
    pub fn set_time(&mut self, rule: FilterRule) {
        let at = self.time_index().unwrap_or(self.rules.len());
        self.clear_time();
        self.rules.insert(at.min(self.rules.len()), rule);
    }

    pub fn clear_time(&mut self) {
        self.rules.retain(|rule| !rule.is_time_range());
    }

    /// Collapse any accumulation of time ranges to the last one -- the most recently
    /// added, hence the user's latest intent. A no-op once the slot invariant holds, so it
    /// is safe to run on every load.
    pub fn dedupe_time_range(&mut self) {
        let Some(last) = self.rules.iter().rposition(FilterRule::is_time_range) else {
            return;
        };
        let mut index = 0;
        self.rules.retain(|rule| {
            let keep = !rule.is_time_range() || index == last;
            index += 1;
            keep
        });
    }

    /// Every rule that is not the time range, with its index into `rules`.
    pub fn text_rules(&self) -> impl Iterator<Item = (usize, &FilterRule)> {
        self.rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| !rule.is_time_range())
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
    exclude: bool,
) -> FilterRule {
    let action = if exclude { "exclude" } else { "include" };
    if dimension == "keyword" {
        return FilterRule::new("message", "contains", keyword, action);
    }
    let value = file.get_field(entry, dimension);
    FilterRule::new(dimension, "equals", value, action)
}

/// One token's regex, and whether it pins down any literal text. A template of nothing
/// but wildcards matches far more than the lines it came from, so callers refuse one.
struct TokenPattern {
    regex: String,
    anchored: bool,
}

/// A test for one value shape, and the regex that matches it.
type ValueClass = (fn(&str) -> bool, &'static str);

/// Value shapes that vary between two runs of the same log statement without changing
/// what the statement *says*. Recognising them yields `Session \d+ opened` where a blanket
/// wildcard would only manage `Session \S+ opened`.
///
/// Ordered most specific first: a UUID is also a run of hex, and `1.5` is also a decimal.
const VALUE_CLASSES: &[ValueClass] = &[
    (
        is_uuid,
        "[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}",
    ),
    (is_ipv4, r"\d{1,3}(?:\.\d{1,3}){3}"),
    (is_prefixed_hex, "0[xX][0-9a-fA-F]+"),
    (is_integer, r"\d+"),
    (is_decimal, r"\d+[.,]\d+"),
    (is_bare_hex, "[0-9a-fA-F]+"),
    (is_single_quoted, "'[^']*'"),
    (is_double_quoted, r#""[^"]*""#),
];

/// Punctuation that clings to a value without being part of it: `id=42,` is a `42`.
const AFFIX: &[char] = &[
    ',', ';', ':', '=', '(', ')', '[', ']', '{', '}', '<', '>', '!', '?', '#',
];

/// One template `H` can offer for a selection, named by the strategy that built it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternOption {
    pub name: &'static str,
    pub hint: &'static str,
    pub pattern: String,
}

/// Every template the selected messages support, from the loosest strategy to the
/// strictest. How greedy each one *actually* is depends on the log, not on the strategy,
/// so the caller measures them against real lines and ranks them by what they match.
///
/// Deduplicated: on a selection whose differing token has no value shape, the `wildcard`
/// and `typed` strategies produce the same regex, and offering it twice teaches nothing.
pub fn pattern_candidates(messages: &[&str]) -> Vec<PatternOption> {
    let token_lists = tokenize(messages);
    if token_lists.is_empty() || token_lists.iter().all(|tokens| tokens.is_empty()) {
        return Vec::new();
    }

    let mut options: Vec<PatternOption> = Vec::new();
    let mut offer = |name, hint, pattern: Option<String>| {
        let Some(pattern) = pattern else { return };
        if !options.iter().any(|option| option.pattern == pattern) {
            options.push(PatternOption {
                name,
                hint,
                pattern,
            });
        }
    };

    offer(
        "loose",
        "shared words, .* between",
        subsequence_pattern(&token_lists),
    );
    offer(
        "prefix",
        "leading words, then .*",
        prefix_pattern(&token_lists),
    );
    // With one message nothing differs, so `wildcard` would just be the line itself.
    if token_lists.len() > 1 {
        offer(
            "wildcard",
            r"\S+ where the lines differ",
            aligned_pattern(&token_lists, false),
        );
    }
    offer(
        "typed",
        "value shapes where they differ",
        aligned_pattern(&token_lists, true),
    );
    offer(
        "exact",
        "just these lines",
        Some(literal_alternation(messages)),
    );
    options
}

/// Derive a regex matching every one of `messages`, generalising the parts where they
/// differ. Always yields a pattern that matches exactly the lines it was given (and
/// their variants), so pressing `H` on any selection is never a dead end.
///
/// This is the template `H` starts on; `pattern_candidates` offers the looser ones
/// alongside it. Three strategies, most general first:
/// 1. Same token count: differing tokens collapse to the tightest class that covers them
///    (`\d+`, a UUID, an IP), or `\S+` when they share no shape. Positions stay aligned.
/// 2. Enough shared tokens: join the tokens common to all with `.*`.
/// 3. Otherwise the lines are not variants of one template, so match them literally.
///    A near-empty token subsequence like `the` would otherwise hide half the file.
pub fn common_message_pattern(messages: &[&str]) -> Option<String> {
    if messages.len() < 2 {
        return None;
    }
    let token_lists = tokenize(messages);
    if token_lists.iter().all(|tokens| tokens.is_empty()) {
        return None;
    }

    if let Some(pattern) = aligned_pattern(&token_lists, true) {
        return Some(pattern);
    }

    let shortest = token_lists
        .iter()
        .map(|tokens| tokens.len())
        .min()
        .unwrap_or(0);
    // Require the shared tokens to carry at least half the shortest message, otherwise
    // the "template" is mostly wildcard. `pattern_candidates` offers it anyway, but with
    // a count beside it: there the user can see what it would cost.
    if shortest > 0 && common_tokens(&token_lists).len() * 2 >= shortest {
        return subsequence_pattern(&token_lists);
    }
    Some(literal_alternation(messages))
}

/// Generalise a *single* message into a template, for a selection of one line where there
/// is no second line to diff against. Only tokens with a recognisable value shape give
/// way; every other word stays literal, so the template still says what the line said.
pub fn message_template(message: &str) -> Option<String> {
    let tokens: Vec<&str> = message.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // Nothing generalised, or nothing survived: the message itself is the pattern.
    aligned_pattern(&[tokens], true).or_else(|| Some(regex::escape(message.trim())))
}

fn tokenize<'a>(messages: &[&'a str]) -> Vec<Vec<&'a str>> {
    messages
        .iter()
        .map(|message| message.split_whitespace().collect())
        .collect()
}

/// Every message read as the same template, token position by token position. `typed`
/// picks the tightest class for a differing token; otherwise it becomes `\S+`.
/// `None` when the messages have different token counts, or when the result would be
/// nothing but wildcards.
fn aligned_pattern(token_lists: &[Vec<&str>], typed: bool) -> Option<String> {
    let width = token_lists.first()?.len();
    if width == 0 || !token_lists.iter().all(|tokens| tokens.len() == width) {
        return None;
    }
    let lone = token_lists.len() == 1;

    let parts: Vec<TokenPattern> = (0..width)
        .map(|index| {
            let column: Vec<&str> = token_lists.iter().map(|tokens| tokens[index]).collect();
            match (typed, lone) {
                (true, true) => lone_token_pattern(column[0]),
                (true, false) => column_pattern(&column),
                (false, _) => wildcard_column(&column),
            }
        })
        .collect();
    join_tokens(&parts)
}

/// The tokens common to every message, in order. `None` shared means no template.
fn common_tokens<'a>(token_lists: &[Vec<&'a str>]) -> Vec<&'a str> {
    let Some(first) = token_lists.first() else {
        return Vec::new();
    };
    token_lists[1..].iter().fold(first.clone(), |acc, tokens| {
        common_subsequence(&acc, tokens)
    })
}

fn subsequence_pattern(token_lists: &[Vec<&str>]) -> Option<String> {
    let common = common_tokens(token_lists);
    if common.is_empty() {
        return None;
    }
    Some(
        common
            .iter()
            .map(|token| regex::escape(token))
            .collect::<Vec<_>>()
            .join(".*"),
    )
}

/// The run of leading tokens every message opens with, then anything. Survives messages
/// of different lengths, which the aligned strategies cannot.
fn prefix_pattern(token_lists: &[Vec<&str>]) -> Option<String> {
    let first = token_lists.first()?;
    let mut shared = 0;
    while shared < first.len()
        && token_lists
            .iter()
            .all(|tokens| tokens.get(shared) == first.get(shared))
    {
        shared += 1;
    }
    if shared == 0 {
        return None;
    }
    let head = first[..shared]
        .iter()
        .map(|token| regex::escape(token))
        .collect::<Vec<_>>()
        .join(r"\s+");
    Some(format!("{head}.*"))
}

/// The regex for one token column when no value shape is wanted: the literal, or `\S+`.
fn wildcard_column(column: &[&str]) -> TokenPattern {
    let first = column[0];
    if column.iter().all(|token| *token == first) {
        return TokenPattern {
            regex: regex::escape(first),
            anchored: true,
        };
    }
    TokenPattern {
        regex: r"\S+".to_string(),
        anchored: false,
    }
}

/// `\s+` between the token patterns, but only if at least one holds literal text.
fn join_tokens(parts: &[TokenPattern]) -> Option<String> {
    if !parts.iter().any(|part| part.anchored) {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|part| part.regex.as_str())
            .collect::<Vec<_>>()
            .join(r"\s+"),
    )
}

/// The regex for one token position across every selected line.
fn column_pattern(column: &[&str]) -> TokenPattern {
    let first = column[0];
    if column.iter().all(|token| *token == first) {
        return TokenPattern {
            regex: regex::escape(first),
            anchored: true,
        };
    }

    // Whole tokens first. `0x800424FB` and `0x8004010C` share the prefix `0x8004`, and
    // splitting on it would hide the `0x` that makes them hex in the first place.
    if let Some(class) = value_class(column) {
        return TokenPattern {
            regex: class.to_string(),
            anchored: false,
        };
    }

    // `id=1` and `id=2` are not values, but they differ only in a tail that is one.
    let head = common_prefix_len(column);
    let tails: Vec<&str> = column.iter().map(|token| &token[head..]).collect();
    let foot = common_suffix_len(&tails);
    let cores: Vec<&str> = tails
        .iter()
        .map(|tail| &tail[..tail.len() - foot])
        .collect();

    let Some(class) = value_class(&cores) else {
        // No shared shape. An affix alone would only over-fit the lines at hand.
        return TokenPattern {
            regex: r"\S+".to_string(),
            anchored: false,
        };
    };
    let prefix = regex::escape(&first[..head]);
    let suffix = regex::escape(&tails[0][tails[0].len() - foot..]);
    TokenPattern {
        anchored: !prefix.is_empty() || !suffix.is_empty(),
        regex: format!("{prefix}{class}{suffix}"),
    }
}

/// The regex for one token of a lone line: the value inside it gives way, the rest stays.
///
/// The value is the *longest* tail of the token that has a shape and begins where a value
/// plausibly could -- at the token's start, or after punctuation. So `retry=4;` yields
/// `retry=\d+;` while `30s,` is left alone: `s,` is not a value and `30s` is not a shape.
fn lone_token_pattern(token: &str) -> TokenPattern {
    let literal = TokenPattern {
        regex: regex::escape(token),
        anchored: true,
    };

    let lead = token.len() - token.trim_start_matches(AFFIX).len();
    let body = token[lead..].trim_end_matches(AFFIX);
    if body.is_empty() {
        return literal;
    }

    for (offset, _) in body.char_indices() {
        let core = &body[offset..];
        // A value starts the token or follows punctuation; `y=1`'s `1` qualifies, `1`
        // inside `x1` does not.
        let after_affix = offset == 0 || body[..offset].ends_with(AFFIX);
        if !after_affix {
            continue;
        }
        let Some(class) = value_class(&[core]) else {
            continue;
        };
        let head = regex::escape(&token[..lead + offset]);
        let tail = regex::escape(&token[lead + offset + core.len()..]);
        return TokenPattern {
            anchored: !head.is_empty() || !tail.is_empty(),
            regex: format!("{head}{class}{tail}"),
        };
    }
    literal
}

/// The tightest class covering every value, or `None` when they share no shape.
fn value_class(values: &[&str]) -> Option<&'static str> {
    if values.iter().any(|value| value.is_empty()) {
        return None;
    }
    VALUE_CLASSES
        .iter()
        .find(|(belongs, _)| values.iter().all(|value| belongs(value)))
        .map(|(_, class)| *class)
}

fn is_integer(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_decimal(value: &str) -> bool {
    match value.split_once(['.', ',']) {
        Some((whole, fraction)) => is_integer(whole) && is_integer(fraction),
        None => false,
    }
}

fn is_prefixed_hex(value: &str) -> bool {
    let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A bare hex id such as `800424FB`. Short runs and pure words (`decade`) are excluded:
/// they are far more likely to be real text than an identifier.
fn is_bare_hex(value: &str) -> bool {
    value.len() >= 6
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().any(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    if groups.len() != 5 {
        return false;
    }
    groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, width)| {
        group.len() == width && group.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn is_ipv4(value: &str) -> bool {
    let octets: Vec<&str> = value.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|octet| {
            is_integer(octet) && octet.parse::<u16>().is_ok_and(|number| number <= 255)
        })
}

fn is_single_quoted(value: &str) -> bool {
    value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'')
}

fn is_double_quoted(value: &str) -> bool {
    value.len() >= 2 && value.starts_with('"') && value.ends_with('"')
}

/// Byte length of the longest prefix shared by every value, on a char boundary.
fn common_prefix_len(values: &[&str]) -> usize {
    let first = values[0];
    let mut shared = 0;
    for (offset, ch) in first.char_indices() {
        let end = offset + ch.len_utf8();
        let agrees = values[1..].iter().all(|value| {
            value.len() >= end && value.is_char_boundary(end) && value[..end] == first[..end]
        });
        if !agrees {
            break;
        }
        shared = end;
    }
    shared
}

/// Byte length of the longest suffix shared by every value, on a char boundary.
fn common_suffix_len(values: &[&str]) -> usize {
    let first = values[0];
    let mut shared = 0;
    for (offset, _) in first.char_indices().rev() {
        let take = first.len() - offset;
        let agrees = values[1..].iter().all(|value| {
            value.len() >= take
                && value.is_char_boundary(value.len() - take)
                && value[value.len() - take..] == first[offset..]
        });
        if !agrees {
            break;
        }
        shared = take;
    }
    shared
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

/// Save one filter into `folder` as its own JSON, returning where it landed.
///
/// The counterpart to saving a single schema: `X` on a filter puts it in the user library so
/// every project can pick it up with `L`. A name already taken gets a `-2`, `-3` suffix
/// rather than overwriting -- two different rules can reasonably describe themselves the
/// same way, and silently replacing the older one would lose it.
pub fn save_filter_file(file: &FilterFile, folder: &Path) -> io::Result<PathBuf> {
    let body = serde_json::to_string_pretty(file).map_err(io::Error::other)?;
    write_library_file(folder, &sanitize_file_component(&file.name), &body)
}

/// Write `body` to `folder/<stem>.json`, stepping the name aside if it is taken.
pub(crate) fn write_library_file(folder: &Path, stem: &str, body: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(folder)?;
    let stem = if stem.is_empty() { "item" } else { stem };
    let mut path = folder.join(format!("{stem}.json"));
    for suffix in 2..100 {
        if !path.exists() {
            break;
        }
        path = folder.join(format!("{stem}-{suffix}.json"));
    }
    fs::write(&path, body)?;
    Ok(path)
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

#[cfg(test)]
mod tests {
    use super::resolve_home;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn home_dir_prefers_home_then_falls_back_to_windows() {
        // HOME wins when present.
        assert_eq!(
            resolve_home(env(&[("HOME", "/home/u"), ("USERPROFILE", r"C:\Users\u")])),
            Some(PathBuf::from("/home/u"))
        );
        // Windows leaves HOME unset, so USERPROFILE is used.
        assert_eq!(
            resolve_home(env(&[("USERPROFILE", r"C:\Users\u")])),
            Some(PathBuf::from(r"C:\Users\u"))
        );
        // Failing that, HOMEDRIVE + HOMEPATH concatenate.
        assert_eq!(
            resolve_home(env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\u")])),
            Some(PathBuf::from(r"C:\Users\u"))
        );
        // An empty HOME is ignored rather than yielding an empty path.
        assert_eq!(
            resolve_home(env(&[("HOME", ""), ("USERPROFILE", r"C:\Users\u")])),
            Some(PathBuf::from(r"C:\Users\u"))
        );
        // Nothing set at all.
        assert_eq!(resolve_home(env(&[])), None);
    }
}
