//! Where the content of a unified diff's hunks begins and ends.
//!
//! A hunk body is exactly as long as its `@@ -a,b +c,d @@` header says (an omitted count is 1).
//! Inside it every line is content whatever it starts with, so a removed `-- x` or an added
//! `++ y`, rendered `--- x` and `+++ y`, never names a file. A diff whose bodies do not match
//! their headers (truncated, hand-edited, or with a count that does not parse) cannot be split
//! into files reliably: an over-counted hunk swallows the next entry's headers and an
//! under-counted one leaves content to be read as headers. [`HunkScanner`] records the first
//! such place so callers report it instead of silently losing or inventing a path.

use std::borrow::Cow;
use std::fmt;

/// The first place a diff stopped matching its hunk headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedDiff {
    /// 1-based line of the diff at which the mismatch became visible.
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for MalformedDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

/// How one line of a diff reads once hunk counts are taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLine<'a> {
    /// A line inside a hunk body, including its `\ No newline at end of file` markers.
    Content,
    /// A `@@ ` hunk header; the text after `@@ `.
    HunkHeader(&'a str),
    /// Anything else: a file or extended header, or text between entries.
    Header,
}

/// Classifies a diff's lines in order. Classification never stops at a malformation: the
/// offending line is read as a header, as a parser without counts would, and the first
/// malformation is kept for [`finish`](Self::finish) or [`malformation`](Self::malformation).
#[derive(Debug, Default)]
pub struct HunkScanner {
    line: usize,
    old: u32,
    new: u32,
    /// The previous line ended a hunk body.
    after_hunk: bool,
    /// The previous line was a `--- ` file header, so this one must be its `+++ `.
    after_old_marker: bool,
    malformed: Option<MalformedDiff>,
}

impl HunkScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan<'a>(&mut self, line: &'a str) -> DiffLine<'a> {
        self.line += 1;
        if self.old > 0 || self.new > 0 {
            if self.take(line) {
                return DiffLine::Content;
            }
            let reason = self.missing_lines("the hunk ends before");
            self.flag(reason);
            self.old = 0;
            self.new = 0;
        }
        let after_hunk = std::mem::take(&mut self.after_hunk);
        let after_old_marker = std::mem::take(&mut self.after_old_marker);
        if after_old_marker && !line.starts_with("+++ ") {
            self.flag("a `---` file header is not followed by its `+++` header".into());
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            match hunk_counts(header) {
                Some((old, new)) => {
                    self.old = old;
                    self.new = new;
                    self.after_hunk = old == 0 && new == 0;
                }
                None => self.flag(format!(
                    "hunk header `@@ {} @@` does not parse",
                    header_ranges(header)
                )),
            }
            return DiffLine::HunkHeader(header);
        }
        if after_hunk && line.starts_with('\\') {
            self.after_hunk = true;
            return DiffLine::Content;
        }
        if line.starts_with("--- ") {
            self.after_old_marker = true;
        } else if line.starts_with("+++ ") {
            if !after_old_marker {
                self.flag("a `+++` line is outside every hunk and follows no `---` header".into());
            }
        } else if after_hunk
            && matches!(line.as_bytes().first(), Some(b'+' | b'-' | b' '))
            // The signature separator `git format-patch` writes after the last hunk.
            && line != "-- "
            && line != "--"
        {
            self.flag("a content line follows a hunk whose header did not count it".into());
        }
        DiffLine::Header
    }

    /// The first malformation seen so far.
    pub fn malformation(&self) -> Option<&MalformedDiff> {
        self.malformed.as_ref()
    }

    /// Ends the diff, reporting a hunk the input stopped inside or the first earlier
    /// malformation.
    pub fn finish(mut self) -> Result<(), MalformedDiff> {
        if self.old > 0 || self.new > 0 {
            let reason = self.missing_lines("the diff ends before");
            self.flag(reason);
        } else if self.after_old_marker {
            self.flag("the diff ends after a `---` file header with no `+++` header".into());
        }
        match self.malformed {
            Some(malformed) => Err(malformed),
            None => Ok(()),
        }
    }

    fn missing_lines(&self, prefix: &str) -> String {
        format!(
            "{prefix} its header's count is reached ({} removed and {} added lines missing)",
            self.old, self.new
        )
    }

    fn flag(&mut self, reason: String) {
        if self.malformed.is_none() {
            self.malformed = Some(MalformedDiff {
                line: self.line,
                reason,
            });
        }
    }

    /// Counts `line` off the open hunk if the remaining counts can place it.
    fn take(&mut self, line: &str) -> bool {
        match line.as_bytes().first() {
            Some(b'-') if self.old > 0 => self.old -= 1,
            Some(b'+') if self.new > 0 => self.new -= 1,
            Some(b' ') | None if self.old > 0 && self.new > 0 => {
                self.old -= 1;
                self.new -= 1;
            }
            Some(b'\\') => {}
            _ => return false,
        }
        self.after_hunk = self.old == 0 && self.new == 0;
        true
    }
}

/// The name a `--- ` or `+++ ` file header gives, still quoted if git quoted it. Git appends a
/// tab to an unquoted name that holds a space, and `diff -u` a tab and a timestamp, so an
/// unquoted name ends at the first tab.
pub fn file_header_name(value: &str) -> &str {
    // `str::lines` keeps the `\r` of a CRLF diff's last line when it has no final newline.
    let value = value.trim_end_matches('\r');
    if value.starts_with('"') {
        return value.trim_end_matches('\t');
    }
    match value.split_once('\t') {
        Some((name, _)) => name,
        // Git always appends a TAB after a name containing a space, so a header without one
        // came from a hand edit or another tool; trailing spaces there are not part of a path.
        None => value.trim_end_matches(' '),
    }
}

/// The old and new paths a `diff --git <old> <new>` line names, given the text after
/// `diff --git `. The `a/` and `b/` prefixes are dropped when both sides carry them, so a diff
/// made with `--no-prefix` reads the same. `None` when the line cannot be split into two paths
/// with certainty.
///
/// The header is the only place git names the path of an entry it writes without `---`/`+++`
/// lines: an empty file added or deleted, a binary change, a mode change. Git quotes a path
/// holding a double quote, backslash or control byte (or, under `core.quotePath`, a non-ASCII
/// byte) but never one holding only spaces, so an unquoted line is split where both halves name
/// the same path, as `git apply` splits it; a path may itself hold ` b/`. Only a rename or copy
/// names two different paths, and it names both again on its `rename`/`copy` lines, so such a
/// line is split only at a lone ` b/`.
pub fn git_header_paths(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim_end_matches('\r');
    if rest.starts_with('"') {
        let (old, after) = unquote_path(rest)?;
        let after = after.strip_prefix(' ')?;
        let new = if after.starts_with('"') {
            let (new, tail) = unquote_path(after)?;
            if !tail.is_empty() {
                return None;
            }
            new
        } else {
            after.to_string()
        };
        return Some(strip_header_prefixes(&old, &new));
    }
    if rest.ends_with('"') {
        // An unquoted path holds no `"`, so the quoted side starts at the first ` "`.
        let at = rest.find(" \"")?;
        let (new, tail) = unquote_path(&rest[at + 1..])?;
        return tail
            .is_empty()
            .then(|| strip_header_prefixes(&rest[..at], &new));
    }
    let splits = rest
        .match_indices(' ')
        .map(|(at, _)| (&rest[..at], &rest[at + 1..]));
    if let Some((old, new)) = splits
        .clone()
        .map(|(old, new)| strip_header_prefixes(old, new))
        .find(|(old, new)| old == new)
    {
        return Some((old, new));
    }
    let mut prefixed = splits.filter(|(old, new)| old.starts_with("a/") && new.starts_with("b/"));
    let (old, new) = prefixed.next()?;
    prefixed
        .next()
        .is_none()
        .then(|| strip_header_prefixes(old, new))
}

/// The old- and new-side path prefixes git can write on a `diff --git` entry. `a/` and `b/` are
/// the default, and what every diff `ok` runs itself pins. `diff.noprefix` writes none.
/// `diff.mnemonicPrefix` names what each side is: a (c)ommit, the (i)ndex, the (w)ork tree, an
/// (o)bject, or `1`/`2` under `--no-index`. `-R` swaps the sides, prefixes included, so every
/// pair is also accepted reversed. Any other prefix (`diff.srcPrefix`, `--src-prefix`) cannot be
/// told apart from part of a path and is not guessed.
const PREFIX_STYLES: &[(&str, &str)] = &[
    ("a/", "b/"),
    ("", ""),
    ("c/", "i/"),
    ("c/", "w/"),
    ("i/", "w/"),
    ("o/", "w/"),
    ("1/", "2/"),
    ("b/", "a/"),
    ("i/", "c/"),
    ("w/", "c/"),
    ("w/", "i/"),
    ("w/", "o/"),
    ("2/", "1/"),
];

/// A `diff --git` entry whose paths cannot be read with certainty. It names the entry by
/// position, not path: the path is what could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableDiffEntry {
    /// 1-based position of the entry among the diff's `diff --git` headers.
    pub entry: usize,
    /// 1-based line of the entry's `diff --git` header.
    pub line: usize,
    pub reason: &'static str,
}

impl fmt::Display for UnreadableDiffEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "entry {} (the `diff --git` header at line {}): {}",
            self.entry, self.line, self.reason
        )
    }
}

/// `diff` with every `diff --git` entry's paths written with git's default `a/` and `b/`
/// prefixes, so a reader that assumes them reads a diff made under any prefix configuration.
///
/// Each entry's prefix style is taken from its `diff --git` header, checked against its
/// `rename`/`copy` lines (which carry no prefix) and its `---`/`+++` lines, and then applied to
/// all of them; the `---`/`+++` paths are never stripped one by one, which reads a prefix-less
/// `a/lib.rs` as `lib.rs`. An entry that fits no style, or more than one, is an error rather
/// than a guess. Entries already in the default style, and text outside `diff --git` entries,
/// are left byte for byte, so a diff `ok` produced comes back borrowed. Rewritten lines keep
/// their line numbers.
pub fn with_default_prefixes(diff: &str) -> Result<Cow<'_, str>, UnreadableDiffEntry> {
    let lines = diff_lines(diff);
    let entries = git_entries(&lines);
    let mut rewrites = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let (prefixes, old, new) = entry.resolve().map_err(|reason| UnreadableDiffEntry {
            entry: index + 1,
            line: entry.header_line + 1,
            reason,
        })?;
        if prefixes != ("a/", "b/") {
            rewrites.push((entry, old, new));
        }
    }
    if rewrites.is_empty() {
        return Ok(Cow::Borrowed(diff));
    }
    let mut replaced = vec![None; lines.len()];
    for (entry, old, new) in rewrites {
        let old_side = quote_header_path(&format!("a/{old}"));
        let new_side = quote_header_path(&format!("b/{new}"));
        replaced[entry.header_line] = Some(format!("diff --git {old_side} {new_side}"));
        for marker in &entry.markers {
            let side = match (&marker.path, marker.old_side) {
                (MarkerPath::DevNull, _) => "/dev/null",
                (MarkerPath::Path(_), true) => &old_side,
                (MarkerPath::Path(_), false) => &new_side,
            };
            let lead = if marker.old_side { "---" } else { "+++" };
            replaced[marker.line] = Some(format!("{lead} {side}"));
        }
    }
    let mut out = String::with_capacity(diff.len() + 64);
    for (line, replacement) in lines.iter().zip(replaced) {
        match replacement {
            Some(text) => {
                out.push_str(&text);
                out.push_str(line.ending);
            }
            None => {
                out.push_str(line.text);
                out.push_str(line.ending);
            }
        }
    }
    Ok(Cow::Owned(out))
}

/// One line of a diff as `str::lines` reads it, with the terminator it had.
struct RawLine<'a> {
    text: &'a str,
    ending: &'a str,
}

fn diff_lines(diff: &str) -> Vec<RawLine<'_>> {
    diff.split_inclusive('\n')
        .map(|chunk| {
            let text = chunk
                .strip_suffix('\n')
                .map(|text| text.strip_suffix('\r').unwrap_or(text))
                .unwrap_or(chunk);
            RawLine {
                text,
                ending: &chunk[text.len()..],
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum MarkerPath {
    DevNull,
    Path(String),
}

/// A `---` (`old_side`) or `+++` file header of a `diff --git` entry, decoded but still prefixed.
struct Marker {
    line: usize,
    old_side: bool,
    path: MarkerPath,
}

/// A prefix pair and the old and new paths it leaves.
type EntryFit = ((&'static str, &'static str), String, String);

/// What a `diff --git` entry says about its paths before its first hunk.
struct GitEntry<'a> {
    header_line: usize,
    header: &'a str,
    /// The `rename`/`copy` `from` and `to` paths, which git writes without a prefix.
    moved_from: Option<Option<String>>,
    moved_to: Option<Option<String>>,
    markers: Vec<Marker>,
    /// A `---`/`+++` value that does not decode, so no style can be checked against it.
    undecodable_marker: bool,
}

fn git_entries<'a>(lines: &[RawLine<'a>]) -> Vec<GitEntry<'a>> {
    let mut entries: Vec<GitEntry<'a>> = Vec::new();
    let mut in_hunks = false;
    let mut scanner = HunkScanner::new();
    for (index, line) in lines.iter().enumerate() {
        let text = line.text;
        let kind = scanner.scan(text);
        if kind != DiffLine::Header {
            in_hunks |= matches!(kind, DiffLine::HunkHeader(_));
            continue;
        }
        if let Some(header) = text.strip_prefix("diff --git ") {
            in_hunks = false;
            entries.push(GitEntry {
                header_line: index,
                header,
                moved_from: None,
                moved_to: None,
                markers: Vec::new(),
                undecodable_marker: false,
            });
            continue;
        }
        let Some(entry) = entries.last_mut().filter(|_| !in_hunks) else {
            continue;
        };
        let moved = |value: &str| {
            let value = value.trim_end_matches('\r');
            if value.starts_with('"') {
                unquote_path(value).and_then(|(path, tail)| tail.is_empty().then_some(path))
            } else {
                Some(value.to_string())
            }
        };
        if let Some(value) = text
            .strip_prefix("rename from ")
            .or_else(|| text.strip_prefix("copy from "))
        {
            entry.moved_from = Some(moved(value));
        } else if let Some(value) = text
            .strip_prefix("rename to ")
            .or_else(|| text.strip_prefix("copy to "))
        {
            entry.moved_to = Some(moved(value));
        } else if let Some((value, old_side)) = text
            .strip_prefix("--- ")
            .map(|value| (value, true))
            .or_else(|| text.strip_prefix("+++ ").map(|value| (value, false)))
        {
            match marker_path(value) {
                Some(path) => entry.markers.push(Marker {
                    line: index,
                    old_side,
                    path,
                }),
                None => entry.undecodable_marker = true,
            }
        }
    }
    entries
}

fn marker_path(value: &str) -> Option<MarkerPath> {
    let name = file_header_name(value);
    let path = if name.starts_with('"') {
        let (path, tail) = unquote_path(name)?;
        if !tail.is_empty() {
            return None;
        }
        path
    } else {
        name.to_string()
    };
    Some(if path == "/dev/null" {
        MarkerPath::DevNull
    } else {
        MarkerPath::Path(path)
    })
}

impl GitEntry<'_> {
    /// The entry's prefix pair and its unprefixed old and new paths.
    fn resolve(&self) -> Result<EntryFit, &'static str> {
        if self.undecodable_marker {
            return Err("a `---` or `+++` path does not decode");
        }
        let moved = match (&self.moved_from, &self.moved_to) {
            (None, None) => None,
            (Some(Some(from)), Some(Some(to))) => Some((from.as_str(), to.as_str())),
            _ => return Err("its `rename`/`copy` lines do not name both paths"),
        };
        let splits = header_splits(self.header);
        // A header names one path twice, or the two paths of its `rename`/`copy` lines.
        let exact = self.fits(&splits, |old, new| match moved {
            Some((from, to)) => old == from && new == to,
            None => old == new,
        });
        let fits = match (exact.len(), moved) {
            (0, None) => {
                // Two different paths with no `rename`/`copy` lines is not something git
                // writes, but read as before: a rename, only under a real prefix pair (with no
                // prefix it is any split of the line) and only at a single place.
                self.fits(&splits, |old, new| old != new)
                    .into_iter()
                    .filter(|(prefixes, _, _)| *prefixes != ("", ""))
                    .collect()
            }
            _ => exact,
        };
        let mut fits = fits.into_iter();
        match (fits.next(), fits.next()) {
            (Some(fit), None) => Ok(fit),
            (Some(_), Some(_)) => Err(
                "its paths read more than one way under git's default, mnemonic and no-prefix \
                 styles",
            ),
            (None, _) => Err(
                "its `diff --git`, `rename`/`copy` and `---`/`+++` lines do not name the same \
                 paths under git's default (`a/`, `b/`), mnemonic (`diff.mnemonicPrefix`) or \
                 no-prefix (`diff.noprefix`) style",
            ),
        }
    }

    /// Every split and prefix pair under which the header's paths satisfy `shape` and agree
    /// with the entry's `---`/`+++` lines.
    fn fits(
        &self,
        splits: &[(String, String)],
        shape: impl Fn(&str, &str) -> bool,
    ) -> Vec<EntryFit> {
        let mut fits = Vec::new();
        for (old_side, new_side) in splits {
            for &(old_prefix, new_prefix) in PREFIX_STYLES {
                let (Some(old), Some(new)) = (
                    old_side.strip_prefix(old_prefix),
                    new_side.strip_prefix(new_prefix),
                ) else {
                    continue;
                };
                if old.is_empty() || new.is_empty() || !shape(old, new) {
                    continue;
                }
                let markers_agree = self.markers.iter().all(|marker| match &marker.path {
                    MarkerPath::DevNull => true,
                    MarkerPath::Path(path) if marker.old_side => path == old_side,
                    MarkerPath::Path(path) => path == new_side,
                });
                if markers_agree {
                    fits.push(((old_prefix, new_prefix), old.to_string(), new.to_string()));
                }
            }
        }
        fits
    }
}

/// Every way the text after `diff --git ` splits into a prefixed old and new side. A quoted
/// side fixes the split; an unquoted line splits at any space, since git never quotes a path
/// for holding one.
fn header_splits(rest: &str) -> Vec<(String, String)> {
    let rest = rest.trim_end_matches('\r');
    if rest.starts_with('"') {
        let Some((old, after)) = unquote_path(rest) else {
            return Vec::new();
        };
        let Some(after) = after.strip_prefix(' ') else {
            return Vec::new();
        };
        let new = if after.starts_with('"') {
            match unquote_path(after) {
                Some((new, "")) => new,
                _ => return Vec::new(),
            }
        } else {
            after.to_string()
        };
        return vec![(old, new)];
    }
    if rest.ends_with('"') {
        // An unquoted path holds no `"`, so the quoted side starts at the first ` "`.
        return rest
            .find(" \"")
            .and_then(|at| match unquote_path(&rest[at + 1..]) {
                Some((new, "")) => Some(vec![(rest[..at].to_string(), new)]),
                _ => None,
            })
            .unwrap_or_default();
    }
    rest.match_indices(' ')
        .map(|(at, _)| (rest[..at].to_string(), rest[at + 1..].to_string()))
        .collect()
}

/// A path as git writes it on a header line: quoted, with C escapes, when it holds a space, a
/// double quote, a backslash or a control byte, so every reader splits and ends it the same way.
fn quote_header_path(path: &str) -> String {
    let needs_quotes = path
        .chars()
        .any(|ch| ch == ' ' || ch == '"' || ch == '\\' || ch.is_ascii_control());
    if !needs_quotes {
        return path.to_string();
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for ch in path.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            ch if ch.is_ascii_control() => quoted.push_str(&format!("\\{:03o}", ch as u32)),
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// Both prefixes or neither: a `--no-prefix` path that starts with `a/` keeps it.
fn strip_header_prefixes<'a>(old: &'a str, new: &'a str) -> (String, String) {
    let (old, new) = old
        .strip_prefix("a/")
        .zip(new.strip_prefix("b/"))
        .unwrap_or((old, new));
    (old.to_string(), new.to_string())
}

/// A git-quoted path at the start of `raw`, decoded, and the text after its closing quote.
/// `None` unless `raw` starts with a well-formed quoted path naming UTF-8 bytes.
fn unquote_path(raw: &str) -> Option<(String, &str)> {
    let inner = raw.strip_prefix('"')?;
    let bytes = inner.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        index += 1;
        match byte {
            b'"' => return Some((String::from_utf8(decoded).ok()?, &inner[index..])),
            b'\\' => {
                let escaped = *bytes.get(index)?;
                index += 1;
                decoded.push(match escaped {
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            match bytes.get(index) {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    index += 1;
                                }
                                _ => break,
                            }
                        }
                        u8::try_from(value).ok()?
                    }
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    other => other,
                });
            }
            _ => decoded.push(byte),
        }
    }
    None
}

/// The hunk header as far as its closing `@@`, leaving out the function-context source line
/// git appends, so a message quoting it does not echo repository content.
fn header_ranges(header: &str) -> &str {
    match header.find("@@") {
        Some(end) => header[..end].trim_end(),
        None => header,
    }
}

/// The old and new line counts of `-a[,b] +c[,d] @@...`; `None` unless both sides parse.
fn hunk_counts(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split_whitespace();
    let old = side_count(parts.next()?.strip_prefix('-')?)?;
    let new = side_count(parts.next()?.strip_prefix('+')?)?;
    parts
        .next()
        .is_some_and(|marker| marker.starts_with("@@"))
        .then_some((old, new))
}

fn side_count(side: &str) -> Option<u32> {
    let (start, count) = side.split_once(',').unwrap_or((side, "1"));
    start.parse::<u32>().ok()?;
    count.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        file_header_name, git_header_paths, with_default_prefixes, DiffLine, HunkScanner,
        MalformedDiff, UnreadableDiffEntry,
    };
    use std::borrow::Cow;

    fn same(path: &str) -> Option<(String, String)> {
        Some((path.to_string(), path.to_string()))
    }

    #[test]
    fn a_header_naming_one_path_twice_is_split_where_both_halves_agree() {
        assert_eq!(
            git_header_paths("a/src/lib.rs b/src/lib.rs"),
            same("src/lib.rs")
        );
        assert_eq!(
            git_header_paths("a/sp ace.bin b/sp ace.bin"),
            same("sp ace.bin")
        );
        // A path holding ` b/` cannot be split at its first ` b/`.
        assert_eq!(
            git_header_paths("a/x b/y.bin b/x b/y.bin"),
            same("x b/y.bin")
        );
        assert_eq!(git_header_paths("a/b/c.png b/b/c.png"), same("b/c.png"));
        assert_eq!(git_header_paths("a/run.sh b/run.sh\r"), same("run.sh"));
    }

    #[test]
    fn a_no_prefix_header_keeps_paths_that_start_like_a_prefix() {
        assert_eq!(
            git_header_paths("src/lib.rs src/lib.rs"),
            same("src/lib.rs")
        );
        assert_eq!(git_header_paths("a/x.bin a/x.bin"), same("a/x.bin"));
        assert_eq!(git_header_paths("b/x.bin b/x.bin"), same("b/x.bin"));
        assert_eq!(
            git_header_paths("sp ace/x b/y sp ace/x b/y"),
            same("sp ace/x b/y")
        );
    }

    #[test]
    fn quoted_header_paths_are_decoded() {
        assert_eq!(
            git_header_paths("\"a/caf\\303\\251 menu.png\" \"b/caf\\303\\251 menu.png\""),
            same("café menu.png")
        );
        assert_eq!(
            git_header_paths("\"a/tab\\there\" \"b/tab\\there\""),
            same("tab\there")
        );
        assert_eq!(
            git_header_paths("\"a/q\\\"uote\" \"b/q\\\"uote\""),
            same("q\"uote")
        );
        assert_eq!(
            git_header_paths("\"x/\\303\\251.bin\" \"x/\\303\\251.bin\""),
            same("x/é.bin")
        );
        // Each side is quoted on its own, so a rename can mix the two.
        assert_eq!(
            git_header_paths("a/plain.bin \"b/\\303\\251.bin\""),
            Some(("plain.bin".into(), "é.bin".into()))
        );
        assert_eq!(
            git_header_paths("\"a/\\303\\251.bin\" b/plain.bin"),
            Some(("é.bin".into(), "plain.bin".into()))
        );
    }

    #[test]
    fn a_header_naming_two_paths_is_split_only_where_certain() {
        assert_eq!(
            git_header_paths("a/old.bin b/new.bin"),
            Some(("old.bin".into(), "new.bin".into()))
        );
        assert_eq!(
            git_header_paths("a/sp ace.bin b/new name.bin"),
            Some(("sp ace.bin".into(), "new name.bin".into()))
        );
        for ambiguous in [
            "a/x b/y b/z",
            "old.bin new.bin",
            "",
            "a/only",
            "\"a/unterminated b/x",
            "\"a/x\" \"b/x\" trailing",
            "\"a/bad\\777\" \"b/bad\\777\"",
        ] {
            assert_eq!(git_header_paths(ambiguous), None, "{ambiguous}");
        }
    }

    fn scan(diff: &str) -> (Vec<DiffLine<'_>>, Result<(), MalformedDiff>) {
        let mut scanner = HunkScanner::new();
        let lines = diff.lines().map(|line| scanner.scan(line)).collect();
        (lines, scanner.finish())
    }

    #[test]
    fn hunk_content_that_looks_like_file_headers_is_content() {
        let (lines, result) = scan(
            "--- a/x\n\
             +++ b/x\n\
             @@ -1,2 +1 @@\n\
             --- old\n\
             --- also old\n\
             +++ new\n\
             \\ No newline at end of file\n\
             --- a/y\n\
             +++ b/y\n\
             @@ -0,0 +1 @@\n\
             +y\n",
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            lines,
            vec![
                DiffLine::Header,
                DiffLine::Header,
                DiffLine::HunkHeader("-1,2 +1 @@"),
                DiffLine::Content,
                DiffLine::Content,
                DiffLine::Content,
                DiffLine::Content,
                DiffLine::Header,
                DiffLine::Header,
                DiffLine::HunkHeader("-0,0 +1 @@"),
                DiffLine::Content,
            ]
        );
    }

    #[test]
    fn over_counted_hunk_that_swallows_the_next_headers_is_malformed() {
        let (_, result) = scan(
            "--- a/x\n\
             +++ b/x\n\
             @@ -1,3 +1,3 @@\n\
             -a\n\
             +b\n\
             --- a/y\n\
             +++ b/y\n\
             @@ -1 +1 @@\n\
             -c\n\
             +d\n",
        );
        let malformed = result.unwrap_err();
        assert_eq!(malformed.line, 8, "{malformed}");
        assert!(
            malformed.reason.contains("1 removed and 1 added"),
            "{malformed}"
        );
    }

    #[test]
    fn truncated_hunk_is_malformed_at_the_end_of_the_diff() {
        let (_, result) = scan("--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n+b\n");
        assert_eq!(result.unwrap_err().line, 5);
    }

    #[test]
    fn under_counted_hunk_leaves_content_that_is_malformed() {
        for stray in ["+more", "+++ b/elsewhere", "-less", " context"] {
            let diff = format!("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n{stray}\n");
            let (_, result) = scan(&diff);
            assert_eq!(result.unwrap_err().line, 6, "{stray}");
        }
    }

    #[test]
    fn unparseable_hunk_header_is_malformed() {
        for header in [
            "@@ -1,x +1 @@",
            "@@ -1 +1",
            "@@ garbage @@",
            "@@ -1 +y,2 @@",
        ] {
            let diff = format!("--- a/x\n+++ b/x\n{header}\n");
            let (_, result) = scan(&diff);
            assert_eq!(result.unwrap_err().line, 3, "{header}");
        }
    }

    #[test]
    fn an_unparsed_hunk_header_is_quoted_without_its_source_context() {
        let (_, result) = scan("--- a/x\n+++ b/x\n@@ -1,x +1 @@ let secret = token();\n");
        let malformed = result.unwrap_err();
        assert!(malformed.reason.contains("`@@ -1,x +1 @@`"), "{malformed}");
        assert!(!malformed.reason.contains("secret"), "{malformed}");
    }

    #[test]
    fn file_header_names_end_at_the_tab_git_or_diff_appends() {
        assert_eq!(file_header_name("b/sp ace.txt\t"), "b/sp ace.txt");
        assert_eq!(
            file_header_name("src/a.rs\t2026-09-25 01:00:00.000000000 +0000"),
            "src/a.rs"
        );
        assert_eq!(file_header_name("b/plain.rs\r"), "b/plain.rs");
        assert_eq!(file_header_name("b/pasted.rs  "), "b/pasted.rs");
        assert_eq!(file_header_name("b/sp ace.txt"), "b/sp ace.txt");
        assert_eq!(
            file_header_name("\"b/tab\\there.rs\""),
            "\"b/tab\\there.rs\""
        );
    }

    #[test]
    fn unpaired_file_headers_are_malformed() {
        assert_eq!(
            scan("--- a/x\n@@ -1 +1 @@\n-a\n+b\n").1.unwrap_err().line,
            2
        );
        assert_eq!(
            scan("+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").1.unwrap_err().line,
            1
        );
        assert_eq!(scan("--- a/x\n").1.unwrap_err().line, 1);
    }

    #[test]
    fn text_between_entries_and_a_patch_signature_are_not_malformed() {
        let (_, result) = scan(
            "commit 0123\n\
             \n\
             \x20   message line\n\
             \n\
             diff --git a/x b/x\n\
             index 1..2 100644\n\
             --- a/x\n\
             +++ b/x\n\
             @@ -1 +1 @@\n\
             -a\n\
             +b\n\
             -- \n\
             2.39.2\n",
        );
        assert_eq!(result, Ok(()));
    }

    fn rewritten(diff: &str) -> String {
        with_default_prefixes(diff).unwrap().into_owned()
    }

    #[test]
    fn default_prefix_diffs_come_back_unchanged() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                    --- a/src/lib.rs\n\
                    +++ b/src/lib.rs\n\
                    @@ -1 +1 @@\n\
                    -a\n\
                    +b\n\
                    diff --git a/x.bin b/x.bin\n\
                    Binary files a/x.bin and b/x.bin differ\n";
        assert!(matches!(with_default_prefixes(diff), Ok(Cow::Borrowed(_))));
        assert!(matches!(with_default_prefixes(""), Ok(Cow::Borrowed(_))));
    }

    #[test]
    fn mnemonic_prefixes_are_rewritten_on_every_line_of_their_entry() {
        assert_eq!(
            rewritten(
                "diff --git i/w/lib.rs w/w/lib.rs\r\n\
                 --- i/w/lib.rs\r\n\
                 +++ w/w/lib.rs\r\n\
                 @@ -1 +1 @@\r\n\
                 --- i/w/lib.rs\r\n\
                 +++ w/w/lib.rs\r\n\
                 diff --git c/src/gone.rs w/src/gone.rs\n\
                 deleted file mode 100644\n\
                 --- c/src/gone.rs\n\
                 +++ /dev/null\n\
                 diff --git w/run.sh c/run.sh\n\
                 old mode 100755\n\
                 new mode 100644\n"
            ),
            // Hunk content that looks like file headers is left as it is.
            "diff --git a/w/lib.rs b/w/lib.rs\r\n\
             --- a/w/lib.rs\r\n\
             +++ b/w/lib.rs\r\n\
             @@ -1 +1 @@\r\n\
             --- i/w/lib.rs\r\n\
             +++ w/w/lib.rs\r\n\
             diff --git a/src/gone.rs b/src/gone.rs\n\
             deleted file mode 100644\n\
             --- a/src/gone.rs\n\
             +++ /dev/null\n\
             diff --git a/run.sh b/run.sh\n\
             old mode 100755\n\
             new mode 100644\n"
        );
    }

    #[test]
    fn prefix_less_paths_keep_a_leading_a_or_b_directory() {
        assert_eq!(
            rewritten(
                "diff --git a/lib.rs a/lib.rs\n\
                 --- a/lib.rs\n\
                 +++ a/lib.rs\n\
                 diff --git b/x.bin b/x.bin\n\
                 Binary files b/x.bin and b/x.bin differ\n\
                 diff --git src/added.rs src/added.rs\n\
                 new file mode 100644\n\
                 --- /dev/null\n\
                 +++ src/added.rs\n"
            ),
            "diff --git a/a/lib.rs b/a/lib.rs\n\
             --- a/a/lib.rs\n\
             +++ b/a/lib.rs\n\
             diff --git a/b/x.bin b/b/x.bin\n\
             Binary files b/x.bin and b/x.bin differ\n\
             diff --git a/src/added.rs b/src/added.rs\n\
             new file mode 100644\n\
             --- /dev/null\n\
             +++ b/src/added.rs\n"
        );
    }

    #[test]
    fn renames_take_their_prefixes_from_the_rename_lines() {
        // Prefix-less, a rename from `a/old` to `b/new` looks like a default-prefix header.
        assert_eq!(
            rewritten(
                "diff --git a/old.rs b/new.rs\n\
                 similarity index 90%\n\
                 rename from a/old.rs\n\
                 rename to b/new.rs\n\
                 --- a/old.rs\n\
                 +++ b/new.rs\n"
            ),
            "diff --git a/a/old.rs b/b/new.rs\n\
             similarity index 90%\n\
             rename from a/old.rs\n\
             rename to b/new.rs\n\
             --- a/a/old.rs\n\
             +++ b/b/new.rs\n"
        );
        assert_eq!(
            rewritten(
                "diff --git c/sp ace/old.rs w/w b/new.rs\n\
                 rename from sp ace/old.rs\n\
                 rename to w b/new.rs\n"
            ),
            "diff --git \"a/sp ace/old.rs\" \"b/w b/new.rs\"\n\
             rename from sp ace/old.rs\n\
             rename to w b/new.rs\n"
        );
        assert_eq!(
            rewritten(
                "diff --git c/src/old.rs i/src/new.rs\n\
                 copy from src/old.rs\n\
                 copy to src/new.rs\n"
            ),
            "diff --git a/src/old.rs b/src/new.rs\n\
             copy from src/old.rs\n\
             copy to src/new.rs\n"
        );
    }

    #[test]
    fn spaces_and_quoted_paths_are_written_quoted() {
        assert_eq!(
            rewritten(
                "diff --git i/sp ace.txt w/sp ace.txt\n\
                 --- i/sp ace.txt\t\n\
                 +++ w/sp ace.txt\t\n\
                 diff --git \"i/caf\\303\\251\\tx\" \"w/caf\\303\\251\\tx\"\n\
                 --- \"i/caf\\303\\251\\tx\"\n\
                 +++ \"w/caf\\303\\251\\tx\"\n"
            ),
            "diff --git \"a/sp ace.txt\" \"b/sp ace.txt\"\n\
             --- \"a/sp ace.txt\"\n\
             +++ \"b/sp ace.txt\"\n\
             diff --git \"a/caf\u{e9}\\tx\" \"b/caf\u{e9}\\tx\"\n\
             --- \"a/caf\u{e9}\\tx\"\n\
             +++ \"b/caf\u{e9}\\tx\"\n"
        );
    }

    #[test]
    fn a_header_fixed_only_by_its_file_headers_is_read_through_them() {
        // Two different paths and no `rename` lines: split where the file headers say.
        assert_eq!(
            rewritten("diff --git i/x w/y w/z\n--- i/x w/y\n+++ w/z\n"),
            "diff --git \"a/x w/y\" b/z\n--- \"a/x w/y\"\n+++ b/z\n"
        );
    }

    #[test]
    fn entries_whose_paths_cannot_be_read_fail_by_position_without_naming_a_path() {
        let unreadable = |diff: &str| with_default_prefixes(diff).unwrap_err();
        let ok_entry =
            "diff --git a/ok.rs b/ok.rs\n--- a/ok.rs\n+++ b/ok.rs\n@@ -1 +1 @@\n-a\n+b\n";
        for (entry, reason) in [
            // A custom prefix is indistinguishable from a directory.
            (
                "diff --git old/src/k.rs new/src/k.rs\n--- old/src/k.rs\n+++ new/src/k.rs\n",
                "do not name the same paths",
            ),
            (
                "diff --git old/src/k.rs new/src/j.rs\nrename from src/k.rs\nrename to src/j.rs\n",
                "do not name the same paths",
            ),
            // Two different paths with no prefix and no rename lines split anywhere.
            (
                "diff --git src/k.rs src/j.rs\n",
                "do not name the same paths",
            ),
            // `---`/`+++` lines that disagree with the header.
            (
                "diff --git i/k.rs w/k.rs\n--- i/k.rs\n+++ w/j.rs\n",
                "do not name the same paths",
            ),
            (
                "diff --git a/k.rs b/k.rs\n--- k.rs\n+++ k.rs\n",
                "do not name the same paths",
            ),
            // A rename split at two places, with nothing to choose between them.
            (
                "diff --git a/x b/y b/z\nBinary files differ\n",
                "more than one way",
            ),
            (
                "diff --git a/k.rs b/k.rs\n--- \"a/bad\\777\"\n+++ b/k.rs\n",
                "does not decode",
            ),
            (
                "diff --git c/k.rs w/j.rs\nrename from k.rs\n",
                "do not name both paths",
            ),
            ("diff --git a/only\n", "do not name the same paths"),
        ] {
            let diff = format!("{ok_entry}{entry}");
            let error = unreadable(&diff);
            assert_eq!((error.entry, error.line), (2, 7), "{entry}: {error}");
            assert!(error.reason.contains(reason), "{entry}: {error}");
            let message = error.to_string();
            assert!(
                message.starts_with("entry 2 (the `diff --git` header at line 7)"),
                "{message}"
            );
            for path in ["k.rs", "j.rs", "only", "src/"] {
                assert!(!message.contains(path), "{message}");
            }
        }
        let first = unreadable("diff --git x/k.rs y/k.rs\n");
        assert!(
            matches!(
                first,
                UnreadableDiffEntry {
                    entry: 1,
                    line: 1,
                    ..
                }
            ),
            "{first}"
        );
    }
}
