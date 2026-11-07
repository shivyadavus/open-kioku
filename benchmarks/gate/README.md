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
```
