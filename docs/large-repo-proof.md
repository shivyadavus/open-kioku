# Public Repository Proof

This audit records local Open Kioku 2.0.1 runs against pinned public
repositories under permissive open-source licenses. It checks whether commands
merely execute and whether their results match the indexed source.

Third-party project names are intentionally omitted. The document focuses only
on Open Kioku behavior across repository and language profiles.

## Environment

- Date: 2026-06-12
- Open Kioku version: 2.0.1
- Open Kioku source revision: `a3703cf`
- Binary: `target/release/ok`
- Platform: macOS
- SCIP mode: off
- Semantic search: disabled

SCIP was intentionally disabled to test the default local tree-sitter,
lexical-search, graph, impact, test-selection, context, and planning path.
Exact reference quality should improve when a repository-specific SCIP index is
available.

## Results

| Repository profile | License and state | Files | Symbols | Chunks | Tests | Graph edges | Index time |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Python AI inference system | Apache-2.0, active | 4,623 | 46,738 | 49,459 | 8,945 | 79,426 | 33.1s |
| TypeScript coding tool | Apache-2.0, archived | 2,276 | 33,031 | 34,664 | 7,288 | 40,965 | 12.5s |
| C++ AI runtime | MIT, active | 591 | 5,837 | 6,257 | 589 | 7,937 | 3.5s |

Counts are the files supported and accepted by the current Open Kioku indexing
configuration, not every file present in each Git checkout.

## Primary Python Proof

Index command:

```sh
target/release/ok index /absolute/path/to/python-ai-repo --with-scip off
```

Result:

```text
index[store] writing 4623 files, 46738 symbols, 49459 chunks, 46738 occurrences, 60 analysis facts
index[graph] writing 54954 graph nodes and 79426 graph edges
index[complete] index ready, elapsed=33.1s
Indexed 4623 files, 46738 symbols, 49459 chunks
```

`ok doctor` reported a healthy index, 8,945 tests, 34,169 imports, and a
responsive MCP initialize request. It also correctly warned that exact SCIP
references were unavailable.

### Source-checked definition

```sh
target/release/ok --repo /absolute/path/to/python-ai-repo \
  symbol definition ModelConfig
```

Open Kioku returned the correct Python class definition at line 107 with high
tree-sitter confidence. The result was checked directly against the pinned
checkout.

### Source-checked planning task

For a concrete prefix-cache behavior change, the plan found:

- the cache-manager implementation
- the lower-level block-pool implementation
- two public entry points
- one end-to-end reset test
- one focused prefix-caching unit test

The returned paths and line ranges were checked directly against the pinned
checkout.

### Search quality

A prefix-cache eviction query returned the relevant cache implementation,
end-to-end reset test, and prefix-caching tests. A distributed-worker query
returned implementations under the repository's distributed and worker
modules.

### Python limitations

- Exact Python references were unavailable because no SCIP index was supplied.
- File-only test selection returned neighboring cache tests but missed the two
  strongest reset-prefix-cache tests.
- A concrete task prompt corrected that weakness and found both focused tests.
- A broad model-validation prompt selected unrelated validation code. Task
  wording still matters without exact references.

## TypeScript Proof

The TypeScript audit correctly resolved a proxy-bypass function to its source
definition and returned five indexed occurrences, including its import, call
site, and nearby tests.

Limitation: generic file-based test selection returned many unrelated
`util`-named test symbols. The exact nearby test file exists, but without SCIP
the ranking was too lexical to use as a strong product claim.

## C++ Coverage Finding

The C++ audit exposed a current language-coverage limitation. The default
parser set indexed supporting Python, TypeScript, JavaScript, and Rust files
while missing important C++ definitions.

A real C++ function present in the source returned `symbol not found`.
Impact and test selection for that implementation file also returned no useful
downstream evidence.

This is a documented product gap. C++ repositories should not be presented as
a successful proof until first-class C/C++ parsing or a suitable
exact-reference index is available.

## Interpretation

The strongest current public proof combines:

- a current, active AI infrastructure codebase
- a standard permissive open-source license
- healthy indexing with visible progress
- source-checked Python definitions and search results
- a concrete planning task that found implementation and focused tests
- explicit disclosure of where no-SCIP test selection remains noisy

The cross-repository audit also sets a clear boundary: TypeScript definition and
reference lookup works, while C++ coverage is not yet strong enough to claim.
