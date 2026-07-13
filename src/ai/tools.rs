//! The tools the assistant is offered.
//!
//! This is the *definition* side only -- names, descriptions, and argument schemas, which
//! are provider-neutral data. Execution lives on `AppState` in the tui module, because a
//! tool call has to run against the live project and its panes; keeping the schemas here
//! lets them be checked without any of that.

use crate::ai::message::ToolSpec;
use serde_json::json;

/// Tool names, in one place so the definition list and the dispatcher cannot drift.
pub const LIST_SOURCES: &str = "list_sources";
pub const LIST_FILTERS: &str = "list_filters";
pub const SAMPLE_LINES: &str = "sample_lines";
pub const COUNT_MATCHES: &str = "count_matches";
pub const LEVEL_BREAKDOWN: &str = "level_breakdown";
pub const ADD_FILTER: &str = "add_filter";
pub const SET_TIME_RANGE: &str = "set_time_range";
pub const SEARCH: &str = "search";
pub const ADD_SOURCE: &str = "add_source";

/// Every tool the model may call, read tools first so it inspects before it acts.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            LIST_SOURCES,
            "List the log sources in the project: display name, assigned log schema, entry \
             count, and load state. Call this first to see what data is available.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            LIST_FILTERS,
            "List the filters currently applied to the project (text filters and the one \
             time-range filter), so you know what is already narrowing the view.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            SAMPLE_LINES,
            "Return the first N raw lines of the focused pane's current view, so you can see \
             the actual log text before deciding what to do.",
            json!({
                "type": "object",
                "properties": {
                    "count": {
                        "type": "integer",
                        "description": "How many lines to return (1-50).",
                    }
                },
            }),
        ),
        spec(
            COUNT_MATCHES,
            "Count how many entries in the focused log match a query, using the same query \
             language as the search box (bare text, \"quoted phrase\", /regex/, field=value, \
             after:<ts>). Use this to measure how much noise a pattern covers before hiding it.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The query to count."}
                },
                "required": ["query"],
            }),
        ),
        spec(
            LEVEL_BREAKDOWN,
            "Return a histogram of the log-level field over the focused log (Error, Warn, \
             Info, ...), so you can see the severity mix at a glance.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            ADD_FILTER,
            "Add a project-wide filter. `action` is \"exclude\" to hide matching lines or \
             \"include\" to keep only matching lines. `op` is one of equals, contains, regex. \
             The view updates immediately; the tool result reports the before/after row count.",
            json!({
                "type": "object",
                "properties": {
                    "field": {"type": "string", "description": "Field to match, e.g. message, level, host, or 'raw' for the whole line."},
                    "op": {"type": "string", "enum": ["equals", "contains", "regex"]},
                    "value": {"type": "string", "description": "The value or pattern."},
                    "action": {"type": "string", "enum": ["exclude", "include"]},
                },
                "required": ["field", "op", "value", "action"],
            }),
        ),
        spec(
            SET_TIME_RANGE,
            "Restrict the view to a time window. Timestamps are 'YYYY-MM-DD HH:MM:SS'. Either \
             end may be omitted for an open range. Replaces any existing time filter.",
            json!({
                "type": "object",
                "properties": {
                    "start": {"type": "string", "description": "Inclusive start, or empty for open."},
                    "end": {"type": "string", "description": "Inclusive end, or empty for open."},
                },
            }),
        ),
        spec(
            SEARCH,
            "Run a search over the focused log and jump the view to it. Uses the search query \
             language (bare text, \"phrase\", /regex/, field=value, after:/before:).",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The search query."}
                },
                "required": ["query"],
            }),
        ),
        spec(
            ADD_SOURCE,
            "Add a log file to the project by path. Use only a path the user has mentioned.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Filesystem path to the log file."}
                },
                "required": ["path"],
            }),
        ),
    ]
}

fn spec(name: &str, description: &str, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
    }
}
