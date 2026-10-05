//! Where the content of a unified diff's hunks begins and ends.
//!
//! A hunk body is exactly as long as its `@@ -a,b +c,d @@` header says (an omitted count is 1).
//! Inside it every line is content whatever it starts with, so a removed `-- x` or an added
//! `++ y`, rendered `--- x` and `+++ y`, never names a file. A diff whose bodies do not match
//! their headers (truncated, hand-edited, or with a count that does not parse) cannot be split
//! into files reliably: an over-counted hunk swallows the next entry's headers and an
//! under-counted one leaves content to be read as headers. [`HunkScanner`] records the first
//! such place so callers report it instead of silently losing or inventing a path.

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
    use super::{file_header_name, git_header_paths, DiffLine, HunkScanner, MalformedDiff};

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
}
