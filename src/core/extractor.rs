use crate::core::filters::{home_dir, json_file_paths, sanitize_file_component, USER_DIR};
use chrono::{NaiveDate, NaiveDateTime};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const BRACKETED_DEFAULT_FORMAT: &str = "<timestamp> [HOST:<host>][SERVER:<server>][PID:<process_id>][THR:<thread_id>][<log_module>][<log_level>][<error_code?>][UID:<user_id>][SID:<session_id>][OID:<object_id>][<file_name>:<line_number>] <message>";

/// The same format before `<error_code?>` existed. Projects saved back then stored it
/// verbatim, and under it an error line's `[0x800424FB]` is swallowed by `<log_level>`
/// (which becomes `Error][0x800424FB`). Recognised on load so those projects heal.
pub const BRACKETED_LEGACY_FORMAT: &str = "<timestamp> [HOST:<host>][SERVER:<server>][PID:<process_id>][THR:<thread_id>][<log_module>][<log_level>][UID:<user_id>][SID:<session_id>][OID:<object_id>][<file_name>:<line_number>] <message>";

pub const DEFAULT_TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S.%f";

/// Name of the built-in bracketed-field schema. Used as the deterministic fallback when
/// no schema matches a file.
pub const DEFAULT_EXTRACTOR_NAME: &str = "Bracketed default";

/// Name of the built-in catch-all schema, and the format behind it.
///
/// It asserts nothing beyond "a line is a record", which is exactly what makes it safe as
/// the last resort: `detect` orders candidates by specificity, so it only wins when no
/// real schema explains the file. Before it existed that file was handed to the bracketed
/// format instead, whose `is_start` probe never matched -- so every line folded into the
/// one above it and the whole file arrived as a single, timestamp-less entry.
pub const GENERIC_EXTRACTOR_NAME: &str = "Generic line";
pub const GENERIC_FORMAT: &str = "<message>";

/// How many of a file's leading lines `detect` reads before deciding on a schema.
pub const DETECT_LINES: usize = 200;

/// A log line the schema claims to parse, with what it should parse *to*. Checked by
/// `compile`, so a schema that quietly mis-parses cannot be saved or imported.
///
/// This exists because a format can match a line and still be wrong: before
/// `<error_code?>`, the bracketed format matched an error line but produced
/// `log_level = "Error][0x800424FB"`, and every `level equals Error` filter silently
/// dropped it. A sample asserting `level: "Error"` turns that into a load-time error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleLine {
    pub line: String,
    /// Expected value of the schema's level field, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

impl SampleLine {
    pub fn new(line: impl Into<String>) -> Self {
        Self {
            line: line.into(),
            level: None,
        }
    }

    pub fn with_level(line: impl Into<String>, level: impl Into<String>) -> Self {
        Self {
            line: line.into(),
            level: Some(level.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extractor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub format: String,
    #[serde(default = "default_timestamp_field")]
    pub timestamp_field: String,
    #[serde(default = "default_timestamp_format")]
    pub timestamp_format: String,
    #[serde(default)]
    pub field_patterns: HashMap<String, String>,
    /// Optional regex tested against each physical line. When present, a matching line
    /// starts a new logical log entry even if the full format spans several lines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entry_start: String,
    /// Optional regex tested against each physical line. When present, a matching line
    /// closes the current logical log entry after that line is appended.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entry_end: String,
    /// Lines this schema must parse correctly. Empty is allowed, but then nothing stops
    /// the schema from being subtly wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<SampleLine>,
    #[serde(skip)]
    regex: Option<Regex>,
    #[serde(skip)]
    start_regex: Option<Regex>,
    /// Anchored regex covering every field *except* a trailing free-text one. Matching
    /// stops at the header, so a multi-KB message is never fed to the capture engine.
    #[serde(skip)]
    head_regex: Option<Regex>,
    /// Name of that trailing field, whose value is the rest of the line.
    #[serde(skip)]
    tail_field: Option<String>,
    #[serde(skip)]
    end_regex: Option<Regex>,
    #[serde(skip)]
    pub field_names: Vec<String>,
}

#[derive(Debug, Clone)]
struct Placeholder {
    name: String,
    start: usize,
    end: usize,
    /// Written `<name?>`. The field *and the literal separator in front of it* may be
    /// absent from a line; see `compile`.
    optional: bool,
}

fn default_timestamp_field() -> String {
    "timestamp".to_string()
}

fn default_timestamp_format() -> String {
    DEFAULT_TIMESTAMP_FORMAT.to_string()
}

impl Extractor {
    pub fn new(name: impl Into<String>, format: impl Into<String>) -> Result<Self, String> {
        let mut extractor = Self {
            name: name.into(),
            description: String::new(),
            format: format.into(),
            timestamp_field: default_timestamp_field(),
            timestamp_format: default_timestamp_format(),
            field_patterns: HashMap::new(),
            entry_start: String::new(),
            entry_end: String::new(),
            samples: Vec::new(),
            regex: None,
            start_regex: None,
            head_regex: None,
            tail_field: None,
            end_regex: None,
            field_names: Vec::new(),
        };
        extractor.compile()?;
        Ok(extractor)
    }

    pub fn with_timestamp_format(
        name: impl Into<String>,
        format: impl Into<String>,
        timestamp_format: impl Into<String>,
    ) -> Result<Self, String> {
        let mut extractor = Self::new(name, format)?;
        extractor.timestamp_format = timestamp_format.into();
        extractor.compile()?;
        Ok(extractor)
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Recompiles, so the samples are checked as they are attached rather than later.
    pub fn with_samples(mut self, samples: Vec<SampleLine>) -> Result<Self, String> {
        self.samples = samples;
        self.compile()?;
        Ok(self)
    }

    /// Build the regexes **and** check the samples. Use when a schema is being defined or
    /// imported: that is the moment to reject one that mis-parses.
    pub fn compile(&mut self) -> Result<(), String> {
        self.build()?;
        self.validate_samples()
    }

    /// Build the regexes only. Use when reading a schema already stored in a project: it
    /// passed `compile` when it was added, and dropping it now because a sample stopped
    /// matching would silently repoint every file that uses it.
    pub fn compile_relaxed(&mut self) -> Result<(), String> {
        self.build()
    }

    fn build(&mut self) -> Result<(), String> {
        let placeholders = placeholders(&self.format)?;
        // Anchored: a format template describes a line from its start. The leading field
        // is unconstrained, so an unanchored search would match at 0 anyway -- but the
        // engine would still try every offset first, which dominates on long lines.
        //
        // `(?s)` so `.` matches newlines: a block schema (`{ ... }` over many physical
        // lines) has fields -- and, worse, a `message` value -- that straddle line breaks.
        // Without it the whole-entry regex refuses such a record, extraction returns no
        // fields, and every column but the raw message renders blank. `field_pattern`
        // carries the same flag for the same reason.
        let mut pattern = String::from("(?s)^");
        let mut names = Vec::with_capacity(placeholders.len());
        let mut pos = 0;
        // Byte length of `pattern` just before the last placeholder's group is appended,
        // i.e. everything needed to consume the header.
        let mut head_len = None;

        for (index, ph) in placeholders.iter().enumerate() {
            let literal = regex::escape(&self.format[pos..ph.start]);
            names.push(ph.name.clone());
            let custom = self.field_patterns.get(&ph.name).cloned();
            let last = index + 1 == placeholders.len();
            let sub_pattern = custom.clone().unwrap_or_else(|| {
                if last {
                    ".*".to_string()
                } else {
                    ".*?".to_string()
                }
            });
            let group = format!("(?P<{}>{})", ph.name, sub_pattern);

            if ph.optional {
                // An optional field swallows the separator *before* it, not after. For
                // `[<log_level>][<error_code?>][UID:<user_id>]` the two shapes on disk are
                // `[Error][0x80..][UID:u]` and `[Error][UID:u]`, so what disappears with
                // the code is the `][` in front of it -- dropping the one behind would
                // leave `[Error][u]`.
                pattern.push_str("(?:");
                pattern.push_str(&literal);
                pattern.push_str(&group);
                pattern.push_str(")?");
            } else {
                pattern.push_str(&literal);
                // The trailing free-text field runs to end-of-line, so the header regex
                // stops here. An optional last field has no fixed start, so it cannot
                // play that role.
                if last && custom.is_none() && ph.end == self.format.len() {
                    head_len = Some(pattern.len());
                }
                pattern.push_str(&group);
            }
            pos = ph.end;
        }
        pattern.push_str(&regex::escape(&self.format[pos..]));

        let regex =
            Regex::new(&pattern).map_err(|exc| format!("Invalid extractor pattern: {exc}"))?;
        let start_regex = if self.entry_start.trim().is_empty() {
            build_start_regex(&self.format, &placeholders).unwrap_or_else(|| regex.clone())
        } else {
            Regex::new(self.entry_start.trim())
                .map_err(|exc| format!("Invalid entry_start pattern: {exc}"))?
        };
        let end_regex = if self.entry_end.trim().is_empty() {
            None
        } else {
            Some(
                Regex::new(self.entry_end.trim())
                    .map_err(|exc| format!("Invalid entry_end pattern: {exc}"))?,
            )
        };

        // The trailing field is "everything after the header", so it needs no group.
        let (head_regex, tail_field) = match head_len {
            Some(head_len) => (
                Regex::new(&pattern[..head_len]).ok(),
                placeholders.last().map(|ph| ph.name.clone()),
            ),
            None => (None, None),
        };

        self.regex = Some(regex);
        self.start_regex = Some(start_regex);
        self.head_regex = head_regex;
        self.tail_field = tail_field;
        self.end_regex = end_regex;
        self.field_names = names;
        Ok(())
    }

    /// Every sample must parse, and parse to what it says. A schema that matches a line
    /// but extracts the wrong value is worse than one that does not match at all: the
    /// wrong value flows into filters and searches without a word.
    fn validate_samples(&self) -> Result<(), String> {
        for (index, sample) in self.samples.iter().enumerate() {
            let position = index + 1;
            let Some(fields) = self.extract(&sample.line) else {
                return Err(format!(
                    "sample {position} does not match the format: {:?}",
                    truncate(&sample.line, 60)
                ));
            };

            let Some(expected) = sample.level.as_deref() else {
                continue;
            };
            let Some(level_field) = self.level_field() else {
                return Err(format!(
                    "sample {position} expects level {expected:?} but the format has no level field"
                ));
            };
            let parsed = fields.get(level_field).map(String::as_str).unwrap_or("");
            if parsed != expected {
                return Err(format!(
                    "sample {position} parsed level {parsed:?}, expected {expected:?}"
                ));
            }
        }
        Ok(())
    }

    /// The capture group holding the severity, under any of its usual spellings.
    pub fn level_field(&self) -> Option<&str> {
        ["log_level", "level", "severity"]
            .into_iter()
            .find(|candidate| self.field_names.iter().any(|name| name == candidate))
    }

    /// How many of `lines` this schema parses. `detect` compares schemas on this.
    pub fn match_score(&self, lines: &[String]) -> usize {
        self.match_coverage(lines).0
    }

    /// `(matching entries, total entries)` after applying this schema's entry-boundary
    /// rules. Block formats often match zero physical lines but every grouped record.
    pub fn match_coverage(&self, lines: &[String]) -> (usize, usize) {
        let entries = crate::core::parser::build_entries(lines, Some(self));
        let total = entries
            .iter()
            .filter(|entry| !entry.raw.trim().is_empty())
            .count();
        let matched = entries
            .iter()
            .filter(|entry| !entry.raw.trim().is_empty())
            .filter(|entry| self.captures(&entry.raw).is_some())
            .count();
        (matched, total)
    }

    /// A rough "how much does this format actually assert" measure, used only to break a
    /// tie in `detect`. A format that is one bare `<message>` asserts nothing and must
    /// lose to one that pins down fourteen fields and their punctuation.
    pub fn specificity(&self) -> usize {
        let placeholder_chars: usize = self
            .field_names
            .iter()
            .map(|name| name.len() + 2)
            .sum::<usize>();
        let literal_chars = self.format.len().saturating_sub(placeholder_chars);
        self.field_names.len() + literal_chars
    }

    pub fn tail_field(&self) -> Option<&str> {
        self.tail_field.as_deref()
    }

    /// Header captures plus the byte offset where the trailing field begins.
    /// Falls back to the full regex when the format has no trailing free-text field.
    pub fn head_captures<'a>(&self, line: &'a str) -> Option<(Captures<'a>, usize)> {
        match (&self.head_regex, &self.tail_field) {
            (Some(head), Some(_)) => {
                let captures = head.captures(line)?;
                let tail_start = captures.get(0)?.end();
                Some((captures, tail_start))
            }
            _ => {
                let captures = self.captures(line)?;
                let tail_start = captures.get(0).map(|m| m.end()).unwrap_or(line.len());
                Some((captures, tail_start))
            }
        }
    }

    /// A regex over a whole raw line that pins each field in `chosen` to its value and
    /// lets every other field be anything.
    ///
    /// This is how the hide menu ANDs fields. `regex` has no lookaround, so "host is h1
    /// *and* level is Trace" cannot be written as two independent tests over one string.
    /// The format template already says where each field sits, so the conjunction becomes
    /// a single positional pattern instead.
    ///
    /// `(?s)` because an entry's raw text carries its continuation lines, and a `.*` that
    /// stopped at the first newline would refuse every multi-line record.
    pub fn field_pattern(&self, chosen: &[(String, String)]) -> Option<String> {
        let placeholders = placeholders(&self.format).ok()?;
        if placeholders.is_empty() {
            return None;
        }

        let mut pattern = String::from("(?s)^");
        let mut pos = 0;
        for (index, ph) in placeholders.iter().enumerate() {
            let literal = regex::escape(&self.format[pos..ph.start]);
            let pinned = chosen
                .iter()
                .find(|(name, _)| *name == ph.name)
                .map(|(_, value)| value.as_str());
            let last = index + 1 == placeholders.len();
            let free = self
                .field_patterns
                .get(&ph.name)
                .cloned()
                .unwrap_or_else(|| {
                    if last {
                        ".*".to_string()
                    } else {
                        ".*?".to_string()
                    }
                });

            match (pinned, ph.optional) {
                // An optional field the source line did not have. Its separator lives
                // inside the group, so dropping the group outright forbids the field --
                // which is exactly what "hide lines with no error code" means.
                (Some(""), true) => {}
                // Pinned: the field must be there, holding this value.
                (Some(value), _) => {
                    pattern.push_str(&literal);
                    pattern.push_str(&regex::escape(value));
                }
                // Free, and may be absent.
                (None, true) => {
                    pattern.push_str("(?:");
                    pattern.push_str(&literal);
                    pattern.push_str(&free);
                    pattern.push_str(")?");
                }
                (None, false) => {
                    pattern.push_str(&literal);
                    pattern.push_str(&free);
                }
            }
            pos = ph.end;
        }
        pattern.push_str(&regex::escape(&self.format[pos..]));
        Some(pattern)
    }

    pub fn is_start(&self, line: &str) -> bool {
        self.start_regex()
            .map(|regex| regex.is_match(line))
            .unwrap_or(true)
    }

    pub fn is_end(&self, line: &str) -> bool {
        self.end_regex
            .as_ref()
            .map(|regex| regex.is_match(line))
            .unwrap_or(false)
    }

    pub fn uses_explicit_entry_boundary(&self) -> bool {
        !self.entry_start.trim().is_empty() || !self.entry_end.trim().is_empty()
    }

    pub fn captures<'a>(&self, line: &'a str) -> Option<Captures<'a>> {
        self.regex().and_then(|regex| regex.captures(line))
    }

    pub fn extract(&self, line: &str) -> Option<HashMap<String, String>> {
        let captures = self.captures(line)?;
        let mut fields = HashMap::with_capacity(self.field_names.len());
        for name in &self.field_names {
            let value = captures
                .name(name)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            fields.insert(name.clone(), value);
        }
        Some(fields)
    }

    pub fn parse_timestamp(&self, fields: &HashMap<String, String>) -> Option<NaiveDateTime> {
        fields
            .get(&self.timestamp_field)
            .and_then(|raw| parse_timestamp_with_format(raw, &self.timestamp_format))
    }

    fn regex(&self) -> Option<&Regex> {
        self.regex.as_ref()
    }

    fn start_regex(&self) -> Option<&Regex> {
        self.start_regex.as_ref()
    }
}

impl Default for Extractor {
    fn default() -> Self {
        default_extractor()
    }
}

pub fn default_extractor() -> Extractor {
    Extractor::new(DEFAULT_EXTRACTOR_NAME, BRACKETED_DEFAULT_FORMAT)
        .expect("built-in extractor is valid")
        .with_description("Bracketed server log format")
        .with_samples(bracketed_samples())
        .expect("built-in samples parse under the built-in format")
}

/// The catch-all schema: one record per line, no fields but the text itself.
///
/// It names no timestamp field, so a merge reads times off the raw line with
/// `sniff_timestamp` instead.
pub fn generic_extractor() -> Extractor {
    Extractor::new(GENERIC_EXTRACTOR_NAME, GENERIC_FORMAT)
        .expect("built-in generic extractor is valid")
        .with_description("One record per line, for logs no other schema explains")
}

/// The schemas every project has, whether or not its `project.json` mentions them.
///
/// Only the two structural schemas live here: one is `detect`'s deterministic fallback, the
/// other its catch-all. Schemas for ordinary third-party formats are *bundled* instead (see
/// `bundled_schemas`), which keeps them out of `project.json` and lets a user file of the
/// same name shadow them.
pub fn builtin_extractors() -> Vec<Extractor> {
    vec![default_extractor(), generic_extractor()]
}

/// Schemas for common third-party log formats, compiled into the binary as the same JSON a
/// user schema library holds. They join detection as the lowest-precedence library layer, so
/// a file in `~/.log-scouter/schemas` with the same name wins and nothing here can silently
/// change how an existing project parses.
///
/// A malformed entry here is a build-time mistake, not a user error: `bundled_schemas_tests`
/// compiles every one, which also validates its samples.
pub const BUNDLED_SCHEMA_FILES: &[(&str, &str)] = &[
    (
        "Spring-Boot",
        include_str!("../../schemas/Spring-Boot.json"),
    ),
    (
        "Spring-Boot-3",
        include_str!("../../schemas/Spring-Boot-3.json"),
    ),
    (
        "Tomcat-Catalina",
        include_str!("../../schemas/Tomcat-Catalina.json"),
    ),
    (
        "Tomcat-Access-Log",
        include_str!("../../schemas/Tomcat-Access-Log.json"),
    ),
    (
        "Log4j2-Default",
        include_str!("../../schemas/Log4j2-Default.json"),
    ),
];

/// Every bundled schema, compiled. A schema that fails to parse or compile is skipped rather
/// than panicking: a broken bundle must not stop the app from opening a folder.
pub fn bundled_schemas() -> Vec<Extractor> {
    BUNDLED_SCHEMA_FILES
        .iter()
        .filter_map(|(_, body)| {
            let mut schema = parse_schema_file(body).ok()?.schema;
            schema.compile().ok()?;
            Some(schema)
        })
        .collect()
}

/// A timestamp at the head of a line, read without help from a schema.
///
/// Merging interleaves entries by time, so an entry with no time has nowhere to go. That
/// happens whenever the schema names no timestamp field (`Generic line`) or names one its
/// `timestamp_format` cannot read. Sniffing the line directly rescues both cases.
///
/// Only the ISO-8601 family is recognised, because a wrong timestamp reorders the merge
/// more damagingly than a missing one: `2026-06-16 10:09:43.288`, `2026-06-16T10:09:43,288Z`
/// and `2026/06/16 10:09:43` all parse, each optionally behind `[` or leading space.
pub fn sniff_timestamp(line: &str) -> Option<NaiveDateTime> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"^[\s\[]*(\d{4})[-/](\d{1,2})[-/](\d{1,2})[T ](\d{1,2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?",
        )
        .expect("built-in timestamp sniffer is valid")
    });

    let captures = head.captures(line)?;
    let number = |index: usize| -> Option<u32> { captures.get(index)?.as_str().parse().ok() };
    let year: i32 = captures.get(1)?.as_str().parse().ok()?;
    let date = NaiveDate::from_ymd_opt(year, number(2)?, number(3)?)?;

    // A fraction is written to whatever precision the writer felt like; right-pad to nanos.
    let nanos = match captures.get(7) {
        Some(fraction) => {
            let digits = fraction.as_str();
            let mut padded = String::with_capacity(9);
            padded.push_str(digits);
            for _ in digits.len()..9 {
                padded.push('0');
            }
            padded.parse().ok()?
        }
        None => 0,
    };
    date.and_hms_nano_opt(number(4)?, number(5)?, number(6)?, nanos)
}

/// The two shapes of an bracketed error line, plus an ordinary one. The middle sample is the
/// regression guard: without `<error_code?>` it parses as `Error][0x800424FB`.
fn bracketed_samples() -> Vec<SampleLine> {
    vec![
        SampleLine::with_level(
            "2026-06-16 10:09:43.288 [HOST:h1][SERVER:AppServer][PID:54][THR:136612056716864][Kernel][Trace][UID:0][SID:0][OID:0][ServerDispatcher.cpp:394] NetChannel : Channel is closed.",
            "Trace",
        ),
        SampleLine::with_level(
            "2026-06-16 10:12:08.631 [HOST:h1][SERVER:AppServer][PID:53][THR:135332369409600][Query Engine][Error][0x800424FB][UID:5CCC][SID:B830][OID:72EF][QueryEngine.cpp:6580] We could not obtain the data.",
            "Error",
        ),
        SampleLine::with_level(
            "2026-06-16 10:12:15.744 [HOST:h1][SERVER:AppServer][PID:53][THR:135332369409600][Query Engine][Error][UID:5CCC][SID:B830][OID:72EF][QueryEngine.cpp:6600] Plain error, no code.",
            "Error",
        ),
    ]
}

/// Pick the schema that best explains `lines`, the way lnav locks onto a format.
///
/// Candidates are tried **most specific first**, not best-scoring first. Scoring first
/// would hand every file to the most permissive schema: a bare `<message>` format matches
/// every line, including the stack-trace continuations that a real schema correctly
/// refuses. So the order is specificity, and a schema only has to explain a decent share
/// of the lines to win -- continuation lines legitimately do not match.
///
/// The comparison is total (schema names are unique), so the result never depends on hash
/// iteration order.
pub fn detect<'a, I>(candidates: I, lines: &[String]) -> Option<&'a Extractor>
where
    I: IntoIterator<Item = &'a Extractor>,
{
    let considered = lines.iter().filter(|line| !line.trim().is_empty()).count();
    if considered == 0 {
        return None;
    }

    let mut ordered: Vec<&Extractor> = candidates.into_iter().collect();
    ordered.sort_by(|left, right| {
        right
            .specificity()
            .cmp(&left.specificity())
            .then_with(|| left.name.cmp(&right.name))
    });

    ordered.into_iter().find(|extractor| {
        let (score, considered) = extractor.match_coverage(lines);
        // A quarter of the grouped entries: enough to rule out an accidental match,
        // loose enough for a log that is mostly multi-line stack traces.
        score > 0 && score * 4 >= considered
    })
}

/// The ordered *set* of schemas a source should be parsed with -- one per format present in
/// `lines`. Where `detect` returns the single best, this attributes each line to the
/// most-specific schema that parses it (so a permissive `.*` format cannot swallow another's
/// lines) and keeps every schema that wins at least a fifth of the lines, most specific
/// first. The `Generic line` catch-all is never included: it explains nothing structurally,
/// and an unmatched line already falls back to showing its raw text.
///
/// An interleaved uvicorn + nginx log yields `[nginx, uvicorn]`; a single-format log yields
/// one schema; a log no schema explains yields an empty set (the caller falls back).
pub fn detect_all<'a, I>(candidates: I, lines: &[String]) -> Vec<&'a Extractor>
where
    I: IntoIterator<Item = &'a Extractor>,
{
    let considered = lines.iter().filter(|line| !line.trim().is_empty()).count();
    if considered == 0 {
        return Vec::new();
    }

    let mut ordered: Vec<&Extractor> = candidates
        .into_iter()
        .filter(|extractor| extractor.name != GENERIC_EXTRACTOR_NAME)
        .collect();
    ordered.sort_by(|left, right| {
        right
            .specificity()
            .cmp(&left.specificity())
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut counts = vec![0usize; ordered.len()];
    for line in lines.iter().filter(|line| !line.trim().is_empty()) {
        if let Some(index) = ordered.iter().position(|ex| ex.captures(line).is_some()) {
            counts[index] += 1;
        }
    }

    // A schema earns its place by being the best match for a fifth of the lines -- enough to
    // pick up a genuine second format while ignoring the odd coincidental match.
    let threshold = considered.div_ceil(5).max(1);
    ordered
        .into_iter()
        .zip(counts)
        .filter(|(_, count)| *count >= threshold)
        .map(|(extractor, _)| extractor)
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}...", text.chars().take(max).collect::<String>())
}

pub fn preview_extraction(extractor: &Extractor, sample: &str) -> Vec<(String, String)> {
    let Some(fields) = extractor.extract(sample) else {
        return Vec::new();
    };
    extractor
        .field_names
        .iter()
        .map(|name| (name.clone(), fields.get(name).cloned().unwrap_or_default()))
        .collect()
}

pub fn parse_timestamp_with_format(raw: &str, fmt: &str) -> Option<NaiveDateTime> {
    let raw = raw.trim();
    if fmt.contains("%f") {
        if let Some(normalized) = normalize_python_fraction(raw) {
            if let Ok(parsed) = NaiveDateTime::parse_from_str(&normalized, fmt) {
                return Some(parsed);
            }
        }
    }
    NaiveDateTime::parse_from_str(raw, fmt).ok()
}

pub fn extractor_from_project(mut extractor: Extractor) -> Option<Extractor> {
    extractor.compile_relaxed().ok()?;
    Some(extractor)
}

// ---- schema packs ----------------------------------------------------------------
//
// The same shape as the filter packs (`~/.log-scouter/filters`): one JSON per item
// in a folder, so a schema can be shared between projects and checked into a repo.

pub const USER_SCHEMAS_SUBDIR: &str = "schemas";

/// `~/.log-scouter/schemas`, or `None` when `$HOME` is unset.
pub fn user_schema_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join(USER_SCHEMAS_SUBDIR))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaFile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub schema: Extractor,
}

pub fn export_schemas_to_folder(schemas: &[Extractor], folder: &Path) -> io::Result<usize> {
    fs::create_dir_all(folder)?;
    for (index, schema) in schemas.iter().enumerate() {
        let schema_file = SchemaFile {
            name: schema.name.clone(),
            description: schema.description.clone(),
            schema: schema.clone(),
        };
        let path = folder.join(format!(
            "{:03}-{}.json",
            index + 1,
            sanitize_file_component(&schema.name)
        ));
        let body = serde_json::to_string_pretty(&schema_file).map_err(io::Error::other)?;
        fs::write(path, body)?;
    }
    Ok(schemas.len())
}

/// Every schema in `folder`, each compiled so a bad `format` is caught here rather than
/// on the first log line. A file naming a schema that fails to compile is an error, not
/// a silent skip.
pub fn load_schemas_from_folder(folder: &Path) -> io::Result<Vec<SchemaFile>> {
    let mut paths = json_file_paths(folder)?;
    paths.sort();

    let mut schemas = Vec::new();
    for path in paths {
        let body = fs::read_to_string(&path)?;
        let mut schema_file = parse_schema_file(&body).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
        schema_file.schema.compile().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
        schemas.push(schema_file);
    }
    Ok(schemas)
}

/// Accept either the wrapped `{name, description, schema}` form or a bare `Extractor`,
/// so a schema copied straight out of `project.json` imports without editing.
fn parse_schema_file(body: &str) -> Result<SchemaFile, serde_json::Error> {
    serde_json::from_str::<SchemaFile>(body).or_else(|_| {
        let schema = serde_json::from_str::<Extractor>(body)?;
        Ok(SchemaFile {
            name: schema.name.clone(),
            description: schema.description.clone(),
            schema,
        })
    })
}

fn placeholders(format: &str) -> Result<Vec<Placeholder>, String> {
    let placeholder_re =
        Regex::new(r"<([a-zA-Z_][a-zA-Z0-9_]*)(\?)?>").map_err(|exc| exc.to_string())?;
    Ok(placeholder_re
        .captures_iter(format)
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            let name = captures.get(1)?.as_str().to_string();
            Some(Placeholder {
                name,
                start: whole.start(),
                end: whole.end(),
                optional: captures.get(2).is_some(),
            })
        })
        .collect())
}

fn build_start_regex(format: &str, placeholders: &[Placeholder]) -> Option<Regex> {
    if placeholders.len() < 2 {
        return None;
    }

    let first = &placeholders[0];
    let second = &placeholders[1];
    // The cheap "does this line start a record" probe keys off the separator between the
    // first two fields. If either may be absent that separator may be too, so fall back
    // to the full regex rather than mis-classify a record as a continuation line.
    if first.optional || second.optional {
        return None;
    }
    let prefix = &format[..first.start];
    let mid = &format[first.end..second.start];
    if mid.is_empty() {
        return None;
    }

    Regex::new(&format!(
        "^{}(.+?){}",
        regex::escape(prefix),
        regex::escape(mid)
    ))
    .ok()
}

fn normalize_python_fraction(raw: &str) -> Option<String> {
    let separator = raw.rfind(|ch| ch == '.' || ch == ',')?;
    let fraction_start = separator + 1;
    let fraction_len = raw[fraction_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if fraction_len == 0 || fraction_len > 6 {
        return None;
    }

    let fraction_end = fraction_start + fraction_len;
    let fraction = &raw[fraction_start..fraction_end];
    let mut normalized = String::with_capacity(raw.len() + 8);
    normalized.push_str(&raw[..fraction_start]);
    normalized.push_str(fraction);
    for _ in fraction_len..6 {
        normalized.push('0');
    }
    normalized.push_str("000");
    normalized.push_str(&raw[fraction_end..]);
    Some(normalized)
}
