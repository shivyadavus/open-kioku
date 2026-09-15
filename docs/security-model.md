# Security Model

Default posture:

- read-only MCP mode
- no shell execution
- no network access
- no file writes
- no hidden-file scanning
- deny `.env` / `.env.*`, `.aws/**`, `.ssh/**`, `id_rsa*`, `id_ed25519*`, and key material
  (`*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`) on every path, whatever the
  file's language (`is_secret_like_path`). A file merely named for a secret is indexed:
  `secrets.yaml`, `credentials.json`, or `SECRETS.md` with its secret-like values replaced
  (see "Secret-value redaction" below), and `secrets.go` as written; parser messages that
  would quote file content are redacted. `[paths] deny` excludes any other path, and the
  default configuration denies `**/secrets/**`
- redact-capable output boundary
- source edits occur in the user's normal editor

Policy is enforced by `open-kioku-actions::PolicyGate`. Commands must exactly match configured allowlist entries. `open-kioku-sandbox` captures output and applies timeouts only after policy allows execution.

Contract verification uses the same exact command allowlist before running validation commands. When attestation writing is requested, each executed or denied validation command records cwd, timestamps, exit code, allowlist status, normalized outcome, and bounded stdout/stderr summaries in a validation ledger under `.ok/contracts/validation/`.

Patch planning is available in read-only mode because it produces a plan, evidence, risks, tests, and a boundary. Open Kioku exposes no MCP source-editing tool; approved patches are applied with the user's normal editor and then verified against the plan.

## Secret-value redaction

Files in a data, config, or prose format (YAML, JSON, TOML, Markdown, plain text, and every document-corpus file) are indexed with secret-like values replaced by `[REDACTED]` before anything is derived from their text (`open-kioku-ingest::redaction`). Chunks, symbols, analysis facts, test candidates, and document sections are built from the redacted text, and `.ok/index.sqlite`, the Tantivy index, `ok snapshot export` artifacts, `ok search` output, and MCP results are built from those, so none of them holds the value. Redaction changes text within a line and never adds or removes a line, so evidence line ranges still match the file on disk. Programming-language source is indexed as written: a credential hard-coded in a `.rs` or `.py` file is searchable, as it was before.

A value is replaced when:

1. **Its key names a secret.** The key, reduced to lower-case letters and digits, contains `password`, `passwd`, `passphrase`, `secret`, `token` (not inside `tokenizer`), `credential`, `apikey`, `privatekey`, `accesskey`, or `authorization`, or its last word is `key` (`signing_key`, `encryptionKey`). This covers `key: value`, `"key": "value"`, `key = "value"`, `KEY=value`, `--key=value`, `key := value`, and `key => value`. A quoted value is replaced inside its quotes, an unquoted one to the end of the line, comment included. When the value is not on the key's line (a YAML block scalar, a nested mapping or list, a pretty-printed JSON object or array), every value indented deeper than the key is replaced and the nested keys are kept; in an INI or TOML section whose header names a secret, every value up to the next header is replaced.
2. **It is a private-key PEM block**: every line between `-----BEGIN ... PRIVATE KEY-----` and its END line.
3. **It is the password of a URL**: `scheme://user:password@host` becomes `scheme://user:[REDACTED]@host`.
4. **It looks machine-generated**: a run of `A-Z a-z 0-9 + / = _ -` at least 20 characters long (separators trimmed from its ends) that uses at least two of lower-case letters, upper-case letters, and digits, has Shannon entropy of at least 3.0 bits per character, and is not made only of word-like pieces between `+ / = _ -`. A piece is word-like when it is all digits, at most four characters, or letters in one case, Title case, or camelCase followed by at most four digits; this is what keeps paths, URLs, slugs, and names such as `aarch64-unknown-linux-gnu` or `ConfidenceSignalInput` readable. Calibrated on 4,000 random tokens per alphabet and length: at 24 characters or more the rule catches 99.5-100% of base64, base64url, alphanumeric, upper-case-plus-digit, lower-case-plus-digit, and hex tokens; at exactly 20 characters it catches 94% of base64 and 97-100% of the rest.

Not replaced: empty values, `null`, `~`, booleans, a bare variable reference (`${DB_PASSWORD}`, `$DB_PASSWORD`, so where a secret is injected stays searchable), and a digest that names its algorithm at exactly that algorithm's length: `sha512-<base64>` (Subresource Integrity, as lockfiles write it), `sha256:<hex>`, or hex under a key ending in the algorithm name (`"sha256": "<hex>"`). An unlabelled hex string such as a commit hash is replaced, because a 40-character hex value can equally be an access token.

Limits:

- A random value shorter than 20 characters, or a passphrase made of words, is replaced only under a secret-named key. A value in a Markdown table cell or an XML element body has no key these rules recognise.
- `File.content_hash` is the SHA-256 of the file as read, used to detect changes between indexes. It does not contain the value, but a low-entropy secret in a very small file could be confirmed by guessing the whole file.
- Re-indexing an index built before redaction replaces its rows, but SQLite can keep the bytes of deleted rows in free pages until they are reused. Remove `.ok/` and run `ok index` to be certain none remain. `ok snapshot export` compacts through `VACUUM INTO` by default; `--quality fast` copies the database file as it is.

`ok index`, `ok status`, and `ok doctor` report how many files had values replaced, and `ok --json status` and MCP `repo_status` carry the count as `quality.redacted_files`. A manifest written before redaction existed has no count: `ok status` says it is not recorded, and `ok doctor` warns that the index stored those files as read and says to run `ok index`.

For the agent-facing threat model, including prompt injection, memory poisoning, MCP over-permissioning, and context-handle handling, see [`docs/guides/security-threat-model.md`](guides/security-threat-model.md).
