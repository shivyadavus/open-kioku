---
description: Navigate, search, and analyze a codebase indexed by Open Kioku. Use when exploring unfamiliar code, tracing symbol definitions, measuring change impact, or planning a refactor.
disable-model-invocation: true
---

# Open Kioku — Code Intelligence

Open Kioku gives you a persistent, evidence-backed memory of the indexed repository. All tools are read-only by default and operate entirely locally — no network calls, no cloud API.

## When to use each tool

### Orientation (start here on an unfamiliar codebase)
- `repo_status` — check what is indexed and when it was last updated
- `list_languages` — understand the technology stack
- `detect_architecture` / `summarize_architecture` — get a high-level map of the codebase layout

### Search
- `search_code` — full-text BM25 search across all indexed chunks; best for business logic keywords
- `regex_search` — pattern-based search when you know the exact syntax
- `semantic_search` — falls back to lexical when embeddings are disabled
- `list_symbols` — browse all indexed symbols by substring

### Symbol navigation
- `get_definition` — find where a symbol is defined before editing it
- `get_references` — find every place a symbol is used before renaming or deleting it
- `get_callers` / `get_callees` — trace call graphs for debugging or refactoring
- `get_implementations` — find all concrete implementations of an interface or trait
- `explain_symbol` — get full context (file, line range, surrounding code)

### Change analysis
- `impact_analysis` — measure the blast radius before modifying a file; shows all direct and transitive dependants
- `find_tests_for_change` — identify which tests cover a file before pushing
- `recommend_validation_plan` — get a structured validation checklist for a change
- `dependency_path` — find how two files or symbols are connected

### Refactoring and patch planning
- `build_context_pack` — assemble a full context bundle (files + symbols + tests + patch boundaries) for a complex task; pass the task in natural language
- `propose_patch` — generate a patch plan without writing any files
- `review_patch` / `validate_patch` — review or validate a planned patch against the index

### File-level exploration
- `explain_file` — get all chunks and metadata for a single file
- `module_dependencies` — list direct graph neighbours of a file or symbol node

## Workflow examples

**Before editing a function:**
1. `get_definition` → locate it
2. `get_references` → see all callers
3. `impact_analysis` → measure downstream risk
4. `find_tests_for_change` → know what to run after

**Starting a large refactor:**
1. `build_context_pack` with your task description
2. `propose_patch` to plan the changes
3. `validate_patch` before applying

**Exploring an unfamiliar repo:**
1. `repo_status` → confirm index is fresh
2. `detect_architecture` → understand the layout
3. `search_code` with a domain keyword → find the relevant module
4. `explain_file` → read the key file in full
