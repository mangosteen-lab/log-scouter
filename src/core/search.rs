use crate::core::filters::contains_ignore_case;
use crate::core::models::{LogEntry, LogFileModel};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub predicates: Vec<Predicate>,
    pub error: String,
}

#[derive(Debug, Clone)]
pub enum Predicate {
    Substring(String),
    Regex(Regex),
    FieldEq {
        field: String,
        value: String,
    },
    FieldContains {
        field: String,
        value: String,
    },
    FieldRegex {
        field: String,
        regex: Regex,
    },
    After(NaiveDateTime),
    Before(NaiveDateTime),
    DateRange {
        lo: Option<NaiveDateTime>,
        hi: Option<NaiveDateTime>,
    },
}

impl Query {
    pub fn matches(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        self.predicates
            .iter()
            .all(|predicate| predicate.matches(file, entry))
    }

    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

impl Predicate {
    pub fn matches(&self, file: &LogFileModel, entry: &LogEntry) -> bool {
        match self {
            // `needle` is already lower-cased by `compile_query`; comparing without
            // allocating a lower-cased copy of every log line matters on big files.
            Predicate::Substring(needle) => contains_ignore_case(&entry.raw, needle),
            Predicate::Regex(regex) => regex.is_match(&entry.raw),
            Predicate::FieldEq { field, value } => {
                file.with_field(entry, field, |val| val == *value)
            }
            Predicate::FieldContains { field, value } => {
                file.with_field(entry, field, |val| contains_ignore_case(val, value))
            }
            Predicate::FieldRegex { field, regex } => {
                file.with_field(entry, field, |val| regex.is_match(val))
            }
            Predicate::After(dt) => file.timestamp(entry).map(|ts| ts >= *dt).unwrap_or(false),
            Predicate::Before(dt) => file.timestamp(entry).map(|ts| ts <= *dt).unwrap_or(false),
            Predicate::DateRange { lo, hi } => {
                let Some(ts) = file.timestamp(entry) else {
                    return false;
                };
                if lo.map(|lo| ts < lo).unwrap_or(false) {
                    return false;
                }
                if hi.map(|hi| ts > hi).unwrap_or(false) {
                    return false;
                }
                true
            }
        }
    }
}

pub fn compile_query(text: &str) -> Query {
    let mut predicates = Vec::new();
    let mut errors = Vec::new();
    if text.trim().is_empty() {
        return Query {
            text: text.to_string(),
            predicates,
            error: String::new(),
        };
    }

    let tokens = shell_words::split(text).unwrap_or_else(|_| {
        text.split_whitespace()
            .map(|token| token.to_string())
            .collect()
    });

    for token in tokens {
        match compile_token(&token) {
            Ok(predicate) => predicates.push(predicate),
            Err(error) => errors.push(format!("{token:?}: {error}")),
        }
    }

    Query {
        text: text.to_string(),
        predicates,
        error: errors.join("; "),
    }
}

pub fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    let normalized = text
        .trim()
        .trim_matches(['\'', '"'])
        .replace(['T', '_'], " ");
    if normalized.is_empty() {
        return None;
    }

    for fmt in [
        "%Y-%m-%d %H:%M:%S.%f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d %H",
        "%Y/%m/%d %H:%M:%S",
    ] {
        if fmt.contains("%f") {
            if let Some(dt) = crate::core::extractor::parse_timestamp_with_format(&normalized, fmt)
            {
                return Some(dt);
            }
        }
        if let Ok(dt) = NaiveDateTime::parse_from_str(&normalized, fmt) {
            return Some(dt);
        }
    }

    for fmt in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(&normalized, fmt) {
            return date.and_hms_opt(0, 0, 0);
        }
    }

    for fmt in ["%H:%M:%S.%f", "%H:%M:%S", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(&normalized, fmt) {
            let date = NaiveDate::from_ymd_opt(1900, 1, 1)?;
            return Some(NaiveDateTime::new(date, time));
        }
    }

    None
}

fn compile_token(token: &str) -> Result<Predicate, String> {
    if let Some(body) = token.strip_prefix("after:") {
        let dt = parse_datetime(body).ok_or_else(|| "unrecognised date".to_string())?;
        return Ok(Predicate::After(dt));
    }
    if let Some(body) = token.strip_prefix("before:") {
        let dt = parse_datetime(body).ok_or_else(|| "unrecognised date".to_string())?;
        return Ok(Predicate::Before(dt));
    }
    if let Some(body) = token.strip_prefix("date:") {
        let body = body.trim().trim_start_matches('[').trim_end_matches(']');
        let (lo_s, hi_s) = body.split_once("..").unwrap_or((body, ""));
        let lo = if lo_s.trim().is_empty() {
            None
        } else {
            parse_datetime(lo_s)
        };
        let hi = if hi_s.trim().is_empty() {
            None
        } else {
            parse_datetime(hi_s)
        };
        if lo.is_none() && hi.is_none() {
            return Err("empty date range".to_string());
        }
        return Ok(Predicate::DateRange { lo, hi });
    }

    if let Some(field_predicate) = compile_field_token(token)? {
        return Ok(field_predicate);
    }

    if token.len() >= 2 && token.starts_with('/') && token.ends_with('/') {
        let regex = Regex::new(&format!("(?i){}", &token[1..token.len() - 1]))
            .map_err(|exc| exc.to_string())?;
        return Ok(Predicate::Regex(regex));
    }

    Ok(Predicate::Substring(token.to_lowercase()))
}

fn compile_field_token(token: &str) -> Result<Option<Predicate>, String> {
    let token_re =
        Regex::new(r"^([a-zA-Z_][a-zA-Z0-9_]*)([=~])(.*)$").map_err(|exc| exc.to_string())?;
    let Some(captures) = token_re.captures(token) else {
        return Ok(None);
    };
    let field = captures[1].to_string();
    let op = &captures[2];
    let value = captures[3].to_string();

    if op == "~" {
        return Ok(Some(Predicate::FieldContains {
            field,
            value: value.to_lowercase(),
        }));
    }

    if value.len() >= 2 && value.starts_with('/') && value.ends_with('/') {
        let regex = Regex::new(&format!("(?i){}", &value[1..value.len() - 1]))
            .map_err(|exc| exc.to_string())?;
        return Ok(Some(Predicate::FieldRegex { field, regex }));
    }

    Ok(Some(Predicate::FieldEq { field, value }))
}
