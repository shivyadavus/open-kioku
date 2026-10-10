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
/// two types even under one name.
///
/// A file in no project's directory is read as one a project compiles from outside it, such as
/// a shared project's (`.shproj`/`.projitems`) file, which every importing project compiles. Such
/// a part joins the parts of each project that has a part of the type, and, with no project
/// part of the type at all, the other parts outside a project, which is the whole repository's
/// type when it holds no project file. A part outside a project that joins several projects
/// takes their common accessibility, or [`Visibility::Unknown`] when they differ. Project files
/// are not read, so a file a project compiles from inside another project's directory is
/// matched with that other project.
///
/// When the parts declare different accessibilities, which C# rejects, or declare none and
/// still disagree, the type's accessibility is not known and every part records
/// [`Visibility::Unknown`].
pub fn unify_partial_type_visibility(symbols: &mut [Symbol], projects: &HashMap<FileId, PathBuf>) {
    /// One type's parts: by the project each file is in, and outside any project.
    #[derive(Default)]
    struct Parts<'a> {
        in_project: BTreeMap<&'a PathBuf, Vec<usize>>,
        outside: Vec<usize>,
    }
    type TypeKey = (String, bool, usize);
    let mut types: BTreeMap<TypeKey, Parts<'_>> = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        if symbol.language != Language::CSharp
            || !matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
        {
            continue;
        }
        let Some(header) = symbol.signature.as_deref().and_then(TypeHeader::read) else {
            continue;
        };
        if !header.partial {
            continue;
        }
        let parts = types
            .entry((
                symbol.qualified_name.clone(),
                symbol.kind == SymbolKind::Interface,
                header.arity,
            ))
            .or_default();
        match projects.get(&symbol.file_id) {
            Some(project) => parts.in_project.entry(project).or_default().push(index),
            None => parts.outside.push(index),
        }
    }
    // Every group is read from the parsed visibilities before any is written, so a part outside
    // a project that joins several groups cannot carry one group's answer into the next.
    let mut unified: Vec<(usize, Visibility)> = Vec::new();
    for parts in types.values() {
        let groups = if parts.in_project.is_empty() {
            vec![parts.outside.clone()]
        } else {
            parts
                .in_project
                .values()
                .map(|indices| [indices.as_slice(), parts.outside.as_slice()].concat())
                .collect()
        };
        let mut outside: Option<Visibility> = None;
        for group in groups.iter().filter(|group| group.len() > 1) {
            let visibility = group_visibility(symbols, group);
            for &index in group {
                if parts.outside.contains(&index) {
                    outside = Some(match outside {
                        Some(seen) if seen != visibility => Visibility::Unknown,
                        _ => visibility,
                    });
                } else {
                    unified.push((index, visibility));
                }
            }
        }
        if let Some(visibility) = outside {
            unified.extend(parts.outside.iter().map(|&index| (index, visibility)));
        }
    }
    for (index, visibility) in unified {
        symbols[index].visibility = visibility;
    }
}

/// The accessibility one assembly's parts of a partial type give it.
fn group_visibility(symbols: &[Symbol], group: &[usize]) -> Visibility {
    let declared = group
        .iter()
        .filter_map(|&index| {
            symbols[index]
                .signature
                .as_deref()
                .and_then(TypeHeader::read)
                .and_then(|header| header.declared)
        })
        .collect::<Vec<_>>();
    match declared.split_first() {
        Some((first, rest)) if rest.iter().all(|other| other == first) => *first,
        Some(_) => Visibility::Unknown,
        None => {
            let first = symbols[group[0]].visibility;
            if group
                .iter()
                .all(|&index| symbols[index].visibility == first)
            {
                first
            } else {
                Visibility::Unknown
            }
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

    /// A shared project's file sits under no project file; the project importing it compiles
    /// the shared part and its own part into one type.
    #[test]
    fn a_shared_project_part_joins_the_project_that_imports_it() {
        let mut symbols = vec![
            part(
                "shared",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
            part(
                "app",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
        ];
        let projects = HashMap::from([(FileId::new("app"), PathBuf::from("App"))]);
        unify_partial_type_visibility(&mut symbols, &projects);
        assert_eq!(symbols[0].visibility, Visibility::Public);
        assert_eq!(symbols[1].visibility, Visibility::Public);
    }

    #[test]
    fn a_part_outside_a_project_is_read_with_each_project_it_joins() {
        // The shared part declares `internal`: with the app's `public` part that is C#'s
        // conflict, so the app's type is unknown; the library's part omits its accessibility,
        // so the library's type is internal. The shared part is in both, and they differ.
        let mut conflicting = vec![
            part(
                "shared",
                "Acme::Ledger::Entry",
                "internal partial class Entry",
                Visibility::Crate,
            ),
            part(
                "app",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
            part(
                "lib",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
        ];
        let projects = HashMap::from([
            (FileId::new("app"), PathBuf::from("App")),
            (FileId::new("lib"), PathBuf::from("Lib")),
        ]);
        unify_partial_type_visibility(&mut conflicting, &projects);
        let visibility = |symbols: &[Symbol]| {
            symbols
                .iter()
                .map(|symbol| symbol.visibility)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            visibility(&conflicting),
            vec![Visibility::Unknown, Visibility::Unknown, Visibility::Crate]
        );

        // The shared part omits its accessibility: the app's type is public, the library's
        // internal, and the one shared declaration is both.
        let mut differing = vec![
            part(
                "shared",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
            part(
                "app",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
            part(
                "lib",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
        ];
        unify_partial_type_visibility(&mut differing, &projects);
        assert_eq!(
            visibility(&differing),
            vec![Visibility::Unknown, Visibility::Public, Visibility::Crate]
        );

        // Both projects give the type one accessibility: so does the shared part.
        let mut agreeing = vec![
            part(
                "shared",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
            part(
                "app",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
            part(
                "lib",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
        ];
        unify_partial_type_visibility(&mut agreeing, &projects);
        assert_eq!(visibility(&agreeing), vec![Visibility::Public; 3]);
    }

    /// With no project file in the repository every part is outside a project, and the parts of
    /// a type are matched across the repository, as before parts were scoped by project.
    #[test]
    fn without_project_files_parts_are_matched_across_the_repository() {
        let mut symbols = vec![
            part(
                "a",
                "Acme::Ledger::Entry",
                "public partial class Entry",
                Visibility::Public,
            ),
            part(
                "b",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
            part(
                "c",
                "Acme::Ledger::Entry",
                "partial class Entry",
                Visibility::Crate,
            ),
        ];
        unify_partial_type_visibility(&mut symbols, &HashMap::new());
        assert!(symbols
            .iter()
            .all(|symbol| symbol.visibility == Visibility::Public));
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
