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
[Log Formats](#log-formats) · [Library](#library-schemas-filters-and-searches) ·
[Hubs](#hubs-shared-remote-libraries) · [Building from Source](#building-from-source) ·
[Development](#development) · [Releasing](#releasing)

New to the code? Read [Concept Model](#concept-model) then [Architecture](#architecture) —
together they explain what the app is built out of and how the pieces fit and run.

## Features

- **Folder = project.** State persists to `<folder>/.logscouter/project.json`, created by
  your first `Ctrl+s` — opening a folder writes nothing into it. An existing `.logscouter`
  is always loaded, and from then on the project autosaves as you work.
- **Session restore.** Quitting records the panes, the logs open in each, the split, the
  search, and the workspace layout (sidebar width, pane sizes, panel visibility, focus mode).
  Reopening the folder resumes exactly there — for a project you have saved at least once.
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
- **Hubs.** Shared libraries of schemas, filters and saved searches, published as ordinary
  git repos — GitHub, or your own GitLab or Gitea. The official hub is configured out of the
  box and refreshes in the background daily, so new schemas arrive without a release — and
  the bundled copies keep working offline. Add your team's own; items are namespaced
  `<hub>/<name>`, so hubs never collide, and your own schemas always win.
- **Elapsed time.** Press `T` on a line to measure every other line from it: the
  timestamp column becomes `+1m56.531s`, `-28.812s`.
- **One operation model.** `Space` selects or deselects whatever the cursor is on —
  a log line, a log source, a filter, a saved search. `Enter` opens its detail view,
  which for sources, filters and searches is an editor you can save.
- **Multi-line records.** Stack traces and wrapped lines fold into the previous
  log entry.
- **Log-aware search.** Supports bare text, quoted phrases, `/regex/`,
  `field=value`, `field~contains`, `after:`, `before:`, and `date:[a..b]`.
- **Search results panel.** Searches open a bottom panel of matched lines, coloured by
  level like the pane. Click a result or focus it and press Enter to jump to the source
  line — and `/` there refines the matches with a second query.
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

Open a specific file directly with `-f`, no folder needed — repeat it for several:

```bash
logscout -f /var/log/app.log
logscout -f app.log -f errors.log
```

A few side commands stand apart from opening logs:

```bash
logscout --version                                      # version, and whether one is newer
logscout upgrade                                        # replace this binary with the latest
logscout uninstall                                      # remove it again
logscout config set --provider anthropic --api-key ...  # set up the AI assistant once
logscout config list                                    # show the current AI settings
logscout hub list                                       # the shared libraries you have
logscout hub add acme/log-scouter-hub                   # add a team's hub and sync it
```

See [Hubs](#hubs-shared-remote-libraries) for the rest of `logscout hub`.

Pipe any command's output straight into logscout as a live source with `-i` (Linux/macOS):

```bash
kubectl logs -n kube-system -l app=istio-ingressgateway -f | logscout -i
tail -f /var/log/app.log | logscout -i
```

logscout reads the pipe on stdin and takes keystrokes from the terminal (`/dev/tty`). The
stream is spooled while you browse, but a stdin source is transient — it is not saved to the
project, since a closed pipe cannot be reopened. `-i` also works alongside a folder argument.

Once installed, `logscout` upgrades and removes itself — no script needed:

```bash
logscout upgrade                  # replace this binary with the latest release
logscout upgrade --check          # is there a newer one? change nothing
logscout upgrade --version 0.0.15 # install an exact release (downgrades allowed)
logscout uninstall                # remove the binary; asks first
logscout uninstall --purge -y     # also remove ~/.log-scouter, no prompt
```

`upgrade` replaces **the binary you ran** (`current_exe`), wherever it lives, so it cannot
leave you on a stale copy by installing somewhere else. `uninstall` keeps
`~/.log-scouter` — your schemas, filters, saved searches and hubs — unless you pass
`--purge`; a project's own `.logscouter` folder is never touched either way.

The install scripts are still there for the first install, and for anyone who prefers them:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/upgrade.sh | bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/uninstall.sh | bash
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.ps1 | iex
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/upgrade.ps1 | iex
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/uninstall.ps1 | iex
```

### Knowing when a release is out

The app offers it. A launch asks GitHub in the background — never on the way to the logs —
and when a newer release exists a popup asks whether to install it:

```text
┌A new logscout is available────────────────────────────────────────────┐
│  logscout 0.0.21  ->  0.0.22                                          │
│  https://github.com/mangosteen-lab/log-scouter/releases/tag/v0.0.22   │
│                                                                       │
│  Install it now? It replaces the binary you are                       │
│  running; this session carries on unchanged, and the                  │
│  new version starts with your next logscout.                          │
│                                                                       │
│  y  upgrade now      n / Esc  not now (asked again next release)      │
└───────────────────────────────────────────────────────────────────────┘
```

`y` downloads and installs in the background, so the app stays usable while it runs and the
outcome lands in the status bar. Nothing about the running session changes: `self_replace`
leaves the binary you started valid, and the new version starts with your next `logscout`.

`n` records that release in `~/.log-scouter/ui.json` and stays quiet about it — the next
*newer* release asks again. Only `y` and `n`/`Esc` do anything while the question is up:
installing a binary is not something a stray keystroke should start. And the offer waits for
a quiet moment — if a popup is open or a load is running when the answer arrives, it goes to
the status bar instead of stealing the keyboard.

`logscout --version` says so too:

```text
$ logscout --version
logscout 0.0.16

A new release of logscout is available: 0.0.16 -> 0.0.17
https://github.com/mangosteen-lab/log-scouter/releases/tag/v0.0.17
Run `logscout upgrade` to update.
```

The lookup is cached in `~/.log-scouter/update.json` and refreshed **at most once a day**, so
only the first `--version` each day touches the network — a script calling it in a loop pays
nothing and cannot be rate-limited into failing. Failures are silent: no network, no home
directory, GitHub down, all mean you get the plain version and exit 0. A version banner is
never worth an error.

To turn the check off entirely — the startup prompt included:

```bash
LOGSCOUT_NO_UPDATE_CHECK=1 logscout --version   # per run, or export it
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
    hub.rs         remote shared libraries: hubs.json, tarball sync, namespaced loading
    library.rs     Origin (project/user/hub/bundled) and the tagged items pickers show
    release.rs     the update check (cached daily), self-upgrade, and uninstall
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

**The AI assistant bridges sync and async.** The chat cannot block the render loop on an
open-ended model call, so it moves off the main thread:

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

**A hub sync runs on whichever side of that line it belongs.** A sync the user *asked* for
(the Hubs prompt) blocks the frame: `core/hub.rs` builds a short-lived tokio runtime, the
client carries a 30-second timeout, and a handful of small JSON files finishes fast enough
that a worker thread — a second channel, generations, a spinner — would buy nothing. The
**start-up refresh** cannot block, so it does not: `spawn_sync` hands the stale hubs to a
one-shot thread, and `drain_hub_events` picks up the outcomes each loop and rebuilds the
schema library. It is best-effort throughout — no home directory, no network, an unwritable
`hubs.json`, a hub that 404s each leave the app exactly as it would have been, on whatever
is already cached.

**Persistence** is centralized in `<project>/.logscouter/project.json` (sources, formats,
filters, saved searches, settings, last session), with user-level libraries under
`~/.log-scouter/` for filter packs (`filters/`), schema packs (`schemas/`), saved searches
(`searches/`), hubs (`hubs.json` plus cached snapshots in `hubs/`), AI config (`ai.json`),
and AI skills (`skills/`).

## Keys

| Key | Action |
|---|---|
| `Ctrl+P` / `:` | open the searchable, context-aware command palette |
| `u` / `Ctrl+r` / `U` | undo / redo / show the action history |
| `a` / `o` | browse for a file to add / browse for a folder (opens where you last browsed) |
| `d` / `Delete` | delete the selected item: a log source, a filter, or a saved search |
| `j k` or arrows | move selection |
| `gg` / `G` / `[count]G` | top / bottom / go to visible row |
| `Ctrl+d` / `Ctrl+u` | half page down/up |
| `Ctrl+f` / `Ctrl+b` | full page down/up |
| `h l` or arrows | horizontal scroll |
| `/` / `n` / `N` / `c` | search / next match / previous match / cycle context |
| `/` (matches panel) | refine the matches with a second query — see below |
| `Shift+Up/Down` | extend the selection (`Shift+PgUp/PgDn` by page) |
| `Ctrl+Up/Down` | move the cursor without changing the selection |
| `Space` (pane) | add/remove the current line from the selection |
| `y` / right-click | copy selected raw lines (cursor line if nothing selected) |
| `Esc` | clear the selection, then the search |
| `f` / `t` | guided filter builder / open the time range picker |
| `T` | measure elapsed time from the current line (again to turn off) |
| `Enter` (source) | edit the log source name, description, tag, and schema |
| `L` / `X` | pick the selected row's schema / filter / saved search out of the library, or save it into the library |
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
| `A` | open the AI chat panel, with the selected lines (or the cursor line) copied into its input (Enter to send, Esc to cancel/leave) |
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

**Asking about specific lines.** Press `A` and the lines you are working on are copied into
the chat input with the caret at the end, so you type your question around them and send one
message. That is the selection if you made one (`Space` to mark, `Shift+↑`/`Shift+↓` for a
range), and otherwise just the line under the cursor — the line you are looking at is
usually the line you meant to ask about. Newlines in the input show as `⏎`; a very large
selection is trimmed to the first 50 lines, with a note saying how many were left out. A
half-typed question is never overwritten — the lines land after it.

**Starting over.** Send `/clear` (or `/reset`) to empty the panel: the transcript goes, the
model forgets the exchange, and a reply still on its way is abandoned rather than landing in
the fresh transcript. Your provider, model, key, and any skills you switched on all stay as
they were — only the conversation is dropped.

**Editing the input.** `Ctrl+A` selects the whole input; `Ctrl+C` then copies it to the
system clipboard and `Ctrl+X` cuts it. With the input selected, `Backspace` or `Delete`
clears it and typing replaces it, as a selection does anywhere else. `Esc` gives up the
selection before it gives up the panel, so a mistaken `Ctrl+A` costs one keystroke rather
than the whole draft. `Ctrl+U` still clears the input outright.

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
openai|anthropic|deepseek` and `/model <name>` (saved to the same file).
`LOGSCOUT_AI_BASE_URL` overrides the endpoint for a corporate gateway, a
compatible self-hosted model, or a test double.

**Skills.** Drop a markdown file in `~/.log-scouter/skills/<name>.md` and switch it on with
`/skill <name>`; its text is appended to the assistant's system prompt (re-read each turn,
so edits take effect live), which is how you teach it your team's playbook for a class of
incident. `/skills` lists what you have written and marks the ones that are on.

## Adding Files and Folders

Press `a` to browse for **one file** to add as a log source, or `o` to browse for a **whole
folder** (which adds every text file in it). Both start where you last left the browser
(remembered in `~/.log-scouter/ui.json`, so it survives restarts and follows you between
projects), falling back to the folder the project is in. They share the same navigation.

The file picker (`a`) lists the text files in each folder alongside the subfolders; `Enter`
on a file adds it, `Enter` on a folder descends into it. If you would rather type or paste a
path — say an absolute one far from here — press `Ctrl+p`.

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
│type to find  Enter add  Left up  Ctrl+p path│
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
│type to find  Enter select  Right in  Left up │
└─────────────────────────────────────────────┘
```

| Key | Action |
| --- | --- |
| any letter | start searching for it by name — see below |
| `↑` / `↓` | move the selection |
| `PgUp` / `PgDn`, `Ctrl+u` / `Ctrl+d` | move by ten |
| `Home` / `End` | first / last row |
| `Enter` | do what the selected row says: open `./`, go up on `../`, otherwise enter the subfolder |
| `→` | enter the selected subfolder |
| `←` / `Backspace` | go up one folder |
| `/` | start searching explicitly |
| `.` | show or hide dot-folders |
| `Ctrl+p` | type a path instead (file picker) |
| `Esc` | cancel without changing the project |

Letters are never navigation keys here: typing goes to the name search, because reaching for
a folder by name is what the listing is for. Every other key is swallowed by the popup, so
nothing you type reaches the logs behind it.

On **Windows** a root has no parent — there is no folder above `C:\` — so the other drives
are listed there instead, mapped network drives included. Walk up to `C:\` and `Z:\` is a
row you can enter like any folder (or just type `z`).

### Finding a folder or file by name

A long listing is faster to type at than to scroll. Press `/` and then type the start of the
name; the cursor jumps to the first entry that matches as you go.

```text
┌Open Folder──────────────────────────────────┐
│…/var/log                                    │
│/jen                                         │
│                                             │
│  ./     open this folder                    │
│  ../    go up                               │
│  archive/                                   │
│> jenkins/                                   │
│  jetty/                                     │
│                                             │
│type to find  Up/Down next match  Enter open │
└─────────────────────────────────────────────┘
```

While the search is running **every letter is text**, so a folder called `jenkins` types as
itself instead of moving the cursor down. Names that start with what you typed win; if none
do, a name that merely contains it is used, so a distinctive word in the middle also finds
its folder. A query nothing matches turns red and leaves the cursor where it was.

| Key | Action |
| --- | --- |
| `/` | start searching (again to start the query over) |
| `↑` / `↓` | previous / next match, wrapping |
| `Enter` | act on the row it found — add the file, or enter the folder |
| `Backspace` | delete a character; on an empty query, stop searching |
| `Esc` | stop searching, keeping the browser open |

The search also ends on its own after about a second and a half without a keystroke, so a
forgotten query never swallows the next name you type.

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

### Bundled schemas

Schemas for common third-party formats ship with the binary and join detection automatically —
nothing to install:

| Schema | Reads |
|---|---|
| `Spring Boot` | Spring Boot 2.x default pattern (`2026-07-16 10:00:01.123  INFO 12345 --- [main] c.e.App : msg`) |
| `Spring Boot 3` | Spring Boot 3.x default pattern (ISO-8601 timestamp with `Z` or an offset) |
| `Tomcat Catalina` | `catalina.out` as written by JULI's `OneLineFormatter` (Tomcat 8+) |
| `Tomcat Access Log` | Tomcat `AccessLogValve` / Apache httpd access logs, common and combined |
| `Log4j2 Default` | The Log4j2 / Logback `%d [%t] %-5level %logger{36} - %msg` pattern |

They are ordinary library schemas, not built-ins: they never enter `project.json`, and they sit
*below* `.logscouter/schemas`, `~/.log-scouter/schemas` and any [hub](#hubs-shared-remote-libraries)
in precedence. So a schema of yours with the same name always wins, and upgrading log-scouter
cannot change how a log you already have a schema for is parsed. All of them fold Java stack traces into the line above.

The JSON lives in [`schemas/`](schemas/) in this repo, in the same shape as a user library file.
The same five are published in the
[official hub](#the-official-hub-and-the-bundled-schemas), which takes precedence once synced —
the bundle is the offline floor, the hub is how a fix ships without a release.

To propose a schema, open a PR against
[log-scouter-hub](https://github.com/mangosteen-lab/log-scouter-hub); it reaches users within
a day. Bundling it as well means adding it to `BUNDLED_SCHEMA_FILES` in
`src/core/extractor.rs`, which is worth it only for formats common enough to want offline.
Either way it must carry `samples`, which the test suite validates.

Select a source in the sidebar and press `Enter` to edit its short name, description,
tag, and schema. In the schema row, press `e` to edit the schema manually, `i` to infer it
with the configured LLM, `L` to load one from the schema library, or `X` to save the
current schema to the user library.

`L` offers **everything the project can resolve by name**: its own `.logscouter/schemas`,
your `~/.log-scouter/schemas`, every enabled [hub](#hubs-shared-remote-libraries), and the
bundled formats — the same set, in the same precedence order, that detection picks from.
Each row is tagged with where it came from:

```text
┌Schema Library  Enter apply  Esc cancel───────────────────────────────────┐
│> [User]         Nginx Access Log     Nginx combined access log           │
│  [Hub official] Spring Boot          Spring Boot 2.x default pattern     │
│  [Hub acme]     Gateway-Access       our edge logs                       │
│  [Bundled]      Tomcat Catalina      catalina.out via JULI               │
└──────────────────────────────────────────────────────────────────────────┘
```

## Library: schemas, filters and searches

Schemas, filters and saved searches all work the same way. On any of them in the sidebar:

| Key | Does |
|---|---|
| `L` | Pick one out of the library — every tier, tagged with where it came from |
| `X` | Save the selected one **into** your `~/.log-scouter` library, so every project can pick it up |

`L` works on an empty section too: with no saved searches yet, `L` on the `Saved Searches`
heading (or its `none` hint) still opens the search library — that is how you get your
first one.

The origin tag is the point: two tiers can offer a `Hide TRACE`, and the tag is what tells
them apart. The same tag then follows the item **after** you pick it — the sidebar says
whose rule each one is, and a source's schema row says which `Spring Boot` is parsing it:

```text
Filters
  Text
    * [Hub official] exclude level equals 'Trace'
    * [User]         include level equals 'ERROR'
    * exclude message contains 'healthz'      <- typed by hand: no tag, because
Saved Searches                                   nothing claims to know where it came from
    * [Hub official] /level=Error
```

A picked filter or search records its origin in `project.json`; one you typed carries no tag
rather than a guessed one. Schemas need no such record — they resolve by name, so the tag is
read live from the library and follows the copy that actually wins:

| Tag | Means |
|---|---|
| `[Project]` | This project — `.logscouter/schemas`, or saved into `project.json` |
| `[User]` | Your `~/.log-scouter/<kind>` |
| `[Hub xxx]` | The hub named `xxx` |
| `[Bundled]` | A third-party format shipped in the binary |
| `[Built-in]` | `Generic line` / `Bracketed default` — structural, in every project |
| *(none)* | You made it here, by hand |

```text
┌Filter Library  Enter add  Esc cancel─────────────────────────────────────┐
│> [Project]      Hide TRACE      drop trace noise                         │
│  [Hub official] Hide TRACE      Drop TRACE-level lines: the noisiest      │
│  [Hub official] Errors only     Keep only ERROR lines.                   │
└──────────────────────────────────────────────────────────────────────────┘
```

Picking a filter or a search **copies** it into the project, so it is yours from then on and
survives the hub going away. Picking a schema references it by name — see
[the hub notes](#names-across-hubs) for what that means when a hub is removed.

The tiers are gathered fresh each time you press `L`, so a hub synced mid-session shows up
without a restart. Filters and searches are not deduped across tiers: two offering the same
name are two things you may want to choose between.

For the bulk, folder-at-a-time route — export the whole filter set, import a whole pack —
use the command palette's `Import / Export ... pack` commands, which still take a folder
path (and accept `hub:<name>`).

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

Windows PowerShell:

```powershell
# OpenAI Codex CLI  ->  $HOME\.codex\skills\log-schema\
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-codex-skill.ps1 | iex

# Claude Code       ->  $HOME\.claude\skills\log-schema\
irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-claude-skill.ps1 | iex
```

Both honour `CODEX_HOME` / `CLAUDE_CONFIG_DIR` for the destination, and `LOG_SCOUTER_REPO`,
`LOG_SCOUTER_REF` and `LOG_SCOUTER_PROXY` for where they download from.

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
every candidate format and the best fit wins. Formats are tried **most specific first**, not
best-scoring first: a permissive format such as a bare `<message>` matches every line of
every file, so scoring alone would hand it every log. A format only has to explain a quarter
of the grouped entries to win, because continuation lines and block records are merged before
the score is evaluated. The status bar names the format that was chosen.

Candidates are gathered from, in order: the **project's own schema library**
(`<project>/.logscouter/schemas`), the **user library** (`~/.log-scouter/schemas`), each
enabled **[hub](#hubs-shared-remote-libraries)** in configured order, and the
**built-in** formats — so a schema you saved once is picked up automatically, without loading
it by hand. A schema chosen from a library is referenced by name, not copied into
`project.json`; it is re-resolved from the library each time the project opens (so editing the
library file updates every source that uses it). If nothing matches, the file falls back to
the built-in `Generic line`.

A piped or live source (`logscout -i`, `kubectl logs -f | logscout -i`) has no data when it is
added, so it starts on `Generic line` and **auto-detects from its first captured batch**:
once enough lines have streamed in, the same library scan runs and, if a schema fits, the
source switches to it and re-groups what it has captured.

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

Edit the set from the source editor (`Enter` on a source): the schema set is listed one per
row, numbered by priority. On a schema row, `L` adds another from the library, `d` removes the
selected one, and `K`/`J` move it up or down to change the match priority. Removing the only
schema leaves the source on the built-in `Generic line` fallback rather than on nothing. The
set is saved with the project. A merged view still uses each contributing file's primary
schema.

This works the same for a piped or live source (`logscout -i`, `kubectl logs -f | logscout
-i`): its editor lists the same schema rows and takes the same keys. Applying a set to a
piped source re-groups the lines it has already captured — its stdin cannot be reopened, so
the capture is never reloaded from the stream.

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
move to the schema row, then press `L` to apply one from the library. You do not have to
import a schema to apply it — `L` lists the whole library, hub and bundled schemas included.
Importing copies one into `project.json`, which is how you keep it if the hub goes away.

## Hubs: shared remote libraries

A **hub** is an ordinary git repo that publishes schemas, filters and saved searches for a
team to share — on GitHub, or on a self-hosted **GitLab** or **Gitea**, which is usually
where a team's own logs knowledge lives.

Every install comes configured with the **official hub**,
[`mangosteen-lab/log-scouter-hub`](https://github.com/mangosteen-lab/log-scouter-hub), and
refreshes it in the background at most once a day. That is how a new or fixed schema reaches
you without waiting for a release. It is the same JSON the binary already bundles, so the
sync is an *update channel*, not a dependency: see
[the official hub](#the-official-hub-and-the-bundled-schemas) for what that means offline,
and [auto-sync](#auto-sync-and-turning-it-off) to turn it off.

A hub is laid out exactly like the user-level library:

```text
log-scouter-hub/
  schemas/*.json      # same shape as an exported schema
  filters/*.json      # same shape as an exported filter pack
  searches/*.json     # same shape as an exported saved-search library
```

Anything else in the repo — README, CI config — is ignored, so a hub can be a normal repo
with docs. To publish one, export what you have (`X` on schemas, filters or searches) into
those folders and push.

Open the palette (`Ctrl+P`) and pick **Hubs** to manage them:

| Prompt | Does |
|---|---|
| `add acme/log-scouter-hub` | Add a hub and sync it immediately |
| `add acme/hub as ops` | Add it under a name you choose |
| `sync` / `sync acme` | Refresh every hub, or one |
| `remove acme` | Forget a hub and delete its cache |
| `disable acme` / `enable acme` | Keep it configured but contribute nothing |
| `auto-sync on` / `auto-sync off` | Refresh stale hubs on start, or never |
| *(empty)* | List the configured hubs |

The repo can be `owner/repo` (GitHub), an HTTP(S) URL, an SSH URL, or a `/tree/<branch>` URL
to pin a branch. Without a branch a hub tracks the repo's default branch.

### Self-hosted hubs, and tokens

Any GitHub, GitLab or Gitea host works — a bare `owner/repo` still means GitHub:

```bash
logscout hub add acme/log-scouter-hub                          # GitHub
logscout hub add https://gitlab.example.com/team/sub/hub       # GitLab, groups and all
logscout hub add http://git.internal/qhu/logs-hub.git          # self-hosted, http is fine
logscout hub add git@gitlab.example.com:team/hub.git           # an SSH remote, fetched over HTTPS
```

log-scouter recognises `github.com` by name. Any other host it identifies **by asking it** —
trying each forge's archive URL and keeping the one that answers — then remembers the answer
in `hubs.json`, so later syncs go straight there. (It cannot ask GitLab's API instead: a
self-hosted GitLab returns 401 for an anonymous `/api/v4/version`, while happily serving a
public repo's tarball.)

A private hub needs a token, and **each token only ever goes to its own kind of host**:

| Variable | Sent to | As |
|---|---|---|
| `GITHUB_TOKEN` | GitHub only | `Authorization: Bearer` |
| `GITLAB_TOKEN` | GitLab only | `PRIVATE-TOKEN` |
| `GITEA_TOKEN` | Gitea only | `Authorization: token` |
| `LOGSCOUT_HUB_TOKEN` | whichever host the hub is on | that host's header |

The scoping matters: `GITHUB_TOKEN` is set automatically all over CI, and a hub URL is
something a user types — sending it to an arbitrary host would hand that host a GitHub
credential. Use `LOGSCOUT_HUB_TOKEN` only when all your hubs are on one host; otherwise
prefer the per-forge variables.

### From the command line

The same operations, without opening the TUI — for scripts, dotfiles, and provisioning:

```bash
logscout hub                              # same as `hub list`
logscout hub list                         # what you have, what it holds, when it synced
logscout hub add acme/log-scouter-hub     # add and sync now
logscout hub add acme/hub --name ops      # add under a name you choose
logscout hub sync                         # refresh every hub
logscout hub sync acme                    # refresh one
logscout hub remove acme                  # forget it and delete its cache
logscout hub disable acme                 # keep it, contribute nothing
logscout hub enable acme
logscout hub auto-sync off                # or `on`
```

`logscout hub list` prints one line per hub:

```text
official [mangosteen-lab/log-scouter-hub] — 5 schema(s), 4 filter(s), 5 search(es), synced 2026-07-17
teamhub [acme/hub] — 3 schema(s), 0 filter(s), 2 search(es), disabled

Auto-sync: stale hubs refresh on start, at most daily.
Configured in /home/you/.log-scouter/hubs.json
```

Failures exit non-zero (an unknown hub, a repo that will not fetch), so a provisioning step
notices instead of quietly reporting success.

### Where hub content lives

Configured hubs are listed in `~/.log-scouter/hubs.json`; syncing downloads the repo over
HTTPS and unpacks those three folders into a cache of its own:

```text
~/.log-scouter/
  schemas/     <- yours, never written to by a sync
  filters/
  searches/
  hubs.json    <- the hub list
  hubs/
    acme/      <- a synced snapshot, replaced whole on each sync
```

Sync never writes into the folders you maintain by hand, so re-syncing or removing a hub
cannot destroy your work. A sync replaces the snapshot whole: a schema deleted upstream
stops being offered. A failed sync leaves the previous snapshot in place.

### The official hub and the bundled schemas

The five schemas in [`schemas/`](schemas/) are compiled into the binary **and** published in
the official hub. The two copies play different roles:

| | Role |
|---|---|
| Bundled in the binary | The **offline floor**. Works on a first run, with no network, forever. |
| Official hub | The **update channel**. Once synced, its copies take precedence. |

So an air-gapped install still parses Spring Boot and Tomcat logs on day one, and a schema
fixed in the hub reaches everyone else within a day without a release.

Unlike third-party hubs, the official hub's items are **not** namespaced: its `Spring Boot`
*is* `Spring Boot`, shadowing the bundled copy from one tier up rather than appearing beside
it as a second entry. That is what keeps a `project.json` that already references
`Spring Boot` resolving to the fresher copy. Precedence is unchanged — your own schemas
still win:

```text
.logscouter/schemas  →  ~/.log-scouter/schemas  →  official hub  →  bundled
```

Nothing about it is special-cased beyond that. `remove official` works and **sticks** — a
later start will not add it back — and `add mangosteen-lab/log-scouter-hub` restores it.

### Auto-sync and turning it off

On start, log-scouter refreshes any enabled hub whose cache is over a day old, on a
background thread. It never blocks the UI, and a failure is silent: the cache you already
have keeps working, so a flaky network or a plane just means slightly older schemas. New
schemas appear as soon as the refresh lands, without a restart.

To stop log-scouter from reaching the network on start:

```bash
LOGSCOUT_NO_HUB_SYNC=1 logscout        # per run, or export it
```

or persistently, from the Hubs prompt:

```text
auto-sync off
```

Either way hubs still refresh when you ask for them with `sync`. The environment variable
wins over the setting, and the prompt says so when both are in play. Turning auto-sync off
does not un-configure the official hub — it stays listed, ready for a manual `sync`.

### Names across hubs

Every item a hub provides is namespaced `<hub>/<name>`. Two hubs shipping a
`Gateway-Access` schema give you `acme/Gateway-Access` and `ops/Gateway-Access` — both
visible, both usable, neither silently shadowing the other, and neither colliding with a
local `Gateway-Access` of your own.

Precedence decides which schema **detects** a log first, and hubs slot in between your
schemas and the bundled ones:

```text
.logscouter/schemas  →  ~/.log-scouter/schemas  →  hubs, in configured order  →  bundled
```

So your own schemas always win, and reordering `hubs.json` reorders the hubs. Hub schemas
join detection as soon as they sync — nothing to import.

Filters and saved searches are copied into a project rather than detected, so you import
them when you want them: use `hub:<name>` as the folder in any import prompt.

```text
Import filter pack   →  hub:acme
Import saved-search  →  hub:acme
```

They arrive namespaced too, and import merges without overwriting what a project already
has. Filters and searches already imported stay if the hub is later removed — they were
copied in, and dropping a hub is not a reason to undo your work.

Schemas are the exception, because a source references a library schema *by name* rather
than copying it. A source parsed by `acme/Spring-Boot` falls back to the default schema if
that hub is removed or disabled, exactly as it would for any library schema that disappears.
To keep one for good, import it into the project (`I` on the schema library, or `hub:acme`)
before removing the hub.

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
or clearing a filter rewrites `<project>/.logscouter/project.json` right away —
once the project exists, that is: the first `Ctrl+s` creates `.logscouter`, and after
it every such edit autosaves without a further `Ctrl+s`. The filters are reapplied the
next time you open the folder. The sidebar `Filters` section always shows the active set.

Filters can optionally be scoped to a log format. A scoped filter applies only to
entries using that format; in a merged pane it applies per source entry. The
current input keyword is `schema=`:

```text
schema="Bracketed default" log_level equals exclude Trace
```

Unscoped filters keep the existing project-wide behavior. Filters are not yet
stored with a first-class list of specific log source ids.

## Filter Export/Import

On a filter in the sidebar, `L` picks one out of the [library](#library-schemas-filters-and-searches)
and `X` saves that filter into your user library. This section covers the **bulk** route
instead: the palette's `Import filter pack` / `Export filter pack`, which move a whole
folder's worth at once (imports merge, skipping duplicates). Both default to the user-level
library, shared by every project:

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

## Search Results Panel

A search opens a panel of every matching line under the pane. `Tab` moves the focus to it
(the border lights up), and it takes the same motions as the pane:

| Key | Action |
| --- | --- |
| `j` / `k` or arrows | move the selection |
| `gg` / `G` | first / last match |
| `[count]G` | the nth match (past the end stops at the last) |
| `PgUp` / `PgDn`, `Home` / `End` | move by a panel-full, or to either end |
| `Enter` or click | jump the pane to the selected match |
| `/` | refine the matches — below |
| `Esc` | drop the refine, then the search |

Rows are coloured by **level**, exactly as in the pane, so an `Error` among the matches is
visible without reading it. The query is highlighted in each row.

### Refining: a second search over what was found

Press `/` with the panel focused and type a second query. It narrows the matches already
found rather than searching the log again, so it is applied live as you type, and both
queries are highlighted in the rows that survive. The title names them in order:

```text
┌Matches 2/3  /node/ + /disk/  click or Enter to jump──────────────────────────┐
│     1/3    row     1 line      1  2026-06-16 10:09:00.000 Kernel  Error  …   │
│>    2/3    row     2 line      2  2026-06-16 10:09:01.000 Kernel  Trace  …   │
│     3/3    row     4 line      4  2026-06-16 10:09:03.000 Kernel  Info   …   │
└──────────────────────────────────────────────────────────────────────────────┘
```

The refine takes the [full query language](#search-query-language) — `level=Error`,
`/regex/`, `after:` and the rest — and it only ever narrows the panel: the pane's own
search, its highlighting, and which lines are visible are all untouched. `Esc` backs out
one layer at a time (the refine first, then the search), and starting a new search drops
it, since it described matches that no longer exist.

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
