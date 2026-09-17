//! Secret-value redaction for data, config and prose files.
//!
//! The file-name rule used to keep `secrets.yaml` or `credentials.json` out of the index
//! entirely, which also hid every key such a file defines (#379). These files are now indexed
//! with every value that looks like a credential replaced by [`REDACTION_MARKER`] before the
//! text reaches the parser. Chunks, symbols, analysis facts, tests and document sections are
//! all derived from the redacted text, and SQLite, Tantivy, snapshot exports, search output and
//! MCP responses are derived from those, so none of them can hold the value.
//!
//! Redaction replaces text inside a line and never adds or removes one: line numbers in
//! evidence still point at the right lines of the file on disk. The rules, in the order they
//! apply, are documented for users in `docs/security-model.md`:
//!
//! 1. A private-key PEM block (`-----BEGIN ... PRIVATE KEY-----`) has every line up to its END
//!    line replaced.
//! 2. A key/value pair whose key names a secret (see [`SECRET_KEY_MARKERS`] and
//!    [`last_word_is_key`]) has its value replaced: `password: x`, `"api_key": "x"`,
//!    `token = "x"`, `SECRET=x`, `--password=x`. When the value is not on the line (a YAML
//!    block scalar, a nested mapping or list, a pretty-printed JSON object or array) every
//!    value indented deeper than the key is replaced, keeping nested keys; an INI or TOML
//!    section whose header names a secret has every value replaced until the next header.
//! 3. The password in a URL's user information: `scheme://user:[REDACTED]@host`.
//! 4. In config and data files only ([`ContentKind::Config`]), any remaining token that looks
//!    machine-generated (see [`is_high_entropy_token`]). Prose keeps rules 1-3 but not this
//!    one, so commit hashes and digests cited in Markdown stay searchable.
//!
//! `null`, booleans, a bare variable reference (`${DB_PASSWORD}`, `$DB_PASSWORD`), and an
//! unquoted number under a quantity-named key (`max_tokens: 4096`, `token_limit: 12`) are not
//! values and stay, so the place a secret is injected remains searchable. A number under a
//! secret-named key that is not a quantity (`password: 123456`) is redacted like any value.

use open_kioku_core::Language;
use std::path::Path;

/// What replaces a redacted value; the same marker `runtime::redact_secrets` writes.
pub const REDACTION_MARKER: &str = "[REDACTED]";

/// Shortest unlabelled token the entropy rule considers. A shorter random value is redacted
/// only when a secret-named key labels it.
pub const ENTROPY_MIN_TOKEN_LEN: usize = 20;

/// Shannon entropy, in bits per character, at or above which a candidate token is redacted.
///
/// Calibrated on 4,000 random tokens per alphabet and length: 24 characters or longer, the rule
/// catches 99.5-100% of base64, base64url, alphanumeric, upper-case-plus-digit,
/// lower-case-plus-digit and hex tokens; at exactly 20 characters it catches 94% of base64
/// (its `+` and `/` can split a token into word-like pieces) and 97-100% of the rest. Over the
/// 1,414 distinct 20-plus-character tokens in this repository's YAML, JSON, TOML, Markdown and
/// text files it redacts every bare hex digest and one other token, a synthetic identifier;
/// the word-like-segment exemption is what keeps paths, URLs, slugs and CamelCase names out,
/// not the threshold.
pub const ENTROPY_MIN_BITS_PER_CHAR: f64 = 3.0;

/// Substrings of a normalized key (ASCII letters and digits only, lower-cased) that mark its
/// value as secret. `password`, `token`, `secret` and `apikey` cover the patterns
/// `runtime::redact_secrets` applies to messages (`api_key` normalizes to `apikey`);
/// `credential` and [`last_word_is_key`] carry over what the file-name rule used to protect by
/// name. `token` does not match inside `tokenizer` or `tokenization`.
pub const SECRET_KEY_MARKERS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "apikey",
    "privatekey",
    "accesskey",
    "authorization",
];

/// Digest lengths per algorithm: hex characters, then unpadded base64 characters.
const DIGEST_LENGTHS: &[(&str, usize, usize)] = &[
    ("md5", 32, 22),
    ("sha1", 40, 27),
    ("sha256", 64, 43),
    ("sha384", 96, 64),
    ("sha512", 128, 86),
];

/// Which rules apply to a file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// YAML, JSON, TOML, Terraform and HCL, Dockerfiles: every rule, the entropy rule included.
    /// A config file has no reason to hold an unlabelled random token.
    Config,
    /// Markdown, plain text, and every document-corpus file: every rule except the entropy
    /// rule, because prose cites commit hashes and digests that must stay searchable.
    Prose,
}

impl ContentKind {
    /// `None` for programming-language source, which is indexed as written. INI, `.env`,
    /// `.properties`, and XML files are not indexed at all (their language is not recognised),
    /// so no rule is chosen for them.
    pub fn for_file(path: &Path, language: &Language) -> Option<Self> {
        if language.is_programming() {
            return None;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let config = matches!(language, Language::Yaml | Language::Json | Language::Toml)
            || matches!(extension.as_str(), "tf" | "tfvars" | "hcl")
            || name == "Dockerfile"
            || name.starts_with("Dockerfile.")
            // A file whose name says it holds credentials gets the config rules whatever its
            // format. The name rule no longer keeps `docs/SECRETS.md` or `secret_key.txt` out
            // of the index, and a token pasted on its own line there carries no key, URL or
            // PEM header for the other rules to match.
            || open_kioku_core::is_secret_named_path(path);
        Some(if config { Self::Config } else { Self::Prose })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedText {
    pub text: String,
    /// Values replaced. Zero means `text` equals the input.
    pub redactions: usize,
}

/// Replaces secret-like values in the content of a data, config or prose file.
pub fn redact_secret_values(content: &str, kind: ContentKind) -> RedactedText {
    let mut redactor = Redactor {
        redactions: 0,
        block: Block::None,
        kind,
    };
    let mut text = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        text.push_str(&redactor.line(body));
        text.push_str(&line[body.len()..]);
    }
    RedactedText {
        text,
        redactions: redactor.redactions,
    }
}

struct Redactor {
    redactions: usize,
    block: Block,
    kind: ContentKind,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Block {
    #[default]
    None,
    /// Between a private-key BEGIN line and its END line.
    PrivateKey,
    /// Lines indented deeper than a secret-named key whose value was not on its line.
    Indented { indent: usize },
    /// Lines after an INI or TOML section header that names a secret.
    Section,
}

impl Redactor {
    fn line(&mut self, line: &str) -> String {
        match self.block {
            Block::PrivateKey => {
                if line.contains("-----END ") {
                    self.block = Block::None;
                    return line.to_string();
                }
                return self.replace_line(line);
            }
            Block::Indented { indent } => {
                if line.trim().is_empty() {
                    return line.to_string();
                }
                if leading_whitespace(line) > indent {
                    return self.redact_values(line, true);
                }
                self.block = Block::None;
            }
            Block::Section => {
                if section_header_name(line).is_none() {
                    return self.redact_values(line, true);
                }
                self.block = Block::None;
            }
            Block::None => {}
        }
        if line.contains("-----BEGIN ")
            && line.contains("PRIVATE KEY-----")
            && !line.contains("-----END ")
        {
            self.block = Block::PrivateKey;
            return line.to_string();
        }
        if let Some(name) = section_header_name(line) {
            if is_secret_key(name) {
                self.block = Block::Section;
            }
            return line.to_string();
        }
        self.redact_values(line, false)
    }

    /// `force` treats every key as secret: the line is inside a secret-named block.
    fn redact_values(&mut self, line: &str, force: bool) -> String {
        let keyed = self.redact_keyed_values(line, force);
        let urls = self.redact_url_passwords(&keyed);
        match self.kind {
            ContentKind::Config => self.redact_high_entropy_tokens(&urls),
            ContentKind::Prose => urls,
        }
    }

    fn replace_line(&mut self, line: &str) -> String {
        let body = line.trim_start();
        if body.is_empty() {
            return line.to_string();
        }
        self.redactions += 1;
        format!("{}{REDACTION_MARKER}", &line[..line.len() - body.len()])
    }

    fn redact_keyed_values(&mut self, line: &str, mut force: bool) -> String {
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut copied = 0;
        let mut found_pair = false;
        let mut at = 0;
        while at < bytes.len() {
            let Some(separator_len) = separator_len(bytes, at) else {
                at += 1;
                continue;
            };
            let after = at + separator_len;
            let Some(key) = key_before(line, at) else {
                at = after;
                continue;
            };
            // YAML needs a space after `:`; JSON's quoted key does not. This is what keeps
            // `https://host` and `a:b` from reading as pairs.
            if bytes[at] == b':'
                && separator_len == 1
                && !key.quoted
                && bytes
                    .get(after)
                    .is_some_and(|next| !matches!(next, b' ' | b'\t'))
            {
                at = after;
                continue;
            }
            found_pair = true;
            let start = skip_blanks(bytes, after);
            if !(force || is_secret_key(key.name)) {
                at = start;
                continue;
            }
            let rest = &line[start..];
            let value = rest.trim_end();
            let value_core = value.strip_suffix(',').unwrap_or(value).trim_end();
            match bytes.get(start).copied() {
                // The value is on the following lines.
                None => {
                    self.open_indented_block(line, force);
                    at = bytes.len();
                }
                Some(b'"' | b'\'') => {
                    let quote = bytes[start];
                    let close = closing_quote(bytes, start + 1, quote).unwrap_or(bytes.len());
                    if !is_non_secret_literal(&line[start + 1..close]) {
                        out.push_str(&line[copied..start + 1]);
                        out.push_str(REDACTION_MARKER);
                        copied = close;
                        self.redactions += 1;
                    }
                    at = (close + 1).min(bytes.len());
                }
                _ if is_non_secret_literal(value_core)
                    || (is_plain_number(value_core) && is_quantity_key(key.name)) =>
                {
                    at = bytes.len();
                }
                Some(b'|' | b'>') if is_block_scalar_indicator(rest) => {
                    self.open_indented_block(line, force);
                    at = bytes.len();
                }
                Some(b'{' | b'[') if value_core.len() == 1 => {
                    self.open_indented_block(line, force);
                    at = bytes.len();
                }
                // An inline mapping keeps its keys; every value inside it is secret.
                Some(b'{') => {
                    force = true;
                    at = start + 1;
                }
                // An inline list under a secret key: every item is a value.
                Some(b'[') => {
                    let end = line
                        .rfind(']')
                        .filter(|end| *end > start)
                        .unwrap_or(bytes.len());
                    if !line[start + 1..end].trim().is_empty() {
                        out.push_str(&line[copied..start + 1]);
                        out.push_str(REDACTION_MARKER);
                        copied = end;
                        self.redactions += 1;
                    }
                    at = end.max(start + 1);
                }
                // Unquoted: everything to the end of the line, a trailing comment included,
                // because a secret may contain ` #`.
                Some(_) => {
                    out.push_str(&line[copied..start]);
                    out.push_str(REDACTION_MARKER);
                    out.push_str(&value[value_core.len()..]);
                    out.push_str(&rest[value.len()..]);
                    copied = bytes.len();
                    self.redactions += 1;
                    at = bytes.len();
                }
            }
        }
        if force && !found_pair {
            return self.redact_bare_value(line);
        }
        out.push_str(&line[copied..]);
        out
    }

    /// Inside a secret block already, the outer block's indent still bounds it.
    fn open_indented_block(&mut self, line: &str, force: bool) {
        if !force {
            self.block = Block::Indented {
                indent: leading_whitespace(line),
            };
        }
    }

    /// A keyless line inside a secret block: a list item, an array element, or a line of a
    /// block scalar.
    fn redact_bare_value(&mut self, line: &str) -> String {
        let body = line.trim();
        let indent = &line[..line.len() - line.trim_start().len()];
        let (dash, item) = match body.strip_prefix("- ") {
            Some(item) => ("- ", item.trim_start()),
            None => ("", body),
        };
        let (item, comma) = match item.strip_suffix(',') {
            Some(item) => (item.trim_end(), ","),
            None => (item, ""),
        };
        let core = item.trim_matches(['"', '\'']);
        if item.starts_with('#')
            || item.starts_with("//")
            || core
                .bytes()
                .all(|byte| matches!(byte, b'{' | b'}' | b'[' | b']' | b','))
            || is_non_secret_literal(core)
        {
            return line.to_string();
        }
        self.redactions += 1;
        format!("{indent}{dash}{REDACTION_MARKER}{comma}")
    }

    fn redact_url_passwords(&mut self, line: &str) -> String {
        if !line.contains("://") {
            return line.to_string();
        }
        let mut out = String::with_capacity(line.len());
        let mut copied = 0;
        let mut from = 0;
        while let Some(found) = line[from..].find("://") {
            let authority_start = from + found + 3;
            let authority_end = line[authority_start..]
                .find(|ch: char| {
                    ch.is_whitespace()
                        || matches!(
                            ch,
                            '/' | '?' | '#' | '"' | '\'' | '`' | '<' | '>' | '(' | ')'
                        )
                })
                .map_or(line.len(), |end| authority_start + end);
            let authority = &line[authority_start..authority_end];
            if let Some(at) = authority.rfind('@') {
                if let Some(colon) = authority[..at].find(':') {
                    let start = authority_start + colon + 1;
                    let end = authority_start + at;
                    if !is_non_secret_literal(&line[start..end]) {
                        out.push_str(&line[copied..start]);
                        out.push_str(REDACTION_MARKER);
                        copied = end;
                        self.redactions += 1;
                    }
                }
            }
            from = authority_end;
        }
        out.push_str(&line[copied..]);
        out
    }

    fn redact_high_entropy_tokens(&mut self, line: &str) -> String {
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut copied = 0;
        let mut at = 0;
        while at < bytes.len() {
            if !is_token_byte(bytes[at]) {
                at += 1;
                continue;
            }
            let start = at;
            while at < bytes.len() && is_token_byte(bytes[at]) {
                at += 1;
            }
            if at - start < ENTROPY_MIN_TOKEN_LEN {
                continue;
            }
            let run = &line[start..at];
            let trimmed = run.trim_start_matches(is_token_separator);
            let core_start = start + (run.len() - trimmed.len());
            let core = trimmed.trim_end_matches(is_token_separator);
            if is_labelled_digest(trimmed, core, &line[..core_start]) {
                continue;
            }
            if is_high_entropy_token(core) {
                out.push_str(&line[copied..core_start]);
                out.push_str(REDACTION_MARKER);
                copied = core_start + core.len();
                self.redactions += 1;
            }
        }
        out.push_str(&line[copied..]);
        out
    }
}

/// A token that looks machine-generated rather than written: at least
/// [`ENTROPY_MIN_TOKEN_LEN`] characters, at least two of lower-case letters, upper-case letters
/// and digits, Shannon entropy of at least [`ENTROPY_MIN_BITS_PER_CHAR`] bits per character,
/// and at least one piece between `+ / = _ -` separators that is not word-like (see
/// [`is_word_like_segment`]). The last condition is what spares `docs/large-java-2026-08-31`,
/// `aarch64-unknown-linux-gnu` and `ConfidenceSignalInput`, whose entropy is as high as a short
/// random key's.
pub fn is_high_entropy_token(token: &str) -> bool {
    if token.len() < ENTROPY_MIN_TOKEN_LEN {
        return false;
    }
    if token
        .split(is_token_separator)
        .filter(|segment| !segment.is_empty())
        .all(is_word_like_segment)
    {
        return false;
    }
    let classes: [fn(&u8) -> bool; 3] = [
        u8::is_ascii_lowercase,
        u8::is_ascii_uppercase,
        u8::is_ascii_digit,
    ];
    let present = classes
        .iter()
        .filter(|class| token.bytes().any(|byte| class(&byte)))
        .count();
    present >= 2 && shannon_entropy(token) >= ENTROPY_MIN_BITS_PER_CHAR
}

/// Digits only; four characters or fewer; or letters in one case, Title case, or camelCase
/// (every capital followed by a lower-case letter), with at most four trailing digits.
fn is_word_like_segment(segment: &str) -> bool {
    if segment.len() <= 4 || segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    let letters = segment.trim_end_matches(|ch: char| ch.is_ascii_digit());
    if letters.is_empty() || segment.len() - letters.len() > 4 {
        return false;
    }
    let bytes = letters.as_bytes();
    bytes.iter().all(u8::is_ascii_alphabetic)
        && (bytes.iter().all(u8::is_ascii_uppercase)
            || bytes.iter().enumerate().all(|(at, byte)| {
                !byte.is_ascii_uppercase() || bytes.get(at + 1).is_some_and(u8::is_ascii_lowercase)
            }))
}

/// A digest that names its algorithm and has exactly that algorithm's length: Subresource
/// Integrity as lockfiles write it (`sha512-<base64>`), a prefixed digest
/// (`image@sha256:<hex>`), or a hex value under a key that ends with the algorithm name
/// (`"sha256": "<hex>"`, `model_sha256 = "<hex>"`). These are public content hashes;
/// redacting them would mark every JavaScript lockfile as holding secrets. A bare hex string
/// with no label is still redacted, since a 40-character hex value can equally be an access
/// token.
fn is_labelled_digest(run: &str, core: &str, preceding: &str) -> bool {
    if let Some((algorithm, digest)) = run.split_once('-') {
        let base64 = digest.trim_end_matches('=');
        if DIGEST_LENGTHS.iter().any(|(name, _, base64_len)| {
            algorithm.eq_ignore_ascii_case(name) && base64.len() == *base64_len
        }) && base64
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        {
            return true;
        }
    }
    let label = preceding.trim_end_matches([' ', '\t', '"', '\'']);
    let Some(label) = label.strip_suffix(':').or_else(|| label.strip_suffix('=')) else {
        return false;
    };
    let label = label.trim_end_matches([' ', '\t', '"', '\'']);
    let key_start = label
        .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
        .map_or(0, |at| at + 1);
    let key = label[key_start..].to_ascii_lowercase();
    DIGEST_LENGTHS.iter().any(|(name, hex_len, _)| {
        key.ends_with(name) && core.len() == *hex_len && core.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn is_secret_key(name: &str) -> bool {
    let normalized = name
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .collect::<String>();
    SECRET_KEY_MARKERS.iter().any(|marker| {
        normalized
            .match_indices(marker)
            .any(|(at, _)| *marker != "token" || !normalized[at + marker.len()..].starts_with("iz"))
    }) || last_word_is_key(name)
}

/// A key whose last word is `key` after at least one other word: `signing_key`, `master-key`,
/// `ssl.key`, `encryptionKey`. The path rule used to block file names ending in `_key`; a bare
/// `key:` (a Kubernetes `secretKeyRef` field, a map entry) is not matched.
fn last_word_is_key(name: &str) -> bool {
    let bytes = name.trim_start_matches('-').as_bytes();
    let Some(split) = bytes.len().checked_sub(3) else {
        return false;
    };
    if split < 2 || !bytes[split..].eq_ignore_ascii_case(b"key") {
        return false;
    }
    let before = bytes[split - 1];
    matches!(before, b'_' | b'-' | b'.')
        || (bytes[split] == b'K' && (before.is_ascii_lowercase() || before.is_ascii_digit()))
}

/// `=`, `:`, `:=` or `=>`; never `==`, `!=`, `<=`, `>=` or `::`. Returns the separator length.
fn separator_len(bytes: &[u8], at: usize) -> Option<usize> {
    let previous = at.checked_sub(1).map(|index| bytes[index]);
    let next = bytes.get(at + 1).copied();
    match bytes[at] {
        b'=' => match (previous, next) {
            (Some(b'=' | b'!' | b'<' | b'>' | b':'), _) | (_, Some(b'=')) => None,
            (_, Some(b'>')) => Some(2),
            _ => Some(1),
        },
        b':' => match (previous, next) {
            (Some(b':'), _) | (_, Some(b':')) => None,
            (_, Some(b'=')) => Some(2),
            _ => Some(1),
        },
        _ => None,
    }
}

struct Key<'a> {
    name: &'a str,
    quoted: bool,
}

fn key_before(line: &str, separator: usize) -> Option<Key<'_>> {
    let bytes = line.as_bytes();
    let mut end = separator;
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    let quote = end
        .checked_sub(1)
        .map(|index| bytes[index])
        .filter(|byte| matches!(byte, b'"' | b'\''));
    let name_end = if quote.is_some() { end - 1 } else { end };
    let mut start = name_end;
    while start > 0 && is_key_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == name_end {
        return None;
    }
    if let Some(quote) = quote {
        if start == 0 || bytes[start - 1] != quote {
            return None;
        }
    }
    Some(Key {
        name: &line[start..name_end],
        quoted: quote.is_some(),
    })
}

fn section_header_name(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim_start_matches('[').trim_end_matches(']').trim();
    (!inner.is_empty() && inner.bytes().all(|byte| is_key_byte(byte) || byte == b'"'))
        .then_some(inner)
}

fn closing_quote(bytes: &[u8], from: usize, quote: u8) -> Option<usize> {
    let mut at = from;
    while at < bytes.len() {
        if quote == b'"' && bytes[at] == b'\\' {
            at += 2;
            continue;
        }
        if bytes[at] == quote {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// `|`, `>`, `|-`, `>+`, `|2`: a YAML block scalar header, optionally followed by a comment.
fn is_block_scalar_indicator(value: &str) -> bool {
    let indicator = value.split(" #").next().unwrap_or_default().trim_end();
    let mut chars = indicator.chars();
    indicator.len() <= 3
        && matches!(chars.next(), Some('|' | '>'))
        && chars.all(|ch| matches!(ch, '+' | '-' | '1'..='9'))
}

/// Empty, `null`, a boolean, a bare variable reference, or the marker itself (so redacting
/// redacted text changes nothing).
fn is_non_secret_literal(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value == REDACTION_MARKER
        || ["null", "~", "true", "false", "none", "nil"]
            .iter()
            .any(|literal| value.eq_ignore_ascii_case(literal))
        || is_variable_reference(value)
}

/// Words that make a secret-named key a quantity rather than the secret itself: `max_tokens`,
/// `token_limit`, `token_ttl_seconds`, `password_min_length`. Without this every number under
/// such a key would be redacted; with it, a numeric secret (`password: 123456`,
/// `access_token: 9876543210`) still is, because none of these words appears in its key.
fn is_quantity_key(name: &str) -> bool {
    let normalized = name
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .collect::<String>();
    [
        "max",
        "min",
        "limit",
        "count",
        "size",
        "length",
        "len",
        "ttl",
        "timeout",
        "expiry",
        "expires",
        "seconds",
        "minutes",
        "hours",
        "days",
        "bytes",
        "port",
        "retry",
        "retries",
        "interval",
        "budget",
        "threshold",
        "rotation",
        "version",
        "window",
        "quota",
        "tokens",
    ]
    .iter()
    .any(|word| normalized.contains(word))
}

/// An unquoted integer or float (`4096`, `-3.5`, `1e6`, `1_000`): a typed number such as a
/// limit or a port, not a credential string. A quoted string of digits is still a value, and a
/// number is only kept under a key [`is_quantity_key`] recognises.
fn is_plain_number(value: &str) -> bool {
    let digits = |part: &str| {
        part.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'_')
    };
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let mantissa_is_number = match mantissa.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(mantissa),
    };
    mantissa_is_number
        && exponent
            .is_none_or(|exponent| digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent)))
}

fn is_variable_reference(value: &str) -> bool {
    let name = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| value.strip_prefix('$'));
    name.is_some_and(|name| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

fn leading_whitespace(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn skip_blanks(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    at
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn is_token_separator(ch: char) -> bool {
    matches!(ch, '+' | '/' | '=' | '_' | '-')
}

fn shannon_entropy(token: &str) -> f64 {
    let mut counts = [0usize; 256];
    for byte in token.bytes() {
        counts[usize::from(byte)] += 1;
    }
    let len = token.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let share = *count as f64 / len;
            -share * share.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const UPPER_DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

    /// Credential-shaped test values are built at run time so that no string in the repository
    /// matches a real provider's key format or reads as a leaked secret to a scanner.
    fn striding(alphabet: &[u8], len: usize, stride: usize, offset: usize) -> String {
        (0..len)
            .map(|index| char::from(alphabet[(index * stride + offset) % alphabet.len()]))
            .collect()
    }

    fn pseudo_random(alphabet: &[u8], len: usize, seed: u64) -> String {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                char::from(alphabet[((state >> 33) % alphabet.len() as u64) as usize])
            })
            .collect()
    }

    fn redacted(text: &str) -> String {
        redact_secret_values(text, ContentKind::Config).text
    }

    #[test]
    fn secret_named_keys_lose_their_values_in_every_config_syntax() {
        let value = striding(ALNUM, 12, 7, 3);
        let cases = [
            format!("password: {value}\n"),
            format!("  db_password: \"{value}\"\n"),
            format!("{{\"apiKey\": \"{value}\", \"region\": \"north\"}}\n"),
            format!("client_secret = '{value}'\n"),
            format!("export GITHUB_TOKEN={value}\n"),
            format!("RUN tool --password={value}\n"),
            format!("<connection password=\"{value}\" />\n"),
            format!("signing_key: {value}\n"),
            format!("encryptionKey := {value}\n"),
            format!("\"authorization\" => \"Bearer {value}\",\n"),
        ];
        for case in cases {
            let result = redact_secret_values(&case, ContentKind::Config);
            assert!(!result.text.contains(&value), "{case} -> {}", result.text);
            assert!(result.text.contains(REDACTION_MARKER), "{}", result.text);
            assert_eq!(result.redactions, 1, "{case} -> {}", result.text);
        }
    }

    #[test]
    fn keys_stay_searchable_and_structure_survives() {
        let value = striding(ALNUM, 12, 7, 3);
        let text = format!(
            "database:\n  host: db.internal\n  password: \"{value}\" # rotate\n  port: 5432\n"
        );
        assert_eq!(
            redacted(&text),
            "database:\n  host: db.internal\n  password: \"[REDACTED]\" # rotate\n  port: 5432\n"
        );
        let json = format!("{{\n  \"token\": \"{value}\",\n  \"retries\": 3\n}}\n");
        assert_eq!(
            redacted(&json),
            "{\n  \"token\": \"[REDACTED]\",\n  \"retries\": 3\n}\n"
        );
    }

    #[test]
    fn every_message_redaction_pattern_is_also_a_content_key() {
        for pattern in crate::runtime::MESSAGE_SECRET_KEY_PATTERNS {
            let key = pattern.trim_end_matches('=');
            let text = format!("service_{key}: plain-value\n");
            assert_eq!(
                redacted(&text),
                format!("service_{key}: [REDACTED]\n"),
                "`{pattern}` redacts messages but not content"
            );
        }
    }

    #[test]
    fn values_nested_under_a_secret_key_are_redacted_and_their_keys_kept() {
        let first = striding(ALNUM, 10, 5, 1);
        let second = striding(ALNUM, 10, 11, 2);
        let yaml = format!(
            "credentials:\n  prod: {first}\n  staging:\n    user: deploy\n  list:\n    - {second}\nregion: north\n"
        );
        assert_eq!(
            redacted(&yaml),
            "credentials:\n  prod: [REDACTED]\n  staging:\n    user: [REDACTED]\n  list:\n    - [REDACTED]\nregion: north\n"
        );
        let json = format!("{{\n  \"secrets\": [\n    \"{first}\",\n    \"{second}\"\n  ],\n  \"name\": \"svc\"\n}}\n");
        assert_eq!(
            redacted(&json),
            "{\n  \"secrets\": [\n    [REDACTED],\n    [REDACTED]\n  ],\n  \"name\": \"svc\"\n}\n"
        );
        let block = format!("private_key: |\n  {first}\n  {second}\nname: svc\n");
        assert_eq!(
            redacted(&block),
            "private_key: |\n  [REDACTED]\n  [REDACTED]\nname: svc\n"
        );
        let inline = format!("credentials = {{ token = \"{first}\", user = \"{second}\" }}\n");
        let result = redacted(&inline);
        assert!(
            !result.contains(&first) && !result.contains(&second),
            "{result}"
        );
    }

    #[test]
    fn secret_named_sections_redact_every_value_until_the_next_header() {
        let value = striding(ALNUM, 10, 5, 1);
        let toml = format!("[server]\nport = 8080\n[credentials.prod]\nuser = \"{value}\"\n[client]\nname = \"cli\"\n");
        assert_eq!(
            redacted(&toml),
            "[server]\nport = 8080\n[credentials.prod]\nuser = \"[REDACTED]\"\n[client]\nname = \"cli\"\n"
        );
    }

    #[test]
    fn private_key_blocks_and_url_passwords_are_redacted() {
        let body = striding(ALNUM, 16, 3, 0);
        let pem = format!(
            "tls:\n  cert: |\n    -----BEGIN PRIVATE KEY-----\n    {body}\n    -----END PRIVATE KEY-----\n"
        );
        let result = redacted(&pem);
        assert!(!result.contains(&body), "{result}");
        assert!(result.contains("-----BEGIN PRIVATE KEY-----"), "{result}");

        let password = striding(ALNUM, 9, 5, 4);
        let url = format!("database_url: postgres://app:{password}@db.internal:5432/orders\n");
        // `database_url` is not a secret-named key, so the URL rule alone must catch it.
        assert_eq!(
            redacted(&url),
            "database_url: postgres://app:[REDACTED]@db.internal:5432/orders\n"
        );
    }

    #[test]
    fn high_entropy_tokens_are_redacted_without_a_label() {
        let alphabets: [&[u8]; 4] = [
            ALNUM,
            UPPER_DIGITS,
            b"abcdefghijklmnopqrstuvwxyz0123456789",
            b"0123456789abcdef",
        ];
        for (index, alphabet) in alphabets.into_iter().enumerate() {
            for seed in 0..50u64 {
                let token = pseudo_random(alphabet, 40, seed * 31 + index as u64);
                let text = format!("callback_nonce: {token}\n");
                let result = redact_secret_values(&text, ContentKind::Config);
                assert_eq!(
                    result.text, "callback_nonce: [REDACTED]\n",
                    "missed {token}"
                );
            }
        }
        let cloud_key_shaped = format!(
            "{}{}",
            ["OK", "CK"].concat(),
            striding(UPPER_DIGITS, 16, 7, 3)
        );
        assert!(
            is_high_entropy_token(&cloud_key_shaped),
            "{cloud_key_shaped}"
        );
    }

    #[test]
    fn written_tokens_digests_and_references_are_not_redacted() {
        let hex64 = striding(b"0123456789abcdef", 64, 7, 1);
        let sri = format!("sha512-{}==", striding(ALNUM, 86, 7, 5));
        let text = format!(
            "path: docs/large-java-validation-2026-08-31.md\n\
             target: aarch64-unknown-linux-gnu\n\
             type: ConfidenceSignalInput\n\
             link: https://github.com/example-org/example-repo/issues/329\n\
             integrity: {sri}\n\
             image: registry.example/app@sha256:{hex64}\n\
             \"sha256\": \"{hex64}\"\n\
             password: ${{DB_PASSWORD}}\n\
             token: null\n\
             max_tokenizer_threads: 4\n\
             password: [REDACTED]\n"
        );
        let result = redact_secret_values(&text, ContentKind::Config);
        assert_eq!(result.text, text);
        assert_eq!(result.redactions, 0);
    }

    #[test]
    fn unlabelled_hex_is_redacted_conservatively() {
        let hex40 = striding(b"0123456789abcdef", 40, 7, 1);
        assert_eq!(
            redacted(&format!("base_commit: {hex40}\n")),
            "base_commit: [REDACTED]\n"
        );
    }

    #[test]
    fn prose_keeps_cited_hashes_but_not_labelled_secrets() {
        let commit = striding(b"0123456789abcdef", 40, 7, 1);
        let digest = format!("{}=", striding(ALNUM, 43, 7, 5));
        let changelog = format!("- Measured at commit `{commit}`; artifact digest {digest}.\n");
        let result = redact_secret_values(&changelog, ContentKind::Prose);
        assert_eq!(result.text, changelog);
        assert_eq!(result.redactions, 0);
        // In a config file nothing labels either value, so both are redacted.
        assert_ne!(redacted(&changelog), changelog);

        let value = striding(ALNUM, 12, 7, 3);
        assert_eq!(
            redact_secret_values(&format!("api_key: {value}\n"), ContentKind::Prose).text,
            "api_key: [REDACTED]\n"
        );
        let password = striding(ALNUM, 9, 5, 4);
        assert_eq!(
            redact_secret_values(
                &format!("Connect with postgres://app:{password}@db.internal/orders\n"),
                ContentKind::Prose
            )
            .text,
            "Connect with postgres://app:[REDACTED]@db.internal/orders\n"
        );
    }

    #[test]
    fn a_number_is_kept_only_where_its_key_reads_as_a_quantity() {
        let quantities = "max_tokens: 4096\ntoken_limit: 12\ntoken_ttl_seconds: 3.5\n\"token_budget\": 1e6,\ncredentials:\n  port: 5432\n";
        let result = redact_secret_values(quantities, ContentKind::Config);
        assert_eq!(result.text, quantities);
        assert_eq!(result.redactions, 0);

        // A number under a secret-named key that names no quantity is the secret itself.
        assert_eq!(redacted("password: 123456\n"), "password: [REDACTED]\n");
        assert_eq!(
            redacted("access_token: 9876543210\n"),
            "access_token: [REDACTED]\n"
        );
        // Inside a secret-named block a bare item has no key to read as a quantity.
        assert_eq!(
            redacted("credentials:\n  - 8080\n"),
            "credentials:\n  - [REDACTED]\n"
        );
        // A quoted string of digits is a string, and stays a value.
        assert_eq!(
            redacted("password: \"123456\"\n"),
            "password: \"[REDACTED]\"\n"
        );
    }

    /// Two narrowings that are each safe alone compose into a hole: the path rule no longer
    /// keeps `docs/SECRETS.md` out of the index, and prose does not run the entropy rule, so a
    /// pasted token with no key, URL or PEM header around it would be indexed as written.
    #[test]
    fn a_secret_named_prose_file_is_read_under_the_config_rules() {
        let token = striding(ALNUM, 32, 17, 5);
        let pasted = format!("# Key rotation\n\n{token}\n");

        // Ordinary prose keeps a bare token: it is as likely a commit hash or a digest.
        assert_eq!(
            redact_secret_values(&pasted, ContentKind::Prose).text,
            pasted
        );

        // The file's name is what decides, for Markdown and text as much as for YAML.
        for named in ["docs/SECRETS.md", "notes/credentials.md", "secret_key.txt"] {
            assert_eq!(
                ContentKind::for_file(Path::new(named), &Language::Markdown),
                Some(ContentKind::Config),
                "{named}"
            );
        }
        assert_eq!(
            ContentKind::for_file(Path::new("docs/architecture.md"), &Language::Markdown),
            Some(ContentKind::Prose)
        );
        let redacted_prose = redact_secret_values(&pasted, ContentKind::Config);
        assert!(!redacted_prose.text.contains(&token), "{redacted_prose:?}");
    }

    #[test]
    fn content_kind_follows_the_file_format() {
        let kind =
            |path: &str, language: Language| ContentKind::for_file(Path::new(path), &language);
        assert_eq!(
            kind("config/app.yaml", Language::Yaml),
            Some(ContentKind::Config)
        );
        assert_eq!(
            kind("infra/main.tf", Language::Text),
            Some(ContentKind::Config)
        );
        assert_eq!(
            kind("Dockerfile", Language::Text),
            Some(ContentKind::Config)
        );
        assert_eq!(
            kind("CHANGELOG.md", Language::Markdown),
            Some(ContentKind::Prose)
        );
        assert_eq!(kind("notes.txt", Language::Text), Some(ContentKind::Prose));
        assert_eq!(kind("src/lib.rs", Language::Rust), None);
    }

    #[test]
    fn line_numbers_and_line_endings_are_preserved() {
        let value = striding(ALNUM, 12, 7, 3);
        let text = format!("a: 1\r\npassword: {value}\r\n\r\nb: 2");
        let result = redacted(&text);
        assert_eq!(result, "a: 1\r\npassword: [REDACTED]\r\n\r\nb: 2");
        assert_eq!(result.lines().count(), text.lines().count());
    }
}
