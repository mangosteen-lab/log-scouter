use chrono::{NaiveDate, Timelike};
use log_scouter::core::extractor::{
    default_extractor, detect, detect_all, export_schemas_to_folder, generic_extractor,
    load_schemas_from_folder, preview_extraction, sniff_timestamp, user_schema_dir, Extractor,
    SampleLine, BRACKETED_LEGACY_FORMAT, GENERIC_EXTRACTOR_NAME,
};
use log_scouter::core::filters::{
    common_message_pattern, expand_tilde, export_filters_to_folder, hide_like,
    load_filters_from_folder, message_template, pattern_candidates, user_filter_dir, FilterFile,
    FilterRule, FilterSet,
};
use log_scouter::core::models::{
    merge_files, LiveSourceConfig, LiveSourceKind, LogFileModel, ViewModel, VisibleIndices,
};
use log_scouter::core::project::{text_files_in_dir, Bookmark, Project};
use log_scouter::core::search::{
    compile_query, default_search_library, export_searches_to_folder,
    install_default_search_library, load_searches_from_folder, parse_datetime, user_search_dir,
    USER_SEARCHES_SUBDIR,
};
use std::path::PathBuf;

const SAMPLE: &str = include_str!("../fixtures/sample.log");

fn sample_model() -> LogFileModel {
    let extractor = default_extractor();
    let mut model = LogFileModel::new(
        "f1",
        "sample.log",
        extractor.name.clone(),
        "",
        Some(extractor),
    );
    model.load_from_lines(SAMPLE.lines());
    model
}

fn synthetic(n: usize) -> LogFileModel {
    let extractor = default_extractor();
    let mut lines = Vec::new();
    let modules = ["Kernel", "Net", "SQL"];
    let levels = ["Trace", "Info", "Error"];
    for i in 0..n {
        lines.push(format!(
            "2026-06-16 10:{:02}:{:02}.{:03} [HOST:h][SERVER:S][PID:1][THR:2][{}][{}][UID:0][SID:0][OID:0][F.cpp:{}] msg {}",
            (i / 60) % 60,
            i % 60,
            i % 1000,
            modules[i % 3],
            levels[i % 3],
            i,
            i
        ));
    }
    let mut model = LogFileModel::new(
        "f1",
        "synthetic.log",
        extractor.name.clone(),
        "",
        Some(extractor),
    );
    model.load_from_lines(lines.iter().map(String::as_str));
    model
}

#[test]
fn default_extractor_parses_all_fields() {
    let extractor = default_extractor();
    let example = SAMPLE.lines().next().unwrap();
    let fields = preview_extraction(&extractor, example);
    let fields: std::collections::HashMap<_, _> = fields.into_iter().collect();

    assert_eq!(fields["timestamp"], "2026-06-16 10:09:43.288");
    assert_eq!(fields["host"], "log-host-1");
    assert_eq!(fields["server"], "AppServer");
    assert_eq!(fields["process_id"], "54");
    assert_eq!(fields["thread_id"], "136612056716864");
    assert_eq!(fields["log_module"], "Kernel");
    assert_eq!(fields["log_level"], "Trace");
    assert_eq!(fields["file_name"], "ServerDispatcher.cpp");
    assert_eq!(fields["line_number"], "394");
    assert!(fields["message"].starts_with("NetChannel : Channel is closed."));
}

#[test]
fn timestamp_is_parsed() {
    let extractor = default_extractor();
    let fields = extractor.extract(SAMPLE.lines().next().unwrap()).unwrap();
    let expected = NaiveDate::from_ymd_opt(2026, 6, 16)
        .unwrap()
        .and_hms_micro_opt(10, 9, 43, 288000)
        .unwrap();
    assert_eq!(extractor.parse_timestamp(&fields), Some(expected));
}

#[test]
fn multiline_continuation_is_appended() {
    let extractor = default_extractor();
    let example = SAMPLE.lines().next().unwrap();
    let mut model = LogFileModel::new(
        "f1",
        "test.log",
        extractor.name.clone(),
        "",
        Some(extractor),
    );
    model.load_from_lines([
        example.to_string(),
        "    at frame_one() (a.cpp:1)".to_string(),
        "    at frame_two() (b.cpp:2)".to_string(),
        example.replace("Trace", "Error"),
    ]);

    assert_eq!(model.entries.len(), 2);
    assert!(model.message(&model.entries[0]).contains("frame_one"));
    assert!(model.message(&model.entries[0]).contains("frame_two"));
    assert_eq!(model.entries[0].raw.matches('\n').count(), 2);
    assert_eq!(model.level(&model.entries[1]), "Error");
}

#[test]
fn multiline_block_schema_groups_and_extracts_entries() {
    let format = "{\n                'timestamp':'<timestamp>',\n                'level': '<level>',\n                'serviceName': '<service>',\n                'className': '<class>',\n                'methodName': '<method>',\n                'message': '<message>',\n                'host': '<host>'\n            }";
    let mut extractor =
        Extractor::with_timestamp_format("python-block", format, "%Y-%m-%d %H:%M:%S,%f").unwrap();
    extractor.entry_start = r"^\s*\{\s*$".to_string();
    extractor.entry_end = r"^\s*\}\s*$".to_string();
    extractor.compile().unwrap();

    let body = "{\n                'timestamp':'2026-07-14 07:14:40,530',\n                'level': 'INFO',\n                'serviceName': 'MSTRBAK-RESTORE',\n                'className': 'mstrbak-main',\n                'methodName': 'main',\n                'message': 'Starting mstrbak restore container...',\n                'host': 'tec-l-1225084-iserver-0'\n            }\n{\n                'timestamp':'2026-07-14 07:14:40,531',\n                'level': 'INFO',\n                'serviceName': 'MSTRBAK-RESTORE',\n                'className': 'mstrbak-main',\n                'methodName': 'main',\n                'message': 'env info: {'python_version': '3.13.11', 'feature_flag': {'collaboration_service': True}}',\n                'host': 'tec-l-1225084-iserver-0'\n            }";

    let mut model = LogFileModel::new(
        "f1",
        "restore.log",
        extractor.name.clone(),
        "",
        Some(extractor.clone()),
    );
    model.load_from_lines(body.lines());

    assert_eq!(model.entries.len(), 2);
    assert_eq!(model.entries[0].line_no, 1);
    assert_eq!(model.entries[1].line_no, 10);
    assert_eq!(
        model.get_field(&model.entries[0], "timestamp"),
        "2026-07-14 07:14:40,530"
    );
    assert_eq!(model.get_field(&model.entries[0], "level"), "INFO");
    assert_eq!(
        model.get_field(&model.entries[0], "service"),
        "MSTRBAK-RESTORE"
    );
    assert_eq!(
        model.get_field(&model.entries[1], "message"),
        "env info: {'python_version': '3.13.11', 'feature_flag': {'collaboration_service': True}}"
    );
    assert_eq!(
        model.timestamp(&model.entries[1]),
        Some(
            NaiveDate::from_ymd_opt(2026, 7, 14)
                .unwrap()
                .and_hms_milli_opt(7, 14, 40, 531)
                .unwrap()
        )
    );
    assert_eq!(extractor.match_score(&lines(body)), 2);
    assert_eq!(
        detect(vec![&extractor], &lines(body)).unwrap().name,
        "python-block"
    );
}

#[test]
fn explicit_entry_start_merges_exception_continuations() {
    let mut extractor = Extractor::with_timestamp_format(
        "mixed",
        "[<level>] <timestamp> - <message>",
        "%Y-%m-%d %H:%M:%S,%f",
    )
    .unwrap();
    extractor.entry_start = r"^\[[A-Za-z]+\]\s+\d{4}-\d{2}-\d{2}\s+".to_string();
    extractor.compile().unwrap();

    let mut model = LogFileModel::new(
        "f1",
        "mixed.log",
        extractor.name.clone(),
        "",
        Some(extractor),
    );
    model.load_from_lines([
        "[Info] 2026-07-14 07:14:40,530 - Starting mstrbak restore container...",
        "[Error] 2026-07-14 07:14:40,531 - Failed to start mstrbak restore container due to the following error:",
        "Traceback (most recent call last):",
        "  File \"/usr/local/lib/python3.13/site-packages/mstrbak/main.py\",",
        "    main()",
        "  File \"/usr/local/lib/python3.13/site-packages/mstrbak/restore.py\",",
        "    perform_restore()",
        "[Warn] 2026-07-14 07:14:40,532 - The restore process encountered a warning.",
    ]);

    assert_eq!(model.entries.len(), 3);
    assert_eq!(model.level(&model.entries[1]), "Error");
    let message = model.message(&model.entries[1]);
    assert!(
        message.contains("Traceback (most recent call last):"),
        "{message}"
    );
    assert!(message.contains("perform_restore()"), "{message}");
    assert_eq!(model.level(&model.entries[2]), "Warn");
}

#[test]
fn logical_field_aliases_work() {
    let model = sample_model();
    let entry = &model.entries[0];
    assert_eq!(model.get_field(entry, "level"), "Trace");
    assert_eq!(model.get_field(entry, "module"), "Kernel");
    assert_eq!(model.get_field(entry, "file"), "ServerDispatcher.cpp");
}

#[test]
fn custom_format_parses() {
    let extractor = Extractor::new("simple", "<level>: <message>").unwrap();
    let fields = extractor.extract("ERROR: disk full").unwrap();
    assert_eq!(fields["level"], "ERROR");
    assert_eq!(fields["message"], "disk full");
}

/// An bracketed error line carries an extra `[0x800424FB]` between level and UID. Before
/// `<error_code?>` the non-greedy `<log_level>` swallowed it, so `log_level` came out as
/// `Error][0x800424FB` and a `level equals Error` filter silently dropped the line.
const ERROR_LINE: &str = "2026-06-16 10:12:08.631 [HOST:h1][SERVER:AppServer][PID:53][THR:135332369409600][Query Engine][Error][0x800424FB][UID:5CCC][SID:B830][OID:72EF][QueryEngine.cpp:6580] We could not obtain the data.";
const PLAIN_LINE: &str = "2026-06-16 10:12:09.000 [HOST:h1][SERVER:AppServer][PID:53][THR:135332369409600][Query Engine][Error][UID:5CCC][SID:B830][OID:72EF][QueryEngine.cpp:6581] Plain error, no code.";

#[test]
fn optional_field_is_captured_when_present() {
    let fields = default_extractor().extract(ERROR_LINE).unwrap();
    assert_eq!(fields["log_level"], "Error");
    assert_eq!(fields["error_code"], "0x800424FB");
    assert_eq!(fields["user_id"], "5CCC");
    assert_eq!(fields["file_name"], "QueryEngine.cpp");
    assert_eq!(fields["message"], "We could not obtain the data.");
}

#[test]
fn optional_field_is_empty_when_absent() {
    let fields = default_extractor().extract(PLAIN_LINE).unwrap();
    assert_eq!(fields["log_level"], "Error");
    assert_eq!(fields["error_code"], "");
    assert_eq!(fields["user_id"], "5CCC");
    assert_eq!(fields["message"], "Plain error, no code.");
}

/// The regression the optional field exists to prevent: one filter must catch both.
#[test]
fn level_filter_matches_lines_with_and_without_the_optional_code() {
    let extractor = default_extractor();
    let mut model = LogFileModel::new("f1", "e.log", extractor.name.clone(), "", Some(extractor));
    model.load_from_lines([ERROR_LINE, PLAIN_LINE]);
    assert_eq!(model.entries.len(), 2);

    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Error", "include"));
    let kept = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .count();
    assert_eq!(
        kept, 2,
        "both error lines must survive a level=Error filter"
    );
}

/// A leading `<error_code?>` capture must not steal the separator behind it.
#[test]
fn optional_field_absorbs_the_separator_in_front_of_it() {
    let extractor = Extractor::new("opt", "[<a>][<b?>][C:<c>]").unwrap();

    let both = extractor.extract("[1][2][C:3]").unwrap();
    assert_eq!(
        (&both["a"], &both["b"], &both["c"]),
        (&"1".into(), &"2".into(), &"3".into())
    );

    let without = extractor.extract("[1][C:3]").unwrap();
    assert_eq!(without["a"], "1");
    assert_eq!(without["b"], "");
    assert_eq!(without["c"], "3");
}

/// Multi-line grouping must still fire: a continuation line is not a record start.
#[test]
fn optional_field_does_not_break_multiline_grouping() {
    let extractor = default_extractor();
    let mut model = LogFileModel::new("f1", "e.log", extractor.name.clone(), "", Some(extractor));
    model.load_from_lines([
        ERROR_LINE,
        "Error type: Odbc error. Connection refused.",
        PLAIN_LINE,
    ]);

    assert_eq!(
        model.entries.len(),
        2,
        "continuation must fold into the error line"
    );
    assert!(model.entries[0].raw.contains("Odbc error"));
    assert_eq!(
        model.get_field(&model.entries[0], "error_code"),
        "0x800424FB"
    );
}

#[test]
fn search_query_language_matches_expected_entries() {
    let model = sample_model();

    let q = compile_query("Cache miss");
    assert!(model.entries.iter().any(|entry| q.matches(&model, entry)));

    let q = compile_query("level=Error");
    let hits: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| q.matches(&model, entry))
        .collect();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|entry| model.level(entry) == "Error"));

    let q = compile_query("module~SQL level=Warn");
    let hits: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| q.matches(&model, entry))
        .collect();
    assert!(!hits.is_empty());
    assert!(hits
        .iter()
        .all(|entry| model.module(entry).to_lowercase().contains("sql")));
    assert!(hits.iter().all(|entry| model.level(entry) == "Warn"));

    let q = compile_query(r#""/completed in \d+ms/""#);
    assert!(model.entries.iter().any(|entry| q.matches(&model, entry)));

    let q = compile_query("after:2026-06-16T10:09:50 before:2026-06-16T10:09:56");
    let hits: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| q.matches(&model, entry))
        .collect();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|entry| {
        let ts = model.timestamp(entry).unwrap();
        ts.second() >= 50 && ts.second() <= 56
    }));
}

#[test]
fn date_range_and_datetime_formats_work() {
    let model = sample_model();
    let q = compile_query("date:[2026-06-16T10:09:44..2026-06-16T10:09:46]");
    assert!(model.entries.iter().any(|entry| q.matches(&model, entry)));

    assert!(parse_datetime("2026-06-16").is_some());
    assert!(parse_datetime("2026-06-16 10:09").is_some());
    assert!(parse_datetime("2026-06-16 10:09:43.288").is_some());
    assert!(parse_datetime("nonsense").is_none());
}

#[test]
fn invalid_regex_is_recorded_not_raised() {
    let q = compile_query("/(unclosed/");
    assert!(!q.error.is_empty());
}

#[test]
fn filters_include_exclude_and_hide_like() {
    let model = sample_model();

    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Trace", "exclude"));
    let visible: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .collect();
    assert!(visible.iter().all(|entry| model.level(entry) != "Trace"));
    assert!(visible.len() < model.entries.len());

    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Error", "include"));
    let visible: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .collect();
    assert!(!visible.is_empty());
    assert!(visible.iter().all(|entry| model.level(entry) == "Error"));

    let kernel_index = model
        .entries
        .iter()
        .position(|entry| model.module(entry) == "Kernel")
        .unwrap();
    let mut filters = FilterSet::default();
    filters.add(hide_like(
        &model,
        &model.entries[kernel_index],
        "module",
        "",
        true,
    ));
    let visible: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .collect();
    assert!(visible.iter().all(|entry| model.module(entry) != "Kernel"));
}

#[test]
fn timestamp_range_filter_works() {
    let model = sample_model();
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new(
        "timestamp",
        "range",
        "2026-06-16 10:09:50..",
        "include",
    ));
    let visible: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .collect();
    assert!(!visible.is_empty());
    assert!(visible
        .iter()
        .all(|entry| model.timestamp(entry).unwrap().second() >= 50
            || model.timestamp(entry).unwrap().minute() >= 10));
}

#[test]
fn filters_export_and_load_as_individual_json_files() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("filters");
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Trace", "exclude"));
    filters.add(FilterRule::new("module", "contains", "SQL", "include"));

    let exported = export_filters_to_folder(&filters, &folder).unwrap();
    assert_eq!(exported, 2);

    let mut files = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 2);

    let first_body = std::fs::read_to_string(&files[0]).unwrap();
    let first: FilterFile = serde_json::from_str(&first_body).unwrap();
    assert!(!first.name.is_empty());
    assert!(first.description.contains("level"));
    assert_eq!(first.filter, filters.rules[0]);

    let loaded = load_filters_from_folder(&folder).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].filter, filters.rules[0]);
    assert_eq!(loaded[1].filter, filters.rules[1]);
}

#[test]
fn view_model_fast_path_and_context_work() {
    let model = synthetic(1000);
    let mut view = ViewModel::new("L1", &model);
    assert_eq!(view.visible, VisibleIndices::Range(1000));

    view.filters
        .add(FilterRule::new("level", "equals", "Trace", "exclude"));
    view.rebuild(&model);
    assert!(matches!(view.visible, VisibleIndices::List(_)));
    assert!(view
        .visible
        .iter()
        .all(|index| model.level(&model.entries[index]) != "Trace"));

    let model = sample_model();
    let mut view = ViewModel::new("L1", &model);
    view.query = Some(compile_query("Fatal"));
    view.context = 2;
    view.rebuild(&model);
    assert!(!view.visible.is_empty());
    assert!(view.visible.len() < model.entries.len());
    assert!(view
        .visible
        .iter()
        .any(|index| view.match_set.contains(&index)));
}

#[test]
fn common_pattern_generalises_differing_tokens_in_place() {
    let pattern = common_message_pattern(&[
        "Distribution Service Trigger: 5 subscriptions queued",
        "Distribution Service Trigger: 7 subscriptions queued",
    ])
    .unwrap();
    // Both differing tokens are integers, so the wildcard is tightened to their shape.
    assert_eq!(
        pattern,
        r"Distribution\s+Service\s+Trigger:\s+\d+\s+subscriptions\s+queued"
    );

    // The derived regex must match every line it was derived from.
    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("Distribution Service Trigger: 5 subscriptions queued"));
    assert!(regex.is_match("Distribution Service Trigger: 7 subscriptions queued"));
    assert!(!regex.is_match("Cache miss for session 12"));
    // ... and only lines of that shape: a non-numeric count is a different statement.
    assert!(!regex.is_match("Distribution Service Trigger: many subscriptions queued"));
}

#[test]
fn common_pattern_escapes_regex_metacharacters() {
    let pattern =
        common_message_pattern(&["UserSession::TimeOut() id=1", "UserSession::TimeOut() id=2"])
            .unwrap();
    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("UserSession::TimeOut() id=9"));
    // `()` was escaped, not treated as an empty group that matches anything.
    assert!(!regex.is_match("UserSession::TimeOut id=9"));
}

#[test]
fn common_pattern_falls_back_to_shared_tokens_when_lengths_differ() {
    let pattern = common_message_pattern(&[
        "Cache miss for session 12",
        "Cache miss for user bob in session 13",
    ])
    .unwrap();
    assert_eq!(pattern, r"Cache.*miss.*for.*session");

    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("Cache miss for session 12"));
    assert!(regex.is_match("Cache miss for user bob in session 13"));
}

#[test]
fn common_pattern_falls_back_to_literals_rather_than_a_catch_all() {
    // Nothing shared: generalising would hide the file, so match the lines exactly.
    let pattern = common_message_pattern(&["alpha beta", "gamma delta"]).unwrap();
    assert_eq!(pattern, r"(?:alpha beta|gamma delta)");
    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("alpha beta"));
    assert!(regex.is_match("gamma delta"));
    assert!(!regex.is_match("alpha delta"));

    // Different lengths, nothing shared -> literals again.
    let pattern = common_message_pattern(&["alpha", "gamma delta"]).unwrap();
    assert_eq!(pattern, r"(?:alpha|gamma delta)");

    // One shared token out of many is too thin to be a template.
    let pattern =
        common_message_pattern(&["the quick brown fox", "the slow purple elephant here"]).unwrap();
    assert!(pattern.starts_with("(?:"), "over-generalised to {pattern}");

    // Duplicates collapse.
    assert_eq!(common_message_pattern(&["same", "same"]).unwrap(), "same");

    // Nothing to generalise against.
    assert_eq!(common_message_pattern(&["alpha beta"]), None);
    assert_eq!(common_message_pattern(&[]), None);
    // All-blank has no content at all.
    assert_eq!(common_message_pattern(&["  ", "   "]), None);
}

#[test]
fn common_pattern_still_generalises_when_one_line_is_an_outlier_free_header() {
    // The real trigger for "H does nothing": a log's banner line shares no tokens.
    let pattern = common_message_pattern(&[
        "# Server Log version 2.0",
        "NetChannel : Channel is closed. remote 'ip-1'",
        "NetChannel : Channel is closed. remote 'ip-2'",
    ])
    .unwrap();
    let regex = regex::Regex::new(&pattern).unwrap();
    // Whatever strategy it picks, it must match every line it was derived from.
    assert!(regex.is_match("# Server Log version 2.0"));
    assert!(regex.is_match("NetChannel : Channel is closed. remote 'ip-1'"));
    assert!(regex.is_match("NetChannel : Channel is closed. remote 'ip-2'"));
    assert!(!regex.is_match("Cache miss for session 12"));
}

#[test]
fn common_pattern_keeps_a_literal_when_only_some_tokens_differ() {
    let pattern = common_message_pattern(&["a 1 c", "a 2 c", "a 3 c"]).unwrap();
    assert_eq!(pattern, r"a\s+\d+\s+c");

    // Nothing ties the differing tokens together, so the wildcard stays a wildcard.
    let pattern = common_message_pattern(&["a x1 c", "a 2y c"]).unwrap();
    assert_eq!(pattern, r"a\s+\S+\s+c");
}

#[test]
fn prepared_filters_agree_with_the_per_entry_path() {
    let model = sample_model();
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new(
        "message",
        "regex",
        r"Cache\s+miss",
        "exclude",
    ));
    filters.add(FilterRule::new("level", "equals", "Error", "include"));

    let prepared = filters.prepare();
    for entry in &model.entries {
        assert_eq!(
            prepared.visible(&model, entry),
            filters.visible(&model, entry)
        );
    }
}

#[test]
fn an_invalid_regex_rule_matches_nothing_rather_than_panicking() {
    let model = sample_model();
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("message", "regex", "(unclosed", "exclude"));
    let visible = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .count();
    assert_eq!(visible, model.entries.len());
}

/// A second, unrelated log format, to prove schemas really are per file.
fn simple_model(file_id: &str, lines: &[&str]) -> LogFileModel {
    let extractor =
        Extractor::with_timestamp_format("simple", "<timestamp> <level>: <message>", "%H:%M:%S")
            .unwrap();
    let mut model = LogFileModel::new(
        file_id,
        format!("{file_id}.log"),
        extractor.name.clone(),
        format!("{file_id}.log"),
        Some(extractor),
    );
    model.load_from_lines(lines.iter().copied());
    model
}

fn bracketed_model(file_id: &str, lines: &[&str]) -> LogFileModel {
    let extractor = default_extractor();
    let mut model = LogFileModel::new(
        file_id,
        format!("{file_id}.log"),
        extractor.name.clone(),
        format!("{file_id}.log"),
        Some(extractor),
    );
    model.load_from_lines(lines.iter().copied());
    model
}

#[test]
fn merge_interleaves_two_files_by_timestamp() {
    let left = bracketed_model(
        "f1",
        &[
            "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] first",
            "2026-06-16 10:00:03.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Error][UID:0][SID:0][OID:0][a.cpp:2] third",
        ],
    );
    let right = bracketed_model(
        "f2",
        &[
            "2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Warn][UID:0][SID:0][OID:0][b.cpp:1] second",
            "2026-06-16 10:00:04.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:2] fourth",
        ],
    );

    let merged = merge_files("m1", &[&left, &right]);
    assert!(merged.is_merged());
    assert_eq!(merged.entries.len(), 4);

    let messages: Vec<String> = merged
        .entries
        .iter()
        .map(|entry| merged.message(entry))
        .collect();
    assert_eq!(messages, ["first", "second", "third", "fourth"]);

    // Each entry still reports the file it came from.
    let origins: Vec<&str> = merged
        .entries
        .iter()
        .filter_map(|entry| merged.source_name(entry))
        .collect();
    assert_eq!(origins, ["f1.log", "f2.log", "f1.log", "f2.log"]);

    let stamps: Vec<_> = merged
        .entries
        .iter()
        .map(|entry| merged.timestamp(entry).unwrap())
        .collect();
    assert!(stamps.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[test]
fn merge_applies_each_files_own_schema() {
    let bracketed = bracketed_model(
        "f1",
        &["2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Error][UID:0][SID:0][OID:0][a.cpp:7] boom"],
    );
    // A totally different format, whose `level` sits somewhere else entirely.
    let simple = simple_model("f2", &["10:00:01 WARN: disk almost full"]);

    let merged = merge_files("m1", &[&bracketed, &simple]);
    assert_eq!(merged.entries.len(), 2);

    // The simple line carries no date, so its timestamp does not parse; it inherits
    // the sentinel and keeps the head position rather than being dropped.
    assert_eq!(merged.level(&merged.entries[0]), "WARN");
    assert_eq!(merged.message(&merged.entries[0]), "disk almost full");
    assert_eq!(merged.level(&merged.entries[1]), "Error");
    assert_eq!(merged.module(&merged.entries[1]), "Kernel");
    assert_eq!(merged.get_field(&merged.entries[1], "file"), "a.cpp");
    assert_eq!(merged.message(&merged.entries[1]), "boom");
}

#[test]
fn merge_keeps_untimestamped_lines_next_to_their_own_file() {
    // A banner with no timestamp of its own has nowhere to sort, so it borrows the first
    // timestamp its own file does have and sits just above it -- rather than ahead of
    // every other file's records, which is where the sentinel would put it.
    let left = bracketed_model(
        "f1",
        &[
            "# Server Log version 2.0",
            "2026-06-16 10:00:03.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] late",
        ],
    );
    let right = bracketed_model(
        "f2",
        &["2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:1] early"],
    );

    let merged = merge_files("m1", &[&left, &right]);
    let messages: Vec<String> = merged
        .entries
        .iter()
        .map(|entry| merged.message(entry))
        .collect();
    assert_eq!(messages[0], "early");
    assert_eq!(messages[1], "# Server Log version 2.0");
    assert_eq!(messages[2], "late");
}

#[test]
fn project_merges_files_and_does_not_persist_the_merged_view() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.log");
    let b = tmp.path().join("b.log");
    std::fs::write(&a, "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] one\n").unwrap();
    std::fs::write(&b, "2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:1] two\n").unwrap();

    let mut project = Project::load(tmp.path());
    let id_a = project.add_file(&a, None).file_id.clone();
    let id_b = project.add_file(&b, None).file_id.clone();
    project.load_all_files();

    let merged_id = project.add_merged(&[id_a.clone(), id_b.clone()]).unwrap();
    assert_eq!(project.files.len(), 3);
    assert_eq!(project.get_file(&merged_id).unwrap().entries.len(), 2);

    // Merging the same pair again reuses the existing view.
    assert_eq!(
        project.add_merged(&[id_a.clone(), id_b.clone()]).unwrap(),
        merged_id
    );
    assert_eq!(project.files.len(), 3);

    // Fewer than two files is not a merge; a merged view cannot be re-merged.
    assert!(project.add_merged(std::slice::from_ref(&id_a)).is_err());
    assert!(project
        .add_merged(&[merged_id.clone(), id_a.clone()])
        .is_err());

    project.save().unwrap();
    let reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.files.len(), 2, "merged view must not be persisted");
    assert!(reloaded.files.iter().all(|file| !file.is_merged()));

    // Removing a source file drops the merge built on it.
    project.remove_file(&id_a);
    assert!(project.get_file(&merged_id).is_none());
}

#[test]
fn a_files_schema_can_be_changed_independently() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.log");
    let b = tmp.path().join("b.log");
    std::fs::write(&a, "10:00:01 WARN: disk almost full\n").unwrap();
    std::fs::write(&b, "2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Net][Info][UID:0][SID:0][OID:0][b.cpp:1] two\n").unwrap();

    let mut project = Project::load(tmp.path());
    let id_a = project.add_file(&a, None).file_id.clone();
    let id_b = project.add_file(&b, None).file_id.clone();
    project.load_all_files();

    // `a` does not match the bracketed schema, so it has no level.
    let file_a = project.get_file(&id_a).unwrap();
    assert_eq!(file_a.level(&file_a.entries[0]), "");

    let simple =
        Extractor::with_timestamp_format("simple", "<timestamp> <level>: <message>", "%H:%M:%S")
            .unwrap();
    project.add_extractor(simple).unwrap();
    project.set_file_extractor(&id_a, "simple").unwrap();
    // Changing the schema invalidates the parse; the caller must re-read.
    assert!(!project.get_file(&id_a).unwrap().loaded);
    project.load_all_files();

    let file_a = project.get_file(&id_a).unwrap();
    assert_eq!(file_a.level(&file_a.entries[0]), "WARN");
    assert_eq!(file_a.message(&file_a.entries[0]), "disk almost full");

    // `b` is untouched and still uses the bracketed schema.
    let file_b = project.get_file(&id_b).unwrap();
    assert_eq!(file_b.level(&file_b.entries[0]), "Info");
    assert_eq!(file_b.module(&file_b.entries[0]), "Net");

    // The per-file schema survives a save/load round trip.
    project.save().unwrap();
    let reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.get_file(&id_a).unwrap().extractor_name, "simple");
    assert_eq!(
        reloaded.get_file(&id_b).unwrap().extractor_name,
        "Bracketed default"
    );
}

/// Two single-line formats a mixed docker log might interleave.
fn uvicorn_schema() -> Extractor {
    let mut ex = Extractor::new("Uvicorn", "<level>:<pad><message>").unwrap();
    ex.field_patterns
        .insert("level".into(), "INFO|ERROR".into());
    ex.field_patterns.insert("pad".into(), " +".into());
    ex.entry_start = "^(?:INFO|ERROR):".into();
    ex.compile().unwrap();
    ex
}

fn nginx_schema() -> Extractor {
    let mut ex = Extractor::with_timestamp_format(
        "Nginx",
        "<addr> - - [<timestamp>] <message>",
        "%d/%b/%Y:%H:%M:%S %z",
    )
    .unwrap();
    ex.entry_start = r"^\S+ - - \[".into();
    ex.compile().unwrap();
    ex
}

const MIXED_LOG: &str = "[boot] starting up\n\
    INFO:     application ready\n\
    1.2.3.4 - - [13/Jul/2026:06:30:11 +0000] GET /api/x\n\
    ERROR:    boom happened\n\
    5.6.7.8 - - [13/Jul/2026:06:30:12 +0000] GET /healthz";

#[test]
fn multi_schema_source_parses_each_entry_with_the_matching_schema() {
    let uvi = uvicorn_schema();
    let nginx = nginx_schema();
    let mut model = LogFileModel::new("f", "mixed.log", uvi.name.clone(), "", Some(uvi.clone()));
    model.set_schemas(vec![uvi, nginx]);
    model.load_from_lines(MIXED_LOG.lines());

    assert!(model.is_multi_schema());
    // One entry per line; the [boot] preamble is its own (unmatched) entry.
    assert_eq!(model.entries.len(), 5);

    let by_msg = |needle: &str| {
        model
            .entries
            .iter()
            .find(|entry| entry.raw.contains(needle))
            .unwrap()
    };

    let info = by_msg("application ready");
    assert_eq!(model.log_schema_name_for(info), "Uvicorn");
    assert_eq!(model.level(info), "INFO");
    assert_eq!(model.message(info), "application ready");

    let access = by_msg("/api/x");
    assert_eq!(model.log_schema_name_for(access), "Nginx");
    assert_eq!(model.get_field(access, "addr"), "1.2.3.4");
    assert_eq!(model.message(access), "GET /api/x");
    assert!(model.timestamp(access).is_some());

    let err = by_msg("boom happened");
    assert_eq!(model.log_schema_name_for(err), "Uvicorn");
    assert_eq!(model.level(err), "ERROR");

    // The preamble matches nothing: it resolves to the primary and shows its raw text.
    let boot = by_msg("[boot] starting");
    assert_eq!(model.level(boot), "");
    assert_eq!(model.message(boot), "[boot] starting up");
}

#[test]
fn detect_all_returns_a_schema_per_format() {
    let uvi = uvicorn_schema();
    let nginx = nginx_schema();
    let generic = generic_extractor();
    let candidates = [&uvi, &nginx, &generic];
    let lines: Vec<String> = MIXED_LOG.lines().map(str::to_string).collect();

    let set: Vec<&str> = detect_all(candidates.iter().copied(), &lines)
        .into_iter()
        .map(|ex| ex.name.as_str())
        .collect();
    // Both real formats, most-specific first; the Generic catch-all is never included.
    assert!(set.contains(&"Uvicorn") && set.contains(&"Nginx"));
    assert!(!set.contains(&GENERIC_EXTRACTOR_NAME));

    // A single-format sample yields just that schema.
    let only_uvi: Vec<String> = ["INFO:     a", "ERROR:    b", "INFO:     c"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let single: Vec<&str> = detect_all(candidates.iter().copied(), &only_uvi)
        .into_iter()
        .map(|ex| ex.name.as_str())
        .collect();
    assert_eq!(single, ["Uvicorn"]);
}

#[test]
fn add_file_auto_assigns_a_schema_set_for_a_mixed_log() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("mixed.log");
    std::fs::write(&log, MIXED_LOG).unwrap();

    let mut project = Project::load(tmp.path());
    project.library.clear(); // isolate from the dev's real ~/.log-scouter/schemas
    project.add_extractor(uvicorn_schema()).unwrap();
    project.add_extractor(nginx_schema()).unwrap();
    // Zero config: adding the file auto-assigns a schema per format present.
    let id = project.add_file(&log, None).file_id.clone();
    let file = project.get_file(&id).unwrap();
    assert!(file.is_multi_schema());
    let names: Vec<&str> = file.schemas.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Uvicorn") && names.contains(&"Nginx"));
}

#[test]
fn add_file_detects_a_schema_from_the_disk_library() {
    let tmp = tempfile::tempdir().unwrap();
    // A schema present only in the project-level library, not in the project itself.
    let lib = tmp.path().join(".logscouter").join("schemas");
    std::fs::create_dir_all(&lib).unwrap();
    let schema = nginx_schema();
    let wrapper = serde_json::json!({
        "name": schema.name,
        "description": "",
        "schema": {
            "name": schema.name,
            "format": schema.format,
            "timestamp_field": "timestamp",
            "timestamp_format": "%d/%b/%Y:%H:%M:%S %z",
            "entry_start": schema.entry_start,
        }
    });
    std::fs::write(
        lib.join("nginx.json"),
        serde_json::to_string_pretty(&wrapper).unwrap(),
    )
    .unwrap();

    let log = tmp.path().join("access.log");
    std::fs::write(&log, "1.2.3.4 - - [13/Jul/2026:06:30:11 +0000] GET /\n").unwrap();

    // The library schema is not added to the project, yet detection finds it...
    let mut project = Project::load(tmp.path());
    let id = project.add_file(&log, None).file_id.clone();
    project.load_all_files();
    let file = project.get_file(&id).unwrap();
    assert_eq!(file.extractor_name, "Nginx");
    assert_eq!(file.get_field(&file.entries[0], "addr"), "1.2.3.4");

    // ...and it is not copied into project.json (the source references it by name only).
    project.save().unwrap();
    let saved = std::fs::read_to_string(tmp.path().join(".logscouter/project.json")).unwrap();
    assert!(
        !saved.contains("<addr> - - "),
        "library schema body must not be persisted: {saved}"
    );
    // A reopened project still resolves it from the library.
    let reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.get_file(&id).unwrap().extractor_name, "Nginx");
}

#[test]
fn multi_schema_set_survives_a_project_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("mixed.log");
    std::fs::write(&log, MIXED_LOG).unwrap();

    let mut project = Project::load(tmp.path());
    project.add_extractor(uvicorn_schema()).unwrap();
    project.add_extractor(nginx_schema()).unwrap();
    let id = project.add_file(&log, None).file_id.clone();
    project
        .set_file_schemas(&id, &["Uvicorn".to_string(), "Nginx".to_string()])
        .unwrap();
    assert!(project.get_file(&id).unwrap().is_multi_schema());

    project.save().unwrap();
    let reloaded = Project::load(tmp.path());
    let file = reloaded.get_file(&id).unwrap();
    assert!(file.is_multi_schema());
    let names: Vec<&str> = file.schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Uvicorn", "Nginx"]);
}

#[test]
fn stdin_source_is_not_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.log");
    std::fs::write(&a, "hello\n").unwrap();

    let mut project = Project::load(tmp.path());
    project.add_file(&a, None);
    let stdin_id = project.add_stdin_source().file_id.clone();
    assert!(project.get_file(&stdin_id).unwrap().is_stdin_source());

    // A closed pipe cannot be reopened, so the stdin source must not survive a round trip;
    // the ordinary file must.
    project.save().unwrap();
    let reloaded = Project::load(tmp.path());
    assert!(reloaded.get_file(&stdin_id).is_none());
    let names: Vec<&str> = reloaded
        .files
        .iter()
        .map(|file| file.display_name.as_str())
        .collect();
    assert_eq!(names, ["a.log"]);
}

#[test]
fn project_persists_compatible_json() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("a.log");
    std::fs::write(&log_path, SAMPLE).unwrap();

    let mut project = Project::load(tmp.path());
    project.add_file(&log_path, None);
    project.saved_searches.push("level=Error".to_string());
    project.save().unwrap();

    let reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.files.len(), 1);
    assert_eq!(reloaded.saved_searches, vec!["level=Error"]);
    assert!(reloaded.config_path().exists());
}

#[test]
fn project_persists_bookmarks_and_prunes_removed_file_bookmarks() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("a.log");
    std::fs::write(&log_path, SAMPLE).unwrap();

    let mut project = Project::load(tmp.path());
    let file_id = project.add_file(&log_path, None).file_id.clone();
    project.bookmarks.push(Bookmark {
        file_id: file_id.clone(),
        line_no: 3,
        note: "first failing request".to_string(),
    });
    project.save().unwrap();

    let mut reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.bookmarks.len(), 1);
    assert_eq!(reloaded.bookmarks[0].line_no, 3);
    assert_eq!(reloaded.bookmarks[0].note, "first failing request");

    reloaded.remove_file(&file_id);
    assert!(reloaded.bookmarks.is_empty());
}

#[test]
fn project_autosaves_and_restores_filters() {
    let tmp = tempfile::tempdir().unwrap();

    let mut project = Project::load(tmp.path());
    assert!(project.filters.rules.is_empty());
    project
        .filters
        .add(FilterRule::new("level", "equals", "Trace", "exclude"));
    project.filters.add(FilterRule::new(
        "message",
        "contains",
        "TimeOut()",
        "exclude",
    ));
    project.save().unwrap();

    let reloaded = Project::load(tmp.path());
    assert_eq!(reloaded.filters.rules.len(), 2);
    assert_eq!(reloaded.filters.rules[0].field, "level");
    assert_eq!(reloaded.filters.rules[1].value, "TimeOut()");

    // The restored set actually filters entries, not just round-trips as data.
    let model = sample_model();
    let visible: Vec<_> = model
        .entries
        .iter()
        .filter(|entry| reloaded.filters.visible(&model, entry))
        .collect();
    assert!(!visible.is_empty());
    assert!(visible.iter().all(|entry| model.level(entry) != "Trace"));
}

#[test]
fn schema_descriptions_survive_project_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = Project::load(tmp.path());
    let schema = Extractor::new("simple", "<timestamp> <level>: <message>")
        .unwrap()
        .with_description("compact one-line service log");

    project.add_extractor(schema).unwrap();
    project.save().unwrap();

    let reloaded = Project::load(tmp.path());
    assert_eq!(
        reloaded.extractors["simple"].description,
        "compact one-line service log"
    );
}

#[test]
fn filter_rules_can_be_scoped_to_a_log_schema() {
    let bracketed = bracketed_model(
        "bracketed",
        &["2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] noisy"],
    );
    let simple = simple_model("simple", &["10:00:01 Trace: same word different schema"]);
    let mut filters = FilterSet::default();
    filters.add(
        FilterRule::new("level", "equals", "Trace", "exclude").for_log_schema("Bracketed default"),
    );

    assert!(!filters.visible(&bracketed, &bracketed.entries[0]));
    assert!(filters.visible(&simple, &simple.entries[0]));
    assert_eq!(
        filters.rules[0].describe(),
        "exclude level equals 'Trace' on schema 'Bracketed default'"
    );
}

#[test]
fn scoped_include_filters_do_not_hide_other_schemas() {
    let bracketed = bracketed_model(
        "bracketed",
        &["2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] noisy"],
    );
    let simple = simple_model("simple", &["10:00:01 WARN: disk almost full"]);
    let mut filters = FilterSet::default();
    filters.add(
        FilterRule::new("level", "equals", "Error", "include").for_log_schema("Bracketed default"),
    );

    assert!(!filters.visible(&bracketed, &bracketed.entries[0]));
    assert!(filters.visible(&simple, &simple.entries[0]));
}

#[test]
fn schema_scoped_filters_apply_per_source_entry_in_a_merge() {
    let bracketed = bracketed_model(
        "bracketed",
        &["2026-06-16 10:00:02.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] bracketed trace"],
    );
    let simple = simple_model("simple", &["10:00:01 Trace: simple trace"]);
    let merged = merge_files("merged", &[&bracketed, &simple]);
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Trace", "exclude").for_log_schema("simple"));

    let visible: Vec<_> = merged
        .entries
        .iter()
        .filter(|entry| filters.visible(&merged, entry))
        .map(|entry| merged.message(entry))
        .collect();
    assert_eq!(visible, ["bracketed trace"]);
}

#[test]
fn project_json_without_filters_key_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".logscouter");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("project.json"),
        r#"{"version":1,"file_counter":0,"saved_searches":["INBOX"]}"#,
    )
    .unwrap();

    let project = Project::load(tmp.path());
    assert!(project.filters.rules.is_empty());
    assert_eq!(project.saved_searches, vec!["INBOX"]);
}

#[test]
fn user_filter_dir_is_under_home() {
    let home = std::env::var("HOME").expect("HOME is set in this environment");
    let dir = user_filter_dir().expect("HOME is set, so a user filter dir exists");
    assert_eq!(dir, PathBuf::from(&home).join(".log-scouter/filters"));
}

#[test]
fn user_search_dir_is_under_home() {
    let home = std::env::var("HOME").expect("HOME is set in this environment");
    let dir = user_search_dir().expect("HOME is set, so a user search dir exists");
    assert_eq!(
        dir,
        PathBuf::from(&home).join(format!(".log-scouter/{USER_SEARCHES_SUBDIR}"))
    );
}

#[test]
fn saved_searches_export_load_and_default_queries_compile() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("searches");
    let searches = vec![
        "level=Error".to_string(),
        r#"/request[-_]?id|trace[-_]?id/"#.to_string(),
    ];

    assert_eq!(export_searches_to_folder(&searches, &folder).unwrap(), 2);
    let loaded = load_searches_from_folder(&folder).unwrap();
    assert_eq!(
        loaded
            .iter()
            .map(|search| search.query.as_str())
            .collect::<Vec<_>>(),
        vec!["level=Error", r#"/request[-_]?id|trace[-_]?id/"#]
    );

    let defaults = default_search_library();
    assert!(defaults
        .iter()
        .any(|search| search.name == "Authentication failures"));
    assert!(defaults
        .iter()
        .any(|search| search.name == "Database connection exhaustion"));
    assert!(defaults
        .iter()
        .any(|search| search.name == "Kubernetes restart patterns"));
    assert!(defaults
        .iter()
        .any(|search| search.name == "Java exception chains"));
    assert!(defaults
        .iter()
        .any(|search| search.name == "Request-ID tracing"));
    for search in defaults {
        let query = compile_query(&search.query);
        assert!(
            query.error.is_empty(),
            "{} did not compile: {}",
            search.name,
            query.error
        );
    }

    let defaults_dir = tmp.path().join("defaults");
    assert_eq!(install_default_search_library(&defaults_dir).unwrap(), 5);
    assert_eq!(
        install_default_search_library(&defaults_dir).unwrap(),
        0,
        "installing defaults is idempotent"
    );
    assert_eq!(load_searches_from_folder(&defaults_dir).unwrap().len(), 5);
}

#[test]
fn expand_tilde_resolves_home_and_leaves_other_paths_alone() {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    assert_eq!(expand_tilde("~"), home);
    assert_eq!(
        expand_tilde("~/.log-scouter/filters"),
        home.join(".log-scouter/filters")
    );
    assert_eq!(expand_tilde("  ~/x  "), home.join("x"));
    assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    assert_eq!(
        expand_tilde("relative/path"),
        PathBuf::from("relative/path")
    );
    // A literal `~name` is a username-style path, not ours to expand.
    assert_eq!(expand_tilde("~other/x"), PathBuf::from("~other/x"));
}

#[test]
fn filters_round_trip_through_the_user_level_library() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join(".log-scouter").join("filters");

    let mut source = Project::load(tmp.path());
    source
        .filters
        .add(FilterRule::new("level", "equals", "Trace", "exclude"));
    source
        .filters
        .add(FilterRule::new("module", "contains", "SQL", "include"));
    assert_eq!(
        export_filters_to_folder(&source.filters, &library).unwrap(),
        2
    );

    // A different project imports them.
    let other = tempfile::tempdir().unwrap();
    let mut target = Project::load(other.path());
    for filter_file in load_filters_from_folder(&library).unwrap() {
        target.filters.add(filter_file.filter);
    }
    target.save().unwrap();

    assert_eq!(
        Project::load(other.path()).filters.rules,
        source.filters.rules
    );
}

// ---- schema packs ----------------------------------------------------------------

#[test]
fn user_schema_dir_sits_beside_the_filter_library() {
    let Some(dir) = user_schema_dir() else {
        return; // $HOME unset in this environment
    };
    assert!(dir.ends_with(".log-scouter/schemas"), "{}", dir.display());
}

#[test]
fn schemas_export_and_import_as_individual_json_files() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");

    let custom =
        Extractor::with_timestamp_format("compact", "<timestamp> <level>: <message>", "%H:%M:%S")
            .unwrap()
            .with_description("compact service log");
    let schemas = vec![default_extractor(), custom];

    assert_eq!(export_schemas_to_folder(&schemas, &folder).unwrap(), 2);
    let files: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");

    let loaded = load_schemas_from_folder(&folder).unwrap();
    assert_eq!(loaded.len(), 2);
    let compact = loaded
        .iter()
        .find(|entry| entry.name == "compact")
        .expect("compact schema round-tripped");
    assert_eq!(compact.description, "compact service log");
    assert_eq!(compact.schema.timestamp_format, "%H:%M:%S");

    // Imported schemas are compiled, so they parse immediately.
    let fields = compact
        .schema
        .extract("10:00:01 WARN: disk almost full")
        .unwrap();
    assert_eq!(fields["level"], "WARN");
    assert_eq!(fields["message"], "disk almost full");
}

/// An optional field must survive the JSON round-trip, not silently become required.
#[test]
fn exported_schema_keeps_optional_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");
    export_schemas_to_folder(&[default_extractor()], &folder).unwrap();

    let loaded = load_schemas_from_folder(&folder).unwrap();
    let schema = &loaded[0].schema;
    assert!(schema.format.contains("<error_code?>"), "{}", schema.format);

    let fields = schema.extract(ERROR_LINE).unwrap();
    assert_eq!(fields["log_level"], "Error");
    assert_eq!(fields["error_code"], "0x800424FB");
}

/// A bare Extractor, copied out of project.json, imports without rewrapping.
#[test]
fn a_bare_extractor_json_is_accepted_as_a_schema_file() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("bare.json"),
        r#"{"name":"bare","format":"<level>: <message>"}"#,
    )
    .unwrap();

    let loaded = load_schemas_from_folder(&folder).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "bare");
    let fields = loaded[0].schema.extract("INFO: hello").unwrap();
    assert_eq!(fields["level"], "INFO");
}

/// A schema whose format cannot compile must fail loudly at import, not at first line.
#[test]
fn an_uncompilable_schema_is_reported_at_import() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("bad.json"),
        r#"{"name":"bad","format":"<a>","field_patterns":{"a":"([unclosed"}}"#,
    )
    .unwrap();

    let error = load_schemas_from_folder(&folder).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(format!("{error}").contains("bad.json"), "{error}");
}

#[test]
fn importing_schemas_from_a_missing_folder_errors_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_schemas_from_folder(&tmp.path().join("nope")).is_err());
}

// ---- schema samples and detection ------------------------------------------------

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

const COMPACT_LOG: &str = "10:00:01 WARN: disk almost full\n10:00:02 INFO: recovered\n";

fn compact_schema() -> Extractor {
    Extractor::with_timestamp_format("compact", "<timestamp> <level>: <message>", "%H:%M:%S")
        .unwrap()
}

/// Every schema shipped in the binary carries samples, and they pass.
#[test]
fn the_builtin_schema_validates_its_own_samples() {
    let extractor = default_extractor();
    assert_eq!(extractor.samples.len(), 3);
    assert_eq!(extractor.level_field(), Some("log_level"));
    for sample in &extractor.samples {
        let fields = extractor.extract(&sample.line).expect("sample parses");
        assert_eq!(fields["log_level"], *sample.level.as_ref().unwrap());
    }
}

/// The whole point of samples: the pre-`<error_code?>` format still *matches* the error
/// line, it just extracts the wrong level. A sample turns that into a load-time error.
#[test]
fn a_sample_catches_a_format_that_matches_but_mis_parses() {
    let error_line = "2026-06-16 10:12:08.631 [HOST:h1][SERVER:S][PID:53][THR:135][Query Engine][Error][0x800424FB][UID:5CCC][SID:B830][OID:72EF][Q.cpp:6580] boom";

    // The legacy format happily matches the line...
    let legacy = Extractor::new("legacy", BRACKETED_LEGACY_FORMAT).unwrap();
    let fields = legacy.extract(error_line).unwrap();
    assert_eq!(
        fields["log_level"], "Error][0x800424FB",
        "matches, but wrongly"
    );

    // ...and a sample asserting the level rejects it, naming the value it actually got.
    let error = Extractor::new("legacy", BRACKETED_LEGACY_FORMAT)
        .unwrap()
        .with_samples(vec![SampleLine::with_level(error_line, "Error")])
        .unwrap_err();
    assert_eq!(
        error,
        r#"sample 1 parsed level "Error][0x800424FB", expected "Error""#
    );

    // The current format accepts the same sample.
    assert!(default_extractor()
        .with_samples(vec![SampleLine::with_level(error_line, "Error")])
        .is_ok());
}

#[test]
fn a_sample_that_does_not_match_at_all_is_rejected() {
    let error = compact_schema()
        .with_samples(vec![SampleLine::new("this is not a compact log line")])
        .unwrap_err();
    assert!(
        error.starts_with("sample 1 does not match the format:"),
        "{error}"
    );
}

#[test]
fn a_sample_expecting_a_level_needs_a_level_field() {
    let error = Extractor::new("bodyonly", "<timestamp> <message>")
        .unwrap()
        .with_samples(vec![SampleLine::with_level("10:00:01 hello", "INFO")])
        .unwrap_err();
    assert!(error.contains("no level field"), "{error}");
}

/// A schema without samples still loads: samples are a guard, not a requirement.
#[test]
fn samples_are_optional() {
    assert!(Extractor::new("bare", "<message>").is_ok());
}

// ---- detection -------------------------------------------------------------------

#[test]
fn detection_prefers_the_specific_schema_over_a_catch_all() {
    let generic = Extractor::new("aaa-generic", "<message>").unwrap();
    let bracketed = default_extractor();
    let candidates = vec![&generic, &bracketed];

    // `<message>` matches every line of an bracketed log, so scoring alone would pick it.
    assert_eq!(generic.match_score(&lines(SAMPLE)), SAMPLE.lines().count());
    assert!(bracketed.specificity() > generic.specificity());

    let chosen = detect(candidates, &lines(SAMPLE)).unwrap();
    assert_eq!(chosen.name, "Bracketed default");
}

#[test]
fn detection_picks_the_schema_that_actually_matches() {
    let compact = compact_schema();
    let bracketed = default_extractor();

    let chosen = detect(vec![&compact, &bracketed], &lines(COMPACT_LOG)).unwrap();
    assert_eq!(chosen.name, "compact");
    assert_eq!(bracketed.match_score(&lines(COMPACT_LOG)), 0);

    let chosen = detect(vec![&compact, &bracketed], &lines(SAMPLE)).unwrap();
    assert_eq!(chosen.name, "Bracketed default");
}

/// Continuation lines of a multi-line record do not match the schema, and must not
/// disqualify it.
#[test]
fn detection_tolerates_multiline_continuations() {
    let body = format!(
        "{}\n    at frame_one() (a.cpp:1)\n    at frame_two() (b.cpp:2)\n{}",
        SAMPLE.lines().next().unwrap(),
        SAMPLE.lines().next().unwrap()
    );
    let bracketed = default_extractor();
    assert_eq!(
        bracketed.match_score(&lines(&body)),
        2,
        "only the record starts match"
    );
    assert_eq!(
        detect(vec![&bracketed], &lines(&body)).unwrap().name,
        "Bracketed default"
    );
}

#[test]
fn detection_returns_none_when_nothing_explains_the_file() {
    let bracketed = default_extractor();
    let compact = compact_schema();
    assert!(detect(
        vec![&bracketed, &compact],
        &lines("just prose\nmore prose\n")
    )
    .is_none());
    assert!(detect(vec![&bracketed], &[]).is_none());
}

/// The bug this replaced: `extractors` is a HashMap, so the old `values().next()` default
/// changed between runs. Detection must be a total, reproducible order.
#[test]
fn detection_is_deterministic_across_equally_scoring_schemas() {
    let a = Extractor::new("aaa", "<message>").unwrap();
    let b = Extractor::new("bbb", "<message>").unwrap();
    let c = Extractor::new("ccc", "<message>").unwrap();
    assert_eq!(a.specificity(), b.specificity());

    let text = lines("anything at all\n");
    for order in [vec![&a, &b, &c], vec![&c, &b, &a], vec![&b, &c, &a]] {
        assert_eq!(
            detect(order, &text).unwrap().name,
            "aaa",
            "ties break by name"
        );
    }
}

// ---- project-level detection -----------------------------------------------------

#[test]
fn adding_a_file_detects_its_schema_instead_of_guessing() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("server.log"), SAMPLE).unwrap();
    std::fs::write(tmp.path().join("compact.log"), COMPACT_LOG).unwrap();

    let mut project = Project::new(tmp.path());
    project.add_extractor(default_extractor()).unwrap();
    project.add_extractor(compact_schema()).unwrap();
    project
        .add_extractor(Extractor::new("aaa-generic", "<message>").unwrap())
        .unwrap();

    assert_eq!(
        project
            .add_file(tmp.path().join("server.log"), None)
            .extractor_name,
        "Bracketed default"
    );
    assert_eq!(
        project
            .add_file(tmp.path().join("compact.log"), None)
            .extractor_name,
        "compact"
    );
}

#[test]
fn adding_a_folder_loads_direct_text_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("b.log"), "plain text\n").unwrap();
    std::fs::write(tmp.path().join("a.txt"), "more text\n").unwrap();
    std::fs::write(tmp.path().join("bin.dat"), b"text\0binary").unwrap();
    std::fs::create_dir(tmp.path().join("nested")).unwrap();
    std::fs::write(tmp.path().join("nested").join("c.log"), "nested\n").unwrap();

    let paths = text_files_in_dir(tmp.path()).unwrap();
    let names: Vec<String> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, ["a.txt", "b.log"]);

    let mut project = Project::new(tmp.path());
    let added = project.add_text_files_from_dir(tmp.path()).unwrap();
    assert_eq!(added, 2);
    let names: Vec<&str> = project
        .files
        .iter()
        .map(|file| file.display_name.as_str())
        .collect();
    assert_eq!(names, ["a.txt", "b.log"]);
}

#[test]
fn live_source_config_builds_supported_follow_commands() {
    let kube = LiveSourceConfig {
        kind: LiveSourceKind::Kubernetes,
        namespace: "prod".to_string(),
        pod: "api-7d9".to_string(),
        container: "web".to_string(),
        context: "cluster-a".to_string(),
        tail: "200".to_string(),
        since: "10m".to_string(),
        ..LiveSourceConfig::default()
    };
    assert_eq!(
        kube.command_parts(),
        (
            "kubectl".to_string(),
            vec![
                "--context",
                "cluster-a",
                "logs",
                "-f",
                "-n",
                "prod",
                "-c",
                "web",
                "--tail",
                "200",
                "--since",
                "10m",
                "api-7d9",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        )
    );

    let docker = LiveSourceConfig {
        kind: LiveSourceKind::Docker,
        docker_container: "worker".to_string(),
        tail: "50".to_string(),
        ..LiveSourceConfig::default()
    };
    assert_eq!(
        docker.command_parts(),
        (
            "docker".to_string(),
            vec!["logs", "-f", "--tail", "50", "worker"]
                .into_iter()
                .map(str::to_string)
                .collect()
        )
    );

    let journal = LiveSourceConfig {
        kind: LiveSourceKind::Journalctl,
        unit: "nginx.service".to_string(),
        since: "2026-07-15 09:00:00".to_string(),
        ..LiveSourceConfig::default()
    };
    assert_eq!(
        journal.command_parts(),
        (
            "journalctl".to_string(),
            vec![
                "-f",
                "-u",
                "nginx.service",
                "--since",
                "2026-07-15 09:00:00"
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        )
    );
}

#[test]
fn project_persists_live_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = Project::new(tmp.path());
    let config = LiveSourceConfig {
        kind: LiveSourceKind::Docker,
        docker_container: "api".to_string(),
        tail: "100".to_string(),
        ..LiveSourceConfig::default()
    };
    let file_id = project
        .add_live_source(config.clone(), "docker api", None)
        .file_id
        .clone();
    {
        let file = project.get_file_mut(&file_id).unwrap();
        file.description = "application container".to_string();
        file.tag = "app_log".to_string();
    }
    project.save().unwrap();

    let reloaded = Project::load(tmp.path());
    let file = reloaded.get_file(&file_id).unwrap();
    assert_eq!(file.display_name, "docker api");
    assert_eq!(file.extractor_name, GENERIC_EXTRACTOR_NAME);
    assert_eq!(file.description, "application container");
    assert_eq!(file.tag, "app_log");
    assert_eq!(file.live.as_ref(), Some(&config));
    assert!(file
        .path
        .ends_with(std::path::Path::new(".logscouter/live/f1.log")));
}

/// An explicit schema name still wins over detection.
#[test]
fn an_explicit_schema_name_skips_detection() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("server.log"), SAMPLE).unwrap();

    let mut project = Project::new(tmp.path());
    project.add_extractor(default_extractor()).unwrap();
    project.add_extractor(compact_schema()).unwrap();
    let name = project
        .add_file(tmp.path().join("server.log"), Some("compact".to_string()))
        .extractor_name
        .clone();
    assert_eq!(name, "compact");
}

/// A file nothing matches, and an unreadable one, both land on a stable fallback.
#[test]
fn the_fallback_schema_is_the_builtin_not_a_hash_order_accident() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("junk.log"), "prose\nmore prose\n").unwrap();

    let mut project = Project::new(tmp.path());
    project.add_extractor(default_extractor()).unwrap();
    project.add_extractor(compact_schema()).unwrap();
    project
        .add_extractor(Extractor::new("aaa-first-alphabetically", "<a>=<b>").unwrap())
        .unwrap();

    assert_eq!(project.default_extractor_obj().name, "Bracketed default");
    assert_eq!(
        project
            .add_file(tmp.path().join("missing.log"), None)
            .extractor_name,
        "Bracketed default",
        "an unreadable file falls back rather than erroring"
    );
}

/// Without the built-in present, the fallback is still stable: first name alphabetically.
#[test]
fn the_fallback_without_the_builtin_is_the_first_name() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = Project::new(tmp.path());
    project.extractors.clear();
    project.add_extractor(compact_schema()).unwrap();
    project
        .add_extractor(Extractor::new("aaa-other", "<message>").unwrap())
        .unwrap();
    assert_eq!(project.default_extractor_obj().name, "aaa-other");
}

/// Import is strict: a schema whose sample mis-parses is rejected, with the file named.
#[test]
fn importing_a_schema_whose_sample_mis_parses_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("wrong.json"),
        serde_json::json!({
            "name": "wrong",
            "schema": {
                "name": "wrong",
                "format": "<timestamp> <level>: <message>",
                "samples": [{"line": "10:00:01 WARN: disk full", "level": "ERROR"}]
            }
        })
        .to_string(),
    )
    .unwrap();

    let error = load_schemas_from_folder(&folder).unwrap_err();
    let text = format!("{error}");
    assert!(text.contains("wrong.json"), "{text}");
    assert!(
        text.contains(r#"parsed level "WARN", expected "ERROR""#),
        "{text}"
    );
}

/// Loading a project is relaxed: a stored schema with a stale sample must survive, or
/// every file using it would be silently repointed at another schema.
#[test]
fn a_stored_schema_with_a_failing_sample_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".logscouter")).unwrap();
    std::fs::write(
        tmp.path().join(".logscouter/project.json"),
        serde_json::json!({
            "version": 1,
            "extractors": [{
                "name": "stale",
                "format": "<timestamp> <level>: <message>",
                "samples": [{"line": "10:00:01 WARN: x", "level": "NOPE"}]
            }],
        })
        .to_string(),
    )
    .unwrap();

    let project = Project::load(tmp.path());
    assert!(
        project.extractors.contains_key("stale"),
        "relaxed load must keep the schema: {:?}",
        project.extractors.keys().collect::<Vec<_>>()
    );

    // But re-adding it through the strict path still rejects it.
    let mut strict = Project::new(tmp.path());
    let stale = project.extractors["stale"].clone();
    assert!(strict.add_extractor(stale).is_err());
}

/// Samples survive an export/import round trip, so the guard travels with the schema.
#[test]
fn samples_round_trip_through_a_schema_pack() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("schemas");
    export_schemas_to_folder(&[default_extractor()], &folder).unwrap();

    let loaded = load_schemas_from_folder(&folder).unwrap();
    assert_eq!(loaded[0].schema.samples.len(), 3);
    assert_eq!(loaded[0].schema.samples[1].level.as_deref(), Some("Error"));
}

// ---- generic fallback schema, timestamp sniffing, and the merge they rescue ----------

#[test]
fn sniff_reads_the_iso_timestamp_family_off_a_raw_line() {
    let expect = NaiveDate::from_ymd_opt(2026, 6, 16)
        .unwrap()
        .and_hms_milli_opt(10, 9, 43, 288)
        .unwrap();

    assert_eq!(
        sniff_timestamp("2026-06-16 10:09:43.288 INFO x"),
        Some(expect)
    );
    assert_eq!(
        sniff_timestamp("2026-06-16T10:09:43,288Z INFO x"),
        Some(expect)
    );
    assert_eq!(
        sniff_timestamp("[2026/06/16 10:09:43.288] INFO x"),
        Some(expect)
    );
    // Sub-millisecond precision is padded, not truncated.
    assert_eq!(
        sniff_timestamp("2026-06-16 10:09:43.2880000 x"),
        Some(expect)
    );

    // No leading timestamp, an impossible date, and a bare time all decline.
    assert_eq!(sniff_timestamp("        at Foo.bar(Foo.java:12)"), None);
    assert_eq!(sniff_timestamp("2026-13-45 10:09:43.288 x"), None);
    assert_eq!(sniff_timestamp("10:09:43 WARN disk full"), None);
}

#[test]
fn a_log_no_schema_explains_falls_back_to_one_entry_per_line() {
    // Under the bracketed schema no line starts a record, so the whole file used to fold
    // into a single entry. The catch-all keeps the lines apart.
    let lines = [
        "2026-06-16 10:00:01,000 INFO alpha".to_string(),
        "2026-06-16 10:00:02,000 INFO beta".to_string(),
    ];
    let bracketed = default_extractor();
    let generic = generic_extractor();

    let chosen = detect([&bracketed, &generic], &lines).expect("a schema always wins");
    assert_eq!(chosen.name, GENERIC_EXTRACTOR_NAME);
    // The bracketed schema is more specific, so it still wins a file it can read.
    let bracketed_lines = [SAMPLE.lines().next().unwrap().to_string()];
    let chosen = detect([&bracketed, &generic], &bracketed_lines).unwrap();
    assert_eq!(chosen.name, bracketed.name);
}

#[test]
fn the_generic_schema_carries_a_sniffed_timestamp() {
    let extractor = generic_extractor();
    let mut model = LogFileModel::new(
        "f1",
        "plain.log",
        extractor.name.clone(),
        "",
        Some(extractor),
    );
    model.load_from_lines(["2026-06-16 10:00:02,500 INFO beta one"]);

    assert_eq!(model.entries.len(), 1);
    assert_eq!(
        model.message(&model.entries[0]),
        "2026-06-16 10:00:02,500 INFO beta one"
    );
    assert_eq!(
        model.timestamp(&model.entries[0]),
        NaiveDate::from_ymd_opt(2026, 6, 16)
            .unwrap()
            .and_hms_milli_opt(10, 0, 2, 500)
    );
}

#[test]
fn a_folder_of_mixed_formats_merges_in_timestamp_order() {
    let tmp = tempfile::tempdir().unwrap();
    let bracketed = tmp.path().join("a.log");
    let plain = tmp.path().join("b.log");
    std::fs::write(
        &bracketed,
        "2026-06-16 10:00:01.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:1] alpha one\n\
         2026-06-16 10:00:03.000 [HOST:h][SERVER:S][PID:1][THR:2][Kernel][Trace][UID:0][SID:0][OID:0][a.cpp:2] alpha two\n",
    )
    .unwrap();
    std::fs::write(
        &plain,
        "2026-06-16 10:00:02.000 INFO beta one\n2026-06-16 10:00:04.000 INFO beta two\n",
    )
    .unwrap();

    let mut project = Project::load(tmp.path());
    project.add_file(&bracketed, None);
    project.add_file(&plain, None);
    project.load_all_files();
    assert_eq!(project.files[1].extractor_name, GENERIC_EXTRACTOR_NAME);

    let sources: Vec<&LogFileModel> = project.files.iter().collect();
    let merged = merge_files("m1", &sources);
    assert_eq!(merged.entries.len(), 4);

    // The two schemas interleave: each file's lines land where their timestamps say.
    let expected = ["alpha one", "beta one", "alpha two", "beta two"];
    for (entry, want) in merged.entries.iter().zip(expected) {
        assert!(
            entry.raw.ends_with(want),
            "{:?} should end with {want}",
            entry.raw
        );
    }
    let stamps: Vec<_> = merged
        .entries
        .iter()
        .map(|entry| merged.timestamp(entry).expect("every line carries a time"))
        .collect();
    assert!(
        stamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "{stamps:?}"
    );
}

#[test]
fn a_stored_schema_that_explains_nothing_is_re_detected_on_load() {
    // A project.json written before the catch-all existed pinned the bracketed schema on
    // a file it cannot read. Loading heals it rather than serving one giant entry.
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain.log");
    std::fs::write(
        &plain,
        "2026-06-16 10:00:02,000 INFO beta one\nsecond line\n",
    )
    .unwrap();

    let mut project = Project::new(tmp.path());
    project.add_file(&plain, Some("Bracketed default".to_string()));
    assert_eq!(project.files[0].extractor_name, "Bracketed default");

    project.load_all_files();
    assert_eq!(project.files[0].extractor_name, GENERIC_EXTRACTOR_NAME);
    assert_eq!(project.files[0].entries.len(), 2);
}

#[test]
fn a_deliberate_schema_that_does_explain_the_file_survives_load() {
    // Healing must only fire on a schema that matches nothing at all.
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("compact.log");
    std::fs::write(&log, COMPACT_LOG).unwrap();

    let mut project = Project::new(tmp.path());
    let compact =
        Extractor::with_timestamp_format("compact", "<timestamp> <level>: <message>", "%H:%M:%S")
            .unwrap();
    project.add_extractor(compact).unwrap();
    project.add_file(&log, Some("compact".to_string()));

    project.load_all_files();
    assert_eq!(project.files[0].extractor_name, "compact");
    assert_eq!(project.files[0].level(&project.files[0].entries[0]), "WARN");
}

// ---- pattern derivation -------------------------------------------------------------

#[test]
fn differing_tokens_collapse_to_the_tightest_shared_shape() {
    let class = |left: &str, right: &str| {
        common_message_pattern(&[&format!("x {left} y"), &format!("x {right} y")]).unwrap()
    };

    assert_eq!(
        class("0x800424FB", "0x8004010C"),
        r"x\s+0[xX][0-9a-fA-F]+\s+y"
    );
    assert_eq!(
        class("10.0.0.5", "10.0.0.6"),
        r"x\s+\d{1,3}(?:\.\d{1,3}){3}\s+y"
    );
    assert_eq!(class("1.25", "3.5"), r"x\s+\d+[.,]\d+\s+y");
    assert_eq!(
        class(
            "8ea1c0de-1111-2222-3333-444455556666",
            "9fb2d1ef-7777-8888-9999-aaaabbbbcccc"
        ),
        r"x\s+[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\s+y"
    );
    // Words are not identifiers, however hex-looking.
    assert_eq!(class("alpha", "gamma"), r"x\s+\S+\s+y");
}

#[test]
fn a_differing_token_keeps_the_part_every_line_agrees_on() {
    let pattern = common_message_pattern(&["session id=1 opened", "session id=27 opened"]).unwrap();
    assert_eq!(pattern, r"session\s+id=\d+\s+opened");

    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("session id=99 opened"));
    assert!(!regex.is_match("session id=x opened"));
}

#[test]
fn a_column_of_pure_wildcards_never_becomes_the_template() {
    // "5" and "7" share a shape but no literal text; a bare `\d+` would hide every line
    // holding a number, so the lines are matched literally instead.
    let pattern = common_message_pattern(&["5", "7"]).unwrap();
    assert_eq!(pattern, "(?:5|7)");
}

#[test]
fn a_single_line_generalises_its_value_shapes_and_keeps_its_words() {
    let pattern = message_template("Session 900 created for user analyst").unwrap();
    assert_eq!(pattern, r"Session\s+\d+\s+created\s+for\s+user\s+analyst");

    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("Session 41 created for user analyst"));
    assert!(!regex.is_match("Session 41 destroyed for user analyst"));

    // Punctuation clinging to a value is kept; the value itself gives way.
    assert_eq!(
        message_template("timeout after 30s, retry=4;").unwrap(),
        r"timeout\s+after\s+30s,\s+retry=\d+;"
    );
    // A message with nothing to generalise is still a usable literal pattern.
    assert_eq!(message_template("cache miss").unwrap(), r"cache\s+miss");
    // ... and one with nothing *but* a value falls back to the value itself, rather than
    // to a bare `\d+` that would hide every line holding a number.
    assert_eq!(message_template("900").unwrap(), "900");
    assert_eq!(message_template("   "), None);
}

// ---- the ladder of templates `H` offers ---------------------------------------------

/// Names of the offered templates, in the order the derivation produced them.
fn candidate_names(messages: &[&str]) -> Vec<&'static str> {
    pattern_candidates(messages)
        .iter()
        .map(|option| option.name)
        .collect()
}

fn candidate(messages: &[&str], name: &str) -> String {
    pattern_candidates(messages)
        .into_iter()
        .find(|option| option.name == name)
        .unwrap_or_else(|| panic!("no {name} template among {:?}", candidate_names(messages)))
        .pattern
}

#[test]
fn the_ladder_runs_from_the_loosest_strategy_to_the_strictest() {
    let lines = ["a 1 c", "a 2 c"];
    assert_eq!(
        candidate_names(&lines),
        ["loose", "prefix", "wildcard", "typed", "exact"]
    );
    assert_eq!(candidate(&lines, "loose"), r"a.*c");
    assert_eq!(candidate(&lines, "prefix"), r"a.*");
    assert_eq!(candidate(&lines, "wildcard"), r"a\s+\S+\s+c");
    assert_eq!(candidate(&lines, "typed"), r"a\s+\d+\s+c");
    assert_eq!(candidate(&lines, "exact"), "(?:a 1 c|a 2 c)");

    // Every template must still match the lines it was derived from.
    for option in pattern_candidates(&lines) {
        let regex = regex::Regex::new(&option.pattern).unwrap();
        for line in lines {
            assert!(regex.is_match(line), "{} misses {line:?}", option.name);
        }
    }
}

#[test]
fn a_template_is_offered_once_however_many_strategies_reach_it() {
    // The differing tokens share no value shape, so `typed` lands on `wildcard`'s regex.
    let lines = ["a x1 c", "a 2y c"];
    let names = candidate_names(&lines);
    assert!(names.contains(&"wildcard"), "{names:?}");
    assert!(!names.contains(&"typed"), "duplicate offered: {names:?}");
    assert_eq!(candidate(&lines, "wildcard"), r"a\s+\S+\s+c");
}

#[test]
fn lines_of_different_lengths_still_get_the_loose_rungs() {
    // The aligned strategies need equal token counts; `loose` and `prefix` do not.
    let lines = [
        "Cache miss for session 12",
        "Cache miss for user bob in session 13",
    ];
    assert_eq!(candidate_names(&lines), ["loose", "prefix", "exact"]);
    assert_eq!(candidate(&lines, "loose"), r"Cache.*miss.*for.*session");
    assert_eq!(candidate(&lines, "prefix"), r"Cache\s+miss\s+for.*");
}

#[test]
fn a_lone_line_is_not_offered_a_wildcard_it_cannot_derive() {
    // With nothing to diff against, `\S+ where the lines differ` would be the line itself.
    let lines = ["Session 900 created for user analyst"];
    let names = candidate_names(&lines);
    assert!(!names.contains(&"wildcard"), "{names:?}");
    assert_eq!(
        candidate(&lines, "typed"),
        r"Session\s+\d+\s+created\s+for\s+user\s+analyst"
    );
    assert_eq!(
        candidate(&lines, "prefix"),
        r"Session\s+900\s+created\s+for\s+user\s+analyst.*"
    );

    assert!(pattern_candidates(&[]).is_empty());
    assert!(pattern_candidates(&["  ", "   "]).is_empty());
}

/// Whatever else the ladder offers, the rung `H` opens on is one of its own.
#[test]
fn the_default_template_is_one_of_the_offered_rungs() {
    for lines in [
        vec!["a 1 c", "a 2 c"],
        vec!["alpha beta", "gamma delta"],
        vec!["Cache miss for session 12", "Cache miss for user bob in 13"],
        vec!["5", "7"],
    ] {
        let default = common_message_pattern(&lines).unwrap();
        let patterns: Vec<String> = pattern_candidates(&lines)
            .into_iter()
            .map(|option| option.pattern)
            .collect();
        assert!(
            patterns.contains(&default),
            "default {default:?} missing from {patterns:?}"
        );
    }

    let default = message_template("Session 900 created").unwrap();
    let patterns: Vec<String> = pattern_candidates(&["Session 900 created"])
        .into_iter()
        .map(|option| option.pattern)
        .collect();
    assert!(patterns.contains(&default), "{patterns:?}");
}

// ---- ANDing a line's fields into one regex -------------------------------------------

const TRACE_LINE: &str = "2026-06-16 10:09:43.288 [HOST:h1][SERVER:AppServer][PID:54][THR:1366][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:394] NetChannel closed";
const OTHER_HOST: &str = "2026-06-16 10:09:44.000 [HOST:h2][SERVER:AppServer][PID:54][THR:1366][Kernel][Trace][UID:0][SID:0][OID:0][D.cpp:395] other host";

#[test]
fn field_pattern_pins_the_chosen_fields_and_frees_the_rest() {
    let extractor = default_extractor();
    let chosen = [
        ("host".to_string(), "h1".to_string()),
        ("log_level".to_string(), "Trace".to_string()),
    ];
    let pattern = extractor.field_pattern(&chosen).unwrap();
    let regex = regex::Regex::new(&pattern).unwrap();

    // Both pinned fields must hold. Neither alone is enough.
    assert!(regex.is_match(TRACE_LINE));
    assert!(!regex.is_match(OTHER_HOST), "host was not pinned");
    assert!(!regex.is_match(ERROR_LINE), "level was not pinned");

    // A continuation line belongs to its entry's raw text, so `.` must cross newlines.
    let multi = format!("{TRACE_LINE}\n  at Foo.bar(Foo.java:12)");
    assert!(regex.is_match(&multi), "multi-line entry rejected");
}

#[test]
fn field_pattern_pins_an_optional_field_in_both_directions() {
    let extractor = default_extractor();

    // `ERROR_LINE` carries `[0x800424FB]`; pinning it excludes lines without a code.
    let present = [("error_code".to_string(), "0x800424FB".to_string())];
    let regex = regex::Regex::new(&extractor.field_pattern(&present).unwrap()).unwrap();
    assert!(regex.is_match(ERROR_LINE));
    assert!(!regex.is_match(TRACE_LINE));

    // Pinning it to the empty value the source line had means "lines with no code",
    // because the optional group carries its own separator and simply goes away.
    let absent = [
        ("log_level".to_string(), "Trace".to_string()),
        ("error_code".to_string(), String::new()),
    ];
    let regex = regex::Regex::new(&extractor.field_pattern(&absent).unwrap()).unwrap();
    assert!(regex.is_match(TRACE_LINE));
    assert!(!regex.is_match(ERROR_LINE), "a coded line slipped through");
}

#[test]
fn field_pattern_of_a_format_with_no_fields_is_none() {
    let bare = Extractor::new("bare", "no placeholders here").unwrap();
    assert_eq!(bare.field_pattern(&[]), None);

    // The catch-all has exactly one field, so pinning it is just the message.
    let generic = generic_extractor();
    let chosen = [("message".to_string(), "hello".to_string())];
    let pattern = generic.field_pattern(&chosen).unwrap();
    let regex = regex::Regex::new(&pattern).unwrap();
    assert!(regex.is_match("hello"));
    assert!(!regex.is_match("goodbye"));
}

// ---- the time range is a slot, not one rule among many -------------------------------

#[test]
fn a_time_range_is_recognised_whatever_the_field_is_called() {
    for field in ["timestamp", "time", "ts"] {
        let rule = FilterRule::new(field, "range", "a..b", "include");
        assert!(rule.is_time_range(), "{field} range not recognised");
    }
    // A range over something else is an ordinary filter, and so is an equals on the time.
    assert!(!FilterRule::new("line_number", "range", "1..9", "include").is_time_range());
    assert!(!FilterRule::new("timestamp", "equals", "a", "include").is_time_range());
}

#[test]
fn time_bounds_split_a_range_and_allow_an_open_end() {
    let bounded = FilterRule::new("timestamp", "range", " a .. b ", "include");
    assert_eq!(bounded.time_bounds(), ("a", "b"));
    let from = FilterRule::new("timestamp", "range", "a..", "include");
    assert_eq!(from.time_bounds(), ("a", ""));
    let until = FilterRule::new("timestamp", "range", "..b", "include");
    assert_eq!(until.time_bounds(), ("", "b"));
}

/// A project answers "when" once. Adding a second range replaces the first, in place.
#[test]
fn a_second_time_range_replaces_the_first_and_keeps_its_place() {
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new("level", "equals", "Trace", "exclude"));
    filters.add(FilterRule::new("timestamp", "range", "a..b", "include"));
    filters.add(FilterRule::new("module", "contains", "SQL", "include"));
    assert_eq!(filters.rules.len(), 3);
    assert_eq!(filters.time_index(), Some(1));

    filters.add(FilterRule::new("timestamp", "range", "c..d", "include"));
    assert_eq!(filters.rules.len(), 3, "a second range was appended");
    assert_eq!(filters.time_index(), Some(1), "the range moved");
    assert_eq!(filters.time_rule().unwrap().value, "c..d");

    // The text rules are everything else, and keep their real indices.
    let text: Vec<(usize, &str)> = filters
        .text_rules()
        .map(|(index, rule)| (index, rule.field.as_str()))
        .collect();
    assert_eq!(text, [(0, "level"), (2, "module")]);

    filters.clear_time();
    assert_eq!(filters.time_rule(), None);
    assert_eq!(filters.rules.len(), 2);
    // Clearing a range that is not there is not an error.
    filters.clear_time();
    assert_eq!(filters.rules.len(), 2);
}

/// The replacement holds through the filter machinery, not just the bookkeeping.
#[test]
fn only_the_latest_time_range_filters_the_log() {
    let model = sample_model();
    let mut filters = FilterSet::default();
    filters.add(FilterRule::new(
        "timestamp",
        "range",
        "2026-06-16 10:09:43..2026-06-16 10:12:20",
        "include",
    ));
    let wide = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .count();

    filters.add(FilterRule::new(
        "timestamp",
        "range",
        "2026-06-16 10:09:43..2026-06-16 10:09:44",
        "include",
    ));
    let narrow = model
        .entries
        .iter()
        .filter(|entry| filters.visible(&model, entry))
        .count();

    assert!(
        narrow < wide,
        "the first range still applies: {narrow} of {wide}"
    );
    assert_eq!(filters.rules.len(), 1);
}

// ---- the time slot tolerates no leftovers -------------------------------------------

#[test]
fn set_time_removes_every_earlier_range_not_just_the_first() {
    let mut filters = FilterSet::default();
    // Two stale ranges, as an old build could have left behind.
    filters
        .rules
        .push(FilterRule::new("timestamp", "range", "a..b", "include"));
    filters
        .rules
        .push(FilterRule::new("level", "equals", "Trace", "exclude"));
    filters
        .rules
        .push(FilterRule::new("timestamp", "range", "c..d", "include"));

    filters.set_time(FilterRule::new("timestamp", "range", "e..f", "include"));
    let ranges: Vec<&str> = filters
        .rules
        .iter()
        .filter(|rule| rule.is_time_range())
        .map(|rule| rule.value.as_str())
        .collect();
    assert_eq!(ranges, ["e..f"], "a stale range survived set_time");
    // The text rule is untouched.
    assert_eq!(filters.text_rules().count(), 1);
}

#[test]
fn dedupe_keeps_the_last_range_and_is_idempotent() {
    let mut filters = FilterSet::default();
    filters
        .rules
        .push(FilterRule::new("timestamp", "range", "old..x", "include"));
    filters
        .rules
        .push(FilterRule::new("host", "equals", "h", "exclude"));
    filters
        .rules
        .push(FilterRule::new("timestamp", "range", "new..y", "include"));

    filters.dedupe_time_range();
    assert_eq!(filters.time_rule().unwrap().value, "new..y");
    assert_eq!(
        filters.rules.iter().filter(|r| r.is_time_range()).count(),
        1
    );
    // Dropping the earlier range shifts the text rule up, but keeps its order.
    assert_eq!(filters.rules[0].field, "host");

    // Running it again changes nothing.
    let before = filters.clone();
    filters.dedupe_time_range();
    assert_eq!(filters, before);

    // Nothing to do when there is no range.
    let mut none = FilterSet::default();
    none.add(FilterRule::new("level", "equals", "Trace", "exclude"));
    let before = none.clone();
    none.dedupe_time_range();
    assert_eq!(none, before);
}

/// The reported bug: a stale wider `include` range OR's a narrow one back open, so lines
/// outside the intended window stay visible. One range must mean one window.
#[test]
fn a_leftover_wider_range_does_not_reopen_the_window() {
    let model = sample_model();
    let inside = |filters: &FilterSet| {
        model
            .entries
            .iter()
            .filter(|entry| filters.visible(&model, entry))
            .count()
    };

    // A narrow window: just the first couple of seconds.
    let mut narrow = FilterSet::default();
    narrow.add(FilterRule::new(
        "timestamp",
        "range",
        "2026-06-16 10:09:43..2026-06-16 10:09:44",
        "include",
    ));
    let narrow_count = inside(&narrow);

    // Simulate the broken on-disk state: a stale wider range sitting beside it.
    let mut broken = narrow.clone();
    broken.rules.insert(
        0,
        FilterRule::new(
            "timestamp",
            "range",
            "2026-06-16 10:09:43..2026-06-16 10:20:00",
            "include",
        ),
    );
    assert!(
        inside(&broken) > narrow_count,
        "two include ranges should OR wider -- otherwise this test proves nothing"
    );

    // Loading heals it; the window is narrow again.
    broken.dedupe_time_range();
    assert_eq!(inside(&broken), narrow_count);
}

/// The memoised one-pass field extraction must agree with per-field access, and repeat
/// reads must stay stable. Uses a many-field JSON schema -- the shape whose ~11 ms parse
/// made field-at-a-time access stall the UI, now cached.
#[test]
fn cached_field_extraction_matches_and_is_stable() {
    let format = "{\"@timestamp\":\"<timestamp>\",\"message\":\"<message>\",\"logger_name\":\"<logger>\",\"level\":\"<level>\"}";
    let mut extractor =
        Extractor::with_timestamp_format("json", format, "%Y-%m-%dT%H:%M:%S%.3fZ").unwrap();
    extractor.entry_start = r"^\{".to_string();
    extractor.compile().unwrap();

    let body = "{\"@timestamp\":\"2026-07-15T08:12:59.414Z\",\"message\":\"boom, with a comma\",\"logger_name\":\"com.mstr.Rest\",\"level\":\"ERROR\"}";
    let mut model = LogFileModel::new(
        "f1",
        "m.log",
        extractor.name.clone(),
        "",
        Some(extractor.clone()),
    );
    model.load_from_lines(body.lines());
    let entry = &model.entries[0];

    assert_eq!(model.get_field(entry, "level"), "ERROR");
    assert_eq!(model.get_field(entry, "logger"), "com.mstr.Rest");
    assert_eq!(model.get_field(entry, "message"), "boom, with a comma");
    // Second read hits the cache and returns the same thing.
    assert_eq!(model.get_field(entry, "message"), "boom, with a comma");

    // fields_for (one pass) agrees with field-at-a-time access.
    let map = model.fields_for(entry);
    for field in ["timestamp", "message", "logger", "level"] {
        assert_eq!(
            map.get(field).map(String::as_str).unwrap_or(""),
            model.get_field(entry, field)
        );
    }
}

/// A block schema whose `message` *value* itself spans several physical lines. Without
/// `(?s)` on the whole-entry regex the record fails to parse: `level`/`host` come back
/// blank and the row collapses to a bare `{`. The message must survive with its newlines,
/// and the other fields must still extract.
#[test]
fn multiline_block_schema_extracts_a_message_value_that_spans_lines() {
    let format = "{\n                'timestamp':'<timestamp>',\n                'level': '<level>',\n                'serviceName': '<service>',\n                'className': '<class>',\n                'methodName': '<method>',\n                'message': '<message>',\n                'host': '<host>'\n            }";
    let mut extractor =
        Extractor::with_timestamp_format("python-block", format, "%Y-%m-%d %H:%M:%S,%f").unwrap();
    extractor.entry_start = r"^\s*\{\s*$".to_string();
    extractor.entry_end = r"^\s*\}\s*$".to_string();
    extractor.compile().unwrap();

    let body = "{\n                'timestamp':'2026-07-14 10:01:36,088',\n                'level': 'INFO',\n                'serviceName': 'MSTRBAK-REFRESH',\n                'className': 'IServerV2',\n                'methodName': 'get_iserver_state',\n                'message': 'current iserver state: <state>starting</state>\n<memory_state>normal</memory_state>\n',\n                'host': 'env-oplssbz618oe2fkp-iserver-0'\n            }";

    let mut model = LogFileModel::new(
        "f1",
        "r.log",
        extractor.name.clone(),
        "",
        Some(extractor.clone()),
    );
    model.load_from_lines(body.lines());

    assert_eq!(model.entries.len(), 1);
    assert_eq!(model.get_field(&model.entries[0], "level"), "INFO");
    assert_eq!(
        model.get_field(&model.entries[0], "service"),
        "MSTRBAK-REFRESH"
    );
    assert_eq!(
        model.get_field(&model.entries[0], "host"),
        "env-oplssbz618oe2fkp-iserver-0"
    );
    assert_eq!(
        model.get_field(&model.entries[0], "message"),
        "current iserver state: <state>starting</state>\n<memory_state>normal</memory_state>\n"
    );
    assert_eq!(
        model.timestamp(&model.entries[0]),
        Some(
            NaiveDate::from_ymd_opt(2026, 7, 14)
                .unwrap()
                .and_hms_milli_opt(10, 1, 36, 88)
                .unwrap()
        )
    );
}
