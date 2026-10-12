//! C# declarations: which nodes are symbols, what they are called, and how far they reach.
//!
//! A C# name belongs to its namespace and enclosing types, never to its file: `Acme.Ledger.Entry`
//! may be declared in any file, and a `partial` type in several. The qualified name is therefore
//! built from the syntax (`Acme::Ledger::Entry::Post`), so every part of a partial type, and every
//! member declared in any of them, shares the type's qualified name while each declaration keeps
//! its own symbol id and line range.
//!
//! Nothing in this module emits a relationship. Besides declarations it reads what the import
//! resolver needs to place a `using` directive (#681): each directive ([`using_directive`]), and
//! the namespaces, type names and shadowing declarations a C# name lookup reads ([`name_facts`]).
//! The same walk reads what the C# relationship pass needs (#682): each member with its
//! parameters and type, each call with its receiver, each variable with its type and scope, and
//! each type declaration's kind and whole base list ([`code`]). The generic `extract_call`,
//! `extract_inheritance` and `extract_binding` skip C#: a C# type name is found by the name
//! lookup over the .NET project model, which the C# pass in ingest runs.

mod code;

use open_kioku_core::csharp::{
    CSharpCall, CSharpCallKind, CSharpHiddenType, CSharpLocal, CSharpLocalKind, CSharpMember,
    CSharpMemberKind, CSharpNameContext, CSharpNameFacts, CSharpNameUse, CSharpNamespaceBlock,
    CSharpPlace, CSharpReceiver, CSharpTypeDeclaration, CSharpTypeKind, CSharpUsing,
    CSharpUsingKind,
};
use open_kioku_core::{LineRange, ScopeKind, Symbol, SymbolId, SymbolKind, Visibility};
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// What a type declaration declares; a delegate declaration is a delegate type.
fn type_kind(node: Node<'_>) -> CSharpTypeKind {
    match node.kind() {
        "struct_declaration" => CSharpTypeKind::Struct,
        "record_declaration" if has_child(node, "struct") => CSharpTypeKind::RecordStruct,
        "record_declaration" => CSharpTypeKind::Record,
        "interface_declaration" => CSharpTypeKind::Interface,
        "enum_declaration" => CSharpTypeKind::Enum,
        "delegate_declaration" => CSharpTypeKind::Delegate,
        _ => CSharpTypeKind::Class,
    }
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
/// A declaration C# could not have written is not a symbol, whatever the grammar made of it: one
/// whose name is a reserved keyword (`#if` recovery has produced a local function named `if`), a
/// constructor or finalizer not named after its type (`extension(Entry e) { .. }`, a C# 14
/// extension block this grammar reads as a constructor), a local function with an access
/// modifier, or a declaration whose place the recovery invented: see [`is_placed`].
/// `recovered` is set when the file parsed with syntax errors, where a local function must also
/// be reached from a member through its body, never from a top-level statement.
pub(crate) fn symbol(
    node: Node<'_>,
    source: &[u8],
    recovered: bool,
) -> Option<(String, SymbolKind)> {
    let (name, kind) = declared(node, source)?;
    let valid = !name.is_empty()
        && !names_a_keyword(node, source)
        && names_its_type(node, source)
        && is_placed(node)
        && (node.kind() != "local_function_statement"
            || match extension_block_of(node, source) {
                Some(block) => {
                    owner(block).is_some_and(|owner| symbol(owner, source, recovered).is_some())
                }
                None => local_function_is_placed(node, source, recovered),
            })
        && owner_is_a_symbol(node, source, recovered);
    valid.then_some((name, kind))
}

/// A member, enum member, record parameter or nested type belongs to a type that is itself a
/// symbol: one recovery moved, or could not read, takes its members with it rather than leaving
/// them under a qualified name that names no type.
fn owner_is_a_symbol(node: Node<'_>, source: &[u8], recovered: bool) -> bool {
    let declaration = declaration_of(node);
    let owner = if declaration.kind() == "parameter" {
        declaration.parent().and_then(|list| list.parent())
    } else {
        owner(declaration)
    };
    match owner {
        Some(owner) if is_type_declaration(owner.kind()) => {
            symbol(owner, source, recovered).is_some()
        }
        _ => true,
    }
}

/// C#'s reserved keywords, which no declaration can be named without an `@`.
const RESERVED_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];

fn names_a_keyword(node: Node<'_>, source: &[u8]) -> bool {
    node.child_by_field_name("name")
        .filter(|name| name.kind() == "identifier")
        .and_then(|name| name.utf8_text(source).ok())
        .is_some_and(|name| RESERVED_KEYWORDS.contains(&name))
}

/// A constructor or finalizer is named after the type declaring it.
fn names_its_type(node: Node<'_>, source: &[u8]) -> bool {
    if !matches!(
        node.kind(),
        "constructor_declaration" | "destructor_declaration"
    ) {
        return true;
    }
    let own = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok());
    let owner = owner(node)
        .filter(|owner| holds_members(owner.kind()))
        .and_then(|owner| owner.child_by_field_name("name"))
        .and_then(|name| name.utf8_text(source).ok());
    own.is_some() && own == owner
}

/// A C# 14 extension block (`extension(string s) { public int Twice() => 2; }`), which this
/// grammar reads as a constructor named `extension` whose body holds the members as local
/// functions. It is not a symbol; its members are members of the static class declaring it.
fn is_extension_block(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "constructor_declaration"
        && node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
            == Some("extension")
        && !names_its_type(node, source)
}

/// The extension block a member written directly in its body belongs to.
fn extension_block_of<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    node.parent()
        .filter(|parent| parent.kind() == "block")?
        .parent()
        .filter(|constructor| is_extension_block(*constructor, source))
}

/// Whether the tree holds a declaration this grammar misread without an error node: a
/// constructor or finalizer not named after its type, which C# rejects, is a construct the
/// grammar cannot read (a C# 14 extension block). The file is then read as one parsed with
/// errors, so its symbols carry medium confidence and coverage names it.
pub(crate) fn misreads_a_declaration(root: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if matches!(
            node.kind(),
            "constructor_declaration" | "destructor_declaration"
        ) && !names_its_type(node, source)
        {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

/// Modifiers a local function may carry; an access modifier is never one of them.
const LOCAL_FUNCTION_MODIFIERS: &[&str] = &["static", "async", "unsafe", "extern"];

/// A local function is declared in a body: the nearest declaration above it, through blocks,
/// statements and expressions, is a member or another local function that is itself a symbol.
/// A program's top-level statements may declare one too, except in a file that parsed with
/// syntax errors, where a member recovery could not keep lands among top-level statements.
fn local_function_is_placed(node: Node<'_>, source: &[u8], recovered: bool) -> bool {
    let mut cursor = node.walk();
    let access = node.children(&mut cursor).any(|child| {
        child.kind() == "modifier"
            && child
                .child(0)
                .is_some_and(|token| !LOCAL_FUNCTION_MODIFIERS.contains(&token.kind()))
    });
    if access {
        return false;
    }
    let mut parent = node.parent();
    while let Some(current) = parent {
        let kind = current.kind();
        if kind == "global_statement" {
            return !recovered;
        }
        if kind == "accessor_declaration" {
            return current
                .parent()
                .and_then(|list| list.parent())
                .is_some_and(|member| symbol(member, source, recovered).is_some());
        }
        if names_local_functions(kind) {
            return symbol(current, source, recovered).is_some();
        }
        // A lambda in a field's initializer: the field is the member.
        if matches!(kind, "field_declaration" | "event_field_declaration") {
            return is_placed(current) && owner_is_a_symbol(current, source, recovered);
        }
        if kind == "ERROR"
            || is_type_declaration(kind)
            || matches!(
                kind,
                "namespace_declaration" | "compilation_unit" | "declaration_list"
            )
        {
            return false;
        }
        parent = current.parent();
    }
    false
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
            let target = unalias(&target);
            Some((format!("{direction} operator {target}"), SymbolKind::Method))
        }
        "local_function_statement" if extension_block_of(node, source).is_some() => {
            Some((name()?, SymbolKind::Method))
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
    let interface = unalias(&squeeze(specifier.utf8_text(source).ok()?));
    let interface = interface.trim_end_matches('.');
    (!interface.is_empty()).then(|| interface.to_string())
}

/// A type name without alias qualifiers, so no `::` enters a name the qualified name joins with
/// `::`: `global::Acme.IAudit` is `Acme.IAudit`, and an extern alias's `Legacy::Acme.IAudit` is
/// `Legacy.Acme.IAudit`, which keeps it apart from the global one.
fn unalias(name: &str) -> String {
    name.replace("global::", "").replace("::", ".")
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
        | "delegate_declaration" => match owner {
            // A type outside every type is `public`, `internal` or `file` (CS1527): a
            // `private` or `protected` one there is a nested type recovery moved out.
            Some("compilation_unit" | "namespace_declaration") => {
                !has_modifier(declaration, "private") && !has_modifier(declaration, "protected")
            }
            Some(owner) => holds_members(owner),
            None => false,
        },
        "enum_member_declaration" => owner == Some("enum_declaration"),
        "local_function_statement" | "parameter" => true,
        _ => owner.is_some_and(holds_members),
    }
}

/// A declaration's line range. A file-scoped namespace (`namespace Acme.Ledger;`) holds every
/// declaration after it to the end of the file, though the grammar ends its node at the `;`.
///
/// In a file that parsed with syntax errors, a type directly in a namespace or the compilation
/// unit also holds the top-level statements and error nodes right after it: C# allows no
/// statement after a type, so they are what recovery cut from it when it closed the type at a
/// brace that belonged to a block inside it.
pub(crate) fn line_range(node: Node<'_>, recovered: bool) -> LineRange {
    let declaration = declaration_of(node);
    let start = declaration.start_position().row as u32 + 1;
    let mut end = match declaration.parent() {
        Some(unit) if declaration.kind() == FILE_SCOPED_NAMESPACE => last_line_of(unit),
        _ => declaration.end_position().row as u32 + 1,
    };
    if recovered
        && is_type_declaration(declaration.kind())
        && owner(declaration).is_some_and(|owner| {
            matches!(owner.kind(), "compilation_unit" | "namespace_declaration")
        })
    {
        let mut next = declaration.next_named_sibling();
        while let Some(sibling) =
            next.filter(|sibling| matches!(sibling.kind(), "global_statement" | "ERROR"))
        {
            end = end.max(last_line_of(sibling));
            next = sibling.next_named_sibling();
        }
    }
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
        } else if names_local_functions(kind) && !is_extension_block(current, source) {
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
pub(crate) fn visibility(node: Node<'_>, source: &[u8]) -> Visibility {
    let declaration = declaration_of(node);
    match declaration.kind() {
        "namespace_declaration" | FILE_SCOPED_NAMESPACE | "enum_member_declaration" => {
            return Visibility::Public;
        }
        // A positional record parameter is a public property.
        "parameter" => return Visibility::Public,
        // A local function is visible only inside the member that declares it, and a finalizer
        // or static constructor cannot be named at all.
        "local_function_statement" if extension_block_of(declaration, source).is_none() => {
            return Visibility::Private;
        }
        "destructor_declaration" => return Visibility::Private,
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

/// A `using` directive as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsingDirective {
    pub(crate) kind: CSharpUsingKind,
    /// The dotted name, without `global::`, an extern alias or generic arguments.
    pub(crate) target: String,
    pub(crate) arity: u16,
    pub(crate) alias: Option<String>,
    pub(crate) global: bool,
    pub(crate) extern_alias: Option<String>,
}

/// Reads a `using_directive` node: `[global] using [static] Name;` or `[global] using Alias =
/// Name;`. The name is the directive's one named child outside its `name` field.
pub(crate) fn using_directive(node: Node<'_>, source: &[u8]) -> Option<UsingDirective> {
    if node.kind() != "using_directive" {
        return None;
    }
    let (mut global, mut is_static, mut target) = (false, false, None);
    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        let field = u32::try_from(index)
            .ok()
            .and_then(|index| node.field_name_for_child(index));
        match child.kind() {
            "global" => global = true,
            "static" => is_static = true,
            "comment" => {}
            _ if child.is_named() && field != Some("name") && target.is_none() => {
                target = Some(child);
            }
            _ => {}
        }
    }
    let name = dotted_name(target?, source)?;
    let alias = node
        .child_by_field_name("name")
        .and_then(|alias| alias.utf8_text(source).ok())
        .map(squeeze)
        .filter(|alias| !alias.is_empty());
    let kind = if alias.is_some() {
        CSharpUsingKind::Alias
    } else if is_static {
        CSharpUsingKind::Static
    } else {
        CSharpUsingKind::Namespace
    };
    let arity = name.segments.last().map_or(0, |(_, arity)| *arity);
    Some(UsingDirective {
        kind,
        target: name.dotted(),
        arity,
        alias,
        global,
        extern_alias: name.extern_alias,
    })
}

/// A namespace or type name as written, one `(identifier, generic arity)` per segment.
struct DottedName {
    segments: Vec<(String, u16)>,
    /// Starts at `global::`: a fully qualified name, which no `using` takes part in.
    global: bool,
    /// Starts at an extern alias (`Legacy::`): a name in an assembly the project references
    /// under that alias.
    extern_alias: Option<String>,
}

impl DottedName {
    fn dotted(&self) -> String {
        self.segments
            .iter()
            .map(|(segment, _)| segment.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn dotted_name(node: Node<'_>, source: &[u8]) -> Option<DottedName> {
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(squeeze);
    match node.kind() {
        "identifier" => Some(DottedName {
            segments: vec![(text(node)?, 0)],
            global: false,
            extern_alias: None,
        }),
        "generic_name" => {
            let mut cursor = node.walk();
            let identifier = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")?;
            Some(DottedName {
                segments: vec![(text(identifier)?, type_arity(node))],
                global: false,
                extern_alias: None,
            })
        }
        "qualified_name" => {
            let mut name = dotted_name(node.child_by_field_name("qualifier")?, source)?;
            let last = dotted_name(node.child_by_field_name("name")?, source)?;
            name.segments.extend(last.segments);
            Some(name)
        }
        "alias_qualified_name" => {
            let alias = text(node.child_by_field_name("alias")?)?;
            let mut name = dotted_name(node.child_by_field_name("name")?, source)?;
            if alias == "global" {
                name.global = true;
            } else {
                name.extern_alias = Some(alias);
            }
            Some(name)
        }
        _ => None,
    }
}

/// The number of type arguments a `generic_name` writes; `Entry<,>` (unbound) writes two.
fn type_arity(generic: Node<'_>) -> u16 {
    let mut cursor = generic.walk();
    let Some(arguments) = generic
        .named_children(&mut cursor)
        .find(|child| child.kind() == "type_argument_list")
    else {
        return 0;
    };
    let named = arguments.named_child_count();
    let count = if named > 0 {
        named
    } else {
        let mut cursor = arguments.walk();
        let commas = arguments
            .children(&mut cursor)
            .filter(|child| child.kind() == ",")
            .count();
        commas + 1
    };
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// The number of type parameters a declaration's `type_parameter_list` declares.
fn type_parameter_count(declaration: Node<'_>) -> u16 {
    let mut cursor = declaration.walk();
    let count = declaration
        .children(&mut cursor)
        .find(|child| child.kind() == "type_parameter_list")
        .map_or(0, |list| {
            let mut cursor = list.walk();
            let count = list
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "type_parameter")
                .count();
            count
        });
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// What a C# name lookup reads in this file: see [`CSharpNameFacts`]. `symbols` are the file's
/// symbols, which name the type declarations the uses sit in. Nothing inside an error node is
/// read, as for symbols.
pub(crate) fn name_facts(unit: Node<'_>, source: &[u8], symbols: &[Symbol]) -> CSharpNameFacts {
    let type_ids = symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface))
        .filter_map(|symbol| {
            let start = symbol.range.as_ref()?.start;
            Some(((start, symbol.qualified_name.clone()), symbol.id.clone()))
        })
        .collect();
    let member_ids = symbols
        .iter()
        .filter(|symbol| {
            !matches!(
                symbol.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Package
            )
        })
        .filter_map(|symbol| {
            let start = symbol.range.as_ref()?.start;
            Some(((start, symbol.qualified_name.clone()), symbol.id.clone()))
        })
        .collect();
    let mut reader = NameReader {
        source,
        type_ids,
        member_ids,
        type_index: HashMap::new(),
        callers: Vec::new(),
        facts: CSharpNameFacts {
            blocks: vec![CSharpNamespaceBlock {
                namespace: String::new(),
                parent: None,
            }],
            ..CSharpNameFacts::default()
        },
        seen: HashSet::new(),
        type_parameters: BTreeSet::new(),
        variables: BTreeSet::new(),
        regions: 0,
        recovered: unit.has_error(),
    };
    reader.unit(unit);
    let mut facts = reader.facts;
    facts.type_parameters = reader.type_parameters.into_iter().collect();
    facts.variables = reader.variables.into_iter().collect();
    facts.syntax_errors = unit.has_error();
    facts.hidden_directives = facts.syntax_errors && directive_zone_has_error(unit);
    facts
}

/// The assembly an `[assembly: InternalsVisibleTo("Name, PublicKey=..")]` attribute names.
fn internals_visible_to(attribute: Node<'_>, source: &[u8]) -> Option<String> {
    let name = dotted_name(attribute.child_by_field_name("name")?, source)?;
    let (last, _) = name.segments.last()?;
    if !matches!(
        last.as_str(),
        "InternalsVisibleTo" | "InternalsVisibleToAttribute"
    ) {
        return None;
    }
    let mut cursor = attribute.walk();
    let arguments = attribute
        .named_children(&mut cursor)
        .find(|child| child.kind() == "attribute_argument_list")?;
    let mut cursor = arguments.walk();
    let first = arguments.named_children(&mut cursor).next()?;
    let mut cursor = first.walk();
    let literal = first
        .named_children(&mut cursor)
        .find(|child| child.kind() == "string_literal")?;
    let mut cursor = literal.walk();
    let content = literal
        .named_children(&mut cursor)
        .find(|child| child.kind() == "string_literal_content")?
        .utf8_text(source)
        .ok()?;
    let assembly = content.split(',').next()?.trim();
    (!assembly.is_empty()).then(|| assembly.to_string())
}

/// The fields of a [`CSharpNameUse`] that tell two uses apart; the first line is kept.
type UseKey = (
    u32,
    Option<SymbolId>,
    String,
    u16,
    Option<String>,
    u16,
    CSharpNameContext,
    bool,
);

struct NameReader<'s> {
    source: &'s [u8],
    /// Type symbols by first line and qualified name, as the main walk recorded them.
    type_ids: HashMap<(u32, String), SymbolId>,
    /// Every other symbol (members, local functions) the same way.
    member_ids: HashMap<(u32, String), SymbolId>,
    /// The index in `facts.types` of each type declaration read so far.
    type_index: HashMap<SymbolId, u32>,
    /// The members around the node being read, innermost last, as indexes into
    /// `facts.members`: what a call or variable there belongs to.
    callers: Vec<u32>,
    facts: CSharpNameFacts,
    seen: HashSet<UseKey>,
    type_parameters: BTreeSet<String>,
    variables: BTreeSet<String>,
    /// How many `#if`, `#elif` or `#else` regions enclose the node being read.
    regions: u32,
    /// The file parsed with syntax errors, as the main walk read its declarations.
    recovered: bool,
}

/// Where a parent holds a child: its kind and the field the child is in.
#[derive(Clone, Copy)]
struct Place<'p> {
    parent: &'p str,
    field: Option<&'p str>,
}

impl NameReader<'_> {
    fn unit(&mut self, unit: Node<'_>) {
        // A file-scoped namespace holds every declaration and directive after it.
        let mut block = 0;
        let mut cursor = unit.walk();
        for child in unit.named_children(&mut cursor) {
            if child.kind() == FILE_SCOPED_NAMESPACE {
                if let Some(name) = name_text(child, self.source) {
                    block = self.open_block(0, &name);
                }
                continue;
            }
            let place = Place {
                parent: unit.kind(),
                field: None,
            };
            self.node(child, place, block, None);
        }
    }

    fn open_block(&mut self, parent: u32, name: &str) -> u32 {
        let name = squeeze(name);
        let outer = &self.facts.blocks[parent as usize].namespace;
        let namespace = if outer.is_empty() {
            name
        } else {
            format!("{outer}.{name}")
        };
        self.facts.blocks.push(CSharpNamespaceBlock {
            namespace,
            parent: Some(parent),
        });
        u32::try_from(self.facts.blocks.len() - 1).unwrap_or(u32::MAX)
    }

    fn children(&mut self, node: Node<'_>, block: u32, enclosing: Option<&SymbolId>) {
        let kind = node.kind();
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !child.is_named() {
                continue;
            }
            let field = u32::try_from(index)
                .ok()
                .and_then(|index| node.field_name_for_child(index));
            self.node(
                child,
                Place {
                    parent: kind,
                    field,
                },
                block,
                enclosing,
            );
        }
    }

    fn node(&mut self, node: Node<'_>, place: Place<'_>, block: u32, enclosing: Option<&SymbolId>) {
        if node.is_error() {
            // An error node may hold a type the lookup would find first, or a directive.
            let outer = self
                .facts
                .blocks
                .get(block as usize)
                .map(|block| block.namespace.clone())
                .unwrap_or_default();
            let text = node.utf8_text(self.source).unwrap_or_default();
            for hidden in hidden_types_in_error(&outer, text) {
                if !self.facts.hidden_types.contains(&hidden) {
                    self.facts.hidden_types.push(hidden);
                }
            }
            return;
        }
        match node.kind() {
            "using_directive" => {
                self.using(node, block);
                return;
            }
            "extern_alias_directive" => {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(self.source).ok())
                {
                    self.facts.extern_aliases.push(squeeze(name));
                }
                return;
            }
            "namespace_declaration" => {
                let Some(name) = name_text(node, self.source) else {
                    return;
                };
                let inner = self.open_block(block, &name);
                if let Some(body) = node.child_by_field_name("body") {
                    self.children(body, inner, enclosing);
                }
                return;
            }
            kind if is_type_declaration(kind) || kind == "delegate_declaration" => {
                self.type_declaration(node, block, enclosing);
                return;
            }
            "type_parameter" => {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(self.source).ok())
                {
                    self.type_parameters.insert(name.to_string());
                }
            }
            "implicit_parameter" => {
                if let Ok(name) = node.utf8_text(self.source) {
                    self.variables.insert(name.trim().to_string());
                }
            }
            "attribute" if place.parent == "global_attribute" => {
                if let Some(assembly) = internals_visible_to(node, self.source) {
                    self.facts.internals_visible_to.push(assembly);
                }
            }
            _ => {}
        }
        if node.kind() == "identifier" && declares_variable(place) {
            if let Ok(name) = node.utf8_text(self.source) {
                self.variables.insert(name.to_string());
            }
        }
        if let Some(context) = use_context(node.kind(), place) {
            self.record_use(node, context, block, enclosing);
        }
        let member =
            code::member_kind(node).and_then(|kind| self.member(node, kind, block, enclosing));
        if let Some(member) = member {
            self.callers.push(member);
        }
        self.code(node, place, block, enclosing);
        let region = matches!(
            node.kind(),
            "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_if_in_attribute_list"
        );
        self.regions += u32::from(region);
        self.children(node, block, enclosing);
        self.regions -= u32::from(region);
        if member.is_some() {
            self.callers.pop();
        }
    }

    fn using(&mut self, node: Node<'_>, block: u32) {
        let Some(directive) = using_directive(node, self.source) else {
            return;
        };
        self.facts.usings.push(CSharpUsing {
            block,
            kind: directive.kind,
            target: directive.target,
            arity: directive.arity,
            alias: directive.alias,
            global: directive.global,
            extern_alias: directive.extern_alias,
            line: node.start_position().row as u32 + 1,
            conditional: self.regions > 0,
        });
    }

    fn type_declaration(&mut self, node: Node<'_>, block: u32, outer: Option<&SymbolId>) {
        let id = symbol(node, self.source, self.recovered).and_then(|(name, _)| {
            let qualified = qualified_name(node, &name, self.source);
            let start = line_range(node, self.recovered).start;
            self.type_ids.get(&(start, qualified)).cloned()
        });
        let mut bases = Vec::new();
        let mut base_types = Vec::new();
        let mut base_place = None;
        let mut base_call = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() != "base_list" {
                continue;
            }
            base_place = Some(self.place(block, outer, child));
            let mut list = child.walk();
            for base in child.named_children(&mut list) {
                let written = if base.kind() == "primary_constructor_base_type" {
                    base_call = Some(base);
                    base.child_by_field_name("type")
                } else {
                    Some(base)
                };
                if let Some(written) =
                    written.and_then(|written| code::type_ref(written, self.source))
                {
                    base_types.push(written);
                }
                if let Some(use_) = written.and_then(|written| {
                    self.name_use(written, CSharpNameContext::Type, block, outer)
                }) {
                    bases.push(use_);
                }
            }
        }
        let mut primary = None;
        if let Some(id) = &id {
            let index = u32::try_from(self.facts.types.len()).unwrap_or(u32::MAX);
            self.facts.types.push(CSharpTypeDeclaration {
                symbol_id: id.clone(),
                arity: type_parameter_count(node),
                bases,
                conditional: self.regions > 0,
                kind: type_kind(node),
                is_static: has_modifier(node, "static"),
                base_types,
                base_place,
            });
            self.type_index.insert(id.clone(), index);
            // The parameter list a record, class or struct declaration writes is its primary
            // constructor; a `: Base(..)` in its base list calls the base type's constructor.
            let list = (node.kind() != "delegate_declaration")
                .then(|| code::parameter_list(node))
                .flatten();
            if let Some(list) = list {
                let place = self.place(block, Some(id), node);
                primary = Some(
                    self.push_member(CSharpMember {
                        symbol_id: id.clone(),
                        kind: CSharpMemberKind::PrimaryConstructor,
                        name: node
                            .child_by_field_name("name")
                            .and_then(|name| name.utf8_text(self.source).ok())
                            .map(squeeze)
                            .unwrap_or_default(),
                        is_static: false,
                        is_override: false,
                        has_body: false,
                        type_parameters: 0,
                        parameters: code::parameters(list, self.source),
                        value_type: None,
                        place,
                    }),
                );
            }
            if let Some(base) = base_call {
                let place = self.place(block, Some(id), base);
                self.facts.calls.push(CSharpCall {
                    caller: primary,
                    kind: CSharpCallKind::BaseConstructor,
                    name: String::new(),
                    type_arguments: None,
                    arguments: code::argument_count(base),
                    receiver: CSharpReceiver::default(),
                    created: None,
                    range: code::source_range(base),
                    place,
                });
            }
        }
        // A base list is read where the declaration stands, outside the type; everything else
        // inside the type. A declaration that is no symbol (misplaced by error recovery) leaves
        // the enclosing type as it was.
        let inner = id.as_ref().or(outer);
        let kind = node.kind();
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !child.is_named() {
                continue;
            }
            let field = u32::try_from(index)
                .ok()
                .and_then(|index| node.field_name_for_child(index));
            let enclosing = if child.kind() == "base_list" {
                outer
            } else {
                inner
            };
            // The primary constructor's parameters, and the arguments its base call passes,
            // belong to it.
            let in_primary = primary.filter(|_| {
                matches!(
                    child.kind(),
                    "parameter_list" | "base_list" | "bracketed_parameter_list"
                )
            });
            if let Some(primary) = in_primary {
                self.callers.push(primary);
            }
            self.node(
                child,
                Place {
                    parent: kind,
                    field,
                },
                block,
                enclosing,
            );
            if in_primary.is_some() {
                self.callers.pop();
            }
        }
    }

    fn push_member(&mut self, member: CSharpMember) -> u32 {
        self.facts.members.push(member);
        u32::try_from(self.facts.members.len() - 1).unwrap_or(u32::MAX)
    }

    /// Where `node` is written, for the lookup of the types it names.
    fn place(&self, block: u32, enclosing: Option<&SymbolId>, node: Node<'_>) -> CSharpPlace {
        CSharpPlace {
            block,
            enclosing: enclosing.and_then(|id| self.type_index.get(id).copied()),
            line: node.start_position().row as u32 + 1,
            conditional: self.regions > 0,
        }
    }

    /// Records the member `node` declares, when it is a symbol of the main walk.
    fn member(
        &mut self,
        node: Node<'_>,
        kind: CSharpMemberKind,
        block: u32,
        enclosing: Option<&SymbolId>,
    ) -> Option<u32> {
        let (name, _) = symbol(node, self.source, self.recovered)?;
        let qualified = qualified_name(node, &name, self.source);
        let start = line_range(node, self.recovered).start;
        let symbol_id = self.member_ids.get(&(start, qualified))?.clone();
        let declaration = declaration_of(node);
        let parameters = match kind {
            CSharpMemberKind::Method
            | CSharpMemberKind::Constructor
            | CSharpMemberKind::LocalFunction
            | CSharpMemberKind::Indexer
            | CSharpMemberKind::Operator => code::parameter_list(declaration)
                .map(|list| code::parameters(list, self.source))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let type_parameters = match kind {
            CSharpMemberKind::Method | CSharpMemberKind::LocalFunction => {
                code::generic_arity(declaration)
            }
            _ => 0,
        };
        let place = self.place(block, enclosing, declaration);
        Some(self.push_member(CSharpMember {
            symbol_id,
            kind,
            name,
            is_static: code::is_static(node),
            is_override: code::is_override(node),
            has_body: code::has_body(declaration),
            type_parameters,
            parameters,
            value_type: code::value_type(node, self.source),
            place,
        }))
    }

    /// Records the call or variable `node` writes, if it writes one.
    fn code(&mut self, node: Node<'_>, place: Place<'_>, block: u32, enclosing: Option<&SymbolId>) {
        let source = self.source;
        match node.kind() {
            "invocation_expression" => {
                let Some((name, type_arguments, receiver)) = node
                    .child_by_field_name("function")
                    .and_then(|function| code::invoked(function, source))
                else {
                    return;
                };
                // `nameof(x)` names `x` and calls nothing, unless a method `nameof` is in scope,
                // which no C# code declares.
                if receiver.is_none() && name == "nameof" {
                    return;
                }
                self.call(
                    node,
                    CSharpCallKind::Invocation,
                    name,
                    type_arguments,
                    receiver,
                    None,
                    block,
                    enclosing,
                );
            }
            "object_creation_expression" => {
                let created = node
                    .child_by_field_name("type")
                    .and_then(|written| code::type_ref(written, source));
                self.call(
                    node,
                    CSharpCallKind::Creation,
                    String::new(),
                    None,
                    CSharpReceiver::default(),
                    created,
                    block,
                    enclosing,
                );
            }
            "implicit_object_creation_expression" => {
                let created = code::target_type(node, source);
                self.call(
                    node,
                    CSharpCallKind::Creation,
                    String::new(),
                    None,
                    CSharpReceiver::default(),
                    created,
                    block,
                    enclosing,
                );
            }
            "constructor_initializer" => {
                if let Some(kind) = code::initializer_kind(node) {
                    self.call(
                        node,
                        kind,
                        String::new(),
                        None,
                        CSharpReceiver::default(),
                        None,
                        block,
                        enclosing,
                    );
                }
            }
            "parameter_list" | "bracketed_parameter_list" => {
                let Some(owner) = node.parent() else {
                    return;
                };
                let kind = match owner.kind() {
                    "lambda_expression" | "anonymous_method_expression" => {
                        CSharpLocalKind::LambdaParameter
                    }
                    "delegate_declaration" | "function_pointer_type" => return,
                    _ => CSharpLocalKind::Parameter,
                };
                for parameter in code::parameters(node, source) {
                    if parameter.name.is_empty() {
                        continue;
                    }
                    let declared = parameter.value_type.is_some();
                    self.local(
                        parameter.name,
                        kind,
                        parameter.value_type,
                        declared,
                        owner,
                        node,
                        block,
                        enclosing,
                    );
                }
            }
            "implicit_parameter" => {
                let name = node.utf8_text(source).map(squeeze).unwrap_or_default();
                let scope = node.parent().unwrap_or(node);
                self.local(
                    name,
                    CSharpLocalKind::LambdaParameter,
                    None,
                    false,
                    scope,
                    node,
                    block,
                    enclosing,
                );
            }
            "variable_declarator" => {
                let Some(declaration) = node
                    .parent()
                    .filter(|parent| parent.kind() == "variable_declaration")
                else {
                    return;
                };
                if declaration.parent().is_some_and(|holder| {
                    matches!(
                        holder.kind(),
                        "field_declaration" | "event_field_declaration"
                    )
                }) {
                    return;
                }
                let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                    .map(squeeze)
                else {
                    return;
                };
                let written = declaration
                    .child_by_field_name("type")
                    .and_then(|written| code::type_ref(written, source));
                let (value_type, declared) = match written {
                    Some(written) => (Some(written), true),
                    None => (
                        code::declarator_value(node)
                            .and_then(|value| code::initializer_type(value, source)),
                        false,
                    ),
                };
                let scope = code::scope_of(node);
                self.local(
                    name,
                    CSharpLocalKind::Variable,
                    value_type,
                    declared,
                    scope,
                    node,
                    block,
                    enclosing,
                );
            }
            "declaration_expression" | "declaration_pattern" | "catch_declaration" => {
                let Some(name) = node
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "identifier")
                    .and_then(|name| name.utf8_text(source).ok())
                    .map(squeeze)
                else {
                    return;
                };
                let value_type = node
                    .child_by_field_name("type")
                    .and_then(|written| code::type_ref(written, source));
                let declared = value_type.is_some();
                let scope = code::scope_of(node);
                self.local(
                    name,
                    CSharpLocalKind::Variable,
                    value_type,
                    declared,
                    scope,
                    node,
                    block,
                    enclosing,
                );
            }
            "foreach_statement" => {
                let Some(name) = node
                    .child_by_field_name("left")
                    .filter(|left| left.kind() == "identifier")
                    .and_then(|left| left.utf8_text(source).ok())
                    .map(squeeze)
                else {
                    return;
                };
                let value_type = node
                    .child_by_field_name("type")
                    .and_then(|written| code::type_ref(written, source));
                let declared = value_type.is_some();
                self.local(
                    name,
                    CSharpLocalKind::Variable,
                    value_type,
                    declared,
                    node,
                    node,
                    block,
                    enclosing,
                );
            }
            // Every other variable a declaration spells (a deconstruction, a query variable, a
            // pattern's designation) has no type the reader takes from it, but its name still
            // takes a receiver from a member or type of that name.
            "identifier"
                if declares_variable(place)
                    && !matches!(
                        place.parent,
                        "variable_declarator"
                            | "parameter"
                            | "parameter_list"
                            | "bracketed_parameter_list"
                            | "catch_declaration"
                            | "declaration_expression"
                            | "declaration_pattern"
                            | "foreach_statement"
                            | "tuple_element"
                    ) =>
            {
                let name = node.utf8_text(source).map(squeeze).unwrap_or_default();
                let scope = code::scope_of(node);
                self.local(
                    name,
                    CSharpLocalKind::Variable,
                    None,
                    false,
                    scope,
                    node,
                    block,
                    enclosing,
                );
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        node: Node<'_>,
        kind: CSharpCallKind,
        name: String,
        type_arguments: Option<u16>,
        receiver: CSharpReceiver,
        created: Option<open_kioku_core::csharp::CSharpTypeRef>,
        block: u32,
        enclosing: Option<&SymbolId>,
    ) {
        let place = self.place(block, enclosing, node);
        self.facts.calls.push(CSharpCall {
            caller: self.callers.last().copied(),
            kind,
            name,
            type_arguments,
            arguments: code::argument_count(node),
            receiver,
            created,
            range: code::source_range(node),
            place,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn local(
        &mut self,
        name: String,
        kind: CSharpLocalKind,
        value_type: Option<open_kioku_core::csharp::CSharpTypeRef>,
        declared: bool,
        scope: Node<'_>,
        node: Node<'_>,
        block: u32,
        enclosing: Option<&SymbolId>,
    ) {
        if name.is_empty() {
            return;
        }
        let place = self.place(block, enclosing, node);
        self.facts.locals.push(CSharpLocal {
            name,
            kind,
            value_type,
            declared,
            owner: self.callers.last().copied(),
            scope: code::source_range(scope),
            place,
        });
    }

    fn record_use(
        &mut self,
        node: Node<'_>,
        context: CSharpNameContext,
        block: u32,
        enclosing: Option<&SymbolId>,
    ) {
        let Some(use_) = self.name_use(node, context, block, enclosing) else {
            return;
        };
        let key = (
            use_.block,
            use_.enclosing_type.clone(),
            use_.name.clone(),
            use_.arity,
            use_.next.clone(),
            use_.next_arity,
            use_.context,
            use_.conditional,
        );
        if self.seen.insert(key) {
            self.facts.uses.push(use_);
        }
    }

    /// The use a name node writes, unless it is fully qualified (`global::`) or starts at an
    /// extern alias, which no `using` of the file takes part in.
    fn name_use(
        &self,
        node: Node<'_>,
        context: CSharpNameContext,
        block: u32,
        enclosing: Option<&SymbolId>,
    ) -> Option<CSharpNameUse> {
        let name = dotted_name(node, self.source)?;
        if name.global || name.extern_alias.is_some() {
            return None;
        }
        let mut segments = name.segments.into_iter();
        let (first, arity) = segments.next()?;
        let (next, next_arity) = segments
            .next()
            .map_or((None, 0), |(next, arity)| (Some(next), arity));
        Some(CSharpNameUse {
            block,
            enclosing_type: enclosing.cloned(),
            name: first,
            arity,
            next,
            next_arity,
            context,
            line: node.start_position().row as u32 + 1,
            conditional: self.regions > 0,
        })
    }
}

/// What an error node's text may hide.
/// The types the error node `text`, standing in a block of `outer`, may hide: every name it
/// spells after a type keyword, in `outer` and in each namespace it spells (see
/// [`CSharpNameFacts::hidden_types`]).
fn hidden_types_in_error(outer: &str, text: &str) -> Vec<CSharpHiddenType> {
    let words = text
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '@'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut namespaces = error_namespaces(outer, &words);
    if !namespaces.iter().any(|namespace| namespace == outer) {
        namespaces.push(outer.to_string());
    }
    let mut types = Vec::new();
    for pair in words.windows(2) {
        if !matches!(
            pair[0],
            "class" | "struct" | "interface" | "enum" | "record" | "delegate"
        ) {
            continue;
        }
        let name = pair[1].trim_start_matches('@');
        if name.is_empty() || name.contains('.') {
            continue;
        }
        for namespace in &namespaces {
            let hidden = CSharpHiddenType {
                namespace: namespace.clone(),
                name: name.to_string(),
            };
            if !types.contains(&hidden) {
                types.push(hidden);
            }
        }
    }
    types
}

/// Whether a syntax error stands where a `using` directive may: in the compilation unit or a
/// namespace body, before its first type or namespace declaration. C# allows a directive only
/// there, and recovery can misread one there into something else (a `using` swallowed into a
/// top-level statement spells no `using` in any error node), so an error anywhere in that zone
/// may hide one; an error after it, inside a declaration, cannot.
fn directive_zone_has_error(list: Node<'_>) -> bool {
    let mut in_zone = true;
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        let kind = child.kind();
        if kind == "namespace_declaration" {
            // Each namespace body has a zone of its own.
            in_zone = false;
            if child
                .child_by_field_name("body")
                .is_some_and(directive_zone_has_error)
            {
                return true;
            }
        } else if is_type_declaration(kind) || kind == "delegate_declaration" {
            in_zone = false;
        } else if in_zone && child.has_error() {
            return true;
        }
    }
    false
}

/// Each `namespace Name` the words of an error node spell, joined to `outer` and to the ones
/// spelled before it: an error node does not say where a block closes, so a later one may be
/// nested in an earlier one.
fn error_namespaces(outer: &str, words: &[&str]) -> Vec<String> {
    let join = |left: &str, right: &str| {
        if left.is_empty() {
            right.to_string()
        } else {
            format!("{left}.{right}")
        }
    };
    let mut namespaces = Vec::new();
    let mut nested = outer.to_string();
    for pair in words.windows(2) {
        if pair[0] != "namespace" {
            continue;
        }
        let name = pair[1].trim_matches('.');
        if name.is_empty() {
            continue;
        }
        nested = join(&nested, name);
        for namespace in [join(outer, name), nested.clone()] {
            if !namespaces.contains(&namespace) {
                namespaces.push(namespace);
            }
        }
    }
    namespaces
}

/// The context a name node of `kind` is written in at `place`, when it is a type name the lookup
/// reads: see [`CSharpNameContext`]. A name inside a qualified name, a generic name or a
/// directive is read with the name holding it.
fn use_context(kind: &str, place: Place<'_>) -> Option<CSharpNameContext> {
    if !matches!(
        kind,
        "identifier" | "generic_name" | "qualified_name" | "alias_qualified_name"
    ) {
        return None;
    }
    match (place.parent, place.field) {
        ("using_directive" | "qualified_name" | "generic_name" | "alias_qualified_name", _) => None,
        (_, Some("type" | "returns")) => Some(CSharpNameContext::Type),
        ("as_expression" | "is_expression", Some("right")) => Some(CSharpNameContext::Type),
        ("base_list" | "type_argument_list" | "explicit_interface_specifier", None) => {
            Some(CSharpNameContext::Type)
        }
        ("attribute", Some("name")) => Some(CSharpNameContext::Attribute),
        ("member_access_expression", Some("expression"))
            if matches!(kind, "identifier" | "generic_name") =>
        {
            Some(CSharpNameContext::Receiver)
        }
        _ => None,
    }
}

/// Whether an identifier at `place` declares a local, parameter, pattern, range or loop
/// variable (or a field, which shadows a receiver the same way). Query clauses spell their
/// variable without a field, so every identifier directly in one counts: a name read as a
/// variable that is not one only keeps a receiver out of the lookup.
fn declares_variable(place: Place<'_>) -> bool {
    matches!(
        (place.parent, place.field),
        (
            "variable_declarator"
                | "parameter"
                | "catch_declaration"
                | "declaration_expression"
                | "declaration_pattern"
                | "from_clause"
                | "tuple_element"
                | "var_pattern"
                | "recursive_pattern"
                | "list_pattern"
                | "tuple_pattern"
                | "parenthesized_variable_designation"
                | "bracketed_parameter_list"
                | "parameter_list",
            Some("name"),
        ) | ("foreach_statement", Some("left"))
            | ("join_clause" | "let_clause" | "query_continuation", None)
    )
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
            generated_by: None,
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
        // Calls and base lists are not read as relationships; `using System;` is an import site
        // the import resolver places.
        assert!(facts.calls.is_empty());
        let imports = facts
            .imports
            .iter()
            .map(|site| site.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(imports, vec!["System"]);
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
        // in C#. A parser reads one file, so it records the default; ingest unifies the parts
        // (`open_kioku_languages::csharp::unify_partial_type_visibility`).
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

    fn qualified(facts: &SyntaxFacts) -> Vec<&str> {
        facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect()
    }

    #[test]
    fn an_extension_block_recovery_misreads_invents_no_member() {
        // A C# 14 extension block, which this grammar reads as a constructor named `extension`
        // and closes the class at the block's brace, leaving the rest as top-level statements.
        let facts = parse(
            "EntryExtensions.cs",
            "namespace Acme.Ledger;\n\npublic static class EntryExtensions\n{\n    extension(Entry entry)\n    {\n        public bool IsEmpty => entry.Value.Amount == 0;\n        public Entry Negate() => entry;\n    }\n\n    public static int Count(this Entry entry) => 1;\n}\n",
        );
        let names = qualified(&facts);
        assert!(
            names.iter().all(|name| !name.contains("extension")),
            "{names:?}"
        );
        // `Count` is lost with the class's tail, not moved to the namespace.
        assert!(!names.contains(&"Acme::Ledger::Count"), "{names:?}");
        // The class still spans to its own closing brace.
        let class = find(&facts, "Acme::Ledger::EntryExtensions");
        assert_eq!(
            class.range.as_ref().map(|range| (range.start, range.end)),
            Some((3, 12))
        );

        let split = parse(
            "Ext.cs",
            "namespace Acme.Split;\n\npublic static class Ext\n{\n    public static int Before(this string s) => 1;\n\n    extension(string s)\n    {\n        public int Length2 => s.Length;\n        public int Twice() => 2;\n    }\n\n    public static int After(this string s) => 3;\n}\n",
        );
        assert_eq!(
            qualified(&split),
            vec![
                "Acme::Split",
                "Acme::Split::Ext",
                "Acme::Split::Ext::Before",
                // An extension block's member is a member of the static class declaring it.
                "Acme::Split::Ext::Twice",
                "Acme::Split::Ext::After",
            ]
        );
    }

    #[test]
    fn a_reserved_keyword_is_never_a_name() {
        // `#if` around an `else if` arm: recovery reads `if (..) { .. }` as a local function.
        let facts = parse(
            "Tally.cs",
            "namespace Acme;\npublic class Tally\n{\n    public bool Settle(out int total)\n    {\n        if (books is null)\n        {\n            total = 0;\n        }\n#if MODERN\n        else if (books.GetType() == typeof(int[,]))\n        {\n            total = 1;\n        }\n#endif\n        else\n        {\n            total = 2;\n        }\n        return true;\n    }\n}\n",
        );
        assert_eq!(
            qualified(&facts),
            vec!["Acme", "Acme::Tally", "Acme::Tally::Settle"]
        );
        // The recovery this guards against is really there: a local function named `if`.
        let source = "namespace Acme;\npublic class Tally\n{\n    public bool Settle(out int total)\n    {\n        if (books is null)\n        {\n            total = 0;\n        }\n#if MODERN\n        else if (books.GetType() == typeof(int[,]))\n        {\n            total = 1;\n        }\n#endif\n        else\n        {\n            total = 2;\n        }\n        return true;\n    }\n}\n";
        let mut parser = crate::parser_for(&Language::CSharp).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut pending = vec![tree.root_node()];
        let mut named_if = false;
        while let Some(node) = pending.pop() {
            named_if |= node.kind() == "local_function_statement"
                && node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                    == Some("if");
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        assert!(named_if);
    }

    #[test]
    fn a_constructor_is_named_after_its_type() {
        // Without syntax errors too: C# has no constructor of another name.
        let facts = parse(
            "Ledger.cs",
            "class Ledger\n{\n    public Ledger() { }\n    Journal(int size) { }\n    ~Ledger() { }\n}\n",
        );
        assert_eq!(
            qualified(&facts),
            vec!["Ledger", "Ledger::Ledger", "Ledger::~Ledger"]
        );
    }

    #[test]
    fn a_local_function_sits_in_a_member_body() {
        // An access modifier is never on a local function, and in a file with syntax errors a
        // top-level statement holds none.
        let facts = parse(
            "Program.cs",
            "Run();\nvoid Run() { }\npublic int Stray() => 1;\nclass Holder\n{\n    int Get\n    {\n        get\n        {\n            int Twice(int x) => x * 2;\n            return Twice(1);\n        }\n    }\n}\n",
        );
        assert_eq!(
            qualified(&facts),
            vec!["Run", "Holder", "Holder::Get", "Holder::Get::Twice"]
        );
        let recovered = parse(
            "Broken.cs",
            "Run();\nvoid Run() { }\nclass Broken { void F() { if (x { } }\n",
        );
        assert!(
            qualified(&recovered).iter().all(|name| *name != "Run"),
            "{:?}",
            qualified(&recovered)
        );
    }

    #[test]
    fn alias_qualifiers_stay_out_of_names() {
        let facts = parse(
            "Bridge.cs",
            "extern alias Legacy;\n\nnamespace Acme.Ledger.Interop;\n\npublic class Bridge : global::Acme.Ledger.IAudit, Legacy::Acme.Ledger.IAudit\n{\n    void global::Acme.Ledger.IAudit.Audit() { }\n    void Legacy::Acme.Ledger.IAudit.Audit() { }\n    public static implicit operator global::Acme.Money(Bridge bridge) => default;\n}\n",
        );
        assert_eq!(
            qualified(&facts),
            vec![
                "Acme::Ledger::Interop",
                "Acme::Ledger::Interop::Bridge",
                "Acme::Ledger::Interop::Bridge::Acme.Ledger.IAudit.Audit",
                "Acme::Ledger::Interop::Bridge::Legacy.Acme.Ledger.IAudit.Audit",
                "Acme::Ledger::Interop::Bridge::implicit operator Acme.Money",
            ]
        );
    }

    #[test]
    fn a_nested_type_recovery_moved_out_of_its_type_is_dropped_with_its_members() {
        // A `private` or `protected` type cannot stand in a namespace: recovery that closes a
        // type early (here, spelled out) leaves its nested types there, under a qualified name
        // that skips their enclosing type.
        let facts = parse(
            "Vault.cs",
            "namespace Acme\n{\n    internal class Vault { }\n    private readonly struct Tumbler\n    {\n        public object Pin => null;\n    }\n    protected class Hook { }\n}\n",
        );
        assert_eq!(qualified(&facts), vec!["Acme", "Acme::Vault"]);
    }

    #[test]
    fn extension_block_members_belong_to_their_class_and_mark_the_file_recovered() {
        // No error node: the grammar reads the block as a constructor named `extension`.
        let source = "namespace Acme.Ext;\n\npublic static class Clean\n{\n    public static int Before(this string s) => 1;\n\n    extension(string s)\n    {\n        public int Twice() => 2;\n        public static string Empty() => \"\";\n    }\n\n    public static int After(this string s) => 3;\n}\n";
        let mut parser = crate::parser_for(&Language::CSharp).unwrap();
        assert!(!parser.parse(source, None).unwrap().root_node().has_error());
        let facts = parse("Clean.cs", source);
        use SymbolKind::*;
        use Visibility::Public;
        assert_eq!(
            table(&facts),
            vec![
                row("Acme::Ext", Package, Public, 1, 14),
                row("Acme::Ext::Clean", Class, Public, 3, 14),
                row("Acme::Ext::Clean::Before", Method, Public, 5, 5),
                row("Acme::Ext::Clean::Twice", Method, Public, 9, 9),
                row("Acme::Ext::Clean::Empty", Method, Public, 10, 10),
                row("Acme::Ext::Clean::After", Method, Public, 13, 13),
            ]
        );
        // What the file declares there is recovered: medium confidence, and coverage counts it.
        assert!(facts.syntax_errors);
        assert!(facts
            .symbols
            .iter()
            .all(|symbol| symbol.confidence == Confidence::Medium));
        let clean = find(&facts, "Acme::Ext::Clean");
        assert_eq!(
            find(&facts, "Acme::Ext::Clean::Twice")
                .parent_symbol_id
                .as_ref(),
            Some(&clean.id)
        );
    }

    #[test]
    fn a_local_function_in_a_field_initializer_lambda_is_kept() {
        let facts = parse(
            "Holder.cs",
            "namespace Acme.Locals;\n\npublic class Holder\n{\n    private static readonly Func<int, int> Scale = x =>\n    {\n        int Twice(int y) => y * 2;\n        return Twice(x);\n    };\n\n    public int Run()\n    {\n        int Local() => 1;\n        return Local();\n    }\n}\n",
        );
        assert!(!facts.syntax_errors);
        assert_eq!(
            qualified(&facts),
            vec![
                "Acme::Locals",
                "Acme::Locals::Holder",
                "Acme::Locals::Holder::Scale",
                "Acme::Locals::Holder::Twice",
                "Acme::Locals::Holder::Run",
                "Acme::Locals::Holder::Run::Local",
            ]
        );
        assert_eq!(
            find(&facts, "Acme::Locals::Holder::Twice").kind,
            SymbolKind::Function
        );
    }

    mod names {
        use super::parse;
        use open_kioku_core::csharp::{
            CSharpNameContext, CSharpNameFacts, CSharpNameUse, CSharpUsing, CSharpUsingKind,
        };
        use open_kioku_core::SyntaxFacts;

        fn names(facts: &SyntaxFacts) -> &CSharpNameFacts {
            facts
                .csharp_names
                .as_ref()
                .expect("a C# file has name facts")
        }

        /// `(name, arity, next, context, line)` of every use, in order.
        fn uses(facts: &SyntaxFacts) -> Vec<(String, u16, Option<String>, CSharpNameContext, u32)> {
            names(facts)
                .uses
                .iter()
                .map(|use_| {
                    (
                        use_.name.clone(),
                        use_.arity,
                        use_.next.clone(),
                        use_.context,
                        use_.line,
                    )
                })
                .collect()
        }

        fn use_(
            name: &str,
            context: CSharpNameContext,
            line: u32,
        ) -> (String, u16, Option<String>, CSharpNameContext, u32) {
            (name.into(), 0, None, context, line)
        }

        fn directive(
            block: u32,
            kind: CSharpUsingKind,
            target: &str,
            alias: Option<&str>,
            global: bool,
            line: u32,
        ) -> CSharpUsing {
            CSharpUsing {
                block,
                kind,
                target: target.into(),
                arity: 0,
                alias: alias.map(str::to_string),
                global,
                extern_alias: None,
                line,
                conditional: false,
            }
        }

        #[test]
        fn using_directives_are_read_with_their_kind_block_and_spelling() {
            let facts = parse(
                "Page.cs",
                "extern alias Legacy;\n\
                 global using System;\n\
                 global using static System.Math;\n\
                 global using Json = Vendor.Serialization;\n\
                 using global::Acme.Ledger;\n\
                 using Legacy::Acme.Old;\n\
                 using Numbers = System.Collections.Generic.List<int>;\n\
                 namespace Acme.Web;\n\
                 using Acme.Billing;\n\
                 class Page { }\n",
            );
            let names = names(&facts);
            use CSharpUsingKind::*;
            let mut numbers = directive(
                0,
                Alias,
                "System.Collections.Generic.List",
                Some("Numbers"),
                false,
                7,
            );
            numbers.arity = 1;
            let mut legacy = directive(0, Namespace, "Acme.Old", None, false, 6);
            legacy.extern_alias = Some("Legacy".into());
            assert_eq!(
                names.usings,
                vec![
                    directive(0, Namespace, "System", None, true, 2),
                    directive(0, Static, "System.Math", None, true, 3),
                    directive(0, Alias, "Vendor.Serialization", Some("Json"), true, 4),
                    directive(0, Namespace, "Acme.Ledger", None, false, 5),
                    legacy,
                    numbers,
                    // After a file-scoped namespace a directive is that namespace's.
                    directive(1, Namespace, "Acme.Billing", None, false, 9),
                ]
            );
            assert_eq!(names.extern_aliases, vec!["Legacy".to_string()]);
            assert_eq!(names.blocks.len(), 2);
            assert_eq!(names.blocks[1].namespace, "Acme.Web");
            assert_eq!(names.blocks[1].parent, Some(0));

            // One import site per directive, named by what it names; an alias binds its name.
            let sites = facts
                .imports
                .iter()
                .map(|site| {
                    (
                        site.source.as_str(),
                        site.is_glob,
                        site.bindings
                            .iter()
                            .map(|binding| binding.local.as_str())
                            .collect::<Vec<_>>(),
                        site.range.start_line,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                sites,
                vec![
                    ("System", true, vec![], 2),
                    ("System.Math", true, vec![], 3),
                    ("Vendor.Serialization", false, vec!["Json"], 4),
                    ("Acme.Ledger", true, vec![], 5),
                    ("Legacy::Acme.Old", true, vec![], 6),
                    ("System.Collections.Generic.List", false, vec!["Numbers"], 7),
                    ("Acme.Billing", true, vec![], 9),
                ]
            );
        }

        #[test]
        fn namespace_blocks_nest_and_hold_their_own_directives() {
            let facts = parse(
                "Core.cs",
                "using Acme.Shared;\nnamespace Acme\n{\n    using Acme.Util;\n    namespace Ledger.Core\n    {\n        using Rules = Acme.Rules;\n        class A { Entry e; }\n    }\n}\n",
            );
            let names = names(&facts);
            let blocks = names
                .blocks
                .iter()
                .map(|block| (block.namespace.as_str(), block.parent))
                .collect::<Vec<_>>();
            assert_eq!(
                blocks,
                vec![("", None), ("Acme", Some(0)), ("Acme.Ledger.Core", Some(1))]
            );
            let directives = names
                .usings
                .iter()
                .map(|using| (using.block, using.target.as_str()))
                .collect::<Vec<_>>();
            assert_eq!(
                directives,
                vec![(0, "Acme.Shared"), (1, "Acme.Util"), (2, "Acme.Rules")]
            );
            assert_eq!(names.uses.len(), 1);
            assert_eq!(names.uses[0].block, 2);
            assert_eq!(names.uses[0].name, "Entry");
        }

        #[test]
        fn type_names_are_read_where_only_a_type_can_stand() {
            use CSharpNameContext::{Attribute, Receiver, Type};
            let facts = parse(
                "Page.cs",
                r#"namespace Acme.Web;
[Serializable, Acme.Marker]
public class Page<TModel> : Base<TModel>, IPage where TModel : Model
{
    private Entry? entry = new Entry();
    public Outer.Inner Nested { get; }
    public IList<Statement> Items(Line[] lines, out Ledger ledger) => Rules.Run<Rule>(Status.Active);
    void M(object o)
    {
        var x = (Cast)o;
        if (o is Pattern p) { }
        var t = typeof(Generic<,>);
        foreach (Item item in items) { }
        var y = o as AsType;
        var z = default(DefaultType);
        try { } catch (LedgerException ex) { }
        global::Acme.Full.Thing.Create();
        Legacy::Acme.Old.Thing.Create();
        nameof(NotAUse);
        Run(Plain);
    }
}
"#,
            );
            let mut generic = use_("Generic", Type, 12);
            generic.1 = 2;
            let mut base = use_("Base", Type, 3);
            base.1 = 1;
            let mut list = use_("IList", Type, 7);
            list.1 = 1;
            let mut marker = use_("Acme", Attribute, 2);
            marker.2 = Some("Marker".into());
            let mut nested = use_("Outer", Type, 6);
            nested.2 = Some("Inner".into());
            assert_eq!(
                uses(&facts),
                vec![
                    use_("Serializable", Attribute, 2),
                    marker,
                    base,
                    use_("TModel", Type, 3),
                    use_("IPage", Type, 3),
                    use_("Model", Type, 3),
                    use_("Entry", Type, 5),
                    nested,
                    list,
                    use_("Statement", Type, 7),
                    use_("Line", Type, 7),
                    use_("Ledger", Type, 7),
                    use_("Rules", Receiver, 7),
                    use_("Rule", Type, 7),
                    use_("Status", Receiver, 7),
                    use_("Cast", Type, 10),
                    use_("Pattern", Type, 11),
                    generic,
                    use_("Item", Type, 13),
                    use_("AsType", Type, 14),
                    use_("DefaultType", Type, 15),
                    use_("LedgerException", Type, 16),
                ]
            );
            let names = names(&facts);
            assert_eq!(names.type_parameters, vec!["TModel".to_string()]);
            for variable in [
                "entry", "lines", "ledger", "o", "x", "p", "t", "item", "y", "z", "ex",
            ] {
                assert!(
                    names.variables.contains(&variable.to_string()),
                    "{variable} in {:?}",
                    names.variables
                );
            }
        }

        #[test]
        fn a_type_records_its_arity_and_base_list_read_outside_it() {
            let facts = parse(
                "Entry.cs",
                "namespace Acme;\npublic class Entry<TKey, TValue> : Base, IEntry\n{\n    class Nested : Entry<int, int> { Inner i; }\n}\npublic record Line(int N) : Row(N);\n",
            );
            let names = names(&facts);
            let id_of = |qualified: &str| {
                facts
                    .symbols
                    .iter()
                    .find(|symbol| symbol.qualified_name == qualified)
                    .map(|symbol| symbol.id.clone())
                    .expect("type symbol")
            };
            let entry = id_of("Acme::Entry");
            let nested = id_of("Acme::Entry::Nested");
            let declared = names
                .types
                .iter()
                .map(|declared| {
                    (
                        declared.symbol_id.clone(),
                        declared.arity,
                        declared
                            .bases
                            .iter()
                            .map(|base| (base.name.clone(), base.enclosing_type.clone()))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                declared,
                vec![
                    (
                        entry.clone(),
                        2,
                        vec![("Base".to_string(), None), ("IEntry".to_string(), None)]
                    ),
                    // A nested type's base list is read in the type around it.
                    (
                        nested.clone(),
                        0,
                        vec![("Entry".to_string(), Some(entry.clone()))]
                    ),
                    (id_of("Acme::Line"), 0, vec![("Row".to_string(), None)]),
                ]
            );
            // A name inside a type is written in that type.
            let inner = names
                .uses
                .iter()
                .find(|use_| use_.name == "Inner")
                .expect("Inner is a use");
            assert_eq!(inner.enclosing_type, Some(nested));
        }

        #[test]
        fn a_constraint_names_its_parameter_but_uses_only_its_types() {
            let facts = parse("Box.cs", "class Box<T> where T : Item, new() { }\n");
            let written = names(&facts)
                .uses
                .iter()
                .map(|use_| use_.name.as_str())
                .collect::<Vec<_>>();
            assert_eq!(written, vec!["Item"]);
        }

        #[test]
        fn a_use_is_kept_once_per_place_at_its_first_line() {
            let facts = parse(
                "Twice.cs",
                "class Twice\n{\n    Entry a;\n    Entry b;\n    void M() { Entry.Make(); Entry.Make(); }\n}\n",
            );
            let written = names(&facts)
                .uses
                .iter()
                .map(|use_: &CSharpNameUse| (use_.name.as_str(), use_.context, use_.line))
                .collect::<Vec<_>>();
            assert_eq!(
                written,
                vec![
                    ("Entry", CSharpNameContext::Type, 3),
                    ("Entry", CSharpNameContext::Receiver, 5),
                ]
            );
        }

        #[test]
        fn nothing_inside_an_error_node_is_read_for_names() {
            let facts = parse(
                "Broken.cs",
                "using Acme.Ledger;\nnamespace Acme;\npublic class Broken\n{\n    public void Lost() { if (x { Entry e; }\n",
            );
            let names = names(&facts);
            assert_eq!(names.usings.len(), 1);
            assert!(
                names.uses.iter().all(|use_| use_.name != "Entry"),
                "{:?}",
                names.uses
            );
        }

        #[test]
        fn friend_assemblies_and_syntax_errors_are_recorded() {
            let facts = parse(
                "AssemblyInfo.cs",
                "using System.Runtime.CompilerServices;\n[assembly: InternalsVisibleTo(\"Acme.Tests\")]\n[assembly: System.Runtime.CompilerServices.InternalsVisibleToAttribute(\"Acme.Bench, PublicKey=0024\")]\n[assembly: Other(\"Acme.Nope\")]\n",
            );
            assert_eq!(
                names(&facts).internals_visible_to,
                vec!["Acme.Tests".to_string(), "Acme.Bench".to_string()]
            );
            assert!(!names(&facts).syntax_errors);
            let broken = parse("Broken.cs", "class Broken { void M() { if (x { } }\n");
            assert!(names(&broken).syntax_errors);
        }

        #[test]
        fn an_error_records_the_types_it_may_hide_and_whether_a_directive_may_be_missing() {
            use open_kioku_core::csharp::CSharpHiddenType;
            let hidden = |namespace: &str, name: &str| CSharpHiddenType {
                namespace: namespace.into(),
                name: name.into(),
            };
            // An error in a method body spells no type keyword and stands after the directives.
            let body = parse(
                "Body.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page\n{\n    void M() { var x = 1 +; }\n}\n",
            );
            assert!(names(&body).syntax_errors);
            assert!(names(&body).hidden_types.is_empty());
            assert!(!names(&body).hidden_directives);
            // Recovery swallowed the namespace declarations themselves: `Page` may be declared in
            // any namespace the error spells, and in the one it stands in.
            let declarations = parse(
                "Broken.cs",
                "namespace Acme\n{\n    namespace Web\n    {\n        public class Page { public void Lost() { if (x { }\n    }\n}\n",
            );
            let read = names(&declarations);
            assert_eq!(
                read.hidden_types,
                vec![
                    hidden("Acme", "Page"),
                    hidden("Web", "Page"),
                    hidden("Acme.Web", "Page"),
                    hidden("", "Page"),
                ],
                "{read:?}"
            );
            // A `using` recovery misreads into a top-level statement spells no `using` in any
            // error node, but the error stands where directives do.
            let directive = parse(
                "Directive.cs",
                "using Acme.Ledger;\nusing using Acme.Billing;\nnamespace Acme.Web;\npublic class Page { }\n",
            );
            assert!(names(&directive).hidden_directives);
            assert_eq!(names(&directive).usings.len(), 1);
            // So does one in a namespace body, before its first type.
            let nested = parse(
                "Nested.cs",
                "namespace Acme\n{\n    using using Acme.Billing;\n    class Page { }\n}\n",
            );
            assert!(names(&nested).hidden_directives);
        }

        #[test]
        fn what_an_if_region_holds_is_conditional() {
            let facts = parse(
                "Page.cs",
                "using System;\n#if NET6\nusing Acme.Modern;\n#else\nusing Acme.Legacy;\n#endif\nnamespace Acme.Web\n{\n#if !NET7\n    internal sealed class Polyfill { }\n#endif\n    public class Page\n    {\n#if DEBUG\n        private Entry entry;\n#elif TRACE\n        private Line line;\n#endif\n        private Kept kept;\n    }\n}\n",
            );
            let read = names(&facts);
            let usings = read
                .usings
                .iter()
                .map(|using| (using.target.as_str(), using.conditional))
                .collect::<Vec<_>>();
            assert_eq!(
                usings,
                vec![
                    ("System", false),
                    ("Acme.Modern", true),
                    ("Acme.Legacy", true)
                ]
            );
            let uses = read
                .uses
                .iter()
                .map(|use_| (use_.name.as_str(), use_.conditional))
                .collect::<Vec<_>>();
            assert_eq!(uses, vec![("Entry", true), ("Line", true), ("Kept", false)]);
            let types = read
                .types
                .iter()
                .map(|declared| declared.conditional)
                .collect::<Vec<_>>();
            assert_eq!(types, vec![true, false]);
        }

        #[test]
        fn other_languages_record_no_csharp_names() {
            let java = open_kioku_core::File {
                language: open_kioku_core::Language::Java,
                ..super::file("Entry.java")
            };
            let facts = crate::parse_file(&java, "class Entry {}\n").expect("parses");
            assert!(facts.csharp_names.is_none());
        }
    }

    mod code {
        use super::parse;
        use open_kioku_core::csharp::{
            CSharpCallKind, CSharpNameFacts, CSharpReceiver, CSharpReceiverRoot, CSharpTypeKind,
            CSharpTypeRef, CSharpTypeShape,
        };

        fn facts(source: &str) -> CSharpNameFacts {
            parse("src/Store.cs", source)
                .csharp_names
                .expect("a C# file has name facts")
        }

        fn written(type_: &CSharpTypeRef) -> String {
            let path = type_
                .path
                .iter()
                .map(|segment| {
                    if segment.arity > 0 {
                        format!("{}`{}", segment.name, segment.arity)
                    } else {
                        segment.name.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(".");
            let arguments = type_
                .arguments
                .iter()
                .map(written)
                .collect::<Vec<_>>()
                .join(",");
            let shape = match type_.shape {
                CSharpTypeShape::Named => "",
                CSharpTypeShape::Nullable => "?",
                CSharpTypeShape::Predefined => "predefined",
                CSharpTypeShape::Other => "other",
            };
            let global = if type_.global { "global::" } else { "" };
            if arguments.is_empty() {
                format!("{global}{path}{shape}")
            } else {
                format!("{global}{path}{shape}[{arguments}]")
            }
        }

        fn receiver(receiver: &CSharpReceiver) -> String {
            let root = match &receiver.root {
                CSharpReceiverRoot::None => String::new(),
                CSharpReceiverRoot::This => "this".into(),
                CSharpReceiverRoot::Base => "base".into(),
                CSharpReceiverRoot::Name(segment) => segment.name.clone(),
                CSharpReceiverRoot::Typed(type_) => format!("({})", written(type_)),
                CSharpReceiverRoot::Unknown => "?".into(),
            };
            std::iter::once(root)
                .chain(receiver.members.iter().map(|member| member.name.clone()))
                .collect::<Vec<_>>()
                .join(".")
        }

        /// `caller: kind receiver.name<type arguments>/arguments` for each call.
        fn calls(read: &CSharpNameFacts) -> Vec<String> {
            read.calls
                .iter()
                .map(|call| {
                    let caller = call
                        .caller
                        .map(|index| read.members[index as usize].name.clone())
                        .unwrap_or_else(|| "-".into());
                    let target = match call.kind {
                        CSharpCallKind::Invocation => {
                            let receiver = receiver(&call.receiver);
                            let dot = if receiver.is_empty() { "" } else { "." };
                            let arguments = call
                                .type_arguments
                                .map(|count| format!("<{count}>"))
                                .unwrap_or_default();
                            format!("{receiver}{dot}{}{arguments}", call.name)
                        }
                        CSharpCallKind::Creation => {
                            format!("new {}", call.created.as_ref().map_or("?".into(), written))
                        }
                        CSharpCallKind::BaseConstructor => "base()".into(),
                        CSharpCallKind::ThisConstructor => "this()".into(),
                    };
                    format!("{caller}: {target}/{}", call.arguments)
                })
                .collect()
        }

        #[test]
        fn calls_record_their_receiver_member_and_argument_count() {
            let read = facts(
                "namespace Acme.Ledger;\n\
                 public class Store : Base<int>, IStore\n\
                 {\n\
                 \x20   private readonly Entry _entry = new();\n\
                 \x20   public static Store Default => new Store(1);\n\
                 \x20   public Store(int x) : base(x) {}\n\
                 \x20   public Store() : this(1) {}\n\
                 \x20   public void Run(Entry a)\n\
                 \x20   {\n\
                 \x20       var s = new Store(2);\n\
                 \x20       s.Get(1);\n\
                 \x20       this.Get(1, \"x\");\n\
                 \x20       base.Save();\n\
                 \x20       Get(2);\n\
                 \x20       _entry?.Post();\n\
                 \x20       Store.Default.Run(a);\n\
                 \x20       Acme.Ledger.Rules.Apply<int>(1);\n\
                 \x20       nameof(Get);\n\
                 \x20       ((Entry)a).Save();\n\
                 \x20       GetStore().Get(1);\n\
                 \x20       int.Parse(\"1\");\n\
                 \x20       Helper();\n\
                 \x20       void Helper() => a.Items.First();\n\
                 \x20   }\n\
                 }\n",
            );
            assert_eq!(
                calls(&read),
                vec![
                    "_entry: new Entry/0",
                    "Default: new Store/1",
                    "Store: base()/1",
                    "Store: this()/1",
                    "Run: new Store/1",
                    "Run: s.Get/1",
                    "Run: this.Get/2",
                    "Run: base.Save/0",
                    "Run: Get/1",
                    "Run: _entry.Post/0",
                    "Run: Store.Default.Run/1",
                    "Run: Acme.Ledger.Rules.Apply<1>/1",
                    "Run: (Entry).Save/0",
                    "Run: ?.Get/1",
                    "Run: GetStore/0",
                    "Run: ?.Parse/1",
                    "Run: Helper/0",
                    "Helper: a.Items.First/0",
                ]
            );
        }

        #[test]
        fn members_record_their_parameters_and_types() {
            let read = facts(
                "namespace Acme.Ledger;\n\
                 public static class Ext\n\
                 {\n\
                 \x20   public static int Post(this Entry e, int x = 1) => 0;\n\
                 \x20   public static void All<T>(this T e, params Entry[] xs) {}\n\
                 }\n\
                 public record Money(decimal Amount, Entry Source) : Base(Amount), IThing;\n\
                 public class Book\n\
                 {\n\
                 \x20   private Entry? _maybe, _other;\n\
                 \x20   public List<Entry> Items { get; }\n\
                 \x20   public override string ToString() => \"\";\n\
                 \x20   public void Clear() {}\n\
                 }\n",
            );
            let members = read
                .members
                .iter()
                .map(|member| {
                    let parameters = member
                        .parameters
                        .iter()
                        .map(|parameter| {
                            format!(
                                "{}{}{}:{}{}",
                                if parameter.is_this { "this " } else { "" },
                                if parameter.is_params { "params " } else { "" },
                                parameter.name,
                                parameter.value_type.as_ref().map_or("-".into(), written),
                                if parameter.optional { "=" } else { "" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "{:?} {}{}{}({parameters}) -> {}",
                        member.kind,
                        if member.is_static { "static " } else { "" },
                        if member.is_override { "override " } else { "" },
                        member.name,
                        member.value_type.as_ref().map_or("-".into(), written)
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                members,
                vec![
                    "Method static Post(this e:Entry, x:predefined=) -> predefined",
                    "Method static All(this e:T, params xs:other[Entry]) -> -",
                    "PrimaryConstructor Money(Amount:predefined, Source:Entry) -> -",
                    "Property Amount() -> predefined",
                    "Property Source() -> Entry",
                    "Field _maybe() -> Entry?",
                    "Field _other() -> Entry?",
                    "Property Items() -> List`1[Entry]",
                    "Method override ToString() -> predefined",
                    "Method Clear() -> -",
                ]
            );
            assert_eq!(read.members[1].type_parameters, 1);
            assert_eq!(calls(&read), vec!["Money: base()/1"]);
            let kinds = read
                .types
                .iter()
                .map(|declared| (declared.kind, declared.is_static, declared.base_types.len()))
                .collect::<Vec<_>>();
            assert_eq!(
                kinds,
                vec![
                    (CSharpTypeKind::Class, true, 0),
                    (CSharpTypeKind::Record, false, 2),
                    (CSharpTypeKind::Class, false, 0),
                ]
            );
        }

        #[test]
        fn variables_record_their_type_and_scope() {
            let read = facts(
                "namespace Acme;\n\
                 class C(Store store)\n\
                 {\n\
                 \x20   void Run(Entry a, out Entry b)\n\
                 \x20   {\n\
                 \x20       var s = new Store();\n\
                 \x20       Store t = new(), u = null;\n\
                 \x20       var c = (Entry)a;\n\
                 \x20       var d = a as Entry;\n\
                 \x20       var e = Make();\n\
                 \x20       foreach (Entry f in list) { }\n\
                 \x20       if (a is Entry g) { }\n\
                 \x20       Get(out var h);\n\
                 \x20       try { } catch (LedgerException ex) { }\n\
                 \x20       Func<Entry, int> k = x => 1;\n\
                 \x20       Action<Entry> m = (Entry y) => y.Save();\n\
                 \x20       var (p, q) = pair;\n\
                 \x20   }\n\
                 }\n",
            );
            let locals = read
                .locals
                .iter()
                .map(|local| {
                    format!(
                        "{} {:?} {}{} lines {}-{}",
                        local.name,
                        local.kind,
                        local.value_type.as_ref().map_or("-".into(), written),
                        if local.declared { "" } else { " (inferred)" },
                        local.scope.start_line,
                        local.scope.end_line,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                locals,
                vec![
                    "store Parameter Store lines 2-19",
                    "a Parameter Entry lines 4-18",
                    "b Parameter Entry lines 4-18",
                    "s Variable Store (inferred) lines 5-18",
                    "t Variable Store lines 5-18",
                    "u Variable Store lines 5-18",
                    "c Variable Entry (inferred) lines 5-18",
                    "d Variable Entry (inferred) lines 5-18",
                    "e Variable - (inferred) lines 5-18",
                    "f Variable Entry lines 11-11",
                    "g Variable Entry lines 5-18",
                    "h Variable - (inferred) lines 5-18",
                    "ex Variable LedgerException lines 14-14",
                    "k Variable Func`2[Entry,predefined] lines 5-18",
                    "x LambdaParameter - (inferred) lines 15-15",
                    "m Variable Action`1[Entry] lines 5-18",
                    "y LambdaParameter Entry lines 16-16",
                    "p Variable - (inferred) lines 5-18",
                    "q Variable - (inferred) lines 5-18",
                ]
            );
            let creations = calls(&read)
                .into_iter()
                .filter(|call| call.contains("new"))
                .collect::<Vec<_>>();
            assert_eq!(creations, vec!["Run: new Store/0", "Run: new Store/0"]);
        }
    }
}
