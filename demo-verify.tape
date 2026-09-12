# Renders assets/demo-verify.gif with VHS (https://github.com/charmbracelet/vhs):
#
#   vhs demo-verify.tape
#
# Short cut of demo.tape: same throwaway copy of the retrieval fixture, same
# task, same two edits. Setup and `ok plan` run hidden so the visible part
# (about 12 seconds) starts with the boundary already written:
# boundary -> edit inside it -> edit outside it -> `ok verify` fails on the
# outside file with a non-zero exit. `pipefail` is set in the hidden block so
# the `exit` line reports ok's status, not grep's.

Output assets/demo-verify.gif

Require ok
Require jq

Set FontSize 15
Set FontFamily "JetBrains Mono, SF Mono, Menlo, DejaVu Sans Mono, Liberation Mono, monospace"
Set Width 1180
Set Height 460
Set Padding 28
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set TypingSpeed 25ms
Set PlaybackSpeed 1.0

# Hidden: stage the fixture, index it, write the plan.
Hide
Type `export OK_DEMO="${TMPDIR:-/tmp}/ok-demo" && rm -rf "$OK_DEMO" && cp -R benchmarks/retrieval-fixture "$OK_DEMO" && cd "$OK_DEMO" && git init -q && git add -A && git -c user.name=demo -c user.email=demo@example.com commit -qm base && set -o pipefail && export PS1='$ ' && clear`
Enter
Sleep 2s
Type "ok setup agent claude --repo . --apply >/dev/null 2>&1"
Enter
Sleep 6s
Type 'ok plan "add token expiry to TokenStore saveDigest" --format json > plan.json'
Enter
Sleep 3s
Type "clear"
Enter
Sleep 500ms
Show

# 1. The boundary the plan declared.
Type "jq -r '.recommended_change_boundary.allowed_files[]' plan.json"
Enter
Sleep 1200ms

# 2. One edit inside the boundary, one drive-by outside it.
Type 'echo "// expiry: TODO" >> java/src/com/acme/auth/TokenStore.java'
Enter
Sleep 400ms
Type 'echo "// drive-by" >> go/shipping/carrier.go'
Enter
Sleep 600ms

# 3. Verify the real diff against the declared boundary.
Type `ok verify --plan plan.json --git | grep -E '^Verification|out_of_boundary'; echo "exit $?"`
Enter
Sleep 2500ms
