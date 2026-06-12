# Open Kioku Roadmap: World-Class Agent Safety Layer

This roadmap is intentionally narrow: Open Kioku should not become a generic agent framework, chatbot, or vector database wrapper. The product category should be:

> A local safety and intelligence layer that helps AI coding agents understand, plan, constrain, and verify code changes before and after editing.

The current repository already has the right foundation: local indexing, Tree-sitter-backed extraction, SCIP import, persisted graph facts, Tantivy lexical search, symbol discovery, impact reports, test recommendations, context packs, patch plans, and read-only MCP support.

The next features should make the `understand -> plan -> edit within boundary -> verify` loop defensible and measurably better than raw grep, vector-only RAG, or generic agent tooling.

## Priority 1: Plan Verification Contract

Agents should not just generate a plan. They should be held to it.

A saved plan should define:

- allowed files
- caution files
- forbidden files
- required evidence references
- required validation commands
- expected graph/architecture invariants
- confidence and risk thresholds

After edits, Open Kioku should verify whether the diff stayed within the plan, whether the right tests ran, and whether dependency/architecture constraints changed.

## Priority 2: Architecture Policy Engine

Heuristic architecture detection is useful, but world-class agent safety needs explicit repo-local policy.

Support an `ok.toml` architecture policy with:

- named layers/components
- path/package globs
- allowed and forbidden dependency edges
- public API boundaries
- generated/vendor/test exemptions
- severity levels

Agents should call this before and after editing.

## Priority 3: Historical Change Intelligence

Code risk is not only structural. It is historical.

Open Kioku should mine local git history for:

- files that change together
- symbols/modules with high churn
- recent authors and reviewers
- prior fixes for similar areas
- hot spots with low test coverage
- commits that introduced or modified a symbol

This evidence should feed ranking, impact, risk, plan confidence, and reviewer suggestions.

## Priority 4: Ownership and Reviewer Intelligence

Agents need to know who owns an area before they make risky changes.

Derive ownership from CODEOWNERS, git blame/log, package/module boundaries, and optional repo memory. Expose CLI/MCP queries for ownership, reviewer suggestions, and ownership risk.

## Priority 5: Runtime Failure Evidence

Static facts tell the agent what could break. Runtime facts tell the agent what already broke.

Optional integrations such as Sentry should map errors, stack traces, failing tests, CI logs, and incidents back to indexed symbols and files. Runtime evidence must be marked with provenance and confidence and should never outrank exact indexed code facts.

## Priority 6: Language-Specific Precision Packs

Tree-sitter is the base. World-class precision needs language-specific resolvers for the highest-value ecosystems.

Start with TypeScript/JavaScript, Python, Java/Kotlin, and Rust. Each pack should improve definitions, references, callers/callees, imports, tests, and build targets beyond generic heuristics.

## Priority 7: Workflow Quality Benchmark Suite

Open Kioku should prove usefulness with repeatable benchmarks, not claims.

Benchmark end-to-end agent workflows:

- context recall
- impact recall
- test recall
- boundary precision/recall
- verification verdict accuracy
- confidence calibration
- token efficiency vs baseline
- agent edit success rate

The benchmark should compare Open Kioku against grep-only, lexical-only, vector-only, and hybrid baselines where possible.

## Priority 8: Agent Contract Export

Make Open Kioku easy for agent frameworks to consume without custom prompt hacking.

Export a durable agent contract containing:

- task
- evidence graph
- plan
- boundaries
- validation commands
- risk score
- tool-call recommendations
- verification requirements

Formats should include JSON, Markdown, and compact TOON for LLM context.
