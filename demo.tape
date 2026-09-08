# Renders assets/demo.gif with VHS (https://github.com/charmbracelet/vhs):
#
#   vhs demo.tape
#
# Run from the repository root with `ok` (release build or an installed
# package) and `jq` on PATH. The session runs against a throwaway copy of the
# 39-file retrieval fixture so the whole recording stays under 45 seconds:
# setup -> context (evidence + confidence) -> plan (boundary) -> an edit
# inside the boundary plus one outside it -> verify fails on the outside file.

Output assets/demo.gif

Require ok
Require jq

Set FontSize 15
Set FontFamily "JetBrains Mono, SF Mono, Menlo, DejaVu Sans Mono, Liberation Mono, monospace"
Set Width 1180
Set Height 700
Set Padding 28
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set TypingSpeed 35ms
Set PlaybackSpeed 1.0

# Throwaway working copy (hidden): the fixture with a single base commit.
Hide
Type `export OK_DEMO="${TMPDIR:-/tmp}/ok-demo" && rm -rf "$OK_DEMO" && cp -R benchmarks/retrieval-fixture "$OK_DEMO" && cd "$OK_DEMO" && git init -q && git add -A && git -c user.name=demo -c user.email=demo@example.com commit -qm base && export PS1='$ ' && clear`
Enter
Sleep 2s
Show

# 1. One command: index, install repo-scoped MCP config + guidance, probe the server.
Type "ok setup agent claude --repo . --apply"
Enter
Sleep 4s

# 2. Evidence before edits: streams attempted vs. succeeded, confidence, caveats, files.
Type "clear"
Enter
Type `ok context "add token expiry to TokenStore saveDigest" --format markdown | grep -vE 'weight|budget|Routing|family|Components|^$' | head -n 22`
Enter
Sleep 6s

# 3. The plan declares an edit boundary.
Type "clear"
Enter
Type 'ok plan "add token expiry to TokenStore saveDigest" --format json > plan.json'
Enter
Sleep 2s
Type "jq '.recommended_change_boundary.allowed_files' plan.json"
Enter
Sleep 4s

# 4. One edit inside the boundary, one drive-by outside it.
Type "clear"
Enter
Type 'echo "// expiry: TODO" >> java/src/com/acme/auth/TokenStore.java'
Enter
Type 'echo "// drive-by" >> go/shipping/carrier.go'
Enter
Sleep 1s

# 5. Verify the real diff against the declared boundary.
Type "ok verify --plan plan.json --git --format json | jq '{verdict, changed_files, boundary_violations}'"
Enter
Sleep 7s
