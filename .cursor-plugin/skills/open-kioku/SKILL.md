---
name: open-kioku
description: Use Open Kioku to search code, resolve symbols, trace blast radius, and build context packs. Invoke before editing unfamiliar code, investigating a bug, or planning a refactor.
---

# Open Kioku — Code Intelligence

Open Kioku gives you precise, evidence-backed answers about this codebase. It runs entirely locally — no cloud, no embeddings API.

## When to use

- Before editing a file you haven't seen before
- When you need to find where a symbol is defined or used
- Before a refactor — to know what else will break
- When building context for a complex multi-file task
- When you need to validate a patch plan before applying it

## Core tools

1. **`search_code`** — BM25 full-text search across the entire indexed codebase. Use for finding functions, types, constants, and patterns by name or content.
2. **`resolve_symbol`** — Jump to the exact definition of any symbol. Use when you see a name and need to know what it is.
3. **`find_references`** — Find every callsite or usage of a symbol. Use before renaming or deleting anything.
4. **`impact_analysis`** — Trace the blast radius of a proposed change. Use before any refactor to understand downstream effects.
5. **`build_context_pack`** — Assemble a token-efficient context bundle for a task description. Use at the start of a complex multi-file task.
6. **`propose_patch`** + **`validate_patch`** — Plan and pre-flight a code change before applying it.

## Instructions

1. Always call `search_code` or `resolve_symbol` before editing a file you have not read in this session.
2. Always call `impact_analysis` before any rename, deletion, or interface change.
3. Use `build_context_pack` when a task touches more than 3 files — it assembles the minimal relevant context.
4. Use `validate_patch` after `propose_patch` to confirm the plan is safe before writing.
5. Prefer evidence from Open Kioku over assumptions about file contents.
