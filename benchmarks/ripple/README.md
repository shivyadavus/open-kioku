# Ripple, measured on merged history

`ok ripple` lists the calls that no longer fit a function a change altered, and the code that
still names a declaration it removed ([`docs/ripple.md`](../../docs/ripple.md)). This is what it
did on the last 300 commits of 46 open-source repositories in seven languages, on 2026-11-12,
with **no index**, and what a coding agent did with and without it on five real changes.

Everything here is from [`scripts/ripple-replay.py`](../../scripts/ripple-replay.py) and
[`scripts/ripple-agent-ab.py`](../../scripts/ripple-agent-ab.py); the per-repository numbers
and every commit with a site are in [`results.json`](results.json), and the agent runs in
[`agent-ab.json`](agent-ab.json).

## The two questions

No project code is run and no compiler is consulted, so "broken" is checked against what the
repository's own authors did:

- **Half.** Take a commit that changes a function's parameters or removes a declaration. Apply
  only the file that declares it, on top of the parent, as someone who has made the first edit
  and not yet the rest. Ripple names the sites that no longer fit. *How many of them did the real
  commit go on to change?* Sites in the declaring file itself are left out, and so are
  declarations the commit moved to another file, which applying one file makes look removed.
- **Whole.** Apply the complete commit. Whatever ripple still names is a site the merged commit
  left as it was: *a false alarm, or code the commit really left broken.* Each one was read by
  hand.

## Rounds on repositories it had not seen

The rules were written against eight repositories, then run on new ones in rounds. After each
round the alarms were read, the causes that were general were fixed, and the next round used
repositories not looked at before. This table is each round **as it first came out**, with the
rules as they stood before that round's fixes:

| Round | New repositories | Commits that change code | Sites named from the declaring file | The commit then changed | Complete commits with an alarm | Real | False |
|---|---|---|---|---|---|---|---|
| 1 | 8 | 1,345 | 169 | 131 (78%) | 9 | 0 | 9 |
| 2 | 7 | 971 | 162 | 148 (91%) | 5 | 1 | 4 |
| 3 | 10 | 1,894 | 92 | 88 (96%) | 4 | 1 | 3 |
| 4 | 3 | 549 | 92 | 51 (55%) | 2 | 0 | 2 |
| 5 | 10 | 1,615 | 341 | 333 (98%) | 6 | 2 | 4 |

Round 4 is the three repositories a harness bug dropped from round 2, run one set of rules
later; one commit of one of them, which deleted a vendored copy of a crate in favour of the
registry crate of the same name, is 80 of its 89 alarm sites and 32 of the 41 sites the commit
did not change.

In the half question, a site the commit did not go on to change is either a wrong claim or a
real leftover. In round 5, of the 8 it did not change, 6 were real leftovers (below) and 2 were
wrong; in round 3, all 4 were one real leftover.

So on untouched repositories, with the rules of the day: in the last two full rounds, **3 and 4
complete commits in 1,894 and 1,615 raised a false alarm** (about 1 in 500), and what it named
from a half-made change was right 96% and 98% of the time before counting the leftovers it was
right about.

### What the false alarms were

Each is a rule in [`docs/ripple.md`](../../docs/ripple.md) now, with a test:

| Round | Cause | Rule since |
|---|---|---|
| 1 | Something else of the same name: `headers::Host`, `std::ops::Index`, `issue.create_comment(…)` | A site counts only when an import or a qualifier leads to the declaring module |
| 1 | A JavaScript call with fewer arguments than parameters | JavaScript calls are not held to a count |
| 1 | A name inside a string literal (`r#"…alias (…"#`) | Occurrences inside string literals are mentions |
| 2 | A removed lowercase Go type whose name is also a local variable, a parameter or an imported package | A file that binds or imports the name is not claimed |
| 2 | `v.logger.Info(…)` read as `Type.method` | `Owner.method` needs a capitalised type, and never applies in Go |
| 3 | A free function whose first parameter is called `cls` | `self`/`cls` is a receiver only inside a type |
| 3 | A method replaced by an instance attribute of the same name | Still bound: not a removal |
| 4 | A vendored module deleted in favour of a dependency of the same name | A deleted module's name that a dependency manifest declares is not evidence |
| 4 | A C# attribute under `#if` before the method name | The parameter list is the one after the name |
| 4 | A name inside a single-quoted module path | Single-quoted strings are read in Python, JavaScript and TypeScript |
| 5 | A name in the comment that trails a line of code | Trailing comments are cut before the line is read |
| 5 | A removed constant of a class, and a property of the same name elsewhere | A member, callable or not, is attributed through its type |
| 5 | A method of a type whose name the same file also imports from elsewhere | Not claimed |
| 5 | A preprocessor line behind a byte-order mark | Read as a comment |

### What the real ones were

Four commits on the main branch of a widely used library left code that names something the
commit removed. None was found by tuning: each appeared in the round its repository was first
run in.

| Repository | Commit | What it left |
|---|---|---|
| `colinhacks/zod` | `9782f87c69` (2026-08-28) | `compileFastpass` is declared nowhere; `scripts/compile-fuzz.ts` still imports it and calls it three times. Still so at `19c40d2`, the head when the repository was cloned for this run. |
| `spf13/viper` | `2d525ac880` | The type `ConfigFileNotFoundFromReadError` was renamed in its declaration; three methods and two uses kept the old name. As committed, those five lines name a type that is declared nowhere; the next commit removed them. |
| `gorilla/mux` | `65cc9b5df8` | The type `queryMatcher` was removed; `old_test.go` still declares a field of it and constructs it four times. Gone at the head. |
| `sindresorhus/p-queue` | `68dfe953e5` | `AbortError` is no longer exported from `source/index.ts`; `test/test.ts` still imports it. Fixed later. |

## Every repository, with the rules as shipped

All 46 again with the rules as they are in this release. **This is not a held-out measurement**:
the rules were changed after looking at these repositories, and the last four rule changes
(round 5's) have not been run on repositories they were not written against.

| Set | Repositories | Commits that change code | Breaking changes found | Sites named from the declaring file | The commit then changed | Sites left by complete commits |
|---|---|---|---|---|---|---|
| Written against | 8 | 1,304 | 555 | 80 | 80 | 0 |
| Round 1 | 8 | 1,345 | 397 | 88 | 88 | 0 |
| Rounds 2 and 4 | 10 | 1,553 | 694 | 141 | 139 | 5 in 1 commit |
| Round 3 | 10 | 1,894 | 506 | 92 | 88 | 4 in 1 commit |
| Round 5 | 10 | 1,615 | 942 | 313 | 307 | 6 in 2 commits |
| **All** | **46** | **7,711** | **3,094** | **714** | **702** | **15 in 4 commits** |

The 4 commits are the four real ones above, and the 12 named sites their commits did not go on
to change are theirs too. No false alarm is left on these repositories.

By language:

| Language | Repositories | Commits that change code | Breaking changes found | Sites named | Then changed | Possible only |
|---|---|---|---|---|---|---|
| Python | 10 | 1,439 | 514 | 299 | 299 | 230 |
| Go | 8 | 1,546 | 553 | 176 | 169 | 1,068 |
| Rust | 8 | 1,581 | 747 | 96 | 96 | 18,680 |
| C# | 5 | 769 | 645 | 50 | 50 | 1,031 |
| Java | 5 | 722 | 216 | 46 | 46 | 896 |
| TypeScript | 5 | 974 | 328 | 39 | 34 | 177 |
| JavaScript | 5 | 680 | 91 | 8 | 8 | 31 |

"Possible only" is what the caution costs: sites where the name was used and nothing led to the
declaring module, or the call could not be read. They are listed, never counted and never held
on. Most of Rust's are in one repository, where a removed or changed `new` or `len` has
thousands of namesakes.

## What this does not measure

- **Recall.** Nothing here says how many broken sites ripple did *not* name. A check of that
  needs a compiler or a type checker per repository, and none was run. What it is known to
  miss is in [`docs/ripple.md`](../../docs/ripple.md#what-it-does-not-do): a method on a receiver
  of unknown type, a call through an alias without an index, a name several declarations share,
  a changed type with the same parameter count. In 3,094 breaking changes it named a site for
  227.
- **The index.** Every run used `--no-index`. The `proven` marking is covered by a test and by
  this repository's own CI dossier, and is not measured here.
- **Merged history is not an agent's history.** Authors and CI had already fixed almost
  everything in a merged commit; a change in progress has more left to find. The half question
  is the closest this comes to that.

## With a coding agent

Five real commits from round 3's repositories that change a free function's signature and then
its callers in other files. The checkout is the commit's parent; the prompt states the new
signature and says "update whatever else in this repository has to change with it"; no code is
run. One arm has the hooks installed (`ok gate install claude --apply`, every default), the
other does not. When the agent stops, the harness runs `ok ripple` on the checkout in both arms.
One Claude Code session per cell, frontier model.

| Task | Arm | Sites left broken | Calls now fitting | Turns | Cost (USD) | Time (s) |
|---|---|---|---|---|---|---|
| `psf/black` `get_cache_file(mode)` → `(mode, cache_dir)` | plain | 0 | 7 | 13 | 0.92 | 35.4 |
| | hooks | 0 | 7 | 10 | 0.83 | 23.7 |
| `psf/black` `load_packaged_grammar`, a parameter removed | plain | 0 | 2 | 9 | 0.31 | 20.9 |
| | hooks | 0 | 2 | 10 | 0.34 | 24.0 |
| `urfave/cli` `buildCompletionCommand(appName)` → `(appName, name)` | plain | 0 | 2 | 11 | 0.34 | 23.4 |
| | hooks | 0 | 2 | 9 | 0.35 | 23.8 |
| `stretchr/testify` `truncatingFormat(data)` → `(format, data)` | plain | 0 | 5 | 9 | 0.29 | 21.0 |
| | hooks | 0 | 5 | 7 | 0.23 | 15.3 |
| `colinhacks/zod` `_properties`, a parameter removed | plain | 0 | 0 | 15 | 0.83 | 46.5 |
| | hooks | 0 | 0 | 16 | 0.73 | 48.3 |

| Arm | Sites left broken | Turns (mean) | Cost (mean) | Time (mean) |
|---|---|---|---|---|
| plain | 0 of 5 tasks | 11.4 | 0.54 | 29.4 s |
| hooks | 0 of 5 tasks | 10.4 | 0.50 | 27.0 s |

**The hook changed nothing about the outcome.** Asked outright to update the callers, in
repositories of this size, a frontier model found every one with `grep`, with or without being
told. In the hooks arm the notice reached the agent after its first edit in all five sessions,
with the sites listed, and no stop was held because nothing was left to hold on. Turns, cost
and time are within the noise of one run per cell; the hook did not cost any.

What it does show: the notice is delivered and is correct in a live session, in three
languages, on repositories the rules were not tuned on; and it costs nothing when the agent
needs no help. Whether it helps an agent that was *not* asked to look, a weaker model, or a
repository too large to `grep` by eye is not measured here. Ten runs, USD 5.16.

## Reproduce

```sh
cargo build --release -p open-kioku-cli
git clone --depth 450 https://github.com/colinhacks/zod ../scratch/zod    # a scratch clone: it is checked out repeatedly
scripts/ripple-replay.py --repo ../scratch/zod --ok-bin target/release/ok --commits 300 --out zod.json

mkdir -p ../ok-bin && cp target/release/ok ../ok-bin/ok
scripts/ripple-agent-ab.py --repos ../scratch --work ../ab/work --out ../ab/out \
  --ok-bin ../ok-bin/ok --tasks benchmarks/ripple/agent-ab-tasks.json
```

A replay of 300 commits takes one to five minutes per repository. Each agent run is a real
Claude Code session and is billed as one; `--budget` caps a single run.
