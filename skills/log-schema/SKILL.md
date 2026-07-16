---
name: log-schema
description: >-
  Generate a log-scouter (logscout) user-level log schema from sample log lines and
  write it to ~/.log-scouter/schemas/<name>.json. Use whenever a user wants logscout to
  parse a new/custom log format — extract timestamp, level, and fields, and group
  multi-line records.
---

# Authoring a log-scouter log schema

log-scouter (the `logscout` TUI) parses each log source with a **schema**: a format
template plus a few regexes. A user-level schema is a JSON file in
`~/.log-scouter/schemas/`. Every schema there is auto-detected against opened files and
offered in the schema picker. Your job: read sample lines, infer the format, and write a
correct schema file.

## Workflow

1. **Read a representative sample** of the log (the first ~200 lines, and a few `ERROR`/
   stack-trace lines if present). Identify: the timestamp, the level, other fixed fields,
   the free-text message, and whether records span multiple physical lines.
2. **Write the format template** (below).
3. **Pick the `timestamp_format`** (chrono strftime, below).
4. **Set `entry_start`** if records can span multiple lines — and make sure it matches the
   header lines (see the pitfall).
5. **Add 1–2 `samples`** copied verbatim from the log, asserting `level` where there is one.
6. **Write** `~/.log-scouter/schemas/<sanitized-name>.json` and tell the user it will be
   picked up next time they open that log (schemas are validated when logscout loads them,
   so a broken one is rejected with an error).

## File shape

```json
{
  "name": "My Service Log",
  "description": "One line describing the format.",
  "schema": {
    "name": "My Service Log",
    "description": "One line describing the format.",
    "format": "<timestamp> [<level>] <message>",
    "timestamp_field": "timestamp",
    "timestamp_format": "%Y-%m-%d %H:%M:%S%.3f",
    "field_patterns": {},
    "entry_start": "^\\d{4}-\\d{2}-\\d{2} ",
    "samples": [
      { "line": "2026-07-15 10:00:01.123 [INFO] started", "level": "INFO" }
    ]
  }
}
```

`name`/`description` appear both at the top and inside `schema` (keep them identical).
`entry_end` and `samples` are optional. `field_patterns` may be `{}`.

## The format template

The template is the literal text of one log line with each variable span replaced by a
`<field>` placeholder. The literal characters **between** placeholders (brackets, colons,
commas, spaces) must appear exactly as in the log.

- Exactly one field must be named `<timestamp>`.
- `<field?>` marks a field only *some* lines carry; it also consumes the literal separator
  immediately **before** it.
- Field **names matter** — these map to logscout's columns/filters:
  `timestamp`, `level`, `module` (also matches a field named `component`), `message`,
  `host`, `server`, `pid`, `thread`, `file`, `line`. Use them where they fit.
- The **last** placeholder is greedy (matches the rest of the line); earlier ones are
  non-greedy and stop at the next literal. So `<message>` usually goes last.

Example — for the line `[2026-06-16 10:09:43.288][Kernel][Info] service started`:

```
"format": "[<timestamp>][<module>][<level>] <message>"
```

## timestamp_format (chrono strftime)

Match the timestamp's punctuation exactly. Common ones:

| Log timestamp                 | `timestamp_format`              |
|-------------------------------|---------------------------------|
| `2026-07-15 10:00:01.123`     | `%Y-%m-%d %H:%M:%S%.3f`          |
| `2026-07-15T10:00:01.123Z`    | `%Y-%m-%dT%H:%M:%S%.3fZ`         |
| `2026-07-15T10:00:01,123`     | `%Y-%m-%dT%H:%M:%S,%3f`          |
| `2026/07/15 10:00:01`         | `%Y/%m/%d %H:%M:%S`              |

Note the **comma** vs dot before milliseconds: `%.3f` includes a leading dot; for a comma
write the literal `,` then `%3f` (no dot). Elasticsearch uses the comma form.

## entry_start / entry_end (multi-line records)

When one logical record spans several physical lines (JSON blocks, Java stack traces), set
`entry_start` to a regex matching the **first** physical line of each record. Lines that do
not match fold into the record above them. Use `entry_end` only when there is a reliable
closing line (e.g. a `}` on its own).

- Block format `{ ... }`: `entry_start` `^\\s*\\{\\s*$`, `entry_end` `^\\s*\\}\\s*$`.
- Bracketed/prefixed lines: anchor on the timestamp, e.g. `^\\[\\d{4}-\\d{2}-\\d{2}T`.

### Pitfall: an entry_start that matches nothing collapses the file

If `entry_start` matches **none** of the header lines, every line becomes a continuation and
the whole file shows up as a single giant record. Always verify it matches the real headers.
A classic mistake: `^\\S+ \\[HOST:` on `2026-06-12 10:17:44.944 [HOST:...]` — `\\S+` stops at
the space inside the timestamp, so it matches nothing. Use `.+?` or a concrete pattern like
`^\\d{4}-\\d{2}-\\d{2} \\d{2}:\\d{2}:\\d{2}` instead. If records are single-line, you can omit
`entry_start` entirely — logscout derives a header probe from the first two fields.

## field_patterns and two useful tricks

`field_patterns` pins a field to a tighter regex than the default. logscout uses Rust's
`regex` crate — **no lookahead/lookbehind**.

**1. Strip trailing padding while staying robust (the pad trick).** Levels are often padded
to a fixed width (`INFO `, `WARN `, but `ERROR`, `DEBUG` are already 5 chars). Capturing the
bracket verbatim yields `"INFO "` (trailing space), which breaks exact `level equals INFO`.
Add an optional pad field that soaks up the spaces so `level` stays clean *and* unpadded
levels still match:

```json
"format": "[<timestamp>][<level><levelpad?>][<component>] [<node>] <message>",
"field_patterns": { "level": "[A-Z]+", "levelpad": " +" }
```

**2. Trailing catch-all for variable tails.** When a JSON-ish line has many optional trailing
keys, capture the reliable head fields and let a final `<detail>` absorb the rest — it parses
all shape variants and is fast:

```json
"format": "{\"@timestamp\":\"<timestamp>\",\"message\":\"<message>\",\"level\":\"<level>\",<detail>"
```

## samples

Copy 1–2 real lines into `samples`. Each must parse under the schema (validated on load).
Add `"level": "<expected>"` to assert the parsed level — this catches a format that matches
but extracts the wrong value. Use a clean sample (e.g. one without a giant stack trace).

## Verify

- The schema JSON must be valid and every `sample` must parse — logscout rejects a schema
  whose samples do not match, with a clear error, when it loads `~/.log-scouter/schemas/`.
- Tell the user to (re)open the log in logscout; the new schema is auto-detected, or they can
  pick it from the schema list. If a field looks wrong, adjust the template/`field_patterns`.

## Naming the file

Write to `~/.log-scouter/schemas/<Name>.json`. Use a filename derived from the schema name
with spaces/`/` replaced by `-` (e.g. `MicroStrategy SearchService` → `MicroStrategy-SearchService.json`).
Do not overwrite an existing schema without confirming.
