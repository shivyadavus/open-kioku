# The gate

`ok gate` is the gate between a coding agent and the merge button. The agent declares a task;
the plan's recommended change boundary becomes the session's boundary; the harness's edit hooks
hold every edit to it; and when the agent stops, the actual change is verified against the same
boundary and written as a **Change Dossier**: one versioned artifact that says what was declared,
what was edited, what was blocked, what verification found, and what is still uncertain.

Every other verification step a coding agent is given today is an instruction to the model.
This one runs.

## Two commands

```sh
ok gate install claude --apply        # once per repository: hooks into .claude/settings.json
ok gate start "add retry handling to token refresh"
# ... the agent edits; every edit passes through the hooks ...
ok gate finish                        # verify, write the dossier, end the session
```

`ok gate start` builds the same plan `ok plan` builds, saves it under `.ok/gate/plan.json`, and
activates a session whose boundary is the plan's `allowed_files`, `caution_files`,
`forbidden_files` and `forbidden_rules`. It records the base revision (`HEAD`, or git's empty
tree in a repository with no commits yet) and every path that is already modified or
untracked, so a dirty working tree is not mistaken for the session's change. `--force`
replaces an active session only once the new plan is built, and keeps the replaced session's
trail as `.ok/gate/events-<session>.jsonl`.

`ok gate finish` verifies the change — every tracked change since the session's recorded base
revision, committed during the session or not, plus untracked files, less what was dirty before
the session and not edited during it — with the same `ChangeVerifier` behind `ok verify`,
writes `.ok/gate/dossiers/<session>.json` and `.md`, ends the session, and exits non-zero on a
`FAIL`. Pass `--run-commands` to execute the plan's validation commands and record their exit
codes; `--keep` writes the dossier and leaves the session active.

## What the hooks do

The Claude Code plugin ships [`hooks/hooks.json`](../hooks/hooks.json); `ok gate install claude`
writes the same entries into a repository's `.claude/settings.json` for installs without the
plugin (and for cloud sessions, which read repository settings but not user plugins). Each hook
is `ok gate hook <event>`: it reads the harness's JSON on stdin and answers on stdout.

| Hook | Event | What it does |
|---|---|---|
| `pre-tool` | `PreToolUse` on `Edit`, `Write`, `MultiEdit`, `NotebookEdit` | Classifies each path against the boundary. Denies a forbidden path in every mode but `observe`; denies an outside path in `enforce`; records each denial to `.ok/gate/events.jsonl`. Never opens the index: one small JSON read per edit. |
| `post-tool` | `PostToolUse`, same tools | Records the edit now that it has happened (a call the user refused or that failed is not an edit) and, after one outside or on a caution path, tells the agent so, with the command that widens the boundary. |
| `pre-tool`, `post-tool` | `PreToolUse` / `PostToolUse` on `Bash` | A shell command can write files the edit tools never see. Before it runs, the paths it names as write targets (`>`/`>>` redirections, `tee`, `sed -i`, `cp`/`mv` destinations, `touch`, `rm`, `git mv`/`git rm`) are judged like edits, and a dirty-tree snapshot is taken; a target outside the repository (`/tmp`, `/dev/null`) is not the gate's concern. After it runs, every path that became dirty and was not named is recorded in the trail and, when outside the boundary, reported to the agent. A write that already landed is recorded as warned, never denied. |
| `stop` | `Stop` | Verifies the change and writes the dossier. On `FAIL` it holds the agent with the violations and the remedies; when `stop_hook_active` is set (the agent is already continuing because of a stop hook) it reports and never blocks again, so a condition the agent cannot resolve cannot loop. A stop with nothing changed since the last verification (same trail, same dirty tree) answers from that verification instead of opening the index again. |
| `prompt`, `session-start` | `UserPromptSubmit`, `SessionStart` | One factual line: whether a session is active, its task and boundary size, and how to start, widen, or finish one. |

The edit hook fails closed: malformed input, an unreadable `ok.toml` or a torn session file
refuse the tool call (exit 2, with the way out on stderr) rather than let an edit through
unjudged. `ok gate abandon` removes a session file the hooks cannot read, so that way out
always works. The informing hooks (`prompt`, `session-start`, `post-tool`, `stop`) never block
on their own errors; a stop that passes reports with `systemMessage`, which the user sees and
which neither blocks nor continues the agent's turn.

A denial reads, for example:

```
Open Kioku gate denied this edit. `src/retry.rs` is outside the declared boundary for task
"add retry handling to token refresh". If this edit is required, widen the boundary first with
a reason: `ok gate expand src/retry.rs --reason "<why this file must change>"`, then retry.
The expansion and its reason are recorded in the Change Dossier.
```

## Modes

| Mode | Outside the boundary | Caution path | Forbidden path |
|---|---|---|---|
| `enforce` | denied until widened | allowed, reported | denied |
| `warn` (default) | allowed, reported; verification fails | allowed, reported | denied |
| `observe` | allowed, recorded | allowed, recorded | allowed, recorded |

Set the default in `ok.toml`:

```toml
[gate]
mode = "warn"          # enforce | warn | observe; `ok gate start --mode` overrides it
require_plan = false   # true: deny every source edit while no session is active
```

A forbidden path is one the plan's forbidden rules match (generated code, dependencies, the
index under `.ok/`) **or** one the secret-path policy matches (`.env`, `.env.*`, `.aws`, `.ssh`,
SSH key names, key-material extensions such as `.pem`, `.key`, `.p12`). `ok gate expand`
refuses a forbidden path: a boundary that can be widened to a secret is not a boundary.

## Widening the boundary

```sh
ok gate expand src/http/retry.rs --reason "the retry helper is a new module"
ok gate check  src/http/retry.rs        # expanded → allow
```

An expansion is recorded with its reason and timestamp, listed in the dossier under *Widened
during the session*, and admitted to the plan's `allowed_files` before verification. Nothing is
admitted silently, and nothing forbidden is admitted at all.

## The Change Dossier

`.ok/gate/dossiers/<session>.json` (schema version 1) and the same as Markdown, the shape a pull
request comment takes. It carries no source text: paths, counts, verdicts, reasons, and hashes.

- **Declared before editing** — risk and confidence from the plan, boundary sizes, the plan's
  SHA-256, allowed paths, and every expansion with its reason.
- **Edits as the harness reported them** — counts of allowed, caution, warned and denied edits,
  and the denied and warned paths. This is what the hooks saw, not what happened: an edit made
  outside the harness is caught by the next section, and the dossier says when the two disagree.
- **The actual change** — changed files, new untracked files, files left out because they were
  dirty before the session, the diff's SHA-256, and where the change was read from: the diff is
  taken against the session's base revision, so a commit made during the session is still part
  of the change, and renames keep both paths. If the base revision no longer exists (a rebase or
  amend rewrote it), the dossier says so and falls back to `HEAD`.
- **Verification** — the `ok verify` verdict, boundary violations, impacted dependents not
  updated, recommended tests the plan did not list, validation targets, commands run with exit
  codes, and warnings.
- **Caveats** — what the dossier cannot claim: a torn event record, a change with no reported
  edits, a non-git repository, observe mode.

The dossier's verdict is `FAIL` when verification fails, `WARN` when verification warns or any
edit was denied or warned during the session, and `PASS` otherwise.

## Measured

[`scripts/gate-sim.py`](../scripts/gate-sim.py) drives synthetic agent sessions against an
indexed repository: tasks are the subjects of real commits, legitimate edits are the files those
commits changed, and drifting edits are files outside the boundary, forbidden paths, and files
created without any hook. On this repository, 38 commit-derived sessions in `enforce` mode
(release build): 114 of 114 drift edits and 76 of 76 forbidden edits denied at the hook, 38 of
38 hook-less bypasses failed at `finish`, 81% of a faithful agent's edits admitted without
widening (18 of 38 sessions needed none), hook median 5.3 ms and p95 8.0 ms per edit. In `warn`
mode (20 sessions), where the hook denies only forbidden paths, the verifier at `finish` failed
60 of 60 drift edits and 20 of 20 bypasses; 38 of 40 forbidden edits were denied at the hook, and
the two that were not were writes under a git-ignored build directory the index never saw, which
are warned, recorded, and cannot reach a commit. Method, caveats and the raw reports:
[`benchmarks/gate/`](../benchmarks/gate/README.md).

## What it is not

- It does not edit source, and it does not decide what the task should be. The plan is the
  agent's declaration; the gate holds the agent to it.
- It is not proof that the change is correct. A `PASS` says the change stayed where it was
  declared and the verifier found no missing dependents; tests and review still decide the rest.
- It is not available to an agent that bypasses its harness. The hooks see what the harness
  reports, including the files a shell command names or leaves changed; a write the shell hook
  could not attribute, or one made from outside the harness, is seen by the verifier at
  `finish`, and the dossier says when the trail and the tree differ.
- A widened boundary is only as honest as its reason. `ok gate expand` records the reason; it
  does not judge it. The reviewer reads the dossier's *Widened during the session* list.
