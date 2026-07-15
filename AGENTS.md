<!-- modric-cli:begin -->
# Modric troubleshooting via `modric-cli` (Codex)

Codex reads project/user guidance from `AGENTS.md`. `install-skill.sh codex` appends this
snippet to `~/.codex/AGENTS.md` (create it if missing). You can also paste it into a
project's `AGENTS.md`.

---

## Using the `modric` CLI

When the user references a Modric job execution (a link containing `#view?job=`, a job id,
or asks to troubleshoot a Modric job / read or edit a Modric script, definition, trigger, or
config-map), use the `modric` command-line tool. It prints JSON by default.

Auth is preconfigured via `modric auth login` or `MODRIC_URL`/`MODRIC_TOKEN`. If `modric`
reports no token, ask the user to run `modric auth login --url <host> --token <API_TOKEN>`
(token from the Modric web UI -> Account settings). Never handle the raw token yourself.

Troubleshoot a failed job:

```bash
modric jobs get <JOB_ID>               # status, error, per-step status, state, inputs
modric jobs logs <JOB_ID> --step <N>   # tail of the failed step's log
modric scripts get <ID>                # the step's script (config-map secrets never shown)
```

Then propose a fix and, after the user confirms, apply it (`modric scripts update ...`,
`modric configmaps update ...`, `modric definitions update ...`) and re-run with
`modric jobs run --definition "<NAME>"`.

Search (results hard-capped at 1000, each <=10 KB; use `-B N`/`-A N` for context lines):

```bash
modric jobs search-logs <JOB_ID> "error|traceback" --regex -B 3 -A 3
modric scripts search "<term>" --content
modric {definitions|triggers|configmaps|jobs} search "<term>"
```

Resources: `modric {scripts|definitions|triggers|configmaps|jobs} --help`.
Rules: confirm before any create/update/run; `delete` is destructive and needs `--yes` -
only with the user's explicit go-ahead; never request secret values; config-map values are
never returned by the API.
<!-- modric-cli:end -->
