//! Manual timing harness for the filter/search hot path. Not part of the normal suite.
//!
//! Run with:
//!   SCOUT_BENCH_LOG=examples/<file>.log cargo test --release --test bench_manual -- --ignored --nocapture
use log_scouter::core::extractor::default_extractor;
use log_scouter::core::filters::FilterRule;
use log_scouter::core::models::{LogFileModel, ViewModel};
use log_scouter::core::search::compile_query;
use std::time::Instant;

fn load() -> LogFileModel {
    let path = std::env::var("SCOUT_BENCH_LOG").expect("set SCOUT_BENCH_LOG");
    let extractor = default_extractor();
    let mut model = LogFileModel::new("f1", &path, extractor.name.clone(), "", Some(extractor));
    let start = Instant::now();
    model.load().unwrap();
    eprintln!(
        "load        {:>8.0} ms  ({} entries)",
        start.elapsed().as_secs_f64() * 1000.0,
        model.entries.len()
    );
    model
}

#[test]
#[ignore]
fn bench_filter_then_search() {
    let model = load();
    let mut view = ViewModel::new("L1", &model);

    let start = Instant::now();
    view.filters.add(FilterRule::new(
        "message",
        "contains",
        "NetChannel",
        "exclude",
    ));
    view.rebuild(&model);
    eprintln!(
        "filter      {:>8.0} ms  ({} visible)",
        start.elapsed().as_secs_f64() * 1000.0,
        view.visible.len()
    );

    // First search after the filter: base is cached, so this should walk only the
    // filtered lines.
    for term in ["Kernel", "session", "INBOX_MSG_NOT_FOUND"] {
        let start = Instant::now();
        view.query = Some(compile_query(term));
        view.rebuild(&model);
        eprintln!(
            "search {:<22} {:>8.0} ms  ({} matches)",
            term,
            start.elapsed().as_secs_f64() * 1000.0,
            view.match_set.len()
        );
    }

    // A fresh view has no cached filter pass, so its search pays for filtering too --
    // this is what every search cost before the base cache existed.
    for term in ["Kernel", "session"] {
        let start = Instant::now();
        let mut cold = ViewModel::new("L2", &model);
        cold.filters = view.filters.clone();
        cold.query = Some(compile_query(term));
        cold.rebuild(&model);
        eprintln!(
            "search+refilter {:<17} {:>8.0} ms",
            term,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

#[test]
#[ignore]
fn bench_regex_filter() {
    let model = load();
    let mut view = ViewModel::new("L1", &model);
    let start = Instant::now();
    view.filters.add(FilterRule::new(
        "message",
        "regex",
        r"NetChannel\s+:\s+Channel\s+is\s+closed\.",
        "exclude",
    ));
    view.rebuild(&model);
    eprintln!(
        "regex filter {:>8.0} ms  ({} visible)",
        start.elapsed().as_secs_f64() * 1000.0,
        view.visible.len()
    );
}
