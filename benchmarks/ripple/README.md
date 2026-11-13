# Ripple, measured on merged history

`ok ripple` lists the calls that no longer fit a function a change altered, and the code that
still names a declaration it removed ([`docs/ripple.md`](../../docs/ripple.md)). This is what it
did on the last 300 commits of 46 open-source repositories in seven languages, with **no
index**; on 14 more it had never been run on; against three type checkers on 835 signature
changes; and what a coding agent did with and without it on five real changes.

The order below is the order it happened in, because each measurement changed the check: five
rounds on merged history, an independent review, a held-out replay, a comparison with type
checkers that showed how much it missed, and the replays again once it missed less.

Everything here is from [`scripts/ripple-replay.py`](../../scripts/ripple-replay.py) and
[`scripts/ripple-agent-ab.py`](../../scripts/ripple-agent-ab.py); the per-repository numbers
and every commit with a site are in [`results.json`](results.json) and
[`held-out.json`](held-out.json), the comparison with type checkers
([`scripts/ripple-recall.py`](../../scripts/ripple-recall.py)) in [`recall.json`](recall.json),
and the agent runs in [`agent-ab.json`](agent-ab.json) and
[`agent-ab-not-told.json`](agent-ab-not-told.json).

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

## Fourteen repositories it had never been run on

After the review, the check as it then stood (the one merged first) was run on 14 repositories
that no rule had been written against: two each in Python, Go, Rust, TypeScript, JavaScript,
Java and C# (`encode/starlette`, `Textualize/rich`, `gofiber/fiber`, `go-playground/validator`,
`tokio-rs/tracing`, `rust-lang/regex`, `honojs/hono`, `sindresorhus/got`, `fastify/fastify`,
`koajs/koa`, `apache/commons-collections`, `apache/commons-text`, `AutoMapper/AutoMapper`,
`FluentValidation/FluentValidation`).

| | Commits that change code | Breaking changes found | Sites named from the declaring file | The commit then changed | Complete commits with an alarm |
|---|---|---|---|---|---|
| The check as merged first, first look | 2,543 | 981 | 250 | 249 | 1, false |
| The check as shipped | 2,543 | 1,116 | 275 | 275 | 1, real |

The false one: a Java static method moved to the superclass, still reached through the
subclass's name (`apache/commons-collections` `167b284e44`). It is a rule now.

The real one, found by the check as shipped: `go-playground/validator` `3ce20afbd4` deletes
`func skipUnless` and leaves `"skip_unless": skipUnless,` in the table of validators. Nothing
declares the name in that commit.

The second row is **no longer held out**: between the two rows the check was changed (below),
and four of its rules were written after reading this replay.

## Against type checkers: what it missed

Every number above is about what ripple names. None says what it does not name. To measure
that, [`scripts/ripple-recall.py`](../../scripts/ripple-recall.py) changes one function at a
time at a repository's head, in one of two ways (a required parameter put first; the
declaration renamed and nothing else touched), runs the language's own type checker over the
whole repository, tests included, and takes the checker's new errors as the sites the change
broke. Ripple is run on the same working tree with no index. No project code is run.

The checkers: Go's type checker over every package of the module
([`ripple-recall-gocheck.go`](../../scripts/ripple-recall-gocheck.go); `go build` stops at the
first package that fails), pyright in basic mode, and `tsc --strict`, each with its own
configuration so a repository's cannot narrow it. A change the checker rejects at the
declaration itself (an override or an overload that no longer agrees) is left out.

835 changes in 15 repositories (30 of each kind per repository, drawn with a
fixed seed from the functions of non-test code whose name stands on some other line; 65
ill-formed ones left out). The checkers reported 3,899 broken sites.

| Language | Repositories | Changes | Sites the checker reports | Counted, as merged first | Counted, as shipped | Counted or listed, as shipped |
|---|---|---|---|---|---|---|
| Go | 6 | 359 | 1,621 | 343 (21%) | 1,265 (78%) | 1,593 (98%) |
| Python | 4 | 236 | 774 | 218 (28%) | 288 (37%) | 629 (81%) |
| TypeScript | 5 | 240 | 1,504 | 127 (8%) | 513 (34%) | 1,397 (93%) |
| **All** | **15** | **835** | **3,899** | **688 (18%)** | **2,066 (53%)** | **3,619 (93%)** |

**The check as merged first counted about one broken site in five.** It followed a member call
only on `self`/`this` or on its type's own name, so `cmd.Run()` and `r := NewRouter();
r.Get(..)` were listed as possible and not counted, and it did not treat `const f = (..) =>
..` as a function at all: it saw 608 of the 835 changes, and sees
777 now.

What changed, each from reading what the checker reported and ripple did not:

- A member call is attributed through what the function around it says its receiver is: a
  typed parameter, a Go method's own receiver, a typed local, a value built by a constructor
  or a typed literal, and in Go and Rust the result of a function or method whose declaration
  says what it returns, to six steps.
- A name is what the nearest function that binds it says: an HTTP handler closure's `r` no
  longer hides the router `r` around it.
- A JavaScript or TypeScript `const` that holds a function is a function.
- A Go method's owner is its receiver type; Go's `f(a, rest...)` is held to the parameters
  before the variadic one; a renamed function passed by name is a site.

By repository. Those marked `*` were not looked at while these rules were written; the others
were, so their figures are how well the rules fit what they were written against:

| Repository | Language | Changes | Sites the checker reports | Counted, as merged first | Counted, as shipped | Counted or listed, as shipped |
|---|---|---|---|---|---|---|
| `go-chi/chi` | Go | 60 | 392 | 32 (8%) | 367 (94%) | 385 (98%) |
| `spf13/cobra` | Go | 60 | 381 | 56 (15%) | 333 (87%) | 381 (100%) |
| `gofiber/fiber` * | Go | 60 | 311 | 131 (42%) | 205 (66%) | 297 (95%) |
| `gin-gonic/gin` | Go | 59 | 212 | 81 (38%) | 161 (76%) | 212 (100%) |
| `gorilla/mux` | Go | 60 | 232 | 21 (9%) | 114 (49%) | 229 (99%) |
| `go-playground/validator` * | Go | 60 | 93 | 22 (24%) | 85 (91%) | 89 (96%) |
| `pallets/click` | Python | 57 | 184 | 83 (45%) | 89 (48%) | 115 (62%) |
| `encode/httpx` * | Python | 60 | 226 | 48 (21%) | 58 (26%) | 201 (89%) |
| `Textualize/rich` * | Python | 59 | 171 | 37 (22%) | 46 (27%) | 134 (78%) |
| `encode/starlette` * | Python | 60 | 193 | 50 (26%) | 95 (49%) | 179 (93%) |
| `sindresorhus/got` * | TypeScript | 54 | 131 | 15 (11%) | 67 (51%) | 110 (84%) |
| `honojs/hono` * | TypeScript | 51 | 235 | 2 (1%) | 60 (26%) | 166 (71%) |
| `sindresorhus/ky` | TypeScript | 55 | 178 | 21 (12%) | 59 (33%) | 170 (96%) |
| `sindresorhus/p-queue` | TypeScript | 36 | 548 | 0 (0%) | 236 (43%) | 545 (99%) |
| `colinhacks/zod` * | TypeScript | 44 | 412 | 89 (22%) | 91 (22%) | 406 (99%) |

On the 8 marked repositories together: 22% counted before,
40% as shipped, 89% counted or listed. On the 7 others:
14% and 64%. By kind of change: an added parameter, 21% to
60%; a rename, 14% to 47%.

**What it still does not count**, by reading the sites it lists as possible or not at all:

- Python: most of them. A parameter with no annotation, the result of a function with no
  return annotation (`progress = click.progressbar(..)`), a field (`ctx.command.get_params()`).
  There is no type to read, and ripple does not infer one.
- A member renamed in a class that has a base, outside Go: the base may be a framework's and
  may declare it, so every site is possible (`queue.add(..)` on a class that extends an event
  emitter).
- A field's type (`c.parent.Run()`, `self.client.send()`), a type that embeds or extends the
  owner, a loop variable, a destructured value.
- A type that stops implementing an interface because one method changed: the checker reports
  that where the value is used as the interface, on lines that do not name the method.

**Whether what it counts is right.** It counted 2,355 sites. The checker reported
2,066 of them as sites and 11 as another error on the same call.
199 are uses in a file where the checker reported the name once, at the import, and
then fell silent on every use of it. 72 are in directories Go's `./...` does not read
(`_examples/`). The remaining 7 were read by hand: five are broken as named (a Windows-only Go file that the checker, run on Linux, does not
compile; an import and two uses through the package's name that pyright does not follow; a
call inside a nested function), and two take a renamed name through the package's own entry
point, which re-exports it: they are broken for as long as that re-export is, and ripple
counts the re-export too.

The comparison's first complete run found fourteen that were wrong, all in one repository that
keeps two copies of a module: the callers of one copy were attributed to the other, because a
relative import was taken as leading to the changed declaration wherever it led. The import is
followed now. That run also showed the TypeScript checker blind to that repository (its
imports carry no extension); the figures here are from the rerun with both corrected.

Limits: two kinds of change, three languages, functions that have a caller somewhere, and
checkers with no installed dependencies, which see a call into a library as untyped.

## Every repository, with the check as shipped

All 46 again, and the 14 above, with the check as it is in this release. **This is not a
held-out measurement**: the rules were changed after looking at these repositories. That, and
the review's verdict, is why the hooks report and do not hold unless asked to.

| Set | Repositories | Commits that change code | Breaking changes found | Sites named from the declaring file | The commit then changed | Sites left by complete commits |
|---|---|---|---|---|---|---|
| Written against | 8 | 1,304 | 589 | 114 | 114 | 0 |
| Round 1 | 8 | 1,345 | 464 | 118 | 118 | 0 |
| Rounds 2 and 4 | 10 | 1,553 | 705 | 223 | 221 | 5 in 1 commit |
| Round 3 | 10 | 1,894 | 1,249 | 510 | 506 | 4 in 1 commit |
| Round 5 | 10 | 1,615 | 964 | 446 | 439 | 6 in 2 commits |
| **All 46** | **46** | **7,711** | **3,971** | **1,411** | **1,398** | **15 in 4 commits** |
| The 14 above | 14 | 2,543 | 1,116 | 275 | 275 | 1 in 1 commit |

The 4 commits are the four real ones above, and the 1 is the fifth. Of the 13 named sites the
46 repositories' commits did not go on to change, 12 are those leftovers. The other one is in
`labstack/echo` `f071367e3c`, which moved the method to another file in the same commit: named
from the half-made change, and rightly not named on the complete commit. No false alarm is
left on these 60 repositories.

How the same 46 read at each stage:

| The check | Breaking changes found | Sites named from the declaring file | The commit then changed | False alarms on complete commits |
|---|---|---|---|---|
| Before the review (calls read as text) | 3,094 | 714 | 702 | 0 |
| As merged first (calls read from the syntax tree) | 2,946 | 913 | 896 | 0 |
| With receivers followed, first run | 4,091 | 1,441 | 1,409 | 8 commits |
| As shipped | 3,971 | 1,411 | 1,398 | 0 |

Following receivers named half as many sites again, and its first run on these repositories
raised a false alarm on eight complete commits (and three on the 14). Each cause is a rule now,
with a case in one test:

| Cause | Rule since |
|---|---|
| A C# 12 primary constructor in place of a written one | A constructor's name outside a call is the type's |
| `{authorization: token}`, `Command{Args: nil}`, `Pack { count: a }` with a removed function of that name | The key of an object or struct literal is a property or a field |
| `fetch: (request, env?) => Response = (request, env) => ..` | An annotated arrow takes its parameters, and which may be left out, from the annotation |
| 61 uses of a renamed function behind `export {name} from './lib.js'` | The re-export is the site; what takes the name through it is `possible` |
| A Go method moved to another file of its type | A method its type still declares has moved |
| `const x = require('x')` rewritten as `const {default: x} = require('x')` | A `require` brings a name in and declares nothing; a destructured one binds it |
| A removed `const name` in a file that can still say `name` | A browser or Node global's name resolves without a declaration |
| A function declared again inside a Rust macro body | A macro's body is tokens: what a name is there is not read |
| `q.IsMatch(a, b)` where a C# base class still has the two-argument overload | In Java and C#, a changed member of a type with a base may be shadowing an overload |

By language, the 46 as shipped:

| Language | Repositories | Commits that change code | Breaking changes found | Sites named | Then changed | Possible only |
|---|---|---|---|---|---|---|
| Python | 10 | 1,439 | 514 | 421 | 421 | 4,104 |
| JavaScript | 5 | 680 | 852 | 364 | 364 | 13,708 |
| Go | 8 | 1,546 | 487 | 207 | 199 | 1,396 |
| C# | 5 | 769 | 638 | 141 | 141 | 1,911 |
| Rust | 8 | 1,581 | 765 | 131 | 131 | 23,702 |
| TypeScript | 5 | 974 | 503 | 83 | 78 | 20,156 |
| Java | 5 | 722 | 212 | 64 | 64 | 2,618 |

"Possible only" is what the caution costs: sites where the name was used and nothing led to the
declaring module or to the owner's type, or the call could not be read. They are listed, never
counted and never held on. Most of Rust's and TypeScript's are in one repository each, where a
removed or changed `new`, `len` or `parse` has thousands of namesakes. JavaScript calls are not
held to an argument count, so there only removals and renames can be named; its count rose
because a `const` that holds a function is a function now.

## What this does not measure

- **What it misses in Rust, Java, C# and JavaScript.** The comparison with type checkers
  covers Go, Python and TypeScript. Rust, Java and C# have compilers that were not set up for
  it, and JavaScript has no checker to compare with. What it is known to miss everywhere is in
  [`docs/ripple.md`](../../docs/ripple.md#what-it-does-not-do).
- **Other kinds of change.** The comparison adds a parameter or renames a function. A removed
  parameter, a reordered one, a changed type and a moved declaration are not in it.
- **On merged history, how much was there to find.** In 3,971 breaking changes it named a site
  for 388; most of the rest have no caller to break, and how many have one it did not see is
  what the comparison estimates, not this.
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

### Not told to update the callers, and a smaller model

The five tasks again, with a prompt that states the signature change and says nothing about
callers, on two models, in three arms: no hooks; the hooks as installed (they tell, and do not
hold); and the hooks with `[gate] ripple = "hold"`. Each finished checkout was judged by
`ok ripple` and by the language's type checker. 30 sessions, USD 6.81.

| Model | Arm | Tasks with a broken call left | Broken calls left | Turns (mean) | Cost (mean, USD) | Time (mean) |
|---|---|---|---|---|---|---|
| The CLI's default (frontier) | no hooks | 0 of 5 | 0 | 10.4 | 0.49 | 24.4 s |
| | hooks | 0 of 5 | 0 | 9.8 | 0.49 | 24.6 s |
| | hooks, hold | 0 of 5 | 0 | 9.4 | 0.37 | 22.7 s |
| `claude-haiku-5-5` | no hooks | 1 of 5 | 6 | 7.6 | 0.007 | 14.0 s |
| | hooks | 2 of 5 | 8 | 9.2 | 0.009 | 22.1 s |
| | hooks, hold | 2 of 5 | 8 | 7.4 | 0.006 | 20.2 s |

**The hooks did not change the outcome here either.** The frontier model updated every caller
without being asked, in every arm. The smaller model sometimes changed only the file the prompt
named, and which tasks it did that on varied from run to run, not with the arm. What the
transcripts show:

- Every time the smaller model left callers broken, its own answer said so and listed them,
  with or without the hooks.
- The notice reached the agent after its edit in every session with hooks in which it changed
  the signature (in one the smaller model did not), with the right sites.
- A held stop was answered with an explanation, not an edit: the task named one file, and the
  agent said that choosing the new argument at each test call was the user's decision. That is
  what the hold asks for ("update it, or say why").
- `ok ripple` and the type checker agreed on whether a call was left broken in all 30
  checkouts. In three, pyright also objected to how a test the agent rewrote reaches a
  submodule, which is not a call that no longer fits.

So on small repositories, with a task that is the signature change itself, current models do
not need this. Whether it helps when the signature change is a side effect of a larger task,
or in a repository too large to search by eye, is not measured.

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
Claude Code session and is billed as one; `--budget` caps a single run, `--model` picks the
model, `--not-told` leaves the callers out of the prompt, and `--arms plain hooks hold` picks
the arms.

The comparison with a type checker, for one repository (Go shown; `--language python` needs
`pyright` and `--language typescript` needs `tsc` on the `PATH`):

```sh
mkdir ../gocheck && cp scripts/ripple-recall-gocheck.go ../gocheck/main.go
(cd ../gocheck && go mod init gocheck && go get golang.org/x/tools/go/packages && go build -o gocheck .)
scripts/ripple-recall.py --repo ../scratch/mux --language go --ok-bin target/release/ok \
  --go-check ../gocheck/gocheck --changes 30 --out mux-recall.json
```
