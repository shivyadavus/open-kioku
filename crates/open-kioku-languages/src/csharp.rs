//! C# facts that span files.

use open_kioku_core::{FileId, Language, Symbol, SymbolKind, Visibility};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Gives every part of a C# `partial` type the accessibility the type declares.
///
/// C# lets one part spell the accessibility and the others omit it: `public partial class
/// Ledger` in one file and `partial class Ledger` in another declare one public type. A parser
/// reads one file, so it records the part with no modifier at the default (`internal` at the top
/// level). Parts are matched by qualified name, kind and type-parameter count, which C# requires
/// every part to share, and by `projects`: the directory of the MSBuild project each file is
/// in. Parts of one partial type are compiled into one assembly, so parts in two projects are
/// two types even under one name. A file in no project is matched with every other such file;
/// a project that compiles files outside its directory (`<Compile Include>`) is not read, so
/// its parts are matched by directory alone. When the parts declare different accessibilities,
/// which C# rejects, or declare none and still disagree, the type's accessibility is not known
/// and every part records [`Visibility::Unknown`].
pub fn unify_partial_type_visibility(symbols: &mut [Symbol], projects: &HashMap<FileId, PathBuf>) {
    type PartKey = (Option<PathBuf>, String, bool, usize);
    let mut parts: BTreeMap<PartKey, Vec<usize>> = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        if symbol.language != Language::CSharp
            || !matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
        {
            continue;
        }
        let Some(header) = symbol.signature.as_deref().and_then(TypeHeader::read) else {
            continue;
        };
        if header.partial {
            parts
                .entry((
                    projects.get(&symbol.file_id).cloned(),
                    symbol.qualified_name.clone(),
                    symbol.kind == SymbolKind::Interface,
                    header.arity,
                ))
                .or_default()
                .push(index);
        }
    }
    for indices in parts.values().filter(|indices| indices.len() > 1) {
        let declared = indices
            .iter()
            .filter_map(|&index| {
                symbols[index]
                    .signature
                    .as_deref()
                    .and_then(TypeHeader::read)
                    .and_then(|header| header.declared)
            })
            .collect::<Vec<_>>();
        let unified = match declared.split_first() {
            Some((first, rest)) if rest.iter().all(|other| other == first) => *first,
            Some(_) => Visibility::Unknown,
            None => {
                let first = symbols[indices[0]].visibility;
                if indices
                    .iter()
                    .all(|&index| symbols[index].visibility == first)
                {
                    first
                } else {
                    Visibility::Unknown
                }
            }
        };
        for &index in indices {
            symbols[index].visibility = unified;
        }
    }
}

/// What a C# type's signature (`public sealed partial class Ledger<T> : Base`) says about the
/// type's parts.
struct TypeHeader {
    partial: bool,
    declared: Option<Visibility>,
    arity: usize,
}

impl TypeHeader {
    fn read(signature: &str) -> Option<Self> {
        let words = signature.split_whitespace().collect::<Vec<_>>();
        let keyword = words.iter().position(|word| {
            matches!(*word, "class" | "struct" | "interface" | "record" | "enum")
        })?;
        let modifiers = &words[..keyword];
        let has = |modifier: &str| modifiers.contains(&modifier);
        // The same reading as the parser's: see `open_kioku_tree_sitter`'s C# visibility.
        let declared = if has("public") {
            Some(Visibility::Public)
        } else if has("protected") {
            Some(if has("private") {
                Visibility::Crate
            } else {
                Visibility::Protected
            })
        } else if has("internal") {
            Some(Visibility::Crate)
        } else if has("private") || has("file") {
            Some(Visibility::Private)
        } else {
            None
        };
        // `record class Name` and `record struct Name` put the name one word later.
        let mut name_at = keyword + 1;
        if words[keyword] == "record"
            && matches!(words.get(name_at).copied(), Some("class" | "struct"))
        {
            name_at += 1;
        }
        let after_name = words[name_at..].join(" ");
        let arity = after_name
            .find('<')
            .filter(|&open| !after_name[..open].contains([':', '(']))
            .map_or(0, |open| {
                let mut depth = 0usize;
                let mut commas = 0usize;
                for ch in after_name[open..].chars() {
                    match ch {
                        '<' => depth += 1,
                        '>' => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                break;
                            }
                        }
                        ',' if depth == 1 => commas += 1,
                        _ => {}
                    }
                }
                commas + 1
            });
        Some(Self {
            partial: has("partial"),
            declared,
            arity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Confidence, EvidenceSourceType, FileId, LineRange, SymbolId};

    fn part(id: &str, qualified: &str, signature: &str, visibility: Visibility) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: qualified.rsplit("::").next().unwrap().into(),
            qualified_name: qualified.into(),
            kind: SymbolKind::Class,
            file_id: FileId::new(id),
            range: Some(LineRange::single(1)),
            language: Language::CSharp,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: Some(signature.into()),
            visibility,
            alias_of: None,
        }
    }

    #[test]
    fn every_part_takes_the_accessibility_one_part_declares() {
        let mut symbols = vec![
            part(
                "a",
                "Acme::Ledger",
                "public partial class Ledger",
                Visibility::Public,
            ),
            part(
                "b",
                "Acme::Ledger",
                "partial class Ledger",
                Visibility::Crate,
            ),
            part(
                "c",
                "Acme::Ledger",
                "sealed partial class Ledger : IAudit",
                Visibility::Crate,
            ),
            // Another arity is another type.
            part(
                "d",
                "Acme::Ledger",
                "partial class Ledger<T>",
                Visibility::Crate,
            ),
            part(
                "e",
                "Acme::Ledger",
                "partial class Ledger<T> where T : class",
                Visibility::Crate,
            ),
            // Not partial: left alone.
            part("f", "Acme::Other", "class Other", Visibility::Crate),
        ];
        unify_partial_type_visibility(&mut symbols, &HashMap::new());
        let visibility = symbols
            .iter()
            .map(|symbol| symbol.visibility)
            .collect::<Vec<_>>();
        assert_eq!(
            visibility,
            vec![
                Visibility::Public,
                Visibility::Public,
                Visibility::Public,
                Visibility::Crate,
                Visibility::Crate,
                Visibility::Crate,
            ]
        );
    }

    #[test]
    fn conflicting_or_disagreeing_parts_are_unknown() {
        let mut conflicting = vec![
            part(
                "a",
                "Acme::Ledger",
                "public partial class Ledger",
                Visibility::Public,
            ),
            part(
                "b",
                "Acme::Ledger",
                "internal partial class Ledger",
                Visibility::Crate,
            ),
        ];
        unify_partial_type_visibility(&mut conflicting, &HashMap::new());
        assert!(conflicting
            .iter()
            .all(|symbol| symbol.visibility == Visibility::Unknown));

        // A nested part's default is private, a top-level one's internal: the parser read two
        // places, and neither says which the type is.
        let mut disagreeing = vec![
            part(
                "a",
                "Acme::Ledger",
                "partial class Ledger",
                Visibility::Crate,
            ),
            part(
                "b",
                "Acme::Ledger",
                "partial class Ledger",
                Visibility::Private,
            ),
        ];
        unify_partial_type_visibility(&mut disagreeing, &HashMap::new());
        assert!(disagreeing
            .iter()
            .all(|symbol| symbol.visibility == Visibility::Unknown));
    }

    #[test]
    fn parts_in_two_projects_are_two_types() {
        let mut symbols = vec![
            part(
                "a",
                "Acme::Shared::Widget",
                "public partial class Widget",
                Visibility::Public,
            ),
            part(
                "b",
                "Acme::Shared::Widget",
                "partial class Widget",
                Visibility::Crate,
            ),
        ];
        let projects = HashMap::from([
            (FileId::new("a"), PathBuf::from("src/A")),
            (FileId::new("b"), PathBuf::from("src/B")),
        ]);
        unify_partial_type_visibility(&mut symbols, &projects);
        assert_eq!(symbols[0].visibility, Visibility::Public);
        assert_eq!(symbols[1].visibility, Visibility::Crate);
        // In one project, the same parts are one type.
        let projects = HashMap::from([
            (FileId::new("a"), PathBuf::from("src/A")),
            (FileId::new("b"), PathBuf::from("src/A")),
        ]);
        unify_partial_type_visibility(&mut symbols, &projects);
        assert_eq!(symbols[1].visibility, Visibility::Public);
    }

    #[test]
    fn type_headers_read_modifiers_and_arity() {
        let header = TypeHeader::read(
            "protected internal partial record struct Pair<TKey, Dictionary<A, B>> : IPair",
        )
        .unwrap();
        assert!(header.partial);
        assert_eq!(header.declared, Some(Visibility::Protected));
        assert_eq!(header.arity, 2);
        let header = TypeHeader::read("partial class Entry : IList<int>").unwrap();
        assert_eq!(header.arity, 0);
        assert_eq!(header.declared, None);
    }
}
