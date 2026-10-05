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
    for (index, line) in masked.lines().enumerate() {
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
/// replaced by spaces, newlines kept, so line numbers and code columns are unchanged.
pub(crate) fn mask_comments_and_literals(content: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        Regular,
        Verbatim,
        Raw(usize),
        Char,
    }
    let bytes = content.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut state = State::Code;
    let mut index = 0;
    let blank = |out: &mut Vec<u8>, byte: u8| out.push(if byte == b'\n' { b'\n' } else { b' ' });
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match state {
            State::Code => match byte {
                b'/' if next == Some(b'/') => {
                    state = State::LineComment;
                    blank(&mut out, byte);
                }
                b'/' if next == Some(b'*') => {
                    state = State::BlockComment;
                    out.extend_from_slice(b"  ");
                    index += 2;
                    continue;
                }
                b'"' => {
                    let quotes = bytes[index..].iter().take_while(|&&b| b == b'"').count();
                    let verbatim = bytes[..index]
                        .iter()
                        .rev()
                        .take_while(|&&b| b == b'$' || b == b'@')
                        .any(|&b| b == b'@');
                    state = if quotes >= 3 {
                        State::Raw(quotes)
                    } else if verbatim {
                        State::Verbatim
                    } else {
                        State::Regular
                    };
                    let opened = if quotes >= 3 { quotes } else { 1 };
                    out.extend(std::iter::repeat_n(b' ', opened));
                    index += opened;
                    continue;
                }
                b'\'' => {
                    state = State::Char;
                    blank(&mut out, byte);
                }
                _ => out.push(byte),
            },
            State::LineComment => {
                if byte == b'\n' {
                    state = State::Code;
                }
                blank(&mut out, byte);
            }
            State::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    state = State::Code;
                    out.extend_from_slice(b"  ");
                    index += 2;
                    continue;
                }
                blank(&mut out, byte);
            }
            State::Regular | State::Char => {
                let close = if state == State::Regular { b'"' } else { b'\'' };
                if byte == b'\\' {
                    blank(&mut out, byte);
                    if let Some(escaped) = next {
                        blank(&mut out, escaped);
                    }
                    index += 2;
                    continue;
                }
                if byte == close || byte == b'\n' {
                    state = State::Code;
                }
                blank(&mut out, byte);
            }
            State::Verbatim => {
                if byte == b'"' && next == Some(b'"') {
                    out.extend_from_slice(b"  ");
                    index += 2;
                    continue;
                }
                if byte == b'"' {
                    state = State::Code;
                }
                blank(&mut out, byte);
            }
            State::Raw(quotes) => {
                let run = bytes[index..].iter().take_while(|&&b| b == b'"').count();
                if run >= quotes {
                    out.extend(std::iter::repeat_n(b' ', run));
                    index += run;
                    state = State::Code;
                    continue;
                }
                blank(&mut out, byte);
            }
        }
        index += 1;
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
        let source = "namespace Acme.Collections\n{\n    public class Dictionary2<TKey, TValue>\n    {\n        private struct Entry\n        {\n            public int Next;\n        }\n        public record Slot(int Index);\n        internal interface IProbe { }\n    }\n    sealed class Sibling\n    {\n        void Broken() { if (x { }\n    }\n";
        assert_eq!(
            found(source),
            vec![
                (
                    "Acme::Collections::Dictionary2".to_string(),
                    SymbolKind::Class,
                    3
                ),
                (
                    "Acme::Collections::Dictionary2::Entry".to_string(),
                    SymbolKind::Class,
                    5
                ),
                (
                    "Acme::Collections::Dictionary2::Slot".to_string(),
                    SymbolKind::Class,
                    9
                ),
                (
                    "Acme::Collections::Dictionary2::IProbe".to_string(),
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
    fn masking_keeps_lines_and_code() {
        let source = "var a = \"x { y\"; // { comment\nvar b = 'c'; var d = @\"q\"\"{\"; /* { */ var e = 1;\n";
        let masked = mask_comments_and_literals(source);
        assert_eq!(masked.lines().count(), source.lines().count());
        assert!(!masked.contains('{'), "{masked}");
        assert!(masked.contains("var e = 1;"));
        assert!(masked.contains("var b ="));
    }
}
