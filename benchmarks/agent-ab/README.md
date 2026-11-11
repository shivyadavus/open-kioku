# Does Open Kioku help a coding agent find where a change goes?

A pilot, run on 2026-11-11, with the result it had: **no measurable gain**, on this repository,
for this agent, on this task. The numbers and their limits are below, with what changed in Open
Kioku because of them. The harness is [`scripts/agent-ab.py`](../../scripts/agent-ab.py); every
run's record is in [`results.json`](results.json).

## Method

Six real commits of this repository. For each, a clone checked out at the commit's parent and
indexed with `ok index` (10 to 12 seconds each). The same prompt, the commit's subject, asks
Claude Code (`claude -p`, default model, read-only tools, edits disallowed) which existing files
have to change, answered as a list of at most twelve paths. The list is scored against the
files the real commit changed (`CHANGELOG.md` and `Cargo.lock` left out). One run per arm per
task.

| Arm | What the agent had |
|---|---|
| plain | Read, Grep, Glob, and `ls`/`rg`/`grep`/`cat`/`sed`/`head`/`wc`/`find` |
| kioku | the same, plus the Open Kioku plugin as shipped: the MCP server connected (16 tools), the hooks, `ok` on PATH |
| brief | the plain tools; the prompt also carries the plan's ranked files (`ok preflight`: 8 edit candidates with line ranges and 8 linked files, about 1.1 KB) |
| impact | the plain tools; the prompt also carries the files with a proven call, import or type use into the two files the plain arm ranked first (`ok impact --file`, 0.6 to 2.4 KB) |

## Results

| Task (commit) | Arm | Recall | Precision | Turns | Cost (USD) | Time (s) |
|---|---|---|---|---|---|---|
| `9dc7179a0f` fix(git): diff entries without file headers | plain | 0.25 | 0.75 | 3 | 0.37 | 13.6 |
| | kioku | 0.25 | 0.75 | 6 | 0.49 | 18.5 |
| | brief | 0.25 | 1.00 | 6 | 0.51 | 19.7 |
| | impact | 0.25 | 1.00 | 6 | 0.63 | 19.6 |
| `bc7f3cdb34` fix(patch,cli): findings by file path | plain | 0.40 | 0.50 | 4 | 0.68 | 13.0 |
| | brief | 0.40 | 0.40 | 7 | 0.32 | 18.5 |
| | impact | 0.40 | 1.00 | 4 | 0.24 | 15.4 |
| `ef1c3bec14` fix(core,impact): SSH key files | plain | 0.41 | 0.88 | 5 | 0.39 | 18.9 |
| | brief | 0.41 | 0.88 | 4 | 0.28 | 16.9 |
| | impact | 0.35 | 0.86 | 6 | 0.34 | 19.6 |
| `d387a7bca2` feat(graph): edge authority in graph queries | plain | 0.71 | 0.83 | 11 | 0.68 | 27.6 |
| | brief | 0.64 | 0.90 | 6 | 0.55 | 30.4 |
| | impact | 0.64 | 0.82 | 10 | 0.52 | 24.4 |
| `c1c3ee4d6c` fix(impact): hub windows and deleted files | plain | 0.64 | 0.58 | 7 | 0.44 | 21.9 |
| | brief | 0.55 | 0.60 | 8 | 0.48 | 31.4 |
| | impact | 0.27 | 0.43 | 5 | 0.35 | 21.3 |
| `7b8aaf0661` fix(tree-sitter,ingest): Go type aliases | plain | 0.83 | 0.83 | 22 | 0.73 | 52.8 |
| | brief | 0.33 | 1.00 | 15 | 0.76 | 46.1 |
| | impact | 0.92 | 0.92 | 10 | 0.52 | 25.8 |

Means over the six tasks:

| Arm | Recall | Precision | Turns | Cost (USD) | Time (s) |
|---|---|---|---|---|---|
| plain | 0.54 | 0.73 | 8.7 | 0.55 | 24.6 |
| brief | 0.43 | 0.80 | 7.7 | 0.48 | 27.2 |
| impact | 0.47 | 0.84 | 6.8 | 0.43 | 21.0 |

The `kioku` arm was run on one task. The MCP server was connected and the agent called none of
its tools: it used Grep and Read, as the plain arm did. The other five were not run, because
the arm is the plain arm for as long as the agent does not choose a tool it did not choose.

## What it says

1. **As shipped, the agent does not reach for Open Kioku.** Sixteen connected tools and no
   reason to use them are, for this agent, the same as no tools.
2. **Handing the agent the index's knowledge up front did not improve which files it named.**
   Recall went from 0.54 to 0.43 and 0.47, precision from 0.73 to 0.80 and 0.84, and turns and
   cost fell by 10 to 20 percent. With one run per cell these differences are inside the
   run-to-run noise: the four arms of the first task all returned the same three files and
   cost between USD 0.37 and 0.63.
3. **The index did hold true facts the agent did not act on.** For `9dc7179a0f`, `ok impact`
   named `contract.rs`, `impact/src/lib.rs` and `mcp/src/lib.rs` as callers of the function
   being changed. The real commit changed all three; no arm listed any of them, the arm that
   was shown the list included.
4. **The answers were too large to read.** On this repository one `ok plan` was 94 KB as text,
   155 KB as Markdown (the MCP default) and 458 KB as JSON; `ok preflight` was 10 KB as text and
   54 KB as JSON (the MCP default); and `ok preflight` printed the same `cargo test` command
   eight times, once per selected test.

## What changed because of it

- `plan_change` has a `brief` detail, and `ok preflight` a `--format brief`: the verdict, up to
  eight files to edit with line ranges, up to eight likely affected files, each validation
  command once with the tests it stands for, the first three risks, and counts of what was left
  out. On the task measured above it is 1.9 KB. The tool description and the agent skill say to
  read it first.
- `ok preflight` names a shared validation command once, in every format.
- The weight of the product moved to what did change an agent's behaviour in the live sessions
  ([`../gate/live-runs.md`](../gate/live-runs.md)): a refusal with a stated remedy, and a
  verified record of the change for the person reviewing it ([`docs/gate.md`](../../docs/gate.md)).
  A hint is something an agent may read; the gate is something its edit has to pass.

## Limits

One repository (Open Kioku's own: Rust, well laid out, commit subjects that name the crate), six
tasks, one run each, one frontier model, a read-only task, and a gold standard, every file the
commit touched, that includes tests and documentation nobody could name from the subject alone.
It does not measure an editing task end to end, a large or unfamiliar or polyglot repository, or
a weaker model, and it says nothing about them. Nineteen runs, USD 9.26.

## Reproduce

```sh
cargo build --release -p open-kioku-cli
scripts/agent-ab.py --source . --work ../ab/work --out ../ab/out \
  --ok-bin target/release/ok --plugin-dir . --arms plain brief impact \
  --commits 9dc7179a0f bc7f3cdb34 ef1c3bec14 d387a7bca2 c1c3ee4d6c 7b8aaf0661
```

Each run is a real Claude Code session and is billed as one; `--budget` caps a single run.
