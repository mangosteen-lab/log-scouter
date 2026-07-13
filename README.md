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
  formats and picks the one that explains them, most specific first. A log no format
  explains falls back to `Generic line`: one entry per line, timestamp read off the line.
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
- **Vim-style navigation.** Supports `j/k`, `gg`, `G`, `[count]j`,
  `[count]G`, paging, and horizontal scroll.
- **Split panes.** Use `|` for columns and `-` for rows.
- **AI assistant.** Press `A` for a chat panel that troubleshoots the logs for you — it
  inspects the sources and drives filters, searches, and time ranges through the same
  operations you would, iterating until the issue is understood. Works with OpenAI,
  Anthropic, and DeepSeek; the key comes from the environment.

## Quick Start

Install the `logscout` command from GitHub Releases:

```bash
curl -fsSL https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install.sh | bash
logscout /path/to/logs
```

Run `logscout .` inside a log folder to add every direct text file in that folder as
a log source. Run `logscout` with no arguments to start an empty project, then press
`o` to browse for a folder and add its text files.

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
| `a` / `o` / `d` | add file / browse for a folder / remove focused file from project |
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
| `Delete` (sidebar) | remove the filter under the cursor |
| `T` | measure elapsed time from the current line (again to turn off) |
| `x` / `L` | export filters / import filters from a folder |
| `X` / `I` | export log schemas / import log schemas from a folder |
| `H` | hide logs like the selection, using fields from the log format |
| `Space` (hide menu) | pick a field; `Enter` ANDs the picks into one regex |
| `H` (hide menu) | derive a pattern from the single current line |
| `↑` / `↓` (pattern popup) | pick a template, greediest first |
| `Tab` (pattern popup) | flip the derived pattern between hide and keep |
| `Space` (sidebar log) | add/remove that log from the view, merged by timestamp |
| `Space` (sidebar filter) | enable/disable that filter |
| `Space` (sidebar search) | run that saved search, or clear it if it is running |
| `Enter` (sidebar log) | edit that log's format |
| `Enter` (sidebar filter) | edit that filter rule |
| `Enter` (sidebar search) | edit that saved search |
| `S` | define a reusable log format |
| `e` | assign or edit the focused file's log format |
| `|` / `-` / `w` | split columns / split rows / close pane |
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

On any filter row, `Space` enables or disables it and `Delete` removes it. On the `Time`
row -- or on the `none - t` under it -- `Enter` reopens the picker on the range in force,
with the caret on `Start`, so changing "when" never means retyping it. Nobody should have
to hand-edit `timestamp range 'a..b'`, so that row does not open the filter text editor.

The row itself is written for reading, not for parsing: two clock times when the range
stays inside a day, `06-16 23:00:00 → 06-17 01:00:00` when it does not, `from …` or
`until …` for an open end, and the span in brackets. The detail panel below spells out the
full start, end and span.

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
┌AI  (Enter send · Esc leave)──────────────────────┐
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

The assistant is given the project's context (folder, sources, their schemas and counts,
current filters, the focused view) and a set of tools that map onto log-scouter's own
operations:

- **Inspect** — list sources, list filters, sample lines, count matches for a query, level
  breakdown.
- **Act** — add a filter, set a time range, run a search, add a log source.

It runs an **agentic loop**: it calls a tool, sees the result, and keeps going until it has
an answer — so one question can play out as several steps, with the panels updating live and
each action noted in the transcript. `Esc` cancels a reply in flight, then leaves the panel;
`↑`/`↓` scroll the transcript; write actions apply immediately (a removed source still
confirms).

**Setup.** No key is stored on disk — the assistant reads it from the environment:
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or `DEEPSEEK_API_KEY`. If the selected provider's
variable is unset, the panel says so — you can type `/key <your-key>` right in the chat
instead (kept in memory for the session, never written to disk). Choose the provider and model from the
chat with `/provider openai|anthropic|deepseek` and `/model <name>` (saved to
`~/.log-scouter/ai.json`); `/clear` resets the conversation. `LOGSCOUT_AI_BASE_URL`
overrides the endpoint for a corporate gateway or a compatible self-hosted model.

## Opening a Folder

Press `o` to browse for a folder, starting from the one the project is already in. The
popup lists the subfolders of wherever you are, plus how many files sit directly in it:

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

A log format is the parser definition that turns a raw line into named fields.
It has a name, expression, optional description, timestamp field, and timestamp
format. Users define formats in the project; a format is then assigned **per
file**, so one project can hold logs of different shapes.

Two formats are built in. `Bracketed default` reads the bracketed server log. `Generic line`
reads anything: its expression is a bare `<message>`, so every line is its own entry and
nothing is extracted. Detection tries formats most specific first, so `Generic line` only
wins a file no other format explains -- which is the point, because under a format that
matches nothing at all, no line starts a record and the whole file folds into one entry.
A file whose stored format explains none of its opening lines is re-detected on load.

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
