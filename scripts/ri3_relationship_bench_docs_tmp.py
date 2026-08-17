from pathlib import Path

path = Path("README.md")
text = path.read_text()
old = "Current top-level commands (37): `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `contract`, `bench`, `workflow-bench`, `retrieval-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`."
new = "Current top-level commands (38): `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `contract`, `bench`, `workflow-bench`, `retrieval-bench`, `relationship-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`."
if text.count(old) != 1:
    raise SystemExit(f"README command inventory seam changed: {text.count(old)}")
path.write_text(text.replace(old, new, 1))
