# Log Scouter (`logscout`)

A keyboard-driven **Rust terminal UI** for browsing large server logs. Open a
folder as a project, extract structured fields into columns, hide noise, search
with a log-aware query language, and split panes while keeping the heavy
parsing/filtering path native.

Built with Rust, [Ratatui](https://ratatui.rs), and Crossterm.

**Contents:** [Features](#features) · [Quick Start](#quick-start) ·
[Concept Model](#concept-model) · [Architecture](#architecture) · [Keys](#keys) ·
[Command Palette](#command-palette) · [Undo / History](#undo-redo-and-action-history) ·
[Filter Builder](#filter-builder) · [Timeline](#timeline) ·
[Filters](#filters-text-and-time) · [Search](#search-query-language) ·
[Hiding by Example](#hiding-by-example) · [AI Assistant](#ai-assistant) ·
[Log Formats](#log-formats) · [Building from Source](#building-from-source) ·
[Development](#development) · [Releasing](#releasing)

New to the code? Read [Concept Model](#concept-model) then [Architecture](#architecture) —
together they explain what the app is built out of and how the pieces fit and run.

## Features

- **Folder = project.** State persists to `<folder>/.logscouter/project.json`.
- **Session restore.** Quitting records the panes, the logs open in each, the split, the
  search, and the workspace layout (sidebar width, pane sizes, panel visibility, focus mode).
  Reopening the folder resumes exactly there.
- **Structured extraction.** Log format expressions use `<field>` placeholders, and
  `<field?>` for a field that is only present on some lines. A bracketed-field
  server log format is built in.
- **Schema detection.** Adding a file matches its first lines against the project's log
  formats and picks the one that explains them, most specific first. A log no format
  explains falls back to `Generic line`: one entry per line, timestamp read off the line.
- **AI schema inference.** For a log nothing parses, press `i` to have the configured LLM
  infer a format from a sample and apply it — the fields appear immediately.
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
- **Filters.** Include/exclude by field equality, substring, regex, or range. The sidebar
  splits them into **Text** (as many as you like) and **Time** (at most one, replaced by
  each new range). Filters apply to the whole project and are saved automatically, so they
  are still in effect the next time you open the folder.
- **Guided filter builder.** Press `f` for dropdowns (schema, field, operator, action,
  value) with field-name and value suggestions, a live match-count preview, and validation —
  `Tab` switches to the raw grammar and back, and `Enter` on a filter reopens it to edit.
- **Interactive timeline.** Press `b` for a compact histogram over the log's time span,
  bucketed by level (or module or source). Spikes jump out; drag across the bars to make a
  time-range filter, or click a bucket to zoom to it.
- **Time range picker.** Press `t` for a picker with an editable start and end plus
  quick ranges (`Last 1 hour`, `Last 24 hours`, `Last 7 days`, ...). Quick ranges
  count back from the newest entry across all loaded logs, not the current time.
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
- **Hide by example.** Press `H` on one line to hide by one of its fields -- each shown
  with the value that line holds -- or pick several fields to AND them into one regex.
  `Tab` flips the menu to **keep only**, building `include` rules instead. Select several
  similar lines and `H` offers a ladder of templates from greedy to strict, each with the
  rows it would remove, again with `Tab` to keep-only.
- **Command palette.** `Ctrl+P` or `:` opens a searchable, context-aware action list — type
  to filter, `Enter` runs it — so the rich feature set is discoverable without leaving the
  keyboard. Every action still has its own key.
- **Undo / redo.** `u` undoes and `Ctrl+r` redoes filters, time ranges, searches, merges,
  layout changes, and anything the AI applied. `U` shows an action-history popup — recent
  User and AI actions with timestamps — so you can see (and reverse) what the assistant did.
- **Vim-style navigation.** Supports `j/k`, `gg`, `G`, `[count]j`,
  `[count]G`, paging, and horizontal scroll.
- **Split panes.** Use `|` for columns and `-` for rows.
- **Resizable, collapsible workspace.** `[`/`]` resize the sidebar and `Ctrl+Arrow` resizes
  the focused pane along the split — or drag any separator with the mouse: the sidebar
  border, the border between panes (widths/heights), or a panel's top border to set the
  height of the results, detail, or chat panel. `z` is a focus mode showing only the active
  pane, and the sidebar/detail/results/chat panels toggle from the palette. Sizes and
  visibility are saved with the session.
- **AI assistant.** Press `A` for a chat panel that troubleshoots the logs for you — it
  inspects the sources and drives filters, searches, and time ranges through the same
  operations you would, iterating until the issue is understood. Works with OpenAI,
  Anthropic, and DeepSeek; the panel title shows the active provider and model. The key
  comes from the environment, from the `api_key` field of `~/.log-scouter/ai.json`, or from
  `/key` typed in the chat for the session only.
- **AI skills.** Drop a markdown file in `~/.log-scouter/skills/` and switch it on with
  `/skill <name>` in the chat; its text is appended to the assistant's instructions, so you
  can teach it how your team triages a class of incident. `/skills` lists what you have.
- **Source metadata.** Press `Enter` on a log source to edit its short name, description,
  tag, and schema. The metadata shows in sidebar detail and is handed to the assistant, so
  it knows that `app.log` is, say, "auth service — handles login". Saved with the project.

## Quick Start

Install the `logscout` command from GitHub Releases:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.sh | bash
logscout /path/to/logs
```

Run `logscout .` inside a log folder to add every direct text file in that folder as
a log source. Run `logscout` with no arguments to start an empty project, then press
`o` to browse for a folder and add its text files.

Two side commands stand apart from opening logs:

```bash
logscout version                                        # print the version
logscout config set --provider anthropic --api-key ...  # set up the AI assistant once
logscout config list                                    # show the current AI settings
```

Pipe any command's output straight into logscout as a live source with `-i` (Linux/macOS):

```bash
kubectl logs -n kube-system -l app=istio-ingressgateway -f | logscout -i
tail -f /var/log/app.log | logscout -i
```

logscout reads the pipe on stdin and takes keystrokes from the terminal (`/dev/tty`). The
stream is spooled while you browse, but a stdin source is transient — it is not saved to the
project, since a closed pipe cannot be reopened. `-i` also works alongside a folder argument.

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
  enforced by the current implementation. A source can carry a user **label** and
  **description** (press `r`), saved with the project and handed to the AI assistant.
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

## Architecture

The codebase is a Cargo binary plus a library crate. `src/main.rs` is a thin `clap` CLI that
builds a `Project` and hands it to `tui::run`; everything testable lives in the library.

```text
src/
  main.rs        CLI entry point (arg parsing, then tui::run)
  lib.rs         pub mod ai / core / tui
  core/          engine — no terminal, no I/O beyond reading log files
    parser.rs      read a file into raw LogEntry records, folding multi-line records
    extractor.rs   an Extractor (a.k.a. "log format"/"schema"): <field> expression → fields
    models.rs      LogFileModel (one source or a merged view), LogEntry, ViewModel
    filters.rs     FilterRule / FilterSet, the include/exclude engine, user dirs
    search.rs      the query language: compile a string to a Query, then match entries
    project.rs     the Project: sources, formats, filters, searches, session; JSON persist
  tui/           Ratatui application — one big AppState in mod.rs
  ai/            the AI assistant (see below)
tests/           integration tests: core.rs, ai.rs, bench_manual.rs
```

**Separation.** `core` knows nothing about the terminal: it parses, extracts, filters,
searches, and (de)serializes project state. `tui` owns an `AppState` that holds the
`Project`, the panes, the focus, the current input `Mode`, and the AI panel, and drives all
rendering. This split is what makes the engine unit-testable without a TTY.

**The render loop is synchronous.** `tui::run` runs a normal event loop: draw a frame, wait
for a crossterm event, mutate `AppState`, repeat. There is no async runtime in the loop.
Because a big log makes loading, filtering, and searching slow, those passes run in **slices
between frames** — each does a bounded chunk of work, updates a progress bar, and yields so
the UI stays responsive and `Esc` can cancel. Filtering is the expensive pass, so its result
is cached against the filter set; a later search walks only the surviving lines.

**Data flow for one pane.** A raw file becomes `LogEntry` records (`parser`), each entry is
parsed into named fields by its file's `Extractor` (`extractor`), the project `FilterSet`
narrows the entries to a visible set (`filters`, cached), and an optional `Query` narrows
further (`search`). The result is a `ViewModel` — the visible row indices, cursor, scroll,
and selection — which the pane renders. A **merged view** interleaves several sources by
timestamp into one synthetic `LogFileModel`; each entry remembers its origin so it is still
parsed by its own format.

**The AI assistant bridges sync and async.** The chat cannot block the render loop on a
network call, so the model request is the *only* thing that leaves the main thread:

```text
main thread (TUI, owns all state)          worker thread (owns a tokio runtime + reqwest)
  submit question ── AgentRequest ─────────►  provider::complete(conversation, tools).await
  drain events each frame ◄─ AgentEvent ───   Assistant { text, tool_calls } | Err(String)
  run tool_calls on AppState via the
  normal mutators (filters/search/…),
  append results ── AgentRequest ──────────►  next turn
  … loop until the reply has no tool calls
```

`ai/worker.rs` owns the thread, a current-thread tokio runtime, a shared `reqwest::Client`,
and the `std::sync::mpsc` channels. All `AppState` mutation stays on the main thread, so tool
execution reuses the existing mutators and the panels refresh for free. Every request carries
a **generation counter**; a reply tagged with a superseded generation (the user asked
something else, or pressed `Esc`) is dropped on arrival. `ai/provider.rs` translates the
neutral `ChatMsg`/`ToolSpec` types (`ai/message.rs`) into each provider's wire format and
parses the response — those builders and parsers are pure functions, round-tripped against
captured JSON in `tests/ai.rs` with no network. `ai/tools.rs` declares the tool schemas the
model sees; `ai/config.rs` handles provider/model/key resolution and `~/.log-scouter/ai.json`;
`ai/skills.rs` reads the user's markdown skills.

**Persistence** is centralized in `<project>/.logscouter/project.json` (sources, formats,
filters, saved searches, settings, last session), with user-level libraries under
`~/.log-scouter/` for filter packs (`filters/`), schema packs (`schemas/`), AI config
(`ai.json`), and AI skills (`skills/`).

## Keys

| Key | Action |
|---|---|
| `Ctrl+P` / `:` | open the searchable, context-aware command palette |
| `u` / `Ctrl+r` / `U` | undo / redo / show the action history |
| `a` / `o` | browse for a file to add / browse for a folder |
| `d` / `Delete` | delete the selected item: a log source, a filter, or a saved search |
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
| `f` / `t` | guided filter builder / open the time range picker |
| `T` | measure elapsed time from the current line (again to turn off) |
| `Enter` (source) | edit the log source name, description, tag, and schema |
| `L` / `X` | load / save the selected schema, filters, bookmarks, or saved search library |
| `H` | hide logs like the selection, using fields from the log format |
| `i` (pane) | infer a schema from the selected lines and append it to the source's set |
| `Space` (hide menu) | pick a field; `Enter` ANDs the picks into one regex |
| `H` (hide menu) | derive a pattern from the single current line |
| `↑` / `↓` (pattern popup) | pick a template, greediest first |
| `Tab` (pattern popup) | flip the derived pattern between hide and keep |
| `Space` (sidebar log) | add/remove that log from the view, merged by timestamp |
| `Space` (sidebar filter) | enable/disable that filter (`d` removes it) |
| `Space` (sidebar search) | run that saved search, or clear it if it is running (`d` removes it) |
| `Enter` (sidebar log) | edit that log source's name, description, tag, and schema |
| `Enter` (sidebar filter) | edit that filter rule |
| `Enter` (sidebar search) | edit that saved search |
| `i` / `e` / `L` / `X` (source schema row) | infer, edit, load, or save that source's schema |
| `|` / `-` / `w` | split columns / split rows / close pane |
| `[` / `]` | narrow / widen the sidebar (or drag its separator) |
| `Ctrl+←/→`, `Ctrl+↑/↓` | resize the focused pane (or drag the border between panes) |
| `z` | focus mode — show only the active pane |
| `b` | timeline histogram: cycle off / level / module / source (drag its bars to filter) |
| `A` | open the AI chat panel (Enter to send, Esc to cancel/leave) |
| `Tab` / `Shift+Tab` | cycle sidebar, panes, and search results |
| `Enter` (pane) | open a larger detail popup for the selected log row |
| `Enter` (results) | jump to selected search result |
| `?` / `Ctrl+s` / `q` | help / save / quit |

`Space` always means *select or deselect*, and `Enter` always means *open the detail
view of the thing under the cursor*. In the sidebar those detail views are editable:
the log format for a source, the rule for a filter, the query for a saved search.
A star in the sidebar marks what is currently selected — the logs feeding the pane,
the enabled filters, the running search.

## Command Palette

Don't remember the key? Press `Ctrl+P` or `:` for a searchable list of actions, filtered as
you type and dispatched with `Enter` (`Esc` closes it, `↑`/`↓` or `Ctrl+p`/`Ctrl+n` move):

```text
┌Command───────────────────────────────────────────┐
│> filter                                           │
│                                                   │
│> Add text filter                                 f│
│  Clear all filters                               F│
│  Import filter pack                              L│
│  Export filter pack                              x│
└───────────────────────────────────────────────────┘
```

The list is **context-aware** — it leads with what makes sense for what's focused, then the
general actions:

- **On a log line:** copy, hide/keep similar, mark elapsed time, show detail, ask AI.
- **On a source:** open, add to view (merge), edit source metadata and schema, delete.
- **On a filter or search:** enable/disable, edit, delete.
- **On a pane:** split into columns or rows, close.

Each row shows the key that also runs it, so the palette doubles as a way to learn the
shortcuts. Under the hood the palette and the keys share one dispatcher (an internal
`Command` enum), so an action behaves identically however you reach it.

## Filters: Text and Time

The sidebar keeps the two kinds apart, because they behave differently:

```text
Filters
  Text
    * exclude log_level equals 'Trace'
    * exclude raw regex '(?s)^.*? \[HOST:…'
  Time
    * 10:09:03 → 10:09:05  (2s)
```

**Text** filters are a list: add as many as you like, and they compose. **Time** is a
single slot. Two ranges over the same field can only ever intersect, and the second one is
what you just asked for -- so a new range *replaces* the old rather than narrowing it. That
holds however the range arrives: the `t` picker, the `f` popup, or an imported filter pack.

On any filter row, `Space` enables or disables it and `d` (or `Delete`) removes it. On the `Time`
row -- or on the `none - t` under it -- `Enter` reopens the picker on the range in force,
with the caret on `Start`, so changing "when" never means retyping it. Nobody should have
to hand-edit `timestamp range 'a..b'`, so that row does not open the filter text editor.

The row itself is written for reading, not for parsing: two clock times when the range
stays inside a day, `06-16 23:00:00 → 06-17 01:00:00` when it does not, `from …` or
`until …` for an open end, and the span in brackets. The detail panel below spells out the
full start, end and span.

## Undo, Redo, and Action History

Filters are saved automatically and the AI can change filters, searches, and time ranges
through the same operations you use — so `u` **undoes** and `Ctrl+r` **redoes** them:

- add / edit / delete a filter, apply a time range, run a saved search,
- add or remove a source from a merged pane, change the layout,
- and anything the AI applied.

It works by snapshotting the state (filters, saved searches, and the pane/layout session) and
committing a step only when an action actually changed something — a whole mouse drag is one
step, and no-ops record nothing.

`U` opens an **action history** popup so you can see what happened, especially what the
assistant did:

```text
Action History  (u undo · Ctrl+r redo · any key closes)
12:31 AI    added filter: exclude level equals 'INFO'
12:31 AI    searched: "connection reset"
12:32 User  changed time range: 10:00:00 → 10:30:00
```

## Filter Builder

Press `f` for a guided builder — no grammar to remember. `↑`/`↓` pick a row, `←`/`→` cycle a
dropdown or step through suggestions, and you type to edit the field or value. A live preview
counts how much it would remove before you commit, and validates as you go:

```text
┌Filter Builder────────────────────────────────────┐
│Scope     Project                                  │
│                                                   │
│  Schema    Any                                  ◀ ▶│
│  Field     log_level                            ◀ ▶│
│  Operator  equals                               ◀ ▶│
│  Action    Exclude                              ◀ ▶│
│> Value     Trace                                  │
│                                                   │
│Preview: hides 903 of 8,201 shown lines            │
│↑↓ row   ←→ change   type to edit   Tab raw   Enter │
└───────────────────────────────────────────────────┘
```

- **Field** cycles the active schema's field names (`←`/`→`) or you can type one.
- **Value** cycles the field's frequent values in view, or you type freely.
- **Operator** is `equals`, `contains`, `regex`, or `range`; **Action** is exclude or include.
- **Preview** reuses the hide-pattern match counter — it says *hides N of M* (or *keeps* for an
  include rule), with sample lines, and shows a red error for, say, an invalid regex.
- **`Tab`** switches to the **raw grammar** editor (below) and back, losslessly.
- **`Enter`** on a sidebar filter reopens the builder on that rule to edit it in place.

The raw grammar, reachable with `Tab`, is still there when you want it:

```text
[schema="<format name>"] field op [include|exclude] value
```

```text
level equals exclude Trace
schema="Bracketed default" log_level equals exclude Trace
module contains include SQL
timestamp range include 2026-06-16 10:09:50..
message regex exclude timeout|closed
```

## Timeline

Press `b` for a compact histogram above the pane, bucketed over the time span of the lines in
view. Each row is one value of the aggregation field, so an incident spike is obvious at a
glance:

```text
┌Timeline · level   (b change · drag to filter)──────────────┐
│Info      ▅▅▂▅ ▅▂▅▂  ▂▅▂ ▅▂▅▂▅                    ▂▅ ▅▂▅▂ ▅▂ │
│Warn      ▂  ▂    ▂ ▂   ▂  ▂     ▂        ▂   ▂ ▂    ▂  ▂    │
│Error                       ██████████                      │
│          10:00:00                                  10:59:59 │
└────────────────────────────────────────────────────────────┘
```

- **`b`** cycles the aggregation: off → by level → by module → by source → off. Level rows are
  coloured like the log (errors red, warnings yellow, …).
- **Drag across the bars** to build the project's time-range filter over that span; a **click**
  zooms to a single bucket. It is the same time filter the `t` picker sets, so the view
  narrows immediately.
- It reads the lines currently in view, so it reflects your filters and search, and works on
  a timestamp-merged pane spanning several sources.

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
│Up/Down move (a preset fills the fields)  Enter apply       │
└────────────────────────────────────────────────────────────┘
```

`Up`/`Down` move between the presets and the two fields. **Landing on a preset fills
`Start` and `End` from it**, so the fields always show the range `Enter` will apply --
highlight `Last 15 minutes` and press `Enter` and you get fifteen minutes. `Enter`
installs that range as the project's one `include` filter on `timestamp`, replacing any
earlier one.

The fields are editable, so a preset is a starting point rather than the only choice.
Moving between rows clamps rather than wraps, so `Down` off `End` cannot land on a preset
and overwrite what you just typed. Leaving a field blank makes that end open, and a bad
timestamp keeps the picker open rather than discarding what you typed.

Reopening the picker on an existing range (`t`, or `Enter` on the sidebar's `Time` row)
starts on `Start` rather than on a preset, so `Space` cannot overwrite the range you came
to adjust.

Quick ranges count back from the **newest entry across all loaded logs**, not from
the current time -- so with several sources loaded, `Last 15 minutes` is the last fifteen
minutes of the whole project, whichever pane has focus. A log written three weeks ago still answers "last 1 hour" with its own final
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

Press `H` on one line to open its field menu. Every field of that line's log format is
listed with the value the line actually carries, shortened to its first three words:

```text
┌Hide───────────────────────────────────────────────────────┐
│Hide logs matching all 2 picked fields                     │
│                                                           │
│   1  timestamp      2026-06-16 10:09:43.288               │
│   2  host           h1                                    │
│ + 6  log_module     Kernel                                │
│>+ 7  log_level      Trace                                 │
│   8  error_code     (empty)                               │
│   d  message        NetChannel : Channel …                │
│                                                           │
│Space picks a field   Enter combines the picks with AND    │
│Tab  switch to keep only                                   │
│H  message pattern, with the ids and counters generalised  │
└───────────────────────────────────────────────────────────┘
```

A field's own key -- `1`-`9`, `0`, then `a`, `b`, `c`, ... -- hides by it in one press, as
an `exclude <field> equals <value>` rule. Or move with `↑`/`↓`, pick fields with `Space`,
and press `Enter` to combine them.

**Hide or keep.** The menu defaults to hiding, since that is what `H` is for, but `Tab`
flips the whole menu to **keep only** -- the title, the heading and the direction all
change. Every action then builds an `include` rule instead of an `exclude` one, so a field
key, an `Enter`-combined regex, and the derived message pattern all narrow the view to the
matching lines rather than dropping them.

### Combining Fields with AND

Two independent `exclude` rules are an OR: each hides on its own. To hide the lines that
are `Kernel` **and** `Trace`, picking both fields builds a single regex over the whole log
line and opens it in the pattern popup:

```text
(?s)^.*? \[HOST:.*?\]\[SERVER:.*?\]\[PID:.*?\]\[THR:.*?\]\[Kernel\]\[Trace(?:\]\[.*?)?\] ...
```

The chosen fields are pinned to their values and every other field is left free. It has to
be one positional pattern because Rust's regex engine has no lookaround, so an unordered
conjunction over one string cannot be written any other way -- but the log format already
says where each field sits, which is exactly what makes the conjunction expressible.

Pinning an optional field to the value the line had works in both directions: `error_code`
set to `0x800424FB` demands that code, and `error_code` shown as `(empty)` demands its
absence. Multi-line records match too, continuation lines included.

The rule is saved as `exclude raw regex '...'`, and the popup counts what it will remove
before you commit it. One field alone stays an `equals` rule: it reads better in the
sidebar, and it still holds on a line the schema cannot fully parse.

Press `H` again in the menu to derive a pattern from the one line instead, generalising
the values inside it while every word stays put:

```text
Session 900 created for user analyst
->  Session\s+\d+\s+created\s+for\s+user\s+analyst
```

Select several similar lines and press `H`. Instead of the single-line field menu, it
derives a regex shared by all of them and opens it in an editable popup:

```text
Distribution Service Trigger: 5 subscriptions queued
Distribution Service Trigger: 7 subscriptions queued
->  Distribution\s+Service\s+Trigger:\s+\d+\s+subscriptions\s+queued
```

### Choosing How Greedy to Be

One derived pattern is a guess about how much you meant to hide. The popup offers the
whole ladder instead, and counts what each rung would take out of the rows on screen:

```text
┌Hide Pattern─────────────────────────────────────────────────────────┐
│Regex over the message field - Up/Down pick a template, or edit it   │
│                                                                     │
│> Session\s+\d+\s+created\s+for\s+user\s+\S+                         │
│                                                                     │
│    prefix    leading words, then .*           matches 30            │
│    loose     shared words, .* between         matches 20            │
│    wildcard  \S+ where the lines differ       matches 20            │
│  ▸ typed     value shapes where they differ   matches 20            │
│    exact     just these lines                 matches 2             │
│                                                                     │
│  hides 20 of 40 shown rows                                          │
│    Session 900 created for user analyst                             │
│    Session 901 created for user admin                               │
│                                                                     │
│  Tab hide/keep   Enter apply   Esc cancel                           │
└─────────────────────────────────────────────────────────────────────┘
```

`Up`/`Down` move between templates, loading each into the editable field. The five
strategies:

| Template | What it does |
| --- | --- |
| `loose` | the tokens common to every line, joined by `.*` |
| `prefix` | the leading tokens every line opens with, then `.*` |
| `wildcard` | same token count: `\S+` wherever the lines differ |
| `typed` | same token count: the tightest shape covering every version of a differing token -- `\d+`, a UUID, an IP, `0x...` hex, a quoted string. A token differing only in its tail keeps its head, so `id=1` and `id=2` give `id=\d+` |
| `exact` | the selected lines, literally, as `(?:one\|two)` |

The list is ordered by how many rows each template matches **in your log**, not by which
strategy built it, because that is the only honest measure of greedy. A template two
strategies both arrive at is offered once. A template of nothing but wildcards is never
offered: it would match far more than the lines it came from. Templates the selection
cannot support are absent -- `wildcard` and `typed` need equal token counts, and a lone
line has nothing to diff against.

It opens on `typed`, the tightest generalising rung, so the looser ones are a deliberate
step rather than a default.

The header counts what the field's *current* regex would do -- `hides 412 of 8,201 shown
rows` -- and lists the first few lines it would remove, so an edit is measured too. An
unfinished regex reports its error there rather than a count. On a pane of more than
50,000 rows the counts are a floor, shown as `matches 412+`.

Press `Tab` to flip the rule between **hide** (`exclude`) and **keep** (`include`), so the
same derived pattern serves "drop this noise" and "show me only these". Press Enter to add
it, saved with the project like any other filter.

## AI Assistant

Press `A` to open a chat panel at the bottom-left, below the Detail panel. Ask it to
troubleshoot the logs — "why did sessions start timing out?", "hide the noise and show me
errors in the last 15 minutes" — and it works through the problem using the same operations
you would:

```text
┌AI · openai gpt-4o · Enter send · Esc leave────────┐
│you hide the trace noise and show me errors        │
│ai  Let me look at the level mix first.            │
│  » ran level_breakdown: Error: 12, Trace: 903 …   │
│ai  Trace dominates. Hiding it.                    │
│  » ran add_filter: exclude level='Trace'; 8,201 → │
│    3,140 rows                                      │
│ai  Done. 12 errors remain — want the last 15 min? │
│>                                                  │
└───────────────────────────────────────────────────┘
```

The panel title always shows the active provider and model, and ` · no key` until one is
configured, so you can see the assistant is ready without sending a message.

The assistant is given the project's context (folder, sources with their schemas, counts,
and any label/note you added, the current filters, and the focused view) and a set of tools
that map onto log-scouter's own operations:

- **Inspect** — list sources, list filters, sample lines, count matches for a query, level
  breakdown.
- **Act** — add a filter, set a time range, run a search, add a log source.

It runs an **agentic loop**: it calls a tool, sees the result, and keeps going until it has
an answer — so one question can play out as several steps, with the panels updating live and
each action noted in the transcript. The loop is capped at 12 turns so a confused model
cannot spin forever. `Esc` cancels a reply in flight, then leaves the panel; `↑`/`↓` scroll
the transcript; write actions apply immediately (a removed source still confirms).

**Configure it once.** Set the provider and key from the command line and every later
session picks it up — press `A` and start chatting, no prompts:

```bash
logscout config set --provider anthropic --api-key sk-ant-...   # or openai / deepseek
logscout config set --model claude-opus-4-8                     # optional
logscout config list                                            # show current settings
```

`config` writes `~/.log-scouter/ai.json` (chmod `600`, the key masked in any printout). Works
with OpenAI, Anthropic, and DeepSeek (OpenAI and DeepSeek share the `/chat/completions` wire
format; Anthropic uses the Messages API).

**Where the key comes from**, in precedence order:

1. a `/key <your-key>` typed in the chat — kept in memory for the session only, never
   written to disk and never echoed into the transcript;
2. the provider's environment variable — `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or
   `DEEPSEEK_API_KEY`;
3. the `api_key` field of `~/.log-scouter/ai.json` — what `logscout config set` writes (you
   can also edit the file by hand).

You can also change provider and model from inside the chat with `/provider
openai|anthropic|deepseek` and `/model <name>` (saved to the same file); `/clear` resets the
conversation. `LOGSCOUT_AI_BASE_URL` overrides the endpoint for a corporate gateway, a
compatible self-hosted model, or a test double.

**Skills.** Drop a markdown file in `~/.log-scouter/skills/<name>.md` and switch it on with
`/skill <name>`; its text is appended to the assistant's system prompt (re-read each turn,
so edits take effect live), which is how you teach it your team's playbook for a class of
incident. `/skills` lists what you have written and marks the ones that are on.

## Adding Files and Folders

Press `a` to browse for **one file** to add as a log source, or `o` to browse for a **whole
folder** (which adds every text file in it). Both start from the folder the project is
already in and share the same navigation.

The file picker (`a`) lists the text files in each folder alongside the subfolders; `Enter`
on a file adds it, `Enter` on a folder descends into it. If you would rather type or paste a
path — say an absolute one far from here — press `p`.

```text
┌Add Log File─────────────────────────────────┐
│…/var/log/appserver                          │
│pick a file to add, or enter a folder        │
│                                             │
│  ../    go up                               │
│  archive/                                   │
│> app.log                                    │
│  server.log                                 │
│                                             │
│j/k move  Enter add/enter  Left up  p type path│
└─────────────────────────────────────────────┘
```

The folder picker (`o`) lists subfolders and how many files sit directly in each, with a
`./ open this folder` row at the top:

```text
┌Open Folder──────────────────────────────────┐
│…/var/log/appserver                          │
│12 files here                                │
│                                             │
│> ./     open this folder                    │
│  ../    go up                               │
│  archive/                                   │
│  nested/                                    │
│                                             │
│j/k move   Enter select   Right in   Left up │
└─────────────────────────────────────────────┘
```

| Key | Action |
| --- | --- |
| `j` / `k`, `↑` / `↓` | move the selection |
| `PgUp` / `PgDn`, `Ctrl+u` / `Ctrl+d` | move by ten |
| `g` / `G` | first / last row |
| `Enter` | do what the selected row says: open `./`, go up on `../`, otherwise enter the subfolder |
| `→` / `l` | enter the selected subfolder |
| `←` / `h` / `Backspace` | go up one folder |
| `.` | show or hide dot-folders |
| `Esc` | cancel without changing the project |

`Enter` only ever does the one thing its row names, so the `./` row is where opening
happens. Opening a folder adds every direct text file in it, exactly as `logscout <folder>`
would. A folder that cannot be read leaves the browser where it was and says so.

## Merging Logs

Give the sidebar focus (`Tab`), then:

- `Space` on a log **adds** it to the pane, interleaving the logs by timestamp.
  `Space` again removes it. A star marks every log feeding the current pane.
- To show **only** one log, deselect the others with `Space`. A view always keeps at
  least one log.
- `Enter` on a log opens its **log format** for editing rather than changing the view.

Lines with no parseable timestamp (banners, continuations) stay next to the line
they belong with rather than sinking to the top. A file's *leading* untimestamped lines
have no earlier line to sit beside, so they borrow that file's first known timestamp.

Each line keeps the log format of the file it came from, so merging a bracketed log with a
differently-formatted one still extracts both correctly. The Detail panel shows a
`from` row naming the origin file, and `source` is available as a search field.

When a log's format names no timestamp field -- as `Generic line` does not -- the time is
read straight off the head of each line, so it still merges in order. The ISO-8601 family
is recognised: `2026-06-16 10:09:43.288`, `2026-06-16T10:09:43,288Z`, `2026/06/16 10:09:43`,
each optionally behind a `[`.

A merge is a property of the pane, not a new log: it never appears in the file
list and is never written to `project.json`. Changing a source file's log format, or
removing it, discards the merge.

## Log Formats

A log format is the parser definition that turns a raw log entry into named fields.
It has a name, expression, optional description, timestamp field, and timestamp
format. It may also carry `entry_start` and `entry_end` regexes that merge several
physical lines into one logical entry before fields are extracted. Users define formats
in the project; a format is then assigned **per file**, so one project can hold logs of
different shapes.

Two formats are built in. `Bracketed default` reads the bracketed server log. `Generic line`
reads anything: its expression is a bare `<message>`, so every line is its own entry and
nothing is extracted. Detection tries formats most specific first, so `Generic line` only
wins a file no other format explains -- which is the point, because under a format that
matches nothing at all, no line starts a record and the whole file folds into one entry.
A file whose stored format explains none of its opening lines is re-detected on load.

Select a source in the sidebar and press `Enter` to edit its short name, description,
tag, and schema. In the schema row, press `e` to edit the schema manually, `i` to infer it
with the configured LLM, `L` to load one from the user schema library, or `X` to save the
current schema to that library.

```text
name | expression | [timestamp strptime format] | [description] | [entry start regex] | [entry end regex]
```

For example, `simple | <timestamp> <level>: <message> | %H:%M:%S | compact service log` turns
`10:00:01 WARN: disk almost full` into a `WARN` level, which then drives the level
colouring, `level=` searches, and `level` filters.

For multiline formats, JSON is usually clearer because the `format` can contain literal
newlines and optional boundary regexes:

```json
{
  "name": "python-block",
  "format": "{\n  'timestamp':'<timestamp>',\n  'level': '<level>',\n  'message': '<message>'\n}",
  "timestamp_format": "%Y-%m-%d %H:%M:%S,%f",
  "entry_start": "^\\s*\\{\\s*$",
  "entry_end": "^\\s*\\}\\s*$"
}
```

Applying a format re-reads the file, because multi-line grouping depends on it. Format
definitions and per-file assignments are saved in `project.json`.

### Inferring a Schema with the LLM

When the built-in detection can't structure a log — it falls back to `Generic line`, one
entry per line with nothing extracted — open that source with `Enter`, move to the schema
row, and press `i` to have the configured LLM work out the format for you. It sends the
first ~80 physical lines to the model (see
[AI Assistant](#ai-assistant) for setting a provider and key with `logscout config set`),
which returns a format expression, timestamp format, and optional entry boundary regexes;
log-scouter builds that schema and applies it to the file. Applying re-parses the file, so
the extracted fields and grouped entries appear immediately. If the guess is off, press `e`
on the schema row to tweak it or `i` to try again. Needs an LLM configured; otherwise the
status line tells you to run `logscout config set`.

### Generating a Schema with Codex or Claude Code

Prefer your coding agent? Install the **log-schema skill** and let Codex or Claude Code write
a user-level schema (`~/.log-scouter/schemas/<name>.json`) from your sample log files:

```bash
# OpenAI Codex CLI  ->  ~/.codex/skills/log-schema/
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-codex-skill | bash

# Claude Code       ->  ~/.claude/skills/log-schema/
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-claude-skill | bash
```

Then start a new agent session and ask it to generate a logscout schema from your log. The
skill teaches the schema format — the template DSL, chrono timestamp formats, multi-line
`entry_start`, the padded-level and trailing-catch-all tricks, and validation samples — and
the agent writes the JSON to `~/.log-scouter/schemas/`, where logscout auto-detects it. The
skill source lives in [`skills/log-schema/`](skills/log-schema/SKILL.md).

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
explain a quarter of the grouped entries to win, because continuation lines and block
records are merged before the score is evaluated. The status bar names the format that was
chosen.

If nothing matches, the file falls back to the built-in `Generic line`.

### Multiple schemas per source

Container logs often interleave formats — a `/docker-entrypoint.sh` or `[entrypoint]`
preamble, a structured app log, sometimes two structured formats at once (nginx access +
error, or an app that logs both JSON and plain). A single schema cannot parse all of it, so
a source can hold an **ordered set of schemas**: each log entry is parsed by the first
schema in the set whose pattern matches it, and anything matching none shows its raw text.

Detection assigns the set automatically. On add, each line is attributed to the
most-specific schema that parses it, and every schema that wins a meaningful share of the
lines joins the set (the `Generic line` catch-all never does). So a log mixing uvicorn and
nginx lines lands on `[Nginx Access Log, Uvicorn Log]` with no configuration — provided both
schemas are in the project (load them once with `L`; they persist).

Edit the set from the source editor (`Enter` on a source, move to the **Schemas** row):
`L` adds a schema from the library (repeat to add more), `d` removes the last, and the order
is the match priority. The set is saved with the project. A merged view still uses each
contributing file's primary schema.

The fastest way to add a schema for a format the set does not cover yet: **select the lines
of that format in the pane (`Space`) and press `i`.** The LLM infers a schema from exactly
those lines and appends it to the schema set of the source(s) they came from (in a merged
view, each contributing source), then re-parses — so a cluster of unparsed lines becomes a
new format in one keystroke. (`i` on a *source* or the Schemas row still infers a single
schema that *replaces* the source's schema, from its first lines.)

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

Importing a schema does not assign it to anything; press `Enter` on a file in the sidebar,
move to the schema row, then press `L` to apply one from the user library.

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

Open a source with `Enter`, move to the schema row, press `e`, and enter:

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

## Building from Source

Requirements: a stable Rust toolchain (Rust 2021 edition; install via [rustup](https://rustup.rs))
and a terminal. The AI networking uses `rustls`, so no OpenSSL is needed. Linux release
artifacts are built against musl for portability.

```bash
git clone https://github.com/mangosteen-lab/log-scouter
cd log-scouter
cargo build --release          # binary at target/release/logscout
cargo run --release -- examples
```

`./run.sh [folder] [files...]` is a convenience wrapper around `cargo run --release`.

## Development

```bash
cargo test                     # unit + integration tests (core.rs, ai.rs)
cargo clippy --all-targets     # lint; CI expects it clean
cargo fmt                      # format
cargo run --release -- examples
```

Tests are deterministic and need no network. The AI provider adapters are exercised by
round-tripping captured request/response JSON (`tests/ai.rs`), and the agentic loop is
driven by feeding scripted `AgentEvent`s, so a full turn is testable offline. To exercise a
real endpoint without a paid key, point `LOGSCOUT_AI_BASE_URL` at a local OpenAI-compatible
mock. See [Architecture](#architecture) for the module map and the threading model.

```text
src/core/   extractor, parser, filters, search, project, models — the engine, no TTY
src/tui/    Ratatui application (one AppState in mod.rs)
src/ai/     assistant: config, message, provider, tools, skills, worker
src/main.rs CLI entry point
tests/      Rust integration tests
```

## Releasing

Version lives in `Cargo.toml`. Bump it, commit, push `master`, then let the Makefile tag it:

```bash
make publish-release           # tags v<version> and pushes the tag
make release-status
```

Pushing the tag triggers the GitHub Actions release workflow, which builds and attaches the
platform binaries the install scripts download.
