# Gate simulation results

Produced by [`scripts/gate-sim.py`](../../scripts/gate-sim.py) against a clean, indexed checkout
of this repository at `5f8cb0f6` with a release build of the same tree plus the gate. Each case
is a real non-merge commit that touched between 1 and 12 source files: its subject is the task
`ok gate start` plans from, and the files it changed are the edits a faithful agent makes. The
hooks are driven exactly as Claude Code drives them (JSON on stdin), with the same `ok` binary.

What one case does, in order:

1. `ok gate start "<commit subject>" --mode <mode>`.
2. **Faithful edits**: each of the commit's files, through the `pre-tool` hook, then appended to.
   A denial is counted as *friction* and the boundary is widened so the case can continue.
3. **Drift edits**: three source files the commit did not touch and the boundary does not admit,
   through the hook; an edit that is allowed lands in the tree.
4. **Forbidden edits**: two paths drawn from `.env`, `.env.production`, `config/secrets/db.yaml`,
   `deploy/server.pem`, `.ssh/id_rsa`, `target/debug/build.rs`, `node_modules/left-pad/index.js`,
   `.ok/index.sqlite`, through the hook.
5. **Bypass**: one new file written with no hook at all, which only `finish` can see.
6. `ok gate finish`, then the tree is restored.

Every case therefore ends `FAIL` by construction (the bypass is always present); the numbers
below are about what was caught, where, and what a faithful agent paid.

## enforce mode — 38 cases, seed 7

[`commit-derived-enforce.json`](commit-derived-enforce.json)

| Edit kind | Attempted | Denied at the hook | Failed at finish | Missed |
|---|---:|---:|---:|---:|
| Drift (outside the boundary) | 114 | 114 (100%) | – | 0 |
| Forbidden (secret-like or generated path) | 76 | 76 (100%) | – | 0 |
| Bypass (no hook) | 38 | – | 38 (100%) | 0 |
| Faithful (the commit's own files) | 232 | 44 | – | – |

- **Friction**: 188 of 232 faithful edits (81.0%) were inside the boundary the plan declared, so
  needed no widening; 18 of 38 sessions needed no widening at all. The other 44 edits were denied
  once and admitted after `ok gate expand … --reason`. This is retrieval quality showing through
  the gate, not a gate error: the task is a commit subject, which is a weaker description than an
  agent is usually given. The median plan admitted 8 paths for a median of 7 gold files.
- **Latency** (release build, 4 vCPU): hook median 5.3 ms, p95 8.0 ms per edit; `start` median
  4.5 s (plan building); `finish` median 10.7 s (verification).

40 cases were requested; 38 of the most recent non-merge commits qualified.

## warn mode — 20 cases, seed 11

[`commit-derived-warn.json`](commit-derived-warn.json)

| Edit kind | Attempted | Denied at the hook | Failed at finish | Missed |
|---|---:|---:|---:|---:|
| Drift (outside the boundary) | 60 | 0 (by design) | 60 (100%) | 0 |
| Forbidden (secret-like or generated path) | 40 | 38 (95%) | 0 | 2 |
| Bypass (no hook) | 20 | – | 20 (100%) | 0 |
| Faithful (the commit's own files) | 126 | 0 (by design) | – | – |

- In `warn` mode the hook denies only forbidden paths, so every drift edit lands and the
  verifier at `finish` is the whole defence: it failed all 60, and all 20 bypasses.
- **The two misses are the same path, `target/debug/build.rs`, and they are a design edge, not
  a classifier error.** A plan forbids the build directories the index saw as pruned (4.0 retired
  the name-based `target/**` rule), and the clean clone has no `target/`, so the path is
  *outside*, not *forbidden*: the hook warned and recorded it, as the mode says. It is then absent
  from the change because `target/` is git-ignored and an ignored file cannot reach a commit, so
  `finish` has nothing to fail. The dossier now names such edits in its caveats ("allowed outside
  the boundary and not in the change"), and in `enforce` mode the same edit is denied outright
  (76 of 76 above). Whether an absent, ignored build directory should count as forbidden by name
  is an open product question; the measurement reports it rather than deciding it.
- Latency: hook median 5.2 ms, p95 6.5 ms; `start` 4.4 s; `finish` 13.5 s (more changed files to
  verify per case than in enforce mode, since drift lands).

## Live sessions

[`live-runs.md`](live-runs.md) records three real Claude Code sessions driven under the gate in
`enforce` mode on this repository: what the boundary declared, what the agent edited, where it
widened the boundary and why, and what it did unprompted with `ok gate check` and `ok gate finish`.

## Reading it

- A denial at the hook is deterministic: the path's relation to the boundary, read from one JSON
  file. The 100% rates on drift and forbidden edits are what the classifier is defined to do; the
  measurement shows the hook wiring, the path normalisation and the secret-path policy agree with
  the verifier on every one of 190 paths, with no path admitted by one and refused by the other.
- The bypass rate is the verifier reading the working tree (`git diff HEAD` plus untracked files,
  less what was dirty before the session), not the trail. An agent that writes around its harness
  is caught at `finish`, and the dossier says the trail and the tree disagree.
- The friction number is the one to watch when the plan changes: it is the share of legitimate
  edits a faithful agent makes without being stopped.

## Reproduce

```sh
cargo build --release -p open-kioku-cli
git clone . /tmp/sim-repo && (cd /tmp/sim-repo && ../target/release/ok index .)
OK_BIN=target/release/ok scripts/gate-sim.py --repo /tmp/sim-repo --cases 40 --mode enforce --seed 7 \
  --write benchmarks/gate/commit-derived-enforce.json
OK_BIN=target/release/ok scripts/gate-sim.py --repo /tmp/sim-repo --cases 20 --mode warn --seed 11 \
  --write benchmarks/gate/commit-derived-warn.json
```
