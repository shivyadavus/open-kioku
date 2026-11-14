# Open Kioku beside an agent on a large repository

Run on 2026-11-14 against [`django/django`](https://github.com/django/django) at `f24f38c`
(7,102 tracked files), with a real Claude Code session per task, arm and model, and the task's
own tests as the judge. The result it had: **no measurable gain from Open Kioku, and a cost for
the smaller model that came from the gate's own notices.** That cost led to a change in the
gate, measured below. The harness is [`scripts/agent-large-ab.py`](../../scripts/agent-large-ab.py),
the tasks are [`large-repository-tasks.json`](large-repository-tasks.json), and every session's
record is in [`large-repository.json`](large-repository.json).

The bar was set before the run: a clearly better outcome, or about a fifth fewer turns or
dollars at the same outcome, on most tasks. It was not met.

## Method

Ten real fixes: every commit among the last 480 on Django's main branch whose subject starts
`Fixed #`, that changes three or more files under `django/` and tests under `tests/`, with 30
to 400 changed source lines, and whose changed test modules fail at the parent with the
commit's tests applied and pass at the commit, on SQLite. Twelve commits matched the first
conditions; one needs PostgreSQL and one did not pass at the commit in this environment.

The checkout is the commit's parent with the commit's test changes applied, as a new
repository of one commit: no object holds the answer. The prompt is the commit's subject, the
instruction to change `django/` so the tests pass without touching `tests/`, and the command
that runs them. The agent may run that command. Afterwards the harness restores `tests/`, runs
the same test modules itself, and records whether they pass. One session per cell, with one
repeat on the smaller model to see how far a cell moves by itself.

| Arm | What the agent had |
|---|---|
| plain | Read, Grep, Glob, Edit, Write, read-only shell, and the test command |
| plugin, as merged | the same, plus the Open Kioku plugin as it was on `main` before this change: the MCP server connected (16 tools), the skill, the hooks, an index built before the session, `ok` on PATH |
| plugin, quiet | the same plugin, with the change this measurement led to: an advisory session no longer tells the agent which edits fall outside the boundary it planned from the prompt |

Two models: the CLI's default, and `claude-haiku-5-5` as a smaller one. The prompt is the same
in every arm and says nothing about Open Kioku.

## Results

Every session's change passed the task's tests: 80 of 80. So the comparison is of what it took.

The default model, means over the ten tasks:

| Arm | Tests pass | Turns | Cost (USD) | Time (s) | Hunks of the commit touched | Open Kioku tool calls | Boundary notices | `ok gate expand` runs |
|---|---|---|---|---|---|---|---|---|
| plain | 10 of 10 | 21.5 | 0.868 | 109 | 121 of 166 | 0 | 0 | 0 |
| plugin, as merged | 10 of 10 | 21.6 | 0.885 | 106 | 117 of 166 | 0 | 70 | 10 |
| plugin, quiet | 10 of 10 | 22.1 | 0.900 | 104 | 119 of 166 | 0 | 0 | 0 |

The smaller model:

| Arm | Tests pass | Turns | Cost (USD) | Time (s) | Hunks of the commit touched | Open Kioku tool calls | Boundary notices | `ok gate expand` runs |
|---|---|---|---|---|---|---|---|---|
| plain | 10 of 10 | 24.3 | 0.023 | 87 | 111 of 166 | 0 | 0 | 0 |
| plain, repeated | 10 of 10 | 26.7 | 0.024 | 88 | 108 of 166 | 0 | 0 | 0 |
| plugin, as merged | 10 of 10 | 36.6 | 0.035 | 131 | 110 of 166 | 0 | 78 | 37 |
| plugin, quiet | 10 of 10 | 30.3 | 0.027 | 104 | 109 of 166 | 0 | 0 | 0 |
| plugin, quiet, repeated | 10 of 10 | 24.0 | 0.029 | 102 | 78 of 166 | 0 | 0 | 0 |

Per task, the default model, turns / cost in USD:

| Task | plain | plugin, as merged | plugin, quiet |
|---|---|---|---|
| `0804aed` Allowed multiline template tags. | 32 / 2.11 | 30 / 1.75 | 33 / 1.81 |
| `5c13b5a` Added support for the HTTP QUERY method. | 29 / 1.06 | 27 / 1.26 | 34 / 1.53 |
| `9e06baf` Wrapped async iteration sites in (maybe_)aclosing. | 25 / 0.80 | 30 / 0.95 | 29 / 0.83 |
| `a013c82` Reduced per-middleware context switching under ASGI. | 13 / 0.81 | 13 / 0.80 | 12 / 0.79 |
| `0398417` Added django.utils.module_loading.qualname() helper. | 8 / 0.35 | 10 / 0.47 | 3 / 0.26 |
| `5f90dc2` Moved MiddlewareMixin out of utils.deprecation. | 28 / 0.87 | 20 / 0.77 | 38 / 0.98 |
| `65a9f14` Supported modules and submodules in import_string(). | 13 / 0.62 | 12 / 0.60 | 11 / 0.62 |
| `99a14b6` Warned via system check on missing "default" entry in MAILERS. | 16 / 0.55 | 12 / 0.48 | 14 / 0.53 |
| `f970a98` Moved django_file_prefixes() to django.utils.warnings. | 35 / 0.83 | 42 / 1.02 | 31 / 0.92 |
| `4bbc27c` Restored exact boolean lookup against literals on SQLite. | 16 / 0.67 | 20 / 0.74 | 16 / 0.72 |

## What it shows

**No gain.** With the default model the three arms are the same: 21.5, 21.6 and 22.1 turns,
0.87 to 0.90 USD, every task solved. The plugin arm took fewer turns than plain on five tasks
of ten and more on the other five.

**The tools were never called.** In 50 sessions with the MCP server connected and its 16 tools
offered, neither model called one, and neither loaded the skill. Both worked the way they do
without it: search, read, edit, run the tests.

**Ripple had nothing to say.** One post-edit ripple notice in 50 plugin sessions, and no broken
site left in any arm, by `ok ripple` on the finished change. With the tests in hand and the
means to run them, neither model left a caller behind.

**The gate's notices cost the smaller model turns, and they were wrong.** As merged, an
advisory session told the agent each time an edit fell outside the boundary planned from the
prompt. Over the 20 sessions of that arm it did so 148 times, and 142 of those were about a
file the real commit changed: the plan from a ticket title names a few files, and a real fix
touches more. The default model mostly ignored the notices (10 `ok gate expand` runs). The
smaller model answered them: 37 `ok gate expand` runs, and 36.6 turns a task against 24.3 and
26.7 without the plugin.

**Quiet, the cost is inside the noise.** With the notices no longer said to the agent (the
edits are still recorded, and the dossier still lists them), the smaller model took 30.3 and
24.0 turns in two runs, against 24.3 and 26.7 for plain in two runs. The default model is
unchanged.

## What changed in Open Kioku

- An advisory session records an edit outside its planned boundary, or on a caution or sibling
  path, and says nothing to the agent. A forbidden path is still denied, a session someone
  declared (`ok gate start`, or `mode = "enforce"`) still reports and holds as before, and the
  Change Dossier still lists every file outside the boundary.
- The line the agent is given at the prompt no longer describes a boundary it has nothing to
  do about; it says a dossier is written at the stop and when the agent will hear from the hooks.

## Other things this run measured

- Indexing a Django checkout took 26 to 33 seconds and wrote a 762 MB `index.sqlite` beside a
  64 MB search index, for a working tree of 87 MB.
- All 80 sessions together cost USD 27.91.

## What this does not measure

- **Tasks with no tests to work from.** Here the tests were given and could be run, which is
  how both models solved every task. A task described only in words, judged by tests the agent
  never sees, is harder, and nothing here says what Open Kioku does there.
- **Other harnesses and models.** One harness, two models from one family.
- **A reviewer's time.** The dossier and `ok gate ci` are for the person reading the change
  afterwards. This run measures the agent's work, not theirs.
- **Variance.** One session per cell, two for the smaller model's plain and quiet arms. The
  two plain runs differ by 2.4 turns a task and the two quiet runs by 6.3, so a difference of
  that size between arms is not a finding.
- **Completeness beyond the tests.** "Hunks of the commit touched" counts how much of the real
  change the agent's edit landed on. It moved between runs of the same arm (109 and 78 of 166)
  while the tests passed in both, so the tests are the verdict and that column is context.

## Reproduce

```sh
git clone --depth 500 https://github.com/django/django dj
python3 -m venv venv && venv/bin/pip install asgiref sqlparse jinja2 pyyaml tblib \
    docutils pillow argon2-cffi bcrypt tzdata
cargo build --release -p open-kioku-cli

scripts/agent-large-ab.py --source dj --work work --out out --venv venv \
    --ok-bin target/release/ok --plugin-dir . \
    --tasks benchmarks/agent-ab/large-repository-tasks.json \
    --models default claude-haiku-5-5
```

The clone has to reach the task commits; `--depth 500` did on the day of the run, and a full
clone always does. Sessions cost money: this run's 80 came to USD 27.91.
