# Ripple: what a change breaks outside itself

A diff shows what changed. It cannot show the caller in another file that still passes two
arguments to a function that now requires three, or the import of a function the change renamed.
`ok ripple` lists those sites.

```console
$ ok ripple
Ripple: 2 site(s) no longer fit 1 changed declaration(s).

src/click/utils.py:62  function `_make_default_short_help`: signature changed
  - def _make_default_short_help(help: str, max_length: int = 45)
  + def _make_default_short_help(help: str, max_length: int, strict: bool)
  broken: 2 site(s)
    src/click/core.py:1275 (in Command.get_short_help_str)  text = _make_default_short_help(self.help, limit)
        does not pass `strict`, which is required now [by name]
    tests/test_utils/test_make_default_short_help.py:62 (in test_make_default_short_help)  out = click.utils._make_default_short_help(value, max_length)
        does not pass `strict`, which is required now [by name]
```

It needs **no index and no setup**: it reads the working tree and git. That run took 0.14
seconds on a repository `ok` had never seen. Where an index exists it is read for one thing, the
call edges it proved, and a site one of them reaches is marked `proven`.

`ok ripple` is experimental in 5.0.0 ([`STABILITY.md`](../STABILITY.md)): its output and the
`[gate] ripple` setting may change in a minor release.

## Three places it runs

| Where | What happens |
|---|---|
| `ok ripple` | Compares the working tree with `HEAD`, or with `--base <rev>` (a branch, a merge base). `--json` for the report, `--file <path>` to examine one file's declarations, `--fail-on-broken` to exit 1 when a site is broken, `--no-index` to ignore an index. Writes nothing. |
| The Claude Code hooks | After an `Edit` or `Write`, the agent is told which sites the edit just broke, once per set of sites. When it stops, the change is checked again and the agent is **held once** on the sites still broken, with each one's path, line and reason; a second stop with the same sites passes and tells the user. No session, plan, index or configuration is involved: `ok gate install claude --apply`, or the plugin, is the setup. |
| `ok gate ci` | The Change Dossier of a pull request carries a *Ripple* section: the broken sites by path and line with the reason, and no source text. A broken site makes a passing verdict `WARN`. |

```toml
[gate]
ripple = "hold"   # hold | report | off
```

`hold` (the default) is the behaviour above. `report` tells the agent and the user and never
holds a stop. `off` does not look. In `observe` mode nothing holds.

A CI step that should fail on a broken site, with no plan or dossier involved:

```sh
ok ripple --base origin/main --fail-on-broken
```

## What it reports

**Changed declarations.** The files of the change are parsed as they were at the base revision
and as they are now (tree-sitter: Rust, Python, TypeScript, JavaScript, Go, Java, C#). A
declaration is reported when its signature changed or it is gone:

| Effect | Meaning | Its sites |
|---|---|---|
| `breaking` | Parameters were added without a default, removed, reordered or (in Python) renamed; or the declaration was removed or renamed. | checked |
| `compatible` | Only parameters with a default were added, at the end or keyword-only. Every existing call still fits. | not searched |
| `types_only` | Same parameters in number and order (and name, where calls can name them); only types or the return type changed. | not searched: what a call passes cannot be read from its text |

A function that became a variable of the same name, a method that became an instance attribute,
a declaration the same file still makes under another kind, and a member of a type that is
itself gone (reported once, as the type) are not removals.

**Sites.** Each line of the working tree that holds the declaration's name as a word (`git
grep`, tracked and untracked, not ignored) is read:

- A call of a function whose signature changed has its arguments counted against the new
  parameter list. One that still fits is **not reported**, whether or not the change touched it;
  one that does not is `broken` with the reason (`passes 2 arguments; 3 are required now`,
  ``does not pass `strict`, which is required now``, ``passes `shorten=`, which is not a
  parameter now``).
- Code that still names a removed declaration (a call, an import, a use of a removed type) is
  `broken`. The bare name of a removed function with no call is not counted: it is as likely a
  field or a variable of that name.
- Comments (a comment that trails a line of code included), string literals, Python
  docstrings, prose and configuration that name a removed declaration are counted as *mentions*
  and listed apart.

What "fits" means per language:

| Language | A call is held to |
|---|---|
| Python | positional count, keyword names, required parameters, keyword-only and positional-only markers; `*args`/`**kwargs` in the declaration accept the rest |
| TypeScript | the number of required and of accepted parameters (`?`, defaults and rest parameters read) |
| Rust, Go, Java, C# | the parameter count (variadics and C# defaults read); a `self` receiver is the call's receiver, or its first argument in a path call |
| JavaScript | nothing: a missing argument is `undefined` and a surplus one is dropped, so only removals and renames are reported |

A call that unpacks an argument list (`*args`, `...items`), or whose arguments cannot be read,
is listed as `possible` and not counted.

## How a site is attributed

A name is not a reference. A site counts as `broken` only when something leads from it to the
declaring module, and is `possible` (listed, not counted, never held on) otherwise:

- **`proven`**: an index exists and holds a structurally proven call or use edge from this line
  to the declaration.
- **`by name`**: no other declaration in the repository has the name, the file has no variable,
  parameter or imported package of that name, and one of: the site is in the declaring file; the
  file imports the name from the declaring module or by a relative path, or imports everything
  the declaring module has; the name is qualified by the declaring module (`utils.shorten`,
  `clearing::owed`, `crate::`, `super::`); the file is in the same Go package or Java package
  directory; the language is C#, where a namespace is opened whole.
- A **member of a type** (a method, or a constant declared inside it) is attributed when it is
  reached through its own type (`Owner.method`, `Owner::method`, for a type written with a
  capital, outside Go) or on the instance inside that type (`self.method`, `this.method`). On
  any other receiver the type is unknown, and a bare name outside Rust may be inherited or
  declared in another part of the type: `possible`. So is every site when the declaring file
  also imports its owner's name from elsewhere.
- A module that was deleted and whose name a dependency manifest declares (a vendored copy
  given up for the package) is not evidence: an import by that name resolves to the dependency.
- A constructor (`__init__`, `constructor`) is checked at the calls of its type's name.

## What it does not do

- It does not type-check. A changed parameter type, a changed return type, a struct field, and a
  behaviour change with the same signature are outside it.
- It does not follow a call through a receiver of unknown type, a function passed as a value, an
  alias (without an index), reflection or a string. Those are `possible` or not seen.
- A name several declarations share is not attributed without an index.
- A move to another file is not a rename: the old name's sites are listed as `possible` with a
  note that it may have moved.
- At most 60 changed declarations are searched for per run and 12 sites listed per declaration;
  the rest are counted.
- At a stop without a gate session, the working tree is compared with `HEAD`: a change the
  conversation already committed is not looked at again. `ok gate ci` and `ok ripple --base`
  cover committed work.

In a compiled language the compiler finds every one of these and more. Ripple finds them before
a build is run, in the languages that have no compiler, and for whoever reads the pull request.

## Measured

Replayed over the last 300 commits of 46 open-source repositories in seven languages, with no
index, in two ways: on each **complete merged commit**, where anything it reports is a false
alarm or a real leftover; and on each commit **with only the declaring file applied**, where what
it reports can be checked against what the real commit went on to change. The rules were run on
new repositories in five rounds and changed after each.

- In the last round (10 repositories it had not been run on, 1,615 commits that change code):
  from the declaring file alone it named 341 sites, of which the commit then changed 333 and 6
  more were real leftovers; the complete commits raised an alarm in 6, 2 real and 4 false. In
  the round before (1,894 commits): 88 of 92, and 4 commits, 1 real and 3 false.
- Each false alarm became a rule above, with a test. With the rules as shipped, all 46
  repositories again (7,711 commits that change code): 714 sites named, 702 then changed, and
  the other 12 belong to the four commits that really left something broken. That figure is not
  held out: the rules were changed after looking at these repositories.
- Four commits on the main branch of a widely used library still named something they had
  removed; one was still in place when its repository was cloned.
- **Recall is not measured**: nothing here says how many broken sites it did not name.
- On five real signature changes, a frontier model asked to update the callers left nothing
  broken with or without the hook, so the hook's effect on that outcome is not shown. It was
  delivered in all five sessions and cost no time.

Method, every number, the false alarms by cause and the limits are in
[`benchmarks/ripple/`](../benchmarks/ripple/README.md).
