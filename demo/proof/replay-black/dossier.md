## ⚠️ Change Dossier: WARN

**Task:** Add --cache-dir command-line option

**Repository:** `black`  ·  base `d9d091de04`  ·  head `c96506121f`  ·  gate mode `warn`  ·  session `gate-20261114T170202Z-b6370a40`

### Declared before editing

- Risk `high` (1.00); overall Medium (0.60) from task relevance, evidence density, references, validation, boundaries, runtime, test coverage, and negative evidence; blocker: 1 negative evidence signal(s) lowered confidence; caveat: 1 hyphenated task word(s) appear in no selected context: command-line
- Boundary: 8 allowed, 14 caution, 18 forbidden rule(s); plan `sha256:ab1f3c0ca721`
- Allowed: `Dockerfile`, `scripts/generate_schema.py`, `scripts/release.py`, `src/black/__init__.py`, `src/black/cache.py`, `src/black/linegen.py`, `src/blib2to3/pgen2/driver.py`, `src/blib2to3/pygram.py`

### Edits as the harness reported them

- 0 allowed, 0 caution, 0 warned, 0 denied (0 event(s))

### The actual change

- 1 changed file(s) from git diff against the session's base revision plus untracked files, less changes that predate the session; 1 commit(s) made during the session are included; diff `sha256:6ea0b7d98f77`
- Files: `src/black/cache.py`

### Ripple

- **6 site(s) no longer fit what this change altered** (3 changed declaration(s); 1 other call(s) fit).
  - `tests/test\_black.py:2663` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2673` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2682` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2694` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2743` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2773` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]

### Verification

- Verdict: **Warn**
- Boundary: every changed file is inside the declared boundary.
- Impacted dependents not updated: 4
  - `src/black/mode.py` post-edit impact candidate was not present in the saved plan: tantivy hybrid lexical match
  - `src/blib2to3/pytree.py` post-edit impact candidate was not present in the saved plan: tantivy hybrid lexical match
  - `src/black/strings.py` post-edit impact candidate was not present in the saved plan: tantivy hybrid lexical match
  - `tests/data/cases/multiline_strings.py` post-edit impact candidate was not present in the saved plan: tantivy hybrid lexical match
- Warnings: 10
  - [evidence_quality_caveat] exact symbol/reference evidence is unavailable
  - [evidence_quality_caveat] runtime evidence is unavailable
  - [evidence_quality_caveat] history evidence is unavailable
  - [evidence_quality_caveat] coverage or JUnit evidence is unavailable
  - [evidence_quality_caveat] index skipped 85 path(s); evidence may be incomplete for skipped areas
  - [evidence_quality_caveat] optional evidence pass did not complete: SCIP was enabled but no SCIP index was imported

### Caveats

- the change was made without the edit hooks reporting any edit: the trail shows what the harness reported, not what happened
- advisory session: the boundary was planned from text nobody declared as a boundary (a prompt, or a pull request's title and description); no changed file is outside it. A forbidden path still fails

<sub>Open Kioku gate · dossier schema v1 · 2026-11-14 17:02 UTC</sub>
