//! C# declarations: which nodes are symbols, what they are called, and how far they reach.
//!
//! A C# name belongs to its namespace and enclosing types, never to its file: `Acme.Ledger.Entry`
//! may be declared in any file, and a `partial` type in several. The qualified name is therefore
//! built from the syntax (`Acme::Ledger::Entry::Post`), so every part of a partial type, and every
//! member declared in any of them, shares the type's qualified name while each declaration keeps
//! its own symbol id and line range.
//!
//! Only declarations are read here; nothing in this module emits a relationship. `using`
//! directives belong in `extract_import`, calls in `extract_call`, base lists in
//! `extract_inheritance` and locals in `extract_binding`, all of which skip C# today. Resolution
//! reads those facts only for a language with a `LanguageSemantics` adapter in
//! `open-kioku-languages`, and C# has none yet.

use open_kioku_core::{LineRange, ScopeKind, SymbolKind, Visibility};
use tree_sitter::Node;

pub(crate) const FILE_SCOPED_NAMESPACE: &str = "file_scoped_namespace_declaration";

/// Declarations that introduce a type, and so a level of the qualified name.
fn is_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
    )
}

/// Types that hold members: every type declaration but an enum, whose body holds only values.
fn holds_members(kind: &str) -> bool {
    is_type_declaration(kind) && kind != "enum_declaration"
}

/// Members whose body can hold a local function, which is named inside them.
fn names_local_functions(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "local_function_statement"
            | "property_declaration"
            | "indexer_declaration"
            | "event_declaration"
    )
}

pub(crate) fn scope_kind(kind: &str) -> Option<ScopeKind> {
    match kind {
        "namespace_declaration" | FILE_SCOPED_NAMESPACE => Some(ScopeKind::Namespace),
        "class_declaration" | "struct_declaration" | "record_declaration" | "enum_declaration" => {
            Some(ScopeKind::Class)
        }
        "interface_declaration" => Some(ScopeKind::Interface),
        "method_declaration"
        | "constructor_declaration"
        | "destructor_declaration"
        | "operator_declaration"
        | "conversion_operator_declaration"
        | "accessor_declaration" => Some(ScopeKind::Method),
        "local_function_statement" => Some(ScopeKind::Function),
        "lambda_expression" | "anonymous_method_expression" => Some(ScopeKind::Closure),
        "block" => Some(ScopeKind::Block),
        _ => None,
    }
}

/// The name and kind of the symbol `node` declares, if it declares one where C# allows it.
///
/// A field or event field declares one symbol per declarator (`int a = 1, b = 2;`), so the
/// declarator is the symbol node; [`declaration_of`] gives the declaration its range, modifiers
/// and signature come from. Members without an identifier take the name C# tooling shows for
/// them: `this[]` for an indexer, `operator +` and `implicit operator Money` for operators, and
/// `~Entry` for a finalizer. An explicit interface implementation (`int IEntry.Total()`) is
/// named with its interface, `IEntry.Total`: it is reachable only through that interface, and
/// must not read as a second `Total` of the class.
///
/// A declaration whose place the grammar's error recovery invented (a method directly in a
/// namespace, a type inside a method) is not a symbol: see [`is_placed`].
pub(crate) fn symbol(node: Node<'_>, source: &[u8]) -> Option<(String, SymbolKind)> {
    let (name, kind) = declared(node, source)?;
    (!name.is_empty() && is_placed(node)).then_some((name, kind))
}

fn declared(node: Node<'_>, source: &[u8]) -> Option<(String, SymbolKind)> {
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(compact);
    let name = || node.child_by_field_name("name").and_then(text);
    let through_interface = |name: String| match explicit_interface(node, source) {
        Some(interface) => format!("{interface}.{name}"),
        None => name,
    };
    match node.kind() {
        "namespace_declaration" | FILE_SCOPED_NAMESPACE => {
            let name = node.child_by_field_name("name")?.utf8_text(source).ok()?;
            Some((squeeze(name), SymbolKind::Package))
        }
        "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "enum_declaration"
        | "delegate_declaration" => Some((name()?, SymbolKind::Class)),
        "interface_declaration" => Some((name()?, SymbolKind::Interface)),
        "method_declaration" => Some((through_interface(name()?), SymbolKind::Method)),
        "constructor_declaration" => Some((name()?, SymbolKind::Method)),
        "destructor_declaration" => Some((format!("~{}", name()?), SymbolKind::Method)),
        "operator_declaration" => {
            let operator = node.child_by_field_name("operator").and_then(text)?;
            Some((format!("operator {operator}"), SymbolKind::Method))
        }
        "conversion_operator_declaration" => {
            let direction = if has_child(node, "explicit") {
                "explicit"
            } else {
                "implicit"
            };
            let target = node.child_by_field_name("type").and_then(text)?;
            Some((format!("{direction} operator {target}"), SymbolKind::Method))
        }
        "local_function_statement" => Some((name()?, SymbolKind::Function)),
        "property_declaration" | "event_declaration" => {
            Some((through_interface(name()?), SymbolKind::Field))
        }
        "indexer_declaration" => Some((through_interface("this[]".into()), SymbolKind::Field)),
        "enum_member_declaration" => Some((name()?, SymbolKind::Constant)),
        "variable_declarator" => {
            let declaration = declaration_of(node);
            let kind = match declaration.kind() {
                "field_declaration" if has_modifier(declaration, "const") => SymbolKind::Constant,
                "field_declaration" | "event_field_declaration" => SymbolKind::Field,
                // A local variable is a binding, not a symbol.
                _ => return None,
            };
            Some((name()?, kind))
        }
        // A positional record parameter is a public property of the record (`record
        // Money(decimal Amount)` declares `Money.Amount`). A class or struct primary constructor
        // parameter is not: it is captured, never a member.
        "parameter" if is_record_parameter(node) => Some((name()?, SymbolKind::Field)),
        _ => None,
    }
}

fn is_record_parameter(node: Node<'_>) -> bool {
    node.parent().is_some_and(|list| {
        list.kind() == "parameter_list"
            && list
                .parent()
                .is_some_and(|owner| owner.kind() == "record_declaration")
    })
}

/// `IEntry` of `int IEntry.Total()`, `IEnumerable<T>` of `IEnumerator<T> IEnumerable<T>.Get..`.
fn explicit_interface(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let specifier = node
        .children(&mut cursor)
        .find(|child| child.kind() == "explicit_interface_specifier")?;
    let interface = squeeze(specifier.utf8_text(source).ok()?);
    let interface = interface.trim_end_matches('.');
    (!interface.is_empty()).then(|| interface.to_string())
}

/// The declaration a symbol node belongs to: a field or event field for one of its declarators,
/// the node itself otherwise.
pub(crate) fn declaration_of(node: Node<'_>) -> Node<'_> {
    if node.kind() == "variable_declarator" {
        if let Some(declaration) = node
            .parent()
            .filter(|parent| parent.kind() == "variable_declaration")
            .and_then(|variable| variable.parent())
        {
            return declaration;
        }
    }
    node
}

/// The declaration or compilation unit `declaration` is written in, through the `{ }` list that
/// holds it and any `#if`/`#elif`/`#else` region around it.
fn owner(declaration: Node<'_>) -> Option<Node<'_>> {
    let mut parent = declaration.parent()?;
    while matches!(
        parent.kind(),
        "declaration_list"
            | "enum_member_declaration_list"
            | "preproc_if"
            | "preproc_elif"
            | "preproc_else"
    ) {
        parent = parent.parent()?;
    }
    Some(parent)
}

/// Whether the declaration stands where C# allows it: a namespace in the compilation unit or a
/// namespace, a type there or in another type, a member in a class, struct, record or interface,
/// an enum member in an enum. A parse with syntax errors keeps every declaration its recovery
/// left whole, but recovery can lift a member out of a type whose header it could not read; that
/// member's enclosing type, and so its qualified name, would be wrong.
fn is_placed(node: Node<'_>) -> bool {
    let declaration = declaration_of(node);
    let owner = owner(declaration).map(|owner| owner.kind());
    match declaration.kind() {
        "namespace_declaration" => {
            matches!(owner, Some("compilation_unit" | "namespace_declaration"))
        }
        FILE_SCOPED_NAMESPACE => owner == Some("compilation_unit"),
        "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "delegate_declaration" => owner.is_some_and(|owner| {
            matches!(owner, "compilation_unit" | "namespace_declaration") || holds_members(owner)
        }),
        "enum_member_declaration" => owner == Some("enum_declaration"),
        "local_function_statement" | "parameter" => true,
        _ => owner.is_some_and(holds_members),
    }
}

/// A declaration's line range. A file-scoped namespace (`namespace Acme.Ledger;`) holds every
/// declaration after it to the end of the file, though the grammar ends its node at the `;`.
pub(crate) fn line_range(node: Node<'_>) -> LineRange {
    let declaration = declaration_of(node);
    let start = declaration.start_position().row as u32 + 1;
    let end = match declaration.parent() {
        Some(unit) if declaration.kind() == FILE_SCOPED_NAMESPACE => last_line_of(unit),
        _ => declaration.end_position().row as u32 + 1,
    };
    LineRange {
        start,
        end: end.max(start),
    }
}

/// The last line a node's text is on: a compilation unit ends at column 0 of the line after a
/// trailing newline, which holds none of it.
pub(crate) fn last_line_of(node: Node<'_>) -> u32 {
    let end = node.end_position();
    if end.column == 0 && end.row > node.start_position().row {
        end.row as u32
    } else {
        end.row as u32 + 1
    }
}

/// The qualified name of `name` declared by `node`: the namespaces and types enclosing it,
/// outermost first, then the name. A namespace name splits on `.` (`namespace Acme.Ledger` is
/// the two levels `Acme::Ledger`), a type in a file-scoped namespace is in that namespace, and a
/// local function is named inside the member that declares it. Type parameters are not part of
/// it, as parameter lists are not part of a method's: `Entry` and `Entry<T>` share a qualified
/// name the way overloads do, and the signature tells them apart.
pub(crate) fn qualified_name(node: Node<'_>, name: &str, source: &[u8]) -> String {
    let mut levels: Vec<String> = Vec::new();
    let declaration = declaration_of(node);
    let mut ancestor = declaration.parent();
    while let Some(current) = ancestor {
        let kind = current.kind();
        let enclosing = if kind == "namespace_declaration" {
            name_text(current, source)
                .map(|name| namespace_levels(&name))
                .unwrap_or_default()
        } else if is_type_declaration(kind) {
            name_text(current, source).into_iter().collect()
        } else if names_local_functions(kind) {
            declared(current, source)
                .map(|(name, _)| vec![name])
                .unwrap_or_default()
        } else if kind == "compilation_unit" && declaration.kind() != FILE_SCOPED_NAMESPACE {
            file_scoped_namespace(current)
                .and_then(|namespace| name_text(namespace, source))
                .map(|name| namespace_levels(&name))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        levels.splice(0..0, enclosing);
        ancestor = current.parent();
    }
    match declaration.kind() {
        "namespace_declaration" | FILE_SCOPED_NAMESPACE => levels.extend(namespace_levels(name)),
        _ => levels.push(name.to_string()),
    }
    levels.join("::")
}

/// The file-scoped namespace a compilation unit declares; C# allows one.
pub(crate) fn file_scoped_namespace(unit: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = unit.walk();
    let found = unit
        .named_children(&mut cursor)
        .find(|child| child.kind() == FILE_SCOPED_NAMESPACE);
    found
}

fn name_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        .map(compact)
        .filter(|name| !name.is_empty())
}

fn namespace_levels(name: &str) -> Vec<String> {
    name.split('.')
        .map(squeeze)
        .filter(|level| !level.is_empty())
        .collect()
}

/// Reads the declaration's own `modifier` children, never its text, and applies C#'s defaults
/// where it has none: a type directly in a namespace or the compilation unit is `internal`, a
/// member of a class, struct or record (a nested type too) is `private`, a member of an
/// interface is `public`, and an enum member is always `public`.
///
/// C#'s access levels map onto the shared scale by who can name the declaration: `internal`
/// (the assembly) is [`Visibility::Crate`]; `protected internal` (the assembly, and derived types
/// anywhere) is [`Visibility::Protected`], which for Java already means "package or subclass";
/// `private protected` (derived types inside the assembly) is [`Visibility::Crate`], the narrowest
/// level holding every type that can name it; `file` is [`Visibility::Private`], as a Rust item
/// private to its module is. A namespace has no accessibility and is `Public`. The signature keeps
/// the modifiers as written, so the exact level is never lost.
pub(crate) fn visibility(node: Node<'_>) -> Visibility {
    let declaration = declaration_of(node);
    match declaration.kind() {
        "namespace_declaration" | FILE_SCOPED_NAMESPACE | "enum_member_declaration" => {
            return Visibility::Public;
        }
        // A positional record parameter is a public property.
        "parameter" => return Visibility::Public,
        // A local function is visible only inside the member that declares it, and a finalizer
        // or static constructor cannot be named at all.
        "local_function_statement" | "destructor_declaration" => return Visibility::Private,
        "constructor_declaration" if has_modifier(declaration, "static") => {
            return Visibility::Private;
        }
        // An explicit interface implementation takes no modifier: it is the interface's member,
        // reachable through the interface alone.
        "method_declaration"
        | "property_declaration"
        | "event_declaration"
        | "indexer_declaration"
            if has_child(declaration, "explicit_interface_specifier") =>
        {
            return Visibility::Public;
        }
        _ => {}
    }
    let declared = |keyword: &str| has_modifier(declaration, keyword);
    if declared("public") {
        Visibility::Public
    } else if declared("protected") {
        if declared("private") {
            Visibility::Crate
        } else {
            Visibility::Protected
        }
    } else if declared("internal") {
        Visibility::Crate
    } else if declared("private") || declared("file") {
        Visibility::Private
    } else {
        match owner(declaration).map(|owner| owner.kind()) {
            Some("interface_declaration") => Visibility::Public,
            Some("namespace_declaration" | "compilation_unit") => Visibility::Crate,
            _ => Visibility::Private,
        }
    }
}

fn has_modifier(node: Node<'_>, keyword: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| {
        child.kind() == "modifier" && child.child(0).is_some_and(|token| token.kind() == keyword)
    });
    found
}

fn has_child(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| child.kind() == kind);
    found
}

/// The declaration's header as written, whitespace collapsed: modifiers, type, name, type
/// parameters, parameters, base list and constraints, without attributes, constructor
/// initializer or body. `public sealed partial class Entry<T> : Base where T : class`, `public
/// Task<int> PostAsync<U>(U value, int n = 1) where U : struct`, `private const int Limit = 3`.
/// Every part of a partial type spells `partial` here.
pub(crate) fn signature(node: Node<'_>, source: &[u8]) -> Option<String> {
    let declaration = declaration_of(node);
    if node.kind() == "variable_declarator" {
        // One declarator of `static readonly int a = 1, b = 2;` is `static readonly int b = 2`.
        let mut parts = Vec::new();
        let mut cursor = declaration.walk();
        for child in declaration.children(&mut cursor) {
            match child.kind() {
                "modifier" | "event" => parts.push(child.utf8_text(source).ok()?),
                "variable_declaration" => {
                    if let Some(kind) = child.child_by_field_name("type") {
                        parts.push(kind.utf8_text(source).ok()?);
                    }
                }
                _ => {}
            }
        }
        parts.push(node.utf8_text(source).ok()?);
        return Some(compact(&parts.join(" ")));
    }
    let mut cursor = declaration.walk();
    let children = declaration.children(&mut cursor).collect::<Vec<_>>();
    let start = children
        .iter()
        .find(|child| !matches!(child.kind(), "attribute_list" | "comment"))?
        .start_byte();
    let end = children
        .iter()
        .enumerate()
        .find(|(index, child)| {
            let field = u32::try_from(*index)
                .ok()
                .and_then(|index| declaration.field_name_for_child(index));
            matches!(field, Some("body" | "accessors"))
                || matches!(
                    child.kind(),
                    "declaration_list"
                        | "enum_member_declaration_list"
                        | "accessor_list"
                        | "arrow_expression_clause"
                        | "block"
                        | "constructor_initializer"
                        | ";"
                        | "{"
                )
        })
        .map_or(declaration.end_byte(), |(_, child)| child.start_byte());
    let header = std::str::from_utf8(source.get(start..end.max(start))?).ok()?;
    let header = compact(header);
    (!header.is_empty()).then_some(header)
}

/// Collapses runs of whitespace, including newlines, to single spaces.
fn compact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drops all whitespace: `Acme . Ledger` and `Acme.Ledger` are one dotted name.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect()
}

#[cfg(test)]
mod tests {
    use crate::parse_file;
    use open_kioku_core::{
        Confidence, File, FileId, Language, RepositoryId, Symbol, SymbolKind, SyntaxFacts,
        Visibility,
    };

    fn file(path: &str) -> File {
        File {
            id: FileId::new(path),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::CSharp,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn parse(path: &str, source: &str) -> SyntaxFacts {
        parse_file(&file(path), source).expect("C# source parses")
    }

    fn find<'a>(facts: &'a SyntaxFacts, qualified: &str) -> &'a Symbol {
        let found = facts
            .symbols
            .iter()
            .filter(|symbol| symbol.qualified_name == qualified)
            .collect::<Vec<_>>();
        assert_eq!(
            found.len(),
            1,
            "{qualified} in {:?}",
            facts
                .symbols
                .iter()
                .map(|symbol| &symbol.qualified_name)
                .collect::<Vec<_>>()
        );
        found[0]
    }

    /// `(qualified name, kind, visibility, first line, last line)` of every symbol, in order.
    fn table(facts: &SyntaxFacts) -> Vec<(String, SymbolKind, Visibility, u32, u32)> {
        facts
            .symbols
            .iter()
            .map(|symbol| {
                let range = symbol
                    .range
                    .as_ref()
                    .expect("tree-sitter symbols have ranges");
                (
                    symbol.qualified_name.clone(),
                    symbol.kind.clone(),
                    symbol.visibility,
                    range.start,
                    range.end,
                )
            })
            .collect()
    }

    fn row(
        qualified: &str,
        kind: SymbolKind,
        visibility: Visibility,
        start: u32,
        end: u32,
    ) -> (String, SymbolKind, Visibility, u32, u32) {
        (qualified.to_string(), kind, visibility, start, end)
    }

    const LEDGER: &str = r#"using System;

namespace Acme.Ledger
{
    /// <summary>A posted entry.</summary>
    [Serializable]
    public sealed class Entry : IEntry
    {
        private const int Limit = 3;
        internal static readonly int Low = 1, High = 2;
        public decimal Amount { get; private set; }
        public decimal this[int index] => Amount;
        public event EventHandler Posted;
        event EventHandler Voided { add { } remove { } }
        public Entry() { }
        static Entry() { }
        ~Entry() { }
        public static Entry operator +(Entry left, Entry right) => left;
        public static explicit operator decimal(Entry entry) => entry.Amount;
        public decimal Settle(int days)
        {
            decimal Rate(int d) => d * Limit;
            return Rate(days);
        }
        decimal IEntry.Total() => Amount;
    }

    public interface IEntry
    {
        decimal Total();
    }

    public enum Side { Debit = 1, Credit }

    public struct Cursor { int offset; }

    public delegate void PostedHandler(Entry entry);
}
"#;

    #[test]
    fn every_declaration_kind_is_a_symbol_with_its_range() {
        let facts = parse("src/Ledger/Entry.cs", LEDGER);
        use SymbolKind::*;
        use Visibility::{Crate, Private, Public};
        assert_eq!(
            table(&facts),
            vec![
                row("Acme::Ledger", Package, Public, 3, 38),
                row("Acme::Ledger::Entry", Class, Public, 6, 26),
                row("Acme::Ledger::Entry::Limit", Constant, Private, 9, 9),
                row("Acme::Ledger::Entry::Low", Field, Crate, 10, 10),
                row("Acme::Ledger::Entry::High", Field, Crate, 10, 10),
                row("Acme::Ledger::Entry::Amount", Field, Public, 11, 11),
                row("Acme::Ledger::Entry::this[]", Field, Public, 12, 12),
                row("Acme::Ledger::Entry::Posted", Field, Public, 13, 13),
                row("Acme::Ledger::Entry::Voided", Field, Private, 14, 14),
                row("Acme::Ledger::Entry::Entry", Method, Public, 15, 15),
                row("Acme::Ledger::Entry::Entry", Method, Private, 16, 16),
                row("Acme::Ledger::Entry::~Entry", Method, Private, 17, 17),
                row("Acme::Ledger::Entry::operator +", Method, Public, 18, 18),
                row(
                    "Acme::Ledger::Entry::explicit operator decimal",
                    Method,
                    Public,
                    19,
                    19
                ),
                row("Acme::Ledger::Entry::Settle", Method, Public, 20, 24),
                row(
                    "Acme::Ledger::Entry::Settle::Rate",
                    Function,
                    Private,
                    22,
                    22
                ),
                row("Acme::Ledger::Entry::IEntry.Total", Method, Public, 25, 25),
                row("Acme::Ledger::IEntry", Interface, Public, 28, 31),
                row("Acme::Ledger::IEntry::Total", Method, Public, 30, 30),
                row("Acme::Ledger::Side", Class, Public, 33, 33),
                row("Acme::Ledger::Side::Debit", Constant, Public, 33, 33),
                row("Acme::Ledger::Side::Credit", Constant, Public, 33, 33),
                row("Acme::Ledger::Cursor", Class, Public, 35, 35),
                row("Acme::Ledger::Cursor::offset", Field, Private, 35, 35),
                row("Acme::Ledger::PostedHandler", Class, Public, 37, 37),
            ]
        );
        assert!(facts
            .symbols
            .iter()
            .all(|symbol| symbol.confidence == Confidence::High));
        // Nothing here is a relationship: calls, base lists and `using` belong to resolution.
        assert!(facts.calls.is_empty());
        assert!(facts.imports.is_empty());
        assert!(facts.inheritance.is_empty());
        assert!(facts.bindings.is_empty());
    }

    #[test]
    fn names_follow_csharp_tooling_for_members_without_an_identifier() {
        let facts = parse("Entry.cs", LEDGER);
        let names = facts
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        for name in [
            "Acme.Ledger",
            "this[]",
            "~Entry",
            "operator +",
            "explicit operator decimal",
            "IEntry.Total",
        ] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
    }

    #[test]
    fn members_belong_to_their_declaring_symbol() {
        let facts = parse("Entry.cs", LEDGER);
        let namespace = find(&facts, "Acme::Ledger");
        let entry = find(&facts, "Acme::Ledger::Entry");
        let settle = find(&facts, "Acme::Ledger::Entry::Settle");
        assert_eq!(namespace.parent_symbol_id, None);
        assert_eq!(entry.parent_symbol_id.as_ref(), Some(&namespace.id));
        assert_eq!(settle.parent_symbol_id.as_ref(), Some(&entry.id));
        // A local function belongs to the member that declares it.
        let rate = find(&facts, "Acme::Ledger::Entry::Settle::Rate");
        assert_eq!(rate.parent_symbol_id.as_ref(), Some(&settle.id));
        let debit = find(&facts, "Acme::Ledger::Side::Debit");
        assert_eq!(
            debit.parent_symbol_id.as_ref(),
            Some(&find(&facts, "Acme::Ledger::Side").id)
        );
    }

    #[test]
    fn visibility_follows_csharp_defaults_and_modifiers() {
        let facts = parse(
            "Access.cs",
            r#"namespace Acme
{
    class Implicit
    {
        int field;
        void Method() { }
        class Nested { }
        public class Open { }
        protected internal void Either() { }
        internal protected void EitherReversed() { }
        private protected void Both() { }
        protected void Derived() { }
        internal void Assembly() { }
        private void Own() { }
    }
    struct Value { int field; }
    interface IShape
    {
        void Draw();
        int Sides { get; }
        private void Helper() { }
        protected void Hook() { }
        class Default { }
    }
    file class Local { }
    internal class Declared { }
}
class Global { void Run() { } }
"#,
        );
        use Visibility::*;
        for (qualified, visibility) in [
            ("Acme::Implicit", Crate),
            ("Acme::Implicit::field", Private),
            ("Acme::Implicit::Method", Private),
            ("Acme::Implicit::Nested", Private),
            ("Acme::Implicit::Open", Public),
            ("Acme::Implicit::Either", Protected),
            ("Acme::Implicit::EitherReversed", Protected),
            ("Acme::Implicit::Both", Crate),
            ("Acme::Implicit::Derived", Protected),
            ("Acme::Implicit::Assembly", Crate),
            ("Acme::Implicit::Own", Private),
            ("Acme::Value", Crate),
            ("Acme::Value::field", Private),
            ("Acme::IShape", Crate),
            ("Acme::IShape::Draw", Public),
            ("Acme::IShape::Sides", Public),
            ("Acme::IShape::Helper", Private),
            ("Acme::IShape::Hook", Protected),
            ("Acme::IShape::Default", Public),
            ("Acme::Local", Private),
            ("Acme::Declared", Crate),
            ("Global", Crate),
            ("Global::Run", Private),
        ] {
            assert_eq!(
                find(&facts, qualified).visibility,
                visibility,
                "{qualified}"
            );
        }
    }

    #[test]
    fn a_modifier_in_a_body_does_not_change_the_declaration() {
        let facts = parse(
            "Body.cs",
            "class Holder\n{\n    void Run()\n    {\n        var text = \"public static\";\n    }\n}\n",
        );
        assert_eq!(find(&facts, "Holder::Run").visibility, Visibility::Private);
    }

    #[test]
    fn nested_namespaces_and_types_qualify_their_members() {
        let facts = parse(
            "Nested.cs",
            r#"namespace Acme
{
    namespace Ledger.Books
    {
        public class Outer
        {
            public class Middle
            {
                public struct Inner
                {
                    public void Touch() { }
                }
            }
        }
    }
}
"#,
        );
        assert_eq!(find(&facts, "Acme::Ledger::Books").name, "Ledger.Books");
        let touch = find(&facts, "Acme::Ledger::Books::Outer::Middle::Inner::Touch");
        let inner = find(&facts, "Acme::Ledger::Books::Outer::Middle::Inner");
        assert_eq!(touch.parent_symbol_id.as_ref(), Some(&inner.id));
        assert_eq!(
            find(&facts, "Acme::Ledger::Books::Outer::Middle").parent_symbol_id,
            Some(find(&facts, "Acme::Ledger::Books::Outer").id.clone())
        );
    }

    #[test]
    fn a_file_scoped_namespace_holds_the_rest_of_the_file() {
        let facts = parse(
            "src/Billing/Invoice.cs",
            "namespace Acme.Billing;\n\nusing System;\n\npublic class Invoice\n{\n    public void Send() { }\n}\n\ninterface IPayable { }\n",
        );
        use SymbolKind::*;
        use Visibility::{Crate, Public};
        assert_eq!(
            table(&facts),
            vec![
                row("Acme::Billing", Package, Public, 1, 10),
                row("Acme::Billing::Invoice", Class, Public, 5, 8),
                row("Acme::Billing::Invoice::Send", Method, Public, 7, 7),
                row("Acme::Billing::IPayable", Interface, Crate, 10, 10),
            ]
        );
        let namespace = find(&facts, "Acme::Billing");
        assert_eq!(
            find(&facts, "Acme::Billing::IPayable")
                .parent_symbol_id
                .as_ref(),
            Some(&namespace.id)
        );
        let scope = facts
            .scopes
            .iter()
            .find(|scope| scope.kind == open_kioku_core::ScopeKind::Namespace)
            .expect("the namespace is a scope");
        assert_eq!((scope.range.start_line, scope.range.end_line), (1, 10));
        assert_eq!(
            find(&facts, "Acme::Billing::Invoice").scope_id.as_ref(),
            Some(&scope.id)
        );
    }

    #[test]
    fn partial_type_parts_share_one_qualified_name_across_files() {
        let first = parse(
            "src/Ledger.cs",
            "namespace Acme;\n\npublic partial class Ledger\n{\n    public void Post() { }\n}\n",
        );
        let second = parse(
            "src/Ledger.Audit.cs",
            "namespace Acme\n{\n    partial class Ledger\n    {\n        private int audits;\n        public void Audit() { }\n    }\n}\n",
        );
        let one = find(&first, "Acme::Ledger");
        let other = find(&second, "Acme::Ledger");
        assert_ne!(one.id, other.id, "each part is its own declaration");
        assert_eq!(
            (one.kind.clone(), other.kind.clone()),
            (SymbolKind::Class, SymbolKind::Class)
        );
        for part in [one, other] {
            assert!(
                part.signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("partial class Ledger")),
                "{:?}",
                part.signature
            );
        }
        // The part with no access modifier takes the type's declared access from the other part
        // in C#; each declaration records only what it spells, so it reads as the default.
        assert_eq!(one.visibility, Visibility::Public);
        assert_eq!(other.visibility, Visibility::Crate);
        // Members of either part are members of the one logical type.
        let post = find(&first, "Acme::Ledger::Post");
        let audit = find(&second, "Acme::Ledger::Audit");
        assert_eq!(post.parent_symbol_id.as_ref(), Some(&one.id));
        assert_eq!(audit.parent_symbol_id.as_ref(), Some(&other.id));
        assert_eq!(
            find(&second, "Acme::Ledger::audits").kind,
            SymbolKind::Field
        );
    }

    #[test]
    fn records_declare_their_positional_properties() {
        let facts = parse(
            "Money.cs",
            r#"namespace Acme;
public record Money(decimal Amount, string Currency);
public readonly record struct Point(int X, int Y)
{
    public int Sum => X + Y;
}
public record class Tag { public string Name { get; init; } }
public class Service(string name)
{
    public string Name => name;
}
"#,
        );
        use SymbolKind::*;
        use Visibility::Public;
        assert_eq!(
            table(&facts),
            vec![
                row("Acme", Package, Public, 1, 11),
                row("Acme::Money", Class, Public, 2, 2),
                row("Acme::Money::Amount", Field, Public, 2, 2),
                row("Acme::Money::Currency", Field, Public, 2, 2),
                row("Acme::Point", Class, Public, 3, 6),
                row("Acme::Point::X", Field, Public, 3, 3),
                row("Acme::Point::Y", Field, Public, 3, 3),
                row("Acme::Point::Sum", Field, Public, 5, 5),
                row("Acme::Tag", Class, Public, 7, 7),
                row("Acme::Tag::Name", Field, Public, 7, 7),
                // A class primary constructor parameter is captured, not a member.
                row("Acme::Service", Class, Public, 8, 11),
                row("Acme::Service::Name", Field, Public, 10, 10),
            ]
        );
        assert_eq!(
            find(&facts, "Acme::Money").signature.as_deref(),
            Some("public record Money(decimal Amount, string Currency)")
        );
        assert_eq!(
            find(&facts, "Acme::Point").signature.as_deref(),
            Some("public readonly record struct Point(int X, int Y)")
        );
    }

    #[test]
    fn generic_types_and_methods_keep_their_parameters_in_the_signature() {
        let facts = parse(
            "Repository.cs",
            r#"namespace Acme;
public class Repository<TEntity, TKey> : IRepository<TEntity>
    where TEntity : class, new()
    where TKey : struct
{
    public TResult Map<TResult>(TEntity entity, Func<TEntity, TResult> map)
        where TResult : notnull
    {
        return map(entity);
    }
}
public class Repository { }
"#,
        );
        let generic = facts
            .symbols
            .iter()
            .filter(|symbol| symbol.qualified_name == "Acme::Repository")
            .collect::<Vec<_>>();
        // `Repository` and `Repository<TEntity, TKey>` share a qualified name the way overloads
        // do; their ids and signatures tell them apart.
        assert_eq!(generic.len(), 2);
        assert_ne!(generic[0].id, generic[1].id);
        assert_eq!(generic[0].name, "Repository");
        assert_eq!(
            generic[0].signature.as_deref(),
            Some("public class Repository<TEntity, TKey> : IRepository<TEntity> where TEntity : class, new() where TKey : struct")
        );
        assert_eq!(
            generic[1].signature.as_deref(),
            Some("public class Repository")
        );
        let map = find(&facts, "Acme::Repository::Map");
        assert_eq!(
            map.range.as_ref().map(|range| (range.start, range.end)),
            Some((6, 10))
        );
        assert_eq!(
            map.signature.as_deref(),
            Some("public TResult Map<TResult>(TEntity entity, Func<TEntity, TResult> map) where TResult : notnull")
        );
    }

    #[test]
    fn attributes_are_inside_the_declaration_and_out_of_its_signature() {
        let facts = parse(
            "Controller.cs",
            r#"namespace Acme.Web;

/// <summary>
/// Serves ledger entries.
/// </summary>
[ApiController]
[Route("api/[controller]")]
public class EntriesController : ControllerBase
{
    /// <summary>Reads one entry.</summary>
    /// <param name="id">The entry id.</param>
    [HttpGet("{id}")]
    [ProducesResponseType(
        typeof(Entry),
        200)]
    public Entry Get(int id) => Find(id);

    [Obsolete] private int legacy;
}
"#,
        );
        let controller = find(&facts, "Acme::Web::EntriesController");
        // Attributes belong to the declaration, as Java annotations do; the `///` documentation
        // above them does not (the chunker carries it with the symbol).
        assert_eq!(
            controller
                .range
                .as_ref()
                .map(|range| (range.start, range.end)),
            Some((6, 19))
        );
        assert_eq!(
            controller.signature.as_deref(),
            Some("public class EntriesController : ControllerBase")
        );
        let get = find(&facts, "Acme::Web::EntriesController::Get");
        assert_eq!(
            get.range.as_ref().map(|range| (range.start, range.end)),
            Some((12, 16))
        );
        assert_eq!(get.signature.as_deref(), Some("public Entry Get(int id)"));
        let legacy = find(&facts, "Acme::Web::EntriesController::legacy");
        assert_eq!(legacy.signature.as_deref(), Some("private int legacy"));
        assert_eq!(legacy.visibility, Visibility::Private);
    }

    #[test]
    fn members_inside_preprocessor_regions_stay_in_their_type() {
        let facts = parse(
            "Platform.cs",
            r#"namespace Acme;
public class Platform
{
#if NET8_0_OR_GREATER
    public void Modern() { }
#else
    public void Legacy() { }
#endif
    #region Helpers
    private void Helper() { }
    #endregion
}
"#,
        );
        let platform = find(&facts, "Acme::Platform");
        for member in ["Modern", "Legacy", "Helper"] {
            let symbol = find(&facts, &format!("Acme::Platform::{member}"));
            assert_eq!(
                symbol.parent_symbol_id.as_ref(),
                Some(&platform.id),
                "{member}"
            );
        }
        assert_eq!(
            find(&facts, "Acme::Platform::Modern").visibility,
            Visibility::Public
        );
    }

    #[test]
    fn a_file_with_syntax_errors_keeps_its_whole_declarations_at_medium_confidence() {
        // This grammar version cannot read `async` as a name, which C# allows.
        let source = "namespace Acme;\npublic class Batch\n{\n    public int Count;\n    public bool Load(bool async)\n    {\n        return Fetch(async);\n    }\n    public void Clear() { }\n}\n";
        let facts = parse("Batch.cs", source);
        use SymbolKind::*;
        use Visibility::Public;
        assert_eq!(
            table(&facts),
            vec![
                row("Acme", Package, Public, 1, 10),
                row("Acme::Batch", Class, Public, 2, 10),
                row("Acme::Batch::Count", Field, Public, 4, 4),
                row("Acme::Batch::Load", Method, Public, 5, 8),
                row("Acme::Batch::Clear", Method, Public, 9, 9),
            ]
        );
        assert!(facts
            .symbols
            .iter()
            .all(|symbol| symbol.confidence == Confidence::Medium));
        // Other languages still reject a file with syntax errors, and fall back to patterns.
        let java = File {
            language: Language::Java,
            ..file("Batch.java")
        };
        assert!(parse_file(&java, "class Batch { void f() { return [; } }").is_err());
    }

    #[test]
    fn nothing_inside_an_error_node_is_a_symbol() {
        // The unbalanced brace makes recovery wrap the class in an error node; what it holds is
        // the recovery's reading, not the source's.
        let facts = parse(
            "Broken.cs",
            "namespace Acme;\npublic class Broken\n{\n    public void Lost() { if (x { }\n    public void Gone() { }\n",
        );
        assert_eq!(
            table(&facts),
            vec![row("Acme", SymbolKind::Package, Visibility::Public, 1, 5)]
        );
        assert_eq!(facts.symbols[0].confidence, Confidence::Medium);
    }

    #[test]
    fn a_member_outside_a_type_is_not_a_symbol() {
        // The grammar accepts a method or field directly in a namespace, which C# rejects; it
        // would have no type to be qualified by.
        let facts = parse(
            "Stray.cs",
            "namespace Acme\n{\n    public void Stray() { }\n    public int Loose;\n    class Kept { }\n}\n",
        );
        use SymbolKind::*;
        use Visibility::{Crate, Public};
        assert_eq!(
            table(&facts),
            vec![
                row("Acme", Package, Public, 1, 6),
                row("Acme::Kept", Class, Crate, 5, 5),
            ]
        );
        // A local function at the top level of a program is one.
        let program = parse(
            "Program.cs",
            "Console.WriteLine(Greet());\nstring Greet() => \"hi\";\n",
        );
        assert_eq!(
            table(&program),
            vec![row("Greet", Function, Visibility::Private, 2, 2)]
        );
    }
}
