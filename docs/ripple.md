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
`[gate] ripple` setting may change in a minor release. It reads syntax, not types, so what it
names is a site to check, not a verdict.

## Three places it runs

| Where | What happens |
|---|---|
| `ok ripple` | Compares the working tree with `HEAD`, or with `--base <rev>` (a branch, a merge base). `--json` for the report, `--file <path>` to examine one file's declarations, `--no-index` to ignore an index, `--sites <n>` to list more than twelve sites per declaration. `--fail-on-broken` exits 1 when a site is broken; a run that could not be made exits 2. Writes nothing. |
| The Claude Code hooks | After an `Edit` or `Write`, the agent is told which sites that edit broke, with each one's path, line and reason, once per set of sites. When it stops, the declarations of the files **this conversation edited** are checked again and the user is told what is still broken. No session, plan, index or configuration is involved: `ok gate install claude --apply`, or the plugin, is the setup. |
| `ok gate ci` | The Change Dossier of a pull request carries a *Ripple* section: the broken sites by path and line with the reason, and no source text. A broken site makes a passing verdict `WARN`. |

In the `[gate]` section of `ok.toml`:

```toml
ripple = "report"   # report | hold | off
```

`report`, the default, is the behaviour above: it tells, and never keeps an agent from
finishing. `hold` also holds a stop, once per set of broken sites and at most three times in a
conversation; a second stop with the same sites passes. `off` does not look. Nothing is held in
`observe` mode, on a stop that is already the answer to a stop hook, for a harness that names no
conversation, when the hooks' record of the conversation cannot be written, or when `ok.toml`
cannot be read: in each of those the sites are reported.

The hold is an opt-in because the check reads syntax, not types. An
[independent review](../benchmarks/ripple/README.md#an-independent-review) of its first form
built more than two dozen small repositories in which ordinary code made it claim a site that
was not broken; each became a rule here, and the default became `report`.

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

A JavaScript or TypeScript `const` that holds a function (`const run = (a, b) => ...`,
`const run = function (a) {}`, a class's `run = (a) => ...`) is a function with those
parameters, and a module's other `const` is a constant whose removal is reported.

A name the file still binds is not a removal: a function that became a variable of the same
name, a method that became an instance attribute, a declaration replaced by an import of the
same name (`from tenacity import retry`) or kept alive by an export list
(`export { fetchEvery as fetchAll }`), and a declaration the file still makes as another kind of
thing at the same level (a field of some type does not keep a function's name bound). A member of a type that is itself gone is reported once, as the type.

**Sites.** The lines of the working tree that hold the declaration's name are found with `git
grep` (tracked and untracked, not ignored), and each file with one is read through its syntax
tree. A use is an identifier node, so a name in a comment, a string, a docstring or another
declaration is never a call:

- A call of a function whose signature changed has its arguments counted from the call's
  argument nodes against the parameter list **as it was and as it is**. It is `broken` only when
  it fit before and does not fit now, with the reason (`passes 2 arguments; 3 are required now`,
  ``does not pass `strict`, which is required now``, ``passes `shorten=`, which is not a
  parameter now``). A call that fits now is **not reported**, whether or not the change touched
  it. A call that did not fit before either is `possible`: it is not a call of this declaration
  (a decorator such as `@click.command()` rewrote it), or it could not be read.
- Code that still names a removed declaration (a call, an import, a use of a removed type) is
  `broken`. The bare name of a removed function with no call (`map(pick)`, `Args: OnlyValidArgs`)
  is counted only where it is attributed, and otherwise left out: it is as likely a field or a
  variable of that name.
- Lines the search found with no identifier of the name on them, and prose and configuration
  that name a removed declaration, are counted as *mentions* and listed apart.

What "fits" means per language:

| Language | A call is held to |
|---|---|
| Python | positional count, keyword names, required parameters, keyword-only and positional-only markers; `*args`/`**kwargs` in the declaration accept the rest |
| TypeScript | the number of required and of accepted parameters (`?`, defaults and rest parameters read) |
| Rust, Go, Java, C# | the parameter count (variadics and C# defaults read). A Rust path call (`Type::method(value)`) and a C# extension method called on its class (`Ext.Trim(value)`) pass the receiver as the first argument. Go's `f(a, rest...)` unpacks into the variadic parameter itself, so the arguments before it are held to the parameters before it |
| JavaScript | nothing: a missing argument is `undefined` and a surplus one is dropped, so only removals and renames are reported |

A call that unpacks an argument list (`*args`, `...items`; not Go's), or that sits in a part of
the file the parser could not read, is `possible` and not counted.

## How a site is attributed

A name is not a reference. A site counts as `broken` only when something leads from it to the
declaring module, and is `possible` (listed, not counted, never held on) otherwise:

- **`proven`**: an index exists and holds a structurally proven call or use edge from this line
  to the declaration.
- **`by name`**, for a function, a type or a constant: no other declaration in the repository
  has the name, the file binds no variable, parameter or field of that name, and one of:
  - the site is in the declaring file;
  - the file imports the name from the declaring module or by a relative path, or imports
    everything the declaring module has;
  - the name is reached through a qualifier that an import in the file binds to the declaring
    module: `utils.shorten` with `import utils`, `clearing::owed` with `mod clearing;` or
    `use crate::clearing`, a path that starts at `crate::`, `super::` or `self::`. In Go the
    file has to import the declaring package by its own path, read from the nearest `go.mod`:
    `http.Get` in a file that imports `net/http` is not a call of a local package `http`, and
    `slog.Logger` is not a local type `Logger`. A qualifier that is a variable of the file
    (`cache = redis.Redis()`) leads nowhere;
  - the file is in the same Go package or Java package directory; or the language is C#, where
    a namespace is opened whole.
- **`by receiver type`**, for a member of a type: the member is reached through something the
  enclosing function says is of the owner's type, and that type's name leads to the declaring
  module as a free name would have to:
  - a parameter or a local with a written type (`cmd: Command`, `Command cmd`, `var c Command`),
    including a Go method's own receiver (`func (c *Command) Run() { c.Out() }`);
  - a local initialised by a constructor or a typed literal (`new Command()`, `Command{}`,
    `&Command{}`, `new(Command)`, `Command()` in Python, `Command::new()`);
  - in Go and Rust, the result of a function or method whose declaration says what it returns,
    through a name or directly, to six steps: `r := NewRouter()`, `r, err := Open(path)`,
    `NewRouter().Get(..)`, `r.NewRoute().Path(..)`, `Route::parse(text)`. The declaration is
    found by name in the working tree; where several of the name return different types, or
    the one the site reaches returns none this reads, nothing is settled.

  A name is what the nearest function around the use that binds it says it is: a closure sees
  the names of the function around it, and its own parameter of the same name hides them only
  inside it. Two bindings of a name in one function that disagree settle nothing. In a language
  with static types, assigning to a name does not change its type.
- **`by name`, for a member of a type** (a method, or a constant declared inside it): the site
  reaches it through its own type (`Owner.method`, `Owner::method`, for a type written with a
  capital, outside Go), on the instance inside that type (`self.method`, `this.method`), or, in
  Java and C#, by its bare name inside that type. A call through `super` or `base` is the
  parent's and is not listed. A member the type no longer declares may still be inherited, so
  where the type names a base its sites are `possible`; a Go type's bases are the types it
  embeds, which the repository declares, so there it applies only when something else declares
  the name. On any other receiver the type is unknown: `possible`.
- A JavaScript or TypeScript module's default export is named by its importers as they like:
  when a default-exported declaration is renamed and the module still has a default export, a
  file that imports the default under the old name is not naming the declaration.
- A removed declaration whose name is declared again elsewhere may have moved there, so its
  sites are `possible`: a method that another file of the same (partial) type now declares, a
  type or a constant that anything else declares, a function that another module now binds at
  its top level (`export const run = create()`).
- When a file that holds the name could not be read within the limits of one run, nothing about
  that name is settled, and every site of it is `possible`.
- A C# preprocessor line (`#if NET8_0_OR_GREATER`) names compilation symbols and is not read.
- A constructor (`__init__`, `constructor`) is checked at the calls of its type's name.
- A module that was deleted and whose name a dependency manifest declares (a vendored copy
  given up for the package) is not evidence: an import by that name resolves to the dependency.

## What it does not do

- It does not type-check. A changed parameter type, a changed return type, a struct field, and a
  behaviour change with the same signature are outside it.
- It does not follow a call through a receiver whose type the function around it does not say:
  a struct field (`c.parent.Run()`), an untyped parameter, a loop variable, the result of a
  function with no written return type, a type that only embeds or extends the owner. Nor a
  function passed as a value under another name, an alias (without an index), reflection or a
  string. Those are `possible` or not seen.
- A member removed from a class that has a base is `possible` everywhere outside Go: the base
  may be a framework's, and may declare it.
- A name several declarations share is not attributed without an index.
- A move to another file is not a rename: the old name's sites are listed as `possible` with a
  note that it may have moved.
- One run compares at most 300 changed code files, searches for at most 60 changed
  declarations, reads at most 400 files of 2 MB or less for sites, lists 12 sites per
  declaration, and stops searching after ten seconds; what is past a limit is counted and said.
  A name on more than 1,500 lines is not read at all.
- The hooks look at the declarations of files the conversation edited with `Edit` or `Write`.
  A file it changed through a shell command is not among them; `ok ripple` and `ok gate ci` see
  every change.
- An index that cannot be read (built by an earlier version, locked, damaged) is the same as no
  index, and the report says so.

In a compiled language the compiler finds every one of these and more. Ripple finds them before
a build is run, in the languages that have no compiler, and for whoever reads the pull request.

## Measured

Replayed over the last 300 commits of 46 open-source repositories in seven languages, with no
index, in two ways: on each **complete merged commit**, where anything it reports is a false
alarm or a real leftover; and on each commit **with only the declaring file applied**, where what
it reports can be checked against what the real commit went on to change.

- The first form of the check was run on new repositories in five rounds and changed after
  each. In the last round (10 repositories it had not been run on, 1,615 commits that change
  code) it named 341 sites from the declaring file alone, of which the commit then changed 333
  and 6 more were real leftovers; the complete commits raised an alarm in 6, 2 real and 4 false.
- An independent review then found 18 faults in it, most of them from reading a call as text,
  and judged it not safe to hold a stop on by default. The reading was rewritten on the syntax
  tree, and its first replay raised six more false alarms, each now a rule above with a test.
- As shipped, on all 46 repositories (7,711 commits that change code): 913 sites named from the
  declaring file, 896 then changed by the commit, 12 of the other 17 real leftovers and 5 in
  tests the commit commented out instead; on the complete commits, 15 sites in 4 commits, all
  four real. **That figure is not held out**: the rules were changed after looking at these
  repositories, which is why the hooks report and do not hold unless asked to.
- The four real ones are commits on the main branch of a widely used library that still named
  something they had removed; one was still in place when its repository was cloned.
- **Recall is not measured**: nothing here says how many broken sites it did not name.
- On five real signature changes, a frontier model asked to update the callers left nothing
  broken with or without the hook, so the hook's effect on that outcome is not shown. It was
  delivered in all five sessions and cost no time.

Method, every number, the review, the false alarms by cause and the limits are in
[`benchmarks/ripple/`](../benchmarks/ripple/README.md).
