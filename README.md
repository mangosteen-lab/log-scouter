# Log Scouter (`logscout`)

A keyboard-driven **Rust terminal UI** for browsing large server logs. Open a
folder as a project, extract structured fields into columns, hide noise, search
with a log-aware query language, and split panes while keeping the heavy
parsing/filtering path native.

Built with Rust, [Ratatui](https://ratatui.rs), and Crossterm.

## Features

- **Folder = project.** State persists to `<folder>/.logscouter/project.json`.
- **Session restore.** Quitting records the panes, the logs open in each, the split
  and the search. Reopening the folder resumes exactly there.
- **Structured extraction.** Log format expressions use `<field>` placeholders, and
  `<field?>` for a field that is only present on some lines. A bracketed-field
  server log format is built in.
- **Schema detection.** Adding a file matches its first lines against the project's log
  formats and picks the one that explains them, most specific first.
- **Validated schemas.** A log format can carry sample lines with their expected level.
  A format that matches a sample but extracts the wrong value is rejected when it is
  defined or imported, rather than quietly mis-parsing your logs.
- **Elapsed time.** Press `T` on a line to measure every other line from it: the
  timestamp column becomes `+1m56.531s`, `-28.812s`.
- **One operation model.** `Space` selects or deselects whatever the cursor is on —
  a log line, a log source, a filter, a saved search. `Enter` opens its detail view,
  which for sources, filters and searches is an editor you can save.
- **Multi-line records.** Stack traces and wrapped lines fold into the previous
  log entry.
- **Log-aware search.** Supports bare text, quoted phrases, `/regex/`,
  `field=value`, `field~contains`, `after:`, `before:`, and `date:[a..b]`.
- **Search results panel.** Searches open a bottom panel of matched lines. Click
  a result or focus it and press Enter to jump to the source line.
- **Filters.** Include/exclude by field equality, substring, regex, or range.
  Filters apply to the whole project and are saved automatically, so they are
  still in effect the next time you open the folder.
- **Time range picker.** Press `t` for a picker with an editable start and end plus
  quick ranges (`Last 1 hour`, `Last 24 hours`, `Last 7 days`, ...). Quick ranges
  count back from the newest entry in the log, not from the current time.
- **Filter packs.** Export the project's filters to a folder as one JSON file
  per filter, then import a folder to merge those filters back. Defaults to the
  user-level library at `~/.log-scouter/filters`, shared across projects.
- **User-defined log formats.** Define reusable log formats, then assign one per file,
  so a project can mix a bracketed server log with an app log that looks nothing like it.
- **Schema packs.** Export the project's log formats as one JSON file per schema, then
  import a folder to merge them into another project. Defaults to the user-level library
  at `~/.log-scouter/schemas`.
- **Merged views.** `Space` a log in the sidebar to add it to the current pane,
  interleaved by timestamp. Each line keeps its own file's log format and origin, and
  no new entry appears in the file list.
- **Detail panel.** The bottom of the sidebar shows log-entry fields when a pane
  has focus, and project item details when the sidebar has focus.
- **Full log detail.** Press `Enter` on a log row to open a larger detail popup
  with parsed fields and the raw log entry.
- **Progress bars.** Loading, filtering and searching run in slices between frames
  and show a bar; `Esc` cancels. Files open in a pane are marked with a star.
- **Multi-line selection.** `Shift+Up/Down` extends a run across pages;
  `Ctrl+Up/Down` travels without losing it and `Space` picks non-adjacent lines.
- **Mouse selection.** Drag within one row of a log pane to select a **substring**;
  drag across rows to select whole rows. Drag in a detail panel to select detail
  text lines.
- **Copy.** `y` or right-click puts the selected raw lines on your clipboard via
  OSC 52, so it works over SSH. Right-click in a detail panel copies selected
  detail text, or the whole detail content when nothing is selected there.
- **Hide by example.** Press `H` on one line to hide by one of that line's log
  format fields, or select several similar lines to derive a regex.
- **Vim-style navigation.** Supports `j/k`, `gg`, `G`, `[count]j`,
  `[count]G`, paging, and horizontal scroll.
- **Split panes.** Use `|` for columns and `-` for rows.

## Quick Start

Install the `logscout` command from GitHub Releases:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.sh | bash
logscout /path/to/logs
```

Run `logscout .` inside a log folder to add every direct text file in that folder as
a log source. Run `logscout` with no arguments to start an empty project, then press
`o` to open a folder and add its text files.

Upgrade or uninstall with the matching scripts:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/upgrade.sh | bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/uninstall.sh | bash
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/install.ps1 | iex
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/upgrade.ps1 | iex
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/uninstall.ps1 | iex
```

Proxy users can pass the proxy to the outer `curl` and to the installer downloads:

```bash
curl -fsSL -x http://proxy:8080 https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.sh \
  | LOG_SCOUTER_CURL_OPTS="-x http://proxy:8080" bash
```

To install a specific release or custom location:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.sh \
  | LOG_SCOUTER_VERSION=v0.0.3 LOG_SCOUTER_INSTALL_DIR="$HOME/bin" bash
```

GitHub release packages are built when a version tag is pushed. After the
version change is committed and pushed to `master`, the Makefile publishes the
current `Cargo.toml` version as an annotated tag:

```bash
make publish-release
make release-status
```

Without installing:

```bash
./run.sh
./run.sh /path/to/logs
./run.sh /path/to/logs a.log b.log
```

Or use Cargo directly:

```bash
cargo run --release -- /path/to/logs
```

## Concept Model

The app currently uses these persisted concepts:

- **Project**: the folder opened by `logscout`. Project state is saved in
  `<project>/.logscouter/project.json`.
- **Log source**: one concrete log file added to the project. A source can live
  anywhere on disk; paths are saved relative to the project when possible. A
  `<project>/logs` folder is a useful convention, but it is not required or
  enforced by the current implementation.
- **Log format**: the parser definition for a log source. It has a name,
  description, expression, timestamp field, and timestamp format. Internally this
  is still named `Extractor`; some UI prompts say "schema". In user-facing terms,
  schema and log format mean the same parser definition.
- **Filter**: an include or exclude rule over parsed fields. Filters are saved at
  project level and are applied to every pane. A filter can currently be scoped to
  one log format by name; source-scoped filters over one or more specific log
  files are not yet represented as first-class filter metadata.
- **Search**: an ad hoc query over the focused view. Recent search strings are
  saved with the project as saved searches.
- **View**: a pane showing one log source or a timestamp-merged set of sources,
  with the project filters and optional search applied. Merged views are derived
  at runtime and are not saved as log sources.

Current persistence is centralized:

```text
<project>/.logscouter/project.json
```

That file stores log sources, log formats, filters, saved searches, settings, and
the last session (which logs were open in which pane, the split, and each pane's
search). The separate files `filters.json`, `formats.json`, and `searches.json` are
not the current on-disk format.

User-level reuse is implemented for filter packs and schema packs:

```text
~/.log-scouter/filters/
~/.log-scouter/schemas/
```

User-level saved searches are not implemented yet.

## Keys

| Key | Action |
|---|---|
| `a` / `o` / `d` | add file / open folder text files / remove focused file from project |
| `j k` or arrows | move selection |
| `gg` / `G` / `[count]G` | top / bottom / go to visible row |
| `Ctrl+d` / `Ctrl+u` | half page down/up |
| `Ctrl+f` / `Ctrl+b` | full page down/up |
| `h l` or arrows | horizontal scroll |
| `/` / `n` / `N` / `c` | search / next match / previous match / cycle context |
| `Shift+Up/Down` | extend the selection (`Shift+PgUp/PgDn` by page) |
| `Ctrl+Up/Down` | move the cursor without changing the selection |
| `Space` (pane) | add/remove the current line from the selection |
| `y` / right-click | copy selected raw lines (cursor line if nothing selected) |
| `Esc` | clear the selection, then the search |
| `f` / `t` / `F` | add filter / open the time range picker / clear filters |
| `T` | measure elapsed time from the current line (again to turn off) |
| `x` / `L` | export filters / import filters from a folder |
| `X` / `I` | export log schemas / import log schemas from a folder |
| `H` | hide logs like the selection, using fields from the log format |
| `Space` (sidebar log) | add/remove that log from the view, merged by timestamp |
| `Space` (sidebar filter) | enable/disable that filter |
| `Space` (sidebar search) | run that saved search, or clear it if it is running |
| `Enter` (sidebar log) | edit that log's format |
| `Enter` (sidebar filter) | edit that filter rule |
| `Enter` (sidebar search) | edit that saved search |
| `S` | define a reusable log format |
| `e` | assign or edit the focused file's log format |
| `|` / `-` / `w` | split columns / split rows / close pane |
| `Tab` / `Shift+Tab` | cycle sidebar, panes, and search results |
| `Enter` (pane) | open a larger detail popup for the selected log row |
| `Enter` (results) | jump to selected search result |
| `?` / `Ctrl+s` / `q` | help / save / quit |

`Space` always means *select or deselect*, and `Enter` always means *open the detail
view of the thing under the cursor*. In the sidebar those detail views are editable:
the log format for a source, the rule for a filter, the query for a saved search.
A star in the sidebar marks what is currently selected — the logs feeding the pane,
the enabled filters, the running search.

## Filter Input

Press `f` and enter:

```text
[schema="<format name>"] field op [include|exclude] value
```

Examples:

```text
level equals exclude Trace
schema="Bracketed default" log_level equals exclude Trace
module contains include SQL
timestamp range include 2026-06-16 10:09:50..
message regex exclude timeout|closed
```

## Time Range Picker

Press `t` for a picker instead of typing a range by hand:

```text
┌Time Range──────────────────────────────────────────────────┐
│Quick select                                                │
│  1  Last 15 minutes                                        │
│> 2  Last 1 hour                                            │
│  3  Last 24 hours                                          │
│  4  Last 7 days                                            │
│  5  All time                                               │
│                                                            │
│  Start   2026-06-16 09:12:15.744                           │
│  End     2026-06-16 10:12:15.744                           │
│                                                            │
│log spans 2026-06-16 10:09:43.288 .. 2026-06-16 10:12:15.744│
│Up/Down move  Space pick preset  Enter apply  Esc cancel    │
└────────────────────────────────────────────────────────────┘
```

`Up`/`Down` move between the presets and the two fields, `Space` picks the preset
under the cursor and fills the fields from it, and `Enter` adds the range as an
`include` filter on `timestamp`. The fields are editable, so a preset is a starting
point rather than the only choice. Leaving a field blank makes that end open, and a
bad timestamp keeps the picker open rather than discarding what you typed.

Quick ranges count back from the **newest entry in the log**, not from the current
time. A log written three weeks ago still answers "last 1 hour" with its own final
hour, instead of an empty pane.

## Selecting and Copying

`Shift+Up/Down` extends a contiguous run from where you started; reversing
direction shrinks it, and the viewport scrolls so a run can span pages. To pick
lines that are *not* adjacent, travel with `Ctrl+Up/Down` (which never disturbs
the selection) and press `Space` to add or remove the line under the cursor.
Selected rows are marked with `+`. Any plain motion (`j`, `k`, `G`, ...) clears
the selection; `Esc` clears it explicitly.

Mouse selection works in the log view too, and the gesture decides what you get:

- **Drag inside one row** selects a **substring** of that row, highlighted in place.
  `y` or right-click copies exactly those characters.
- **Drag across rows** selects whole rows, anchored on the row you pressed in. A drag
  that starts as a substring becomes a row selection the moment it leaves the row.
- **Click** moves the cursor without selecting anything; `Ctrl`+click toggles a single
  row; `Shift`+drag extends the existing row selection.

A substring cannot start on the three `>`/`+`/`*` marker columns, and dragging past the
right edge stops at the last visible character rather than silently taking text scrolled
off-screen. `Esc`, `Space`, or any motion key clears it.

In the inline Detail panel or the larger `Enter` detail popup, click-drag selects wrapped
detail lines; right-click copies the selected detail lines, or all detail content if no
detail text is selected.

In a log pane, `y` or a right-click copies the selected entries' raw text,
including multi-line continuations. With nothing selected it copies the cursor
line. Copying uses the
OSC 52 escape sequence, which asks the terminal to set the clipboard, so it works
over SSH. Your terminal must allow it: iTerm2, kitty, WezTerm and Windows Terminal
do by default; `xterm` needs `disallowedWindowOps` adjusted and `tmux` needs
`set -g set-clipboard on`.

## Hiding by Example

Press `H` on one line to choose a field from that line's log format and add an
exclude filter for the current value. For the built-in bracketed format, choices
include `timestamp`, `host`, `log_module`, `log_level`, `error_code`, `file_name`,
`line_number`, and `message`. Choices are keyed `1`-`9`, `0`, then `a`, `b`, `c`, ...

Select several similar lines and press `H`. Instead of the single-line field menu, it
derives a regex shared by all of them and opens it in an editable popup:

```text
Distribution Service Trigger: 5 subscriptions queued
Distribution Service Trigger: 7 subscriptions queued
->  Distribution\s+Service\s+Trigger:\s+\S+\s+subscriptions\s+queued
```

It picks the most general pattern the lines support, and never a catch-all:

1. Same token count: differing tokens become `\S+`.
2. Enough shared tokens: the tokens common to all, joined by `.*`.
3. Otherwise the lines are not variants of one template, so they are matched
   literally as `(?:one|two)`. Generalising from, say, a banner line and two stack
   traces would hide half the file.

Press Enter to add it as an `exclude` filter, saved with the project like any other.

## Merging Logs

Give the sidebar focus (`Tab`), then:

- `Space` on a log **adds** it to the pane, interleaving the logs by timestamp.
  `Space` again removes it. A star marks every log feeding the current pane.
- To show **only** one log, deselect the others with `Space`. A view always keeps at
  least one log.
- `Enter` on a log opens its **log format** for editing rather than changing the view.

Lines with no parseable timestamp (banners, continuations) stay next to the line
they belong with rather than sinking to the top.

Each line keeps the log format of the file it came from, so merging a bracketed log with a
differently-formatted one still extracts both correctly. The Detail panel shows a
`from` row naming the origin file, and `source` is available as a search field.

A merge is a property of the pane, not a new log: it never appears in the file
list and is never written to `project.json`. Changing a source file's log format, or
removing it, discards the merge.

## Log Formats

A log format is the parser definition that turns a raw line into named fields.
It has a name, expression, optional description, timestamp field, and timestamp
format. Users define formats in the project; a format is then assigned **per
file**, so one project can hold logs of different shapes.

Press `S` to provide a reusable log format:

```text
name | expression | [timestamp strptime format] | [description]
```

For example, `simple | <timestamp> <level>: <message> | %H:%M:%S | compact service log` turns
`10:00:01 WARN: disk almost full` into a `WARN` level, which then drives the level
colouring, `level=` searches, and `level` filters.

Press `e` with a file focused to choose the log format that file uses. Type an
existing format name, such as `simple`, to assign it. You can also paste the full
format definition into `e`; that saves the format and applies it to the focused
file in one step. Applying a format re-reads the file, because multi-line grouping
depends on it. Format definitions and per-file assignments are saved in
`project.json`.

`Enter` on a log in the sidebar opens the same editor as `e`.

## Elapsed Time

Press `T` on a line to make it the origin. The timestamp column then shows every line's
signed offset from it, so "how long did this take" is a glance rather than arithmetic:

```text
   -28.812s                Kernel         Trace    NetChannel : Channel is closed.
>  +0ms                    Query Engine   Info     Executing report
   +1m56.531s              Query Engine   Error    We could not obtain the data ...
   +2m03.644s              Query Engine   Error    Plain error with no code
```

`T` again restores absolute timestamps. The origin is remembered as a timestamp rather
than a row, so filtering and searching do not disturb it, and it applies only to the file
it was set in. A line with no timestamp of its own — a banner, or a stack-trace
continuation that began a new record — shows `-`.

## Schema Detection

When a file is added without an explicit format, its first 200 lines are matched against
every log format in the project and the best fit wins. Formats are tried **most specific
first**, not best-scoring first: a permissive format such as a bare `<message>` matches
every line of every file, so scoring alone would hand it every log. A format only has to
explain a quarter of the lines to win, because the continuation lines of multi-line records
legitimately do not match. The status bar names the format that was chosen.

If nothing matches, the file falls back to the built-in `Bracketed default`.

## Schema Validation

A log format may carry `samples`: lines it must parse, and the level each should parse to.
They are checked whenever a format is defined or imported.

```json
"samples": [
  { "line": "2026-06-16 10:12:08.631 [HOST:h1]...[Error][0x800424FB][UID:5CCC]... boom",
    "level": "Error" }
]
```

This exists because a format can match a line and still be wrong. Before `<error_code?>`
was added, the built-in format matched the error line above and produced
`log_level = "Error][0x800424FB"`, so every `level equals Error` filter silently dropped
exactly the lines you were hunting. With a sample, that becomes a refusal to load:

```text
sample 1 parsed level "Error][0x800424FB", expected "Error"
```

Samples are optional, travel with the schema through export/import, and are checked
strictly on definition and import. A format already stored in a project is loaded
leniently, so an upgrade that invalidates a sample cannot silently repoint the files
using it.

## Schema Export/Import

Press `X` to export this project's log formats and `I` to import a folder of them.
Both default to the user-level library, shared by every project:

```text
~/.log-scouter/schemas
```

Type any other path to override; `~` is expanded, and relative paths resolve inside
the project folder. Each schema is a separate JSON file:

```json
{
  "name": "compact",
  "description": "small svc log",
  "schema": {
    "name": "compact",
    "format": "<timestamp> <level>: <message>",
    "timestamp_field": "timestamp",
    "timestamp_format": "%H:%M:%S"
  }
}
```

A bare `Extractor` object — one lifted straight out of `project.json` — is also accepted,
so you can share a schema by copying it out of a project file.

Import **merges**, and a schema whose name already exists in the project is left alone
rather than overwritten. Silently replacing it would change how every file using that name
parses. Rename the incoming schema if you mean to replace one. A schema whose `format`
does not compile is reported at import rather than failing on the first log line.

Importing a schema does not assign it to anything; press `e` (or `Enter` in the sidebar)
on a file to apply it.

## Performance

Filtering is the expensive pass, so its result is cached against the filter set: a
search after filtering walks only the surviving lines. Field extraction captures
just the fixed-width header and takes the message as the rest of the line, rather
than running the capture engine across a multi-KB record.

On a 152 MB / 116k-line bracketed server log:

| operation | time |
|---|---|
| apply a `contains` filter | ~0.7 s |
| search within the filtered lines | ~0.03-0.4 s |
| search when the filters changed too | ~0.7 s |

## Filter Persistence

Filters belong to the project, not to a single pane. Adding, hiding, importing,
or clearing a filter rewrites `<project>/.logscouter/project.json` right away — no
`Ctrl+s` needed — and the filters are reapplied the next time you open the
folder. The sidebar `Filters` section always shows the active set.

Filters can optionally be scoped to a log format. A scoped filter applies only to
entries using that format; in a merged pane it applies per source entry. The
current input keyword is `schema=`:

```text
schema="Bracketed default" log_level equals exclude Trace
```

Unscoped filters keep the existing project-wide behavior. Filters are not yet
stored with a first-class list of specific log source ids.

## Filter Export/Import

Press `x` to export the project's filters and `L` to import a folder of exported
filters (merged into the project, skipping duplicates). Both default to the
user-level library, shared by every project:

```text
~/.log-scouter/filters
```

Type any other path to override; `~` is expanded, and relative paths resolve
inside the project folder. That makes `<project>/.logscouter/filters` reachable as
`.logscouter/filters`.

Each exported filter is a separate JSON file:

```json
{
  "name": "filter-001-schema-Bracketed default-exclude-log_level-equals-Trace",
  "description": "exclude log_level equals 'Trace' on schema 'Bracketed default'",
  "filter": {
    "log_schema": "Bracketed default",
    "field": "log_level",
    "op": "equals",
    "value": "Trace",
    "action": "exclude",
    "enabled": true
  }
}
```

## Log Format Expression

The expression uses `<name>` placeholders for fields and
literal text anchors the match:

```text
<timestamp> [HOST:<host>][SERVER:<server>][PID:<process_id>][THR:<thread_id>][<log_module>][<log_level>][<error_code?>][UID:<user_id>][SID:<session_id>][OID:<object_id>][<file_name>:<line_number>] <message>
```

### Optional Fields

A trailing `?` on the name — `<error_code?>` — marks a field that only some lines
carry. An optional field takes the **literal separator in front of it** with it, so
both of these parse under the format above:

```text
... [Query Engine][Error][0x800424FB][UID:5CCC...][SID:...] ...   error_code = 0x800424FB
... [Query Engine][Error][UID:5CCC...][SID:...] ...               error_code = ""
```

Without the `?`, the `[0x800424FB]` on an error line is swallowed by the preceding
field: `log_level` becomes `Error][0x800424FB`, and a `level equals Error` filter
then silently drops exactly the error lines you were looking for. Mark the field
optional and both shapes yield `log_level = Error`.

An absent optional field reads as the empty string, so `error_code=` matches lines
that have no code. Put optional fields between two required ones; an optional field
in the very first position has no separator in front of it to absorb.

Press `e` and enter:

```text
format name
```

or define and apply a log format in one step:

```text
name | expression | [timestamp strptime format] | [description]
```

## Search Query Language

```text
Cache miss
"Sales by Region"
/completed in \d+ms/
level=Error
module~sql
file=/Dispatcher/
after:2026-06-16T10:09:50
before:"2026-06-16 10:10"
date:[2026-06-16T10:09..2026-06-16T10:10]
```

## Development

```bash
cargo test
cargo run --release -- examples
```

Layout:

```text
src/core/   extractor, parser, filters, search, project, models
src/tui/    Ratatui application
src/main.rs CLI entry point
tests/      Rust integration tests
```
