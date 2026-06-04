# Compressed Context And TOON

Open Kioku has two prompt-size controls:

- Reversible compressed context handles store original snippets locally in `.ok/context.sqlite`.
- TOON renders context and plans in a compact prompt-oriented text format.

Use compressed context when an agent needs many files but only a few may need expansion:

```sh
ok --repo /path/to/repo context "update auth validation" --compressed --format toon
ok --repo /path/to/repo retrieve-context ctx:...
```

Use TOON when the result is going directly into an LLM prompt:

```sh
ok --repo /path/to/repo plan "update auth validation" --format toon
ok --repo /path/to/repo context "update auth validation" --format toon
```

Use JSON when another tool needs structured data:

```sh
ok --repo /path/to/repo --json context "update auth validation" --compressed
ok --repo /path/to/repo --json plan "update auth validation"
```

Rules of thumb:

- JSON is the internal and MCP structured format.
- TOON is for prompt handoff.
- Compressed handles are reversible, local, and should not be treated as public URLs.
- If an agent needs exact code, retrieve the handle instead of asking it to infer from a summary.
