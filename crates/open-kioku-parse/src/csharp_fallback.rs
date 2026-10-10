//! Type declarations of a C# file tree-sitter could not read, found line by line.
//!
//! Matching runs over the source with every comment and literal blanked, so a type mentioned in
//! a block comment or spelled inside a verbatim string is never declared, and braces are counted
//! to name a nested type inside the namespaces and types that enclose it. A file reaches this
//! because its braces may not balance, so the nesting is a reading, not a parse: every symbol is
//! recorded at low confidence with heuristic provenance.

use open_kioku_core::{
    Confidence, EvidenceSourceType, File, LineRange, Symbol, SymbolId, SymbolKind, Visibility,
};
use regex::Regex;
use std::sync::OnceLock;

fn declaration_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^\s*(?:\[[^\]]*\]\s*)*(?:(?:public|private|protected|internal|file|static|sealed|abstract|partial|readonly|ref|unsafe|new)\s+)*(class|struct|interface|enum|record)(?:\s+(?:class|struct))?\s+@?([A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("valid C# declaration pattern")
    })
}

fn namespace_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*namespace\s+([A-Za-z_][A-Za-z0-9_.\s]*?)\s*(;)?\s*(?:\{.*)?$")
            .expect("valid C# namespace pattern")
    })
}

/// What an open brace belongs to.
enum Frame {
    Namespace(Vec<String>),
    Type(String),
    Other,
}

pub(crate) fn symbols(file: &File, content: &str) -> Vec<Symbol> {
    let masked = mask_comments_and_literals(content);
    let mut file_namespace: Vec<String> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut symbols = Vec::new();
    // A declaration claims the next `{` in code, on its line or a later one, unless a `;`
    // ends it first.
    let mut pending: Option<Frame> = None;
    // The branch each open `#if` group is in: only a group's first branch is read, since
    // branches that each repeat a header (`#if X` / `class Host : A {` / `#else` /
    // `class Host : B {` / `#endif`) would otherwise open it twice.
    let mut conditions: Vec<usize> = Vec::new();
    for (index, line) in masked.lines().enumerate() {
        let directive = line.trim_start();
        if let Some(directive) = directive.strip_prefix('#') {
            let word = directive.split_whitespace().next().unwrap_or("");
            match word {
                "if" => conditions.push(0),
                "elif" | "else" => {
                    if let Some(branch) = conditions.last_mut() {
                        *branch += 1;
                    }
                }
                "endif" => {
                    conditions.pop();
                }
                _ => {}
            }
            continue;
        }
        if conditions.iter().any(|&branch| branch > 0) {
            continue;
        }
        if let Some(captures) = namespace_pattern().captures(line) {
            let levels = captures[1]
                .split('.')
                .map(|level| level.split_whitespace().collect::<String>())
                .filter(|level| !level.is_empty())
                .collect::<Vec<_>>();
            if captures.get(2).is_some() {
                file_namespace = levels;
            } else {
                pending = Some(Frame::Namespace(levels));
            }
        } else if let Some(captures) = declaration_pattern().captures(line) {
            let name = captures[2].to_string();
            let kind = if &captures[1] == "interface" {
                SymbolKind::Interface
            } else {
                SymbolKind::Class
            };
            let mut levels = file_namespace.clone();
            for frame in &frames {
                match frame {
                    Frame::Namespace(names) => levels.extend(names.iter().cloned()),
                    Frame::Type(name) => levels.push(name.clone()),
                    Frame::Other => {}
                }
            }
            levels.push(name.clone());
            let qualified_name = levels.join("::");
            let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            symbols.push(Symbol {
                id: SymbolId::new(super::stable_id(&format!(
                    "{}:{}:{}",
                    file.path.display(),
                    line_number,
                    qualified_name
                ))),
                name: name.clone(),
                qualified_name,
                kind,
                file_id: file.id.clone(),
                range: Some(LineRange::single(line_number)),
                language: file.language.clone(),
                confidence: Confidence::Low,
                provenance: EvidenceSourceType::Heuristic,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: Visibility::Unknown,
                alias_of: None,
            });
            pending = Some(Frame::Type(name));
        }
        for byte in line.bytes() {
            match byte {
                b'{' => frames.push(pending.take().unwrap_or(Frame::Other)),
                b'}' => {
                    frames.pop();
                }
                // `record Money(decimal Amount);` declares a type with no body.
                b';' => pending = None,
                _ => {}
            }
        }
    }
    symbols
}

/// `content` with every comment, string, verbatim string, raw string and character literal
/// replaced by spaces, newlines kept, so line numbers and code columns are unchanged. The holes
/// of an interpolated string (`$"{a}"`, `$$"""{{a}}"""`) are blanked with it: a brace there is
/// an expression's, never a declaration's.
pub(crate) fn mask_comments_and_literals(content: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Literal {
        Regular,
        Verbatim,
        /// Closed by this many quotes.
        Raw(usize),
    }
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        /// A string; `dollars` is how many `{` open a hole in it (0: not interpolated).
        Str {
            literal: Literal,
            dollars: usize,
        },
        Char,
        /// Code inside an interpolation hole, with the braces opened in it and the `}` count
        /// that closes it.
        Hole {
            depth: usize,
            close: usize,
        },
    }
    let bytes = content.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut stack = vec![State::Code];
    let mut index = 0;
    let run = |from: usize, byte: u8| bytes[from..].iter().take_while(|&&b| b == byte).count();
    let blank = |out: &mut Vec<u8>, from: usize, len: usize| {
        for &byte in &bytes[from..(from + len).min(bytes.len())] {
            out.push(if byte == b'\n' { b'\n' } else { b' ' });
        }
    };
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        let state = *stack.last().unwrap_or(&State::Code);
        let mut step = 1;
        match state {
            State::Code | State::Hole { .. } => {
                let in_hole = matches!(state, State::Hole { .. });
                match byte {
                    b'/' if next == Some(b'/') => stack.push(State::LineComment),
                    b'/' if next == Some(b'*') => {
                        stack.push(State::BlockComment);
                        step = 2;
                    }
                    b'"' => {
                        let quotes = run(index, b'"');
                        let prefix = bytes[..index]
                            .iter()
                            .rev()
                            .take_while(|&&b| b == b'$' || b == b'@')
                            .copied()
                            .collect::<Vec<_>>();
                        let dollars = prefix.iter().filter(|&&b| b == b'$').count();
                        let literal = if quotes >= 3 {
                            step = quotes;
                            Literal::Raw(quotes)
                        } else if prefix.contains(&b'@') {
                            Literal::Verbatim
                        } else {
                            Literal::Regular
                        };
                        stack.push(State::Str { literal, dollars });
                    }
                    b'\'' => stack.push(State::Char),
                    b'{' if in_hole => {
                        if let Some(State::Hole { depth, .. }) = stack.last_mut() {
                            *depth += 1;
                        }
                    }
                    b'}' if in_hole => {
                        if let Some(State::Hole { depth, close }) = stack.last_mut() {
                            if *depth == 0 {
                                step = run(index, b'}').min(*close).max(1);
                                stack.pop();
                            } else {
                                *depth -= 1;
                            }
                        }
                    }
                    _ => {}
                }
                if in_hole || stack.len() > 1 && !matches!(stack.last(), Some(State::Code)) {
                    blank(&mut out, index, step);
                } else {
                    out.extend_from_slice(&bytes[index..index + step]);
                }
            }
            State::LineComment => {
                if byte == b'\n' {
                    stack.pop();
                }
                blank(&mut out, index, 1);
            }
            State::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    stack.pop();
                    step = 2;
                }
                blank(&mut out, index, step);
            }
            State::Char => {
                if byte == b'\\' {
                    step = 2;
                } else if byte == b'\'' || byte == b'\n' {
                    stack.pop();
                }
                blank(&mut out, index, step);
            }
            State::Str { literal, dollars } => {
                match (literal, byte) {
                    (Literal::Regular, b'\\') => step = 2,
                    (Literal::Regular, b'"' | b'\n') => {
                        stack.pop();
                    }
                    (Literal::Verbatim, b'"') if next == Some(b'"') => step = 2,
                    (Literal::Verbatim, b'"') => {
                        stack.pop();
                    }
                    (Literal::Raw(quotes), b'"') => {
                        step = run(index, b'"');
                        if step >= quotes {
                            stack.pop();
                        }
                    }
                    (_, b'{') if dollars > 0 => {
                        let braces = run(index, b'{');
                        let opens = match literal {
                            // `{{` is a literal brace in a single-`$` string.
                            Literal::Regular | Literal::Verbatim => braces % 2 == 1,
                            Literal::Raw(_) => braces >= dollars,
                        };
                        step = braces;
                        if opens {
                            stack.push(State::Hole {
                                depth: 0,
                                close: dollars,
                            });
                        }
                    }
                    _ => {}
                }
                blank(&mut out, index, step);
            }
        }
        index += step;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{mask_comments_and_literals, symbols};
    use open_kioku_core::{Confidence, File, FileId, Language, RepositoryId, SymbolKind};

    fn file() -> File {
        File {
            id: FileId::new("file-cs"),
            repository_id: RepositoryId::new("repo"),
            path: "src/Ghost.cs".into(),
            language: Language::CSharp,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
            generated_by: None,
        }
    }

    fn found(content: &str) -> Vec<(String, SymbolKind, u32)> {
        symbols(&file(), content)
            .into_iter()
            .map(|symbol| {
                assert_eq!(symbol.confidence, Confidence::Low);
                (
                    symbol.qualified_name,
                    symbol.kind,
                    symbol.range.map_or(0, |range| range.start),
                )
            })
            .collect()
    }

    #[test]
    fn types_in_comments_and_literals_are_not_declared() {
        let source = "namespace Acme.Ghost;\n\n/*\n   this class Phantom does not exist\n   and so the interface IGhost is only mentioned here\n*/\npublic class Real\n{\n    const string Text = @\"\n    public class FromVerbatim\n    {\n    }\";\n    const string Raw = \"\"\"\n        interface IRaw { }\n        \"\"\";\n    // class Commented { }\n    public void Broken() { if (x { }\n";
        assert_eq!(
            found(source),
            vec![("Acme::Ghost::Real".to_string(), SymbolKind::Class, 7)]
        );
    }

    #[test]
    fn nested_types_are_named_inside_their_enclosing_types() {
        let source = "namespace Acme.Collections\n{\n    public class SlotMap<TKey, TValue>\n    {\n        private struct Entry\n        {\n            public int Next;\n        }\n        public record Slot(int Index);\n        internal interface IProbe { }\n    }\n    sealed class Sibling\n    {\n        void Broken() { if (x { }\n    }\n";
        assert_eq!(
            found(source),
            vec![
                (
                    "Acme::Collections::SlotMap".to_string(),
                    SymbolKind::Class,
                    3
                ),
                (
                    "Acme::Collections::SlotMap::Entry".to_string(),
                    SymbolKind::Class,
                    5
                ),
                (
                    "Acme::Collections::SlotMap::Slot".to_string(),
                    SymbolKind::Class,
                    9
                ),
                (
                    "Acme::Collections::SlotMap::IProbe".to_string(),
                    SymbolKind::Interface,
                    10
                ),
                (
                    "Acme::Collections::Sibling".to_string(),
                    SymbolKind::Class,
                    12
                ),
            ]
        );
    }

    #[test]
    fn only_the_first_branch_of_a_conditional_header_is_read() {
        // Each branch repeats the namespace and class header: reading both opened them twice.
        let source = "#if LEGACY\nnamespace Acme.Fall\n{\n    public class Host : OldBase\n    {\n#else\nnamespace Acme.Fall\n{\n    public class Host : NewBase\n    {\n#endif\n        string s = $\"{(flag ? \"{\" : \"}\")}\";\n        string r = $$\"\"\"\n            {{ \"{\" }}\n            \"\"\";\n        char c = '{';\n        public class Inner { }\n    }\n\n    public class After { }\n}\n";
        assert_eq!(
            found(source),
            vec![
                ("Acme::Fall::Host".to_string(), SymbolKind::Class, 4),
                ("Acme::Fall::Host::Inner".to_string(), SymbolKind::Class, 17),
                ("Acme::Fall::After".to_string(), SymbolKind::Class, 20),
            ]
        );
    }

    #[test]
    fn interpolation_holes_are_blanked_with_their_string() {
        let source = "var a = $\"{(x ? \"{\" : \"}\")} and {{literal}\"; var b = $$\"\"\"{{ new { Q = 1 } }}\"\"\"; var c = @$\"{y}\"\"\"; { }\n";
        let masked = mask_comments_and_literals(source);
        assert_eq!(masked.matches('{').count(), 1, "{masked}");
        assert_eq!(masked.matches('}').count(), 1, "{masked}");
        assert!(masked.contains("var c ="), "{masked}");
    }

    #[test]
    fn masking_keeps_lines_and_code() {
        let source = "var a = \"x { y\"; // { comment\nvar b = 'c'; var d = @\"q\"\"{\"; /* { */ var e = 1;\n";
        let masked = mask_comments_and_literals(source);
        assert_eq!(masked.lines().count(), source.lines().count());
        assert!(!masked.contains('{'), "{masked}");
        assert!(masked.contains("var e = 1;"));
        assert!(masked.contains("var b ="));
    }
}
