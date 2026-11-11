# The gate

`ok gate` is the gate between a coding agent and the merge button. A task is declared; its
plan's recommended change boundary becomes the session's boundary; the harness's hooks judge
every edit against it; and the actual change is verified against the same boundary and written
as a **Change Dossier**: one versioned artifact that says what was declared, what was edited,
what was blocked, what verification found, and what is still uncertain.

Every other verification step a coding agent is given today is an instruction to the model.
This one runs.

Who declared the task decides how binding the boundary is:

| Declared by | How | The boundary |
|---|---|---|
| Nobody: the prompt | the hooks alone (`auto`, the default) | **advisory**: files outside it are reported and recorded, and are not a failure; forbidden paths are still denied |
| The repository | `mode = "enforce"` in `ok.toml` | binding for every session, prompt-started ones included, from the next tool call after it is set |
| A person | `ok gate start "<task>"` | binding: a failing change holds the agent at stop; `enforce` also denies at the edit |
| Nobody: a pull request's words | `ok gate ci --base … --task "<title>"` | **advisory**: files outside it are listed in the dossier; a forbidden path fails the check |
| The team, for CI | `ok gate ci … --mode enforce`, or `mode = "enforce"` on the base branch | binding: a file outside the boundary fails the check |

## Zero commands

Install the Claude Code plugin (or run `ok gate install claude --apply` once) and the gate runs
itself. The first prompt of a conversation plans a boundary in the background and starts a
session; each later prompt of the same conversation widens that boundary with its own plan,
recorded with the prompt as the reason; every edit is judged against it; every stop that
follows a change verifies it and refreshes the dossier; the session ends when the conversation
does. A conversation that changes nothing writes nothing. A repository with no index is
indexed in the background the first time a session opens.

A prompt whose plan has Low confidence starts nothing and widens nothing: a greeting or a
question about the project plans that way, and nothing is held to it. The next prompt is
planned on its own (`.ok/gate/start.log` says what the last planner decided). Under
`require_plan` an edit with no session is denied, so there a task the planner has Low
confidence in needs `ok gate start`.

A prompt-started session belongs to the conversation that sent the prompt. Every hook carries
its conversation's id, and a conversation is judged only against its own session: a second
conversation open in the same repository is not denied, warned or held for the first one's
task, and its stop and its end leave the first one's session and plan alone. One repository
holds one prompt-started session, so the second conversation's first task prompt closes the
first one's session, dossier written, and starts its own; what the first left uncommitted then
predates the second. Two conversations editing one working tree share one diff: a file either
of them changes is in the change of whichever session is active. A session started by hand
binds everyone working in the repository.

A session started this way is **advisory**. Its boundary is a plan built from the prompt's
words, and a plan cannot tell a task from a remark: measured on this repository, `"fix it"` and
`"try again"` plan with the same High confidence as a real commit subject
([`benchmarks/gate/`](../benchmarks/gate/README.md)). So the gate does not hold an agent to a
boundary nobody declared. It tells the agent which edits fall outside it, with
`ok gate expand <path> --reason "<why>"` as the way to record why one belongs; it denies the
paths that are forbidden by policy rather than by plan (secrets, key material, generated code,
dependencies, the index); and it writes the dossier, which lists every changed file outside the
boundary under *Outside the planned boundary* and gives `WARN`, not `FAIL`, for them. A verdict
that fails changes a reviewer would accept teaches everyone to ignore it, so `FAIL` is kept
for what someone declared, and for forbidden paths. Holding the agent is for a boundary someone
declared: the table above.

This is `[gate] auto = true`, the default. Automatic work happens only at the top of a git
working tree, indexed or not: the hooks stay silent anywhere else, do not index a repository
with more than 20,000 files (tracked, or untracked and not ignored: what the indexer would
walk; they say to run `ok index` once instead), and do nothing automatic while `ok.toml` cannot
be read. The first 400 characters of a prompt become the session's task, stored under
`.ok/gate/` and shown in the dossier. That directory holds prompt text, so the gate writes a
`.gitignore` into `.ok/gate/` when it creates it, and into `.ok/` when the hooks index a
repository that does not already ignore it: nothing the hooks create unasked is left for
`git add -A` to pick up. Set `auto = false` to run the gate only by hand.

## By hand

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

## In CI, for any pull request

```sh
ok gate ci --base origin/main --task "$PR_TITLE

$PR_BODY" --write dossier.md
```

No hooks, no session: `ci` plans from the task text, verifies what the branch added since it
left `--base` (the merge base, so what the base gained meanwhile is not counted) plus untracked
files against the plan's boundary, prints the dossier (and writes the Markdown to `--write` for
a pull request comment; an untracked dossier file an earlier run left in the working tree is
not counted as the change, and a `--write` path the repository tracks is refused, so a branch
cannot take one of its own files out of the judgement by naming it), and exits non-zero on
`FAIL`. It judges a
pull request whatever wrote it — any agent, any harness, a person — with the trail section
empty, since no hook saw the edits.

By default the boundary is **advisory** here too, for the reason it is in a prompt-started
session, and with a measurement behind it. Replayed over the last 20 merged commits of this
repository, each judged against its own subject, a binding boundary fails all 20
([`benchmarks/gate/`](../benchmarks/gate/README.md#ci-over-merged-history)): 269 of their 432
changed files fall outside the plan, which names eight files to edit while a real pull request
also changes its tests, its documentation, the changelog, a lockfile, and four in ten of its
source files. So by default
`ok gate ci` lists the files outside the planned boundary and exits 0 with `WARN`. It still
fails on a [forbidden path](#modes) (a secret-like path or key material, a dependency
directory, generated output) and, with `--run-commands`, on a validation command that fails.
`--mode enforce` makes the boundary binding for a team that wants every file a pull request
touches to be one its description leads to.

Without `--mode`, the mode and `sibling_caution` are `[gate]` of `ok.toml` **as the base
revision has it**, not as the judged change has it: a branch cannot make its own judgement
advisory, or widen its own boundary, by editing the file, and the dossier says so when the two
differ. `--mode` is the caller's. The task text may begin with a dash, and reaches the Markdown
dossier as one clipped line with Markdown and HTML escaped, since a pull request's description
is text someone else wrote.

This repository runs it on its own pull requests: the `change-dossier` job in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) builds the plan from the pull
request's title and description, reads the change against the first parent of the merge
commit it checked out (`--base 'HEAD^1'`: exactly what was merged, however far the base branch
has moved), and puts the dossier in the job summary.

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
| `stop` | `Stop` | Verifies the change and writes the dossier. On `FAIL` in a binding session it holds the agent with the violations and the remedies; when `stop_hook_active` is set (the agent is already continuing because of a stop hook) it reports and never blocks again, so a condition the agent cannot resolve cannot loop. In an advisory session and in `observe` it reports and never holds. A stop with nothing changed since the last verification answers from that verification instead of opening the index again; a prompt-started session that has changed nothing is not verified at all. A session stays open after a stop. A plan still being built for the conversation is waited for (up to twelve seconds), so what the turn edited is judged now; a planner that outlasts the wait is left to finish, and the next stop or the end of the conversation judges those edits against the baseline the prompt recorded. |
| `prompt`, `session-start` | `UserPromptSubmit`, `SessionStart` | With `auto`: index the repository in the background when it has none; on a prompt, plan in the background, starting a session or widening the conversation's (a slash command, a `!` or `#` line, or a prompt under three characters is not a task; a prompt whose plan has Low confidence starts nothing and changes nothing). The prompt reaches the planner through a marker file, never a command line. A session started by hand is never touched. Otherwise one factual line: whether a session is active, its task and boundary size, and how to widen it. |
| `session-end` | `SessionEnd` | Ends the conversation's prompt-started session, writing its dossier if it changed anything since the last verification, and cancels a plan still being built for it once the usual wait is over. A new conversation's first task prompt does the same for a session a crash left behind, which in the meantime judges nobody else. |

While a prompt's plan is still being built, the edit hook waits up to twelve seconds for it;
if the planner is not done, the edit proceeds and verification judges it, because the
session's baseline is taken when the prompt arrives, not when the plan is ready (under
`require_plan` the agent is asked to retry in a moment instead). A planner acts only while its
own marker stands: a newer prompt, the end of the conversation or a manual `ok gate start`
replaces or removes the marker and the planner backs out. A newer prompt of the same
conversation does not drop the earlier one: the new plan is built from both prompts' words and
keeps the earlier prompt's baseline. The marker records the planner's process, and a marker
whose process is gone is ignored at once (so is one with no known process after ninety
seconds, and the marker of an indexer that was killed): a dead planner never stalls a tool
call. The edit hook fails closed:
malformed input, an unreadable `ok.toml` or a torn session file refuse the tool call (exit 2,
with the way out on stderr) rather than let an edit through unjudged. `ok gate abandon` removes a session file the hooks cannot read, so that way out
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
| `warn` (default) | allowed, reported; in a declared session verification fails, which holds the agent at stop; in an advisory one it is listed and the verdict is `WARN` | allowed, reported | denied |
| `observe` | allowed, recorded; listed in the dossier, never a failure in an advisory session | allowed, recorded | allowed, recorded |

Set the defaults in `ok.toml`:

```toml
[gate]
mode = "warn"            # enforce | warn | observe; `ok gate start --mode` overrides it
require_plan = false     # true: deny every source edit while no session is active
auto = true              # plan from each prompt, index when needed; such sessions are advisory in `warn`
sibling_caution = false  # true: a path beside an allowed path is allowed and reported, not outside
```

`sibling_caution` reads a path in the same directory as an allowed (or widened) path as a
*sibling*: allowed and reported, never denied, and a warning rather than a violation at
verification. It does not apply at the repository root, where one allowed file would make
every top-level name a sibling. A new module beside the touched one is the most
common legitimate edit a plan cannot name, so this trades some of `enforce`'s strictness for
less friction. Measured on 38 commit-derived sessions, it admits 87.9% of a faithful agent's
edits without widening instead of 81.0%, and reports 12 of 114 out-of-boundary drift edits
rather than denying them ([`benchmarks/gate/`](../benchmarks/gate/README.md)).
The secret-path policy and the plan's forbidden rules still win.

A forbidden path is one of three things, in every mode and every session:

- one the secret-path policy matches (`.env`, `.env.*`, `.aws`, `.ssh`, SSH key names,
  key-material extensions such as `.pem`, `.key`, `.p12`), in any letter case;
- one the gate's own policy matches: a dependency directory at any depth (`node_modules/`,
  `third_party/`, `.venv/`) and a file named as generated output (`x_generated.rs`,
  `x.generated.ts`), in any letter case;
- one the plan's forbidden rules match: the directories the index pruned as build output or
  dependencies, `vendor/`, `generated/`, `**/secrets/**`, `**/*Generated*`, `.git/`, `.ok/`. A
  rule written in lower case names a directory and matches any spelling of it, as a
  case-insensitive filesystem would; a rule with capitals is a naming convention and matches
  as written.

`ok gate expand` refuses a forbidden path: a boundary that can be widened to a secret is not a
boundary. What this does not cover, so that it is not read as more than it is: `vendor/` below
the repository root (the word is as often a domain term as a dependency directory, and a rule
that fails legitimate changes is worse than none); a secret copied to a file with an ordinary
name, which is content and not a path; and a change under `.ok/` made outside the hooks, which
is denied at the edit hook but left out of the change at verification, because the gate's own
state lives there.

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
edit was denied or warned during the session, and `PASS` otherwise. In an advisory session
(started from a prompt in `warn` mode, or `ok gate ci` without `--mode enforce`) a changed file
outside the planned boundary is listed under `change.outside_boundary`, makes the verdict
`WARN`, and is not a boundary violation; a forbidden path is one in every session.

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
