# Open Kioku

**Your coding agent shows its evidence before it edits, and its diff is verified against the plan it declared.**

A local index of your repository feeds a bounded plan; after the edit, `ok verify` checks the actual changed files against that plan. Nothing leaves your machine: no hosted index, no source upload, and the MCP server is read-only by default.

Source, measured accuracy on four real repositories, and the method behind every number: https://github.com/shivyadavus/open-kioku

## First win: 2 commands

```sh
npm install -g open-kioku
ok setup agent cursor --repo . --apply
```

Use `claude` instead of `cursor` for Claude Code. One command indexes the repository, writes repository-scoped MCP configuration and agent guidance, and checks that the local server answers (run without `--apply` to preview; nothing is written). Every other MCP client listed by `ok mcp install --help` gets a read-only configuration snippet from `ok mcp install <client> --repo .`.

Then ask for evidence on a real task:

```sh
ok context "change token expiration" --format markdown
```

Every context pack says which evidence streams ran, which succeeded, and what is missing. Missing evidence lowers the stated confidence; it is never papered over.

## The loop

```sh
ok plan "change token expiration" --format json > plan.json   # context, impact, tests, edit boundary, caveats
# ...edit with your normal agent or editor...
ok verify --plan plan.json --git                               # the real diff against the declared boundary
```

`ok plan` (or the `plan_change` MCP tool) returns primary context with provenance, impact candidates split into structurally proven and heuristic, validation targets tiered by evidence, an edit boundary (allowed, caution, forbidden paths), and explicit caveats. `ok verify` reads the actual changed files and reports anything outside the boundary. A green exit code from a test runner is not proof the right files changed; this is.

## MCP

The server is local, read-only, and speaks stdio. It advertises 16 tools, one per question nothing else answers: `repo_status`, `list_files`, `search_code`, `regex_search`, `search_symbols`, `get_definition`, `get_references`, `dependency_path`, `impact_analysis`, `explain_flow`, `build_context_pack`, `retrieve_context`, `plan_change`, `verify_change`, `find_tests_for_change`, and `query_evidence_graph`. Reference: https://github.com/shivyadavus/open-kioku/blob/main/docs/mcp-tools.md

Agent guidance that `ok setup agent --apply` installs for you, if you would rather paste it yourself:

```text
Use Open Kioku before editing. Check repo_status, search_code, get_definition,
get_references, impact_analysis, and find_tests_for_change. Build a plan with
plan_change first, then edit, and verify after the edit with verify_change.
```

Upgrading from 3.x: run `ok index` once. The index format changed in 4.0.0, and a pre-4.0 index withholds relationship evidence and says so rather than answering from an empty graph. Details: https://github.com/shivyadavus/open-kioku/blob/main/CHANGELOG.md

## Try it on a sample repository

```sh
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token
```

## Package layout

`open-kioku` is a small JavaScript wrapper. It installs one platform-specific optional dependency containing the native `ok` binary:

- `@open-kioku/darwin-arm64`
- `@open-kioku/linux-x64`
- `@open-kioku/linux-arm64`
- `@open-kioku/win32-x64`

Since 3.0, macOS builds are Apple Silicon only. On Intel macOS, build from source with `cargo install open-kioku-cli`.

## Links

- Repository: https://github.com/shivyadavus/open-kioku
- Website and setup guides: https://www.openkioku.com/
- Releases (binaries with checksums, SBOM, and provenance): https://github.com/shivyadavus/open-kioku/releases
- Security model: https://github.com/shivyadavus/open-kioku/blob/main/docs/security-model.md

If Open Kioku improves your agent workflow, consider starring the repository.
