use crate::core::extractor::Extractor;
use crate::core::models::LogEntry;
use std::fs;
use std::io::{BufRead, BufReader, Read};

use std::path::Path;

/// Incremental multi-line grouping: lines that do not start a new record fold into the
/// previous entry. Shared by the blocking reader and the chunked loader so both agree.
#[derive(Debug, Default)]
pub struct EntryBuilder {
    entries: Vec<LogEntry>,
    current: Option<LogEntry>,
    line_index: usize,
}

impl EntryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold `raw` into the running grouping under a **set** of schemas: a line starts a new
    /// entry when *any* schema's `is_start` matches, and closes one when *any* schema's
    /// `is_end` matches -- so a source carrying several formats groups each correctly. An
    /// empty slice means "no schema": every non-empty line is its own entry, as before. A
    /// one-element slice is byte-for-byte the old single-extractor behaviour.
    pub fn push_line(&mut self, raw: &str, schemas: &[&Extractor]) {
        let line = trim_line_end(raw);
        let explicit_boundary = schemas
            .iter()
            .any(|extractor| extractor.uses_explicit_entry_boundary());
        if self.current.is_none() && explicit_boundary && line.trim().is_empty() {
            self.line_index += 1;
            return;
        }

        // With no schema, every line starts an entry (nothing marks a continuation).
        let starts_entry =
            schemas.is_empty() || schemas.iter().any(|extractor| extractor.is_start(line));
        let is_continuation = self.current.is_some() && !starts_entry;

        if is_continuation {
            if let Some(entry) = &mut self.current {
                entry.raw.push('\n');
                entry.raw.push_str(line);
            }
        } else {
            self.flush_current();
            self.current = Some(LogEntry {
                index: self.entries.len(),
                line_no: self.line_index + 1,
                raw: line.to_string(),
                source: 0,
            });
        }
        if schemas.iter().any(|extractor| extractor.is_end(line)) {
            self.flush_current();
        }
        self.line_index += 1;
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn snapshot(&self) -> Vec<LogEntry> {
        let mut entries = self.entries.clone();
        if let Some(entry) = &self.current {
            let mut entry = entry.clone();
            entry.index = entries.len();
            entries.push(entry);
        }
        entries
    }

    pub fn finish(mut self) -> Vec<LogEntry> {
        self.flush_current();
        self.entries
    }

    fn flush_current(&mut self) {
        if let Some(entry) = self.current.take() {
            self.entries.push(entry);
        }
    }
}

pub fn build_entries<I>(lines: I, extractor: Option<&Extractor>) -> Vec<LogEntry>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let schemas: Vec<&Extractor> = extractor.into_iter().collect();
    build_entries_multi(lines, &schemas)
}

/// Group `lines` under an ordered set of schemas. See `EntryBuilder::push_line`.
pub fn build_entries_multi<I>(lines: I, schemas: &[&Extractor]) -> Vec<LogEntry>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut builder = EntryBuilder::new();
    for raw in lines {
        builder.push_line(raw.as_ref(), schemas);
    }
    builder.finish()
}

/// `progress` receives the number of **bytes** consumed so far, so callers can show a
/// bar against the file size.
pub fn read_entries(
    path: &Path,
    schemas: &[&Extractor],
    mut progress: Option<&mut dyn FnMut(u64)>,
) -> std::io::Result<Vec<LogEntry>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut builder = EntryBuilder::new();
    let mut bytes: u64 = 0;

    for (line_index, line_result) in reader.lines().enumerate() {
        let raw_line = line_result?;
        bytes += raw_line.len() as u64 + 1;
        builder.push_line(&raw_line, schemas);

        if let Some(progress) = progress.as_deref_mut() {
            if (line_index & 0x0FFF) == 0 {
                progress(bytes);
            }
        }
    }

    Ok(builder.finish())
}

/// The first `limit` lines, for schema detection. Undecodable bytes are lossy-converted
/// rather than aborting: a schema guess should not fail on one bad byte.
pub fn read_first_lines(path: &Path, limit: usize) -> std::io::Result<Vec<String>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::with_capacity(limit.min(1024));
    for line in reader.split(b'\n').take(limit) {
        lines.push(
            String::from_utf8_lossy(&line?)
                .trim_end_matches('\r')
                .to_string(),
        );
    }
    Ok(lines)
}

pub fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

pub fn file_line_count_hint(path: &Path) -> u64 {
    file_size(path)
}

pub fn read_to_string_lossy(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn trim_line_end(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}
