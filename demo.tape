Output demo.gif

Require cargo
Require target/release/ok

Set FontSize 16
Set FontFamily "JetBrains Mono"
Set Width 1200
Set Height 800
Set Padding 40
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0

# Initial setup
Hide
Type "cargo build --release -p open-kioku-cli"
Enter
Sleep 3s
Type "export PATH=$PWD/target/release:$PATH"
Enter
Type "clear"
Enter
Show

Sleep 1s
Type "# Welcome to Open Kioku - The Local-First Code Intelligence MCP"
Enter
Sleep 1s

# 1. Initialization
Type "clear"
Enter
Type "# 1. Lightning fast local indexing"
Enter
Sleep 500ms
Type "ok init ."
Enter
Sleep 3s

# 2. Doctor Check
Type "clear"
Enter
Type "# 2. Built-in environment & health verification"
Enter
Sleep 500ms
Type "ok doctor"
Enter
Sleep 4s

# 3. Context Packs
Type "clear"
Enter
Type "# 3. Generate high-quality context packs for LLMs"
Enter
Sleep 500ms
Type "ok context 'Add a new JSON export format for tools' --format markdown | head -n 30"
Enter
Sleep 4s

# 4. Search with Evidence
Type "clear"
Enter
Type "# 4. Powerful structural and regex search with evidence"
Enter
Sleep 500ms
Type "ok search 'ContextPackFormat'"
Enter
Sleep 4s

# 5. Benchmarks
Type "clear"
Enter
Type "# 5. Blazing fast speeds (built in Rust)"
Enter
Sleep 500ms
Type "ok bench ."
Enter
Sleep 4s

# 6. MCP Tools list
Type "clear"
Enter
Type "# 6. Rich Model Context Protocol (MCP) tool integration"
Enter
Sleep 500ms
Type "ok mcp tools/list --hide-experimental"
Enter
Sleep 5s

Type "clear"
Enter
Type "# Try it out today!"
Enter
Sleep 3s
