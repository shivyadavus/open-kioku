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
rules as they stood before that round's fixes. All five rounds used the first form of the
check, which read a call from its text; [the review below](#an-independent-review) replaced it.

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

Each is a rule in [`docs/ripple.md`](../../docs/ripple.md) now, with a test. Those about
strings and comments are no longer rules of their own: the check reads identifier nodes of the
syntax tree, and a string or a comment has none.

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

## An independent review

Before it was merged, the change was handed to a reviewer that had no part in writing it and
was asked to break it. It built more than two dozen small repositories of ordinary code and
reported 18 findings. Its verdict: **not safe to hold an agent's stop on by default.**

| What it found | Examples | What changed |
|---|---|---|
| A call's arguments were counted from its text | `Grow(1<<10, 2)`, an f-string with a comma, a comment inside the call, a bracket inside a quoted string, a string longer than the line limit | Arguments are counted from the argument nodes of the syntax tree |
| A name in the wrong place was read as a use | a method declaration with the function's name, the name inside a multi-line string or comment, an import that rebinds it (`from tenacity import retry`), an export list that keeps it | A use is an identifier node; a name the file still binds is not a removal |
| A site was attributed by how its receiver is spelled | `http.Get` with `net/http` imported and a local package `http`; `cache.lookup()` where `cache = redis.Redis()` | A qualifier counts only when an import binds it to the declaring module; in Go, by the module path in `go.mod` |
| A call that never fit was reported as newly broken | a decorated function (`@click.command()`), a C# extension method called on its class, `super().get(id)` in an override whose signature changed | A call is counted only when it fit the parameter list before the change and does not fit it after |
| The hold was not scoped to the conversation | a stop held on a colleague's uncommitted change; on an unreadable `ok.toml`; in `observe` with no session; forever, when the record of the conversation could not be written | The hooks look only at files the conversation edited; every failure reports and none holds; **`report` is the default and `hold` is opt-in** |
| An index that could not be read turned the check off or failed the run | an index built by an earlier version | The same as no index, and the report says so |
| Cost grew with the size of the change | 2,000 changed files: 9.4 s; one name on thousands of lines: 18.9 s | 300 changed files and ten seconds per run, each file parsed once: 1.8 s and 0.49 s on the same two cases |

All of its reproductions now report nothing broken, except the one that has two really broken
calls, which reports those two.

The rewritten check was then replayed over the 46 repositories. Its first run raised a false
alarm on six complete commits, with these causes, each fixed with a test before the run below:

| Cause | Rule since |
|---|---|
| A method moved to another file of its C# partial class | A removed method that its type still declares elsewhere has moved: `possible` |
| A removed nested type whose name another declaration, or a package opened by a project-wide `using`, still provides | A removed type whose name is declared elsewhere: `possible` |
| The ten-second budget ran out before the file that now declares a moved type was read | With a file that holds the name unread, nothing about that name is counted |
| `*slog.Logger` read as a local type `Logger` | A qualified type name is reached through its qualifier, like a call |
| A function that became `export const name = create(...)` in another module | A name another module binds at its top level may have moved there: `possible` |
| The compilation symbol on a C# `#if` line | Preprocessor lines are not read |

## Every repository, with the check as shipped

All 46 again with the check as it is in this release. **This is not a held-out measurement**:
the rules were changed after looking at these repositories, and neither the rewrite nor the six
rules above have been run on repositories they were not written against. That, and the review's
verdict, is why the hooks report and do not hold unless asked to.

| Set | Repositories | Commits that change code | Breaking changes found | Sites named from the declaring file | The commit then changed | Sites left by complete commits |
|---|---|---|---|---|---|---|
| Written against | 8 | 1,304 | 525 | 88 | 88 | 0 |
| Round 1 | 8 | 1,345 | 366 | 117 | 117 | 0 |
| Rounds 2 and 4 | 10 | 1,553 | 665 | 199 | 197 | 5 in 1 commit |
| Round 3 | 10 | 1,894 | 487 | 160 | 151 | 4 in 1 commit |
| Round 5 | 10 | 1,615 | 903 | 349 | 343 | 6 in 2 commits |
| **All** | **46** | **7,711** | **2,946** | **913** | **896** | **15 in 4 commits** |

The 4 commits are the four real ones above. Of the 17 named sites their commits did not go on
to change, 12 are those leftovers. The other 5 are in `tokio-rs/bytes` `43ac8e5494`, which
removed `Bytes::with_capacity` and put the tests that call it inside a block comment instead of
changing them: named from the half-made change, and rightly not named on the complete commit.
No false alarm is left on these repositories.

Before the review, the same 46 gave 714 sites named and 702 then changed, with the same four
commits. The syntax-tree reader names more (it follows `Type::method` and the qualifiers an
import binds) and counts fewer changes as breaking (a name that is still bound is no longer a
removal).

By language:

| Language | Repositories | Commits that change code | Breaking changes found | Sites named | Then changed | Possible only |
|---|---|---|---|---|---|---|
| Python | 10 | 1,439 | 490 | 407 | 407 | 3,951 |
| Go | 8 | 1,546 | 531 | 156 | 149 | 2,483 |
| Rust | 8 | 1,581 | 726 | 147 | 142 | 21,934 |
| C# | 5 | 769 | 616 | 85 | 85 | 1,483 |
| Java | 5 | 722 | 201 | 64 | 64 | 2,389 |
| TypeScript | 5 | 974 | 305 | 52 | 47 | 407 |
| JavaScript | 5 | 680 | 77 | 2 | 2 | 4,104 |

"Possible only" is what the caution costs: sites where the name was used and nothing led to the
declaring module, or the call could not be read. They are listed, never counted and never held
on. Most of Rust's are in one repository, where a removed or changed `new` or `len` has
thousands of namesakes. JavaScript calls are not held to an argument count, so only removals
and renames can be named there.

## What this does not measure

- **Recall.** Nothing here says how many broken sites ripple did *not* name. A check of that
  needs a compiler or a type checker per repository, and none was run. What it is known to
  miss is in [`docs/ripple.md`](../../docs/ripple.md#what-it-does-not-do): a method on a receiver
  of unknown type, a call through an alias without an index, a name several declarations share,
  a changed type with the same parameter count, a declaration that moved. In 2,946 breaking
  changes it named a site for 268.
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
One Claude Code session per cell, frontier model. These runs were made with the first form of
the check, before the review, when the default was still to hold a stop; no stop was held in
any of them, and they have not been repeated with the rewritten check.

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

What it does show: the notice is delivered and was correct in a live session, in three
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
