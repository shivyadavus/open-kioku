# Memory Profiling

Deterministic allocation accounting for memory work such as [#329](https://github.com/shivyadavus/open-kioku/issues/329).

## Why not peak RSS

Peak RSS is the number that matters to a user, and the wrong instrument for comparing two builds. On the large-Java validation corpus, repeated runs of the same binary over the same corpus on the same host produced:

| run | max RSS |
|---|---:|
| cold index | 6.56 GB |
| cold index, repeat | 6.99 GB |
| bulk index | 8.03 GB |
| repeat index | 7.44 GB |

The closest same-protocol repeat pair differs by **438 MB**, because RSS reflects the allocator's page-return policy, fragmentation, and page-cache behavior rather than what the program requested. The tractable savings identified in #329 total roughly the same magnitude, so a peak-RSS A/B cannot resolve them without many repeat runs at 20–40 minutes each.

## What this measures instead

`--features mem-profile` installs a counting global allocator that tracks bytes **requested** from the Rust global allocator:

- `peak_live_bytes` — high-water mark of allocated-and-not-yet-freed bytes
- `live_at_exit_bytes` — still-held bytes at process exit
- `allocations` — total allocation calls, which exposes churn that peak alone hides

Because it counts requests rather than resident pages, it is far steadier than RSS. Measured over three identical indexing runs of the same build:

| instrument | values | spread |
|---|---|---:|
| `peak_live_bytes` | 59,679,221 / 59,579,761 / 59,554,212 | **0.21%** |
| peak RSS, same runs | 100,483,072 / 101,412,864 / 102,150,144 | **1.66%** |

It is **not** perfectly repeatable. Thread scheduling changes which transient allocations overlap, and the allocation count itself varies between runs, so a difference below roughly 1% should be treated as noise rather than signal.

```sh
cargo build -p open-kioku-cli --features mem-profile
./target/debug/ok index /path/to/repo
# ok[mem-profile] peak_live_bytes=... peak_live_mib=... live_at_exit_bytes=... allocations=...
```

Output goes to stderr, so `--json` on stdout stays machine-parseable.

## Limits — read before quoting a number

**It does not see `mmap`.** Tantivy segments, the SQLite page cache, and allocations that C dependencies make through their own allocators are all invisible. The number is a **floor on retained memory, never a substitute for peak RSS**.

**These are two different claims and must never be combined in one sentence:**

- an allocator counter proves *bytes are no longer requested*
- only a full-corpus run proves *peak RSS dropped*

A change can reduce requested bytes without moving the OS-visible ceiling, because the system allocator decides when to return pages.

**Never quote a timing from a `mem-profile` build.** Two atomics on every allocation is a real cost. The feature is off by default for this reason.

## Suggested protocol for an A/B

1. Build both arms with `--features mem-profile` from clean checkouts at known SHAs.
2. Use a pinned, **public** corpus so the result is replayable — a fixed subtree of a public Java repository at a fixed commit works well. The confidential large-Java corpus can establish a scale record but never a reproducible one.
3. Measure edges-per-file on the candidate corpus rather than assuming the whole-repo ratio carries over to a subtree; `ok index --json` reports graph edge counts.
4. Fresh `.ok` store per run, machine otherwise quiet.
5. Report both arms with their build SHAs, and state the corpus and its commit.

Repeats are still worth running, but three are enough rather than the five to seven a peak-RSS A/B needs. If two runs of the same build disagree by more than ~1%, something in the protocol moved.

## Scope

`peak_live_bytes` is process-wide, not phase-attributed. Attributing it to indexing phases is a natural extension — `IndexProgressReporter::emit_progress` already observes every phase transition — but is not implemented.
