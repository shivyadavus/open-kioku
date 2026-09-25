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
    use super::{file_header_name, DiffLine, HunkScanner, MalformedDiff};

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
