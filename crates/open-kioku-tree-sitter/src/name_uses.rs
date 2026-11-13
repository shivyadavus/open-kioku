//! Where a file uses a name, read from its syntax tree.
//!
//! A line that holds a name is not a use of it: the name may stand in a string, a comment, a
//! parameter list or another declaration. This module answers from the tree, where an
//! identifier is an identifier: for each name asked about, every call of it with its arguments
//! counted from the argument nodes, every other reference, every import and every declaration.
//! Nothing here resolves a name; it says how the name is written at each place, and a caller
//! that knows what it is looking for decides what that means.

use crate::parser_for;
use open_kioku_core::Language;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// The arguments of one call, as the tree holds them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallArguments {
    pub positional: usize,
    /// Names of the arguments passed by name (`f(limit=3)`, C# `f(limit: 3)`).
    pub keywords: Vec<String>,
    /// An argument is unpacked (`*args`, `**kwargs`, `...items`, Go `items...`).
    pub spread: bool,
    /// The only argument is itself a call, which in Go may supply several values.
    pub single_call: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameUseKind {
    Call(CallArguments),
    /// Named without being called: passed, read, used as a type.
    Reference,
    /// Named by an import, a `use`, or an export list.
    Import,
    /// Declared: a function, method, type, variant or module (`binding` false), or a variable,
    /// parameter or field (`binding` true).
    Declaration {
        binding: bool,
    },
    /// The label of an argument passed by name (`f(limit=3)`): neither a use of anything called
    /// `limit` nor a binding of it.
    Label,
}

/// How a name is reached at one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reached {
    /// Written bare.
    Bare,
    /// Through a value or a name and a dot: `x.name`, `mod.name`, `Type.name`. The text is what
    /// stands before the dot.
    Member(String),
    /// Through a path: Rust `a::b::name`. The text is the path before the last `::`.
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameUse {
    /// 1-based line of the name.
    pub line: u32,
    pub kind: NameUseKind,
    pub reached: Reached,
    /// The innermost function or method the use is in.
    pub within_callable: Option<String>,
    /// The innermost type (class, struct `impl`, interface) the use is in.
    pub within_type: Option<String>,
    /// The use, or the call it is the callee of, sits in a part of the tree the parser could
    /// not read. What it says is then a guess.
    pub in_error: bool,
    /// For a member (`cmd.run()`): what the enclosing function says its receiver is. Set only
    /// when every binding of the receiver's name in that function agrees.
    pub receiver: Option<Receiver>,
}

/// What a member's receiver is, as far as the function around it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    /// A name declared with this type, or a value constructed as it: a parameter or a local
    /// with a written type, a local initialised by a constructor or a typed literal. The type
    /// is as written, without pointers and type arguments (`Command`, `cobra.Command`).
    Typed(String),
    /// The result of a call, directly (`NewRouter().Get(..)`) or through a name bound to it
    /// (`r := NewRouter()`): of the function or path `function` (`NewRouter`, `mux.NewRouter`,
    /// `Route::parse`), or, with `on`, of the method `function` called on another receiver.
    /// `result` is which of the call's results the name was bound to (Go: `r, err := f()`).
    Call {
        function: String,
        on: Option<Box<Receiver>>,
        result: usize,
    },
}

impl NameUse {
    /// `Type.callable`, `callable` or `Type`: where the use is, for a reader.
    pub fn within(&self) -> Option<String> {
        match (&self.within_type, &self.within_callable) {
            (Some(ty), Some(callable)) => Some(format!("{ty}.{callable}")),
            (None, Some(callable)) => Some(callable.clone()),
            (Some(ty), None) => Some(ty.clone()),
            (None, None) => None,
        }
    }
}

/// What one file says about the names asked of it.
#[derive(Debug, Clone, Default)]
pub struct FileNameUses {
    /// The uses of each requested name, in source order.
    pub uses: BTreeMap<String, Vec<NameUse>>,
    /// Every name the file binds as a variable, a parameter or a field: a bare use of such a
    /// name in this file may mean the binding.
    pub bound: BTreeSet<String>,
    /// The text of each import, `use` and export-list statement.
    pub imports: Vec<String>,
    /// The types that declare a base, a superclass, an interface or an embedded type, by name.
    pub types_with_bases: BTreeSet<String>,
}

/// Reads `content` for the uses of `names`. `None` when the language has no grammar here.
pub fn name_uses(
    language: &Language,
    content: &str,
    names: &BTreeSet<String>,
) -> Option<FileNameUses> {
    let mut parser = parser_for(language).ok()?;
    let tree = parser.parse(content, None)?;
    let source = content.as_bytes();
    let mut out = FileNameUses::default();
    for name in names {
        out.uses.insert(name.clone(), Vec::new());
    }
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_import_statement(node) {
            if let Ok(text) = node.utf8_text(source) {
                out.imports.push(text.to_string());
            }
        }
        if let Some(name) = type_with_bases(node, source) {
            out.types_with_bases.insert(name);
        }
        if node.child_count() == 0 {
            if is_identifier(node) && !on_directive_line(language, node, source) {
                if let Ok(text) = node.utf8_text(source) {
                    let mut found = classify(language, node, source);
                    if matches!(found.kind, NameUseKind::Declaration { binding: true }) {
                        out.bound.insert(text.to_string());
                    }
                    if let Some(list) = out.uses.get_mut(text) {
                        if matches!(found.reached, Reached::Member(_)) {
                            found.receiver = receiver_of(node)
                                .and_then(|receiver| receiver_value(language, receiver, source, 0));
                        }
                        list.push(found);
                    }
                }
            }
            continue;
        }
        // Reverse, so the stack yields children in source order.
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index) {
                stack.push(child);
            }
        }
    }
    Some(out)
}

/// A C# preprocessor line (`#if NET5_0_OR_GREATER`, `#define X`) names compilation symbols,
/// never a declaration of the program.
fn on_directive_line(language: &Language, node: Node<'_>, source: &[u8]) -> bool {
    if *language != Language::CSharp {
        return false;
    }
    let start = node.start_byte().min(source.len());
    let line_start = source[..start]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    source[line_start..start]
        .iter()
        .find(|byte| !byte.is_ascii_whitespace() && !matches!(**byte, 0xEF | 0xBB | 0xBF))
        == Some(&b'#')
}

fn is_identifier(node: Node<'_>) -> bool {
    let kind = node.kind();
    node.is_named()
        && (kind.ends_with("identifier") || kind == "shorthand_property_identifier_pattern")
}

fn is_import_statement(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "use_declaration"
            | "extern_crate_declaration"
            | "import_statement"
            | "import_from_statement"
            | "future_import_statement"
            | "import_declaration"
            | "import_spec"
            | "using_directive"
            | "export_clause"
            | "mod_item"
    ) && !(node.kind() == "mod_item" && node.child_by_field_name("body").is_some())
        // `export {name} from './module.js'`: the whole statement, with where it takes from.
        || (node.kind() == "export_statement" && node.child_by_field_name("source").is_some())
}

fn within_import(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "use_declaration"
                | "extern_crate_declaration"
                | "import_statement"
                | "import_from_statement"
                | "future_import_statement"
                | "import_declaration"
                | "import_spec"
                | "using_directive"
                | "export_clause"
                | "export_specifier"
        ) {
            return true;
        }
        // `export * from './a'` and `export { a } from './a'`.
        if parent.kind() == "export_statement" && parent.child_by_field_name("source").is_some() {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// The name of a type declaration that names something it derives from.
fn type_with_bases(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(str::to_string);
    let has_child = |kind: &str| {
        (0..node.child_count())
            .filter_map(|index| node.child(index))
            .any(|child| child.kind() == kind)
    };
    let derives = match node.kind() {
        // Python `class A(B):`.
        "class_definition" => node
            .child_by_field_name("superclasses")
            .is_some_and(|bases| bases.named_child_count() > 0),
        // JavaScript and TypeScript `class A extends B`, Java `extends`/`implements`, C#
        // `class A : B`.
        "class_declaration"
        | "class"
        | "abstract_class_declaration"
        | "record_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "enum_declaration" => {
            has_child("class_heritage")
                || has_child("base_list")
                || has_child("extends_type_clause")
                || has_child("extends_interfaces")
                || node.child_by_field_name("superclass").is_some()
                || node.child_by_field_name("interfaces").is_some()
        }
        // A Rust `impl Trait for Type`: the type's methods include the trait's defaults.
        "impl_item" => {
            return node
                .child_by_field_name("trait")
                .and_then(|_| node.child_by_field_name("type"))
                .and_then(text)
                .map(|ty| last_segment(&ty));
        }
        // A Go struct that embeds a type has its methods.
        "type_spec" => node.child_by_field_name("type").is_some_and(|ty| {
            let mut stack = vec![ty];
            while let Some(current) = stack.pop() {
                if current.kind() == "field_declaration"
                    && current.child_by_field_name("name").is_none()
                {
                    return true;
                }
                if matches!(current.kind(), "struct_type" | "field_declaration_list") {
                    stack.extend(
                        (0..current.named_child_count()).filter_map(|i| current.named_child(i)),
                    );
                }
            }
            false
        }),
        _ => false,
    };
    derives
        .then(|| node.child_by_field_name("name").and_then(text))
        .flatten()
}

fn last_segment(text: &str) -> String {
    let head = text.split(['<', '(', '[']).next().unwrap_or(text);
    head.rsplit(['.', ':'])
        .next()
        .unwrap_or(head)
        .trim()
        .to_string()
}

/// Nodes whose `name` field is a use of the name, not a declaration of it.
fn names_a_use(kind: &str) -> bool {
    matches!(
        kind,
        "member_access_expression"
            | "method_invocation"
            | "scoped_identifier"
            | "scoped_type_identifier"
            | "qualified_name"
            | "alias_qualified_name"
            | "member_binding_expression"
            | "qualified_type"
            | "nested_identifier"
            | "nested_type_identifier"
            | "attribute"
            | "annotation"
            | "marker_annotation"
            | "generic_type"
            | "explicit_interface_specifier"
    )
}

/// Declarations that bind a value in the file: a variable, a parameter or a field. Everything
/// else with a `name` is an item: a function, a method, a type, a variant, a module.
fn binds_a_value(kind: &str) -> bool {
    kind.contains("parameter")
        || kind.contains("variable")
        || matches!(
            kind,
            "var_spec"
                | "field_declaration"
                | "field_definition"
                | "public_field_definition"
                | "property_signature"
                | "let_declaration"
                | "named_expression"
                | "catch_declaration"
                | "resource"
                | "enhanced_for_statement"
                | "foreach_statement"
                | "from_clause"
                | "import_alias"
                | "aliased_import"
                | "namespace_import"
        )
}

/// The field of `parent` that `node` is, if it is one.
fn field_of<'a>(parent: Node<'a>, node: Node<'a>) -> Option<&'static str> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node() == node {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// Whether `node` stands where a pattern binds a name that no `name` field marks: a Python
/// parameter or assignment target, a Go `:=`, a Rust `let`, a JavaScript parameter.
fn binds_by_position(node: Node<'_>, parent: Node<'_>) -> bool {
    let field = field_of(parent, node);
    match parent.kind() {
        "parameters"
        | "lambda_parameters"
        | "formal_parameters"
        | "closure_parameters"
        | "inferred_parameters"
        | "typed_parameter"
        | "list_splat_pattern"
        | "dictionary_splat_pattern"
        | "object_pattern"
        | "array_pattern"
        | "rest_pattern"
        | "tuple_pattern"
        | "pattern_list"
        | "global_statement"
        | "nonlocal_statement"
        | "identifier_pattern"
        | "captured_pattern"
        | "reference_pattern"
        | "mut_pattern" => true,
        "let_declaration" | "parameter" | "required_parameter" | "optional_parameter"
        | "for_expression" | "closure_expression" => field == Some("pattern"),
        "assignment"
        | "augmented_assignment"
        | "assignment_expression"
        | "for_statement"
        | "for_in_statement"
        | "for_in_clause"
        | "range_clause"
        | "short_var_declaration"
        | "assignment_statement" => field == Some("left"),
        "arrow_function" => field == Some("parameter"),
        "with_item" | "as_pattern" | "as_pattern_target" | "catch_clause" => true,
        // `a, b := f()` and `a, b = f()`: the list is the left side.
        "expression_list" => parent.parent().is_some_and(|grand| {
            matches!(
                grand.kind(),
                "short_var_declaration" | "range_clause" | "assignment_statement"
            ) && field_of(grand, parent) == Some("left")
        }),
        _ => false,
    }
}

fn classify(language: &Language, node: Node<'_>, source: &[u8]) -> NameUse {
    let text = |node: Node<'_>| node.utf8_text(source).unwrap_or_default().to_string();
    let line = node.start_position().row as u32 + 1;
    let (within_callable, within_type) = enclosing(node, source);
    let mut found = NameUse {
        line,
        kind: NameUseKind::Reference,
        reached: Reached::Bare,
        within_callable,
        within_type,
        in_error: in_error(node),
        receiver: None,
    };
    let Some(parent) = node.parent() else {
        return found;
    };
    if within_import(node) {
        found.kind = NameUseKind::Import;
        return found;
    }
    // An argument's label is neither a use nor a binding of the name.
    if matches!(
        parent.kind(),
        "keyword_argument" | "name_colon" | "name_equals"
    ) && (parent.child_by_field_name("name") == Some(node)
        || parent.named_child(0) == Some(node))
    {
        found.kind = NameUseKind::Label;
        return found;
    }
    // The key of an object or struct literal (`{authorization: token}`, `Command{Args: run}`)
    // names a property or a field, not whatever else is called that.
    let literal_key = (parent.kind() == "pair" && parent.child_by_field_name("key") == Some(node))
        || (parent.kind() == "literal_element"
            && parent.parent().is_some_and(|element| {
                element.kind() == "keyed_element" && element.named_child(0) == Some(parent)
            }));
    if literal_key {
        found.kind = NameUseKind::Label;
        return found;
    }
    let is_name = parent.child_by_field_name("name") == Some(node);
    if (is_name && !names_a_use(parent.kind())) || binds_by_position(node, parent) {
        // The declaration's own name is not "within" itself.
        let binding = binds_a_value(parent.kind()) || !is_name;
        found.kind = NameUseKind::Declaration { binding };
        // For a binding too: one made outside every function and type binds the name for the
        // whole module.
        let (callable, ty) = enclosing(parent, source);
        found.within_callable = callable;
        found.within_type = ty;
        return found;
    }

    // Climb from the name to the whole expression it is the last part of: `a.b.name`,
    // `a::b::name`, `name::<T>`, `name<T>`.
    let mut expression = node;
    while let Some(outer) = expression.parent() {
        let field = field_of(outer, expression);
        let reached = match (outer.kind(), field) {
            ("attribute", Some("attribute")) => outer
                .child_by_field_name("object")
                .map(|object| Reached::Member(text(object))),
            ("member_expression", Some("property")) => outer
                .child_by_field_name("object")
                .map(|object| Reached::Member(text(object))),
            ("field_expression", Some("field")) => outer
                .child_by_field_name("value")
                .map(|value| Reached::Member(text(value))),
            ("selector_expression", Some("field")) => outer
                .child_by_field_name("operand")
                .map(|operand| Reached::Member(text(operand))),
            ("field_access", Some("field")) => outer
                .child_by_field_name("object")
                .map(|object| Reached::Member(text(object))),
            ("member_access_expression", Some("name")) => outer
                .child_by_field_name("expression")
                .or_else(|| outer.child(0))
                .map(|object| Reached::Member(text(object))),
            ("qualified_name", Some("name")) | ("nested_identifier", Some("name")) => outer
                .child_by_field_name("qualifier")
                .or_else(|| outer.child_by_field_name("scope"))
                .or_else(|| outer.child(0))
                .map(|qualifier| Reached::Member(text(qualifier))),
            ("member_binding_expression", Some("name")) => Some(Reached::Member("?".into())),
            // Go's `slog.Logger` and TypeScript's `ns.Type` in a type position.
            ("qualified_type", Some("name")) => outer
                .child_by_field_name("package")
                .map(|package| Reached::Member(text(package))),
            ("nested_type_identifier", Some("name")) => outer
                .child_by_field_name("module")
                .map(|module| Reached::Member(text(module))),
            ("scoped_identifier", Some("name")) | ("scoped_type_identifier", Some("name")) => {
                Some(Reached::Path(
                    outer
                        .child_by_field_name("path")
                        .or_else(|| outer.child_by_field_name("scope"))
                        .map(text)
                        .unwrap_or_default(),
                ))
            }
            // Java's `Map.Entry` in a type position has no fields: the name is its last part.
            ("scoped_type_identifier", None)
                if outer
                    .named_child_count()
                    .checked_sub(1)
                    .and_then(|last| outer.named_child(last))
                    == Some(expression) =>
            {
                outer
                    .named_child(0)
                    .map(|qualifier| Reached::Member(text(qualifier)))
            }
            ("generic_function", Some("function")) | ("generic_name", _) => None,
            ("generic_type", Some("type")) | ("type_arguments", _) => {
                // `new Entry<int>()`, `Vec::<u8>::new()`: the type's arguments wrap nothing
                // the name is reached through.
                if outer.kind() == "type_arguments" {
                    break;
                }
                None
            }
            _ => break,
        };
        if let Some(reached) = reached {
            found.reached = reached;
        } else if !matches!(
            outer.kind(),
            "generic_function" | "generic_name" | "generic_type"
        ) {
            break;
        }
        expression = outer;
    }

    let Some(call) = expression.parent() else {
        return found;
    };
    let callee_field = field_of(call, expression);
    let (is_call, arguments) = match (call.kind(), callee_field) {
        ("call", Some("function"))
        | ("call_expression", Some("function"))
        | ("invocation_expression", Some("function")) => {
            (true, call.child_by_field_name("arguments"))
        }
        ("new_expression", Some("constructor")) | ("object_creation_expression", Some("type")) => {
            (true, call.child_by_field_name("arguments"))
        }
        // Java names the method on the invocation itself.
        ("method_invocation", Some("name")) => {
            if let Some(object) = call.child_by_field_name("object") {
                found.reached = Reached::Member(text(object));
            }
            (true, call.child_by_field_name("arguments"))
        }
        _ => (false, None),
    };
    if is_call {
        found.in_error |= call.has_error();
        found.kind = NameUseKind::Call(arguments_of(language, arguments, source));
    }
    found
}

/// The expression a member name is reached through: `cmd` of `cmd.run()`.
fn receiver_of(node: Node<'_>) -> Option<Node<'_>> {
    let mut name = node;
    while let Some(parent) = name.parent() {
        if matches!(parent.kind(), "generic_name" | "generic_function") {
            name = parent;
        } else {
            break;
        }
    }
    let parent = name.parent()?;
    let field = field_of(parent, name);
    match (parent.kind(), field) {
        ("attribute", Some("attribute")) | ("member_expression", Some("property")) => {
            parent.child_by_field_name("object")
        }
        ("field_expression", Some("field")) => parent.child_by_field_name("value"),
        ("selector_expression", Some("field")) => parent.child_by_field_name("operand"),
        ("field_access", Some("field")) | ("method_invocation", Some("name")) => {
            parent.child_by_field_name("object")
        }
        ("member_access_expression", Some("name")) => parent
            .child_by_field_name("expression")
            .or_else(|| parent.child(0)),
        _ => None,
    }
}

fn is_callable_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_declaration"
            | "function_item"
            | "method_declaration"
            | "method_definition"
            | "constructor_declaration"
            | "local_function_statement"
            | "generator_function_declaration"
            | "function_expression"
            | "arrow_function"
            | "func_literal"
            | "lambda"
            | "lambda_expression"
            | "closure_expression"
            | "anonymous_method_expression"
    )
}

/// How deep a receiver is followed through the calls and names it comes from.
const RECEIVER_DEPTH: usize = 6;

/// What the expression a member is reached through is. See [`Receiver`].
fn receiver_value(
    language: &Language,
    receiver: Node<'_>,
    source: &[u8],
    depth: usize,
) -> Option<Receiver> {
    if depth > RECEIVER_DEPTH {
        return None;
    }
    match receiver.kind() {
        "identifier" => name_receiver(language, receiver, source, depth),
        "parenthesized_expression" => receiver
            .named_child(0)
            .and_then(|inner| receiver_value(language, inner, source, depth + 1)),
        "call_expression" | "call" | "method_invocation" | "invocation_expression" => {
            call_receiver(language, receiver, source, depth, 0)
        }
        _ => None,
    }
}

/// The result of a call, as a receiver.
fn call_receiver(
    language: &Language,
    call: Node<'_>,
    source: &[u8],
    depth: usize,
    result: usize,
) -> Option<Receiver> {
    if depth > RECEIVER_DEPTH {
        return None;
    }
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(str::to_string);
    let free = |function: String| {
        Some(Receiver::Call {
            function,
            on: None,
            result,
        })
    };
    // Java names the method on the invocation; the others call an expression.
    let (object, name) = if call.kind() == "method_invocation" {
        (
            call.child_by_field_name("object"),
            call.child_by_field_name("name")?,
        )
    } else {
        let function = call.child_by_field_name("function")?;
        match function.kind() {
            // Go's `new(T)` is a `*T`.
            "identifier"
                if *language == Language::Go && text(function).as_deref() == Some("new") =>
            {
                return call
                    .child_by_field_name("arguments")
                    .and_then(|arguments| arguments.named_child(0))
                    .and_then(|ty| ty.utf8_text(source).ok())
                    .and_then(clean_type)
                    .map(Receiver::Typed);
            }
            "identifier" | "scoped_identifier" => return free(text(function)?),
            "selector_expression" => (
                function.child_by_field_name("operand"),
                function.child_by_field_name("field")?,
            ),
            "attribute" => (
                function.child_by_field_name("object"),
                function.child_by_field_name("attribute")?,
            ),
            "member_expression" => (
                function.child_by_field_name("object"),
                function.child_by_field_name("property")?,
            ),
            "field_expression" => (
                function.child_by_field_name("value"),
                function.child_by_field_name("field")?,
            ),
            "member_access_expression" => (
                function
                    .child_by_field_name("expression")
                    .or_else(|| function.child(0)),
                function.child_by_field_name("name")?,
            ),
            _ => return None,
        }
    };
    let name = text(name)?;
    let Some(object) = object else {
        return free(name);
    };
    match receiver_value(language, object, source, depth + 1) {
        Some(on) => Some(Receiver::Call {
            function: name,
            on: Some(Box::new(on)),
            result,
        }),
        // A name the function says nothing of may be a package or a module: `mux.NewRouter()`.
        None if matches!(object.kind(), "identifier" | "scoped_identifier") => {
            let separator = if object.kind() == "scoped_identifier" || *language == Language::Rust {
                "::"
            } else {
                "."
            };
            free(format!("{}{separator}{name}", text(object)?))
        }
        None => None,
    }
}

/// What a plain name is: what the nearest function around the use that binds it says, when
/// every binding of the name in that function's own scope agrees. A closure sees the names of
/// the functions around it, and its own parameter of the same name hides them only inside it.
fn name_receiver(
    language: &Language,
    receiver: Node<'_>,
    source: &[u8],
    depth: usize,
) -> Option<Receiver> {
    let name = receiver.utf8_text(source).ok()?;
    let mut current = receiver.parent();
    while let Some(scope) = current {
        current = scope.parent();
        if !is_callable_node(scope.kind()) {
            continue;
        }
        let mut settled: Option<Receiver> = None;
        let mut bound = false;
        let mut stack = vec![scope];
        while let Some(here) = stack.pop() {
            if here.child_count() == 0 {
                if here.kind() == "identifier" && here.utf8_text(source).ok() == Some(name) {
                    let binds = matches!(
                        classify(language, here, source).kind,
                        NameUseKind::Declaration { binding: true }
                    );
                    // Where types are static, assigning to a name does not change its type.
                    let reassigned = !matches!(language, Language::Python | Language::JavaScript)
                        && is_reassignment(here);
                    if binds && !reassigned {
                        bound = true;
                        // One binding that says nothing, or two that disagree, settle nothing.
                        let found = binding_receiver(language, here, source, depth)?;
                        if settled.as_ref().is_some_and(|known| *known != found) {
                            return None;
                        }
                        settled = Some(found);
                    }
                }
                continue;
            }
            for index in 0..here.child_count() {
                if let Some(child) = here.child(index) {
                    // A function inside this one has its own names.
                    if child != scope && is_callable_node(child.kind()) {
                        continue;
                    }
                    stack.push(child);
                }
            }
        }
        if bound {
            return settled;
        }
    }
    None
}

/// `name = value` on a name declared elsewhere, as opposed to a declaration.
fn is_reassignment(binding: Node<'_>) -> bool {
    let assigns = |kind: &str| {
        matches!(
            kind,
            "assignment_statement"
                | "assignment_expression"
                | "augmented_assignment_expression"
                | "compound_assignment_expr"
        )
    };
    binding.parent().is_some_and(|parent| {
        assigns(parent.kind())
            || (parent.kind() == "expression_list"
                && parent.parent().is_some_and(|grand| assigns(grand.kind())))
    })
}

/// What one binding of a name says it is: the type it is written with, the type its
/// initialiser constructs, or the call its initialiser is.
fn binding_receiver(
    language: &Language,
    binding: Node<'_>,
    source: &[u8],
    depth: usize,
) -> Option<Receiver> {
    let text = |node: Node<'_>| node.utf8_text(source).ok();
    let parent = binding.parent()?;
    let written = |node: Node<'_>| {
        node.child_by_field_name("type")
            .and_then(text)
            .and_then(clean_type)
            .map(Receiver::Typed)
    };
    // What a value constructs: `new T(..)`, `T{..}`, `&T{..}`, `new(T)`, `T(..)`, `T::new(..)`.
    let constructed = |value: Node<'_>| -> Option<String> {
        match value.kind() {
            "new_expression" => value.child_by_field_name("constructor").and_then(text),
            "object_creation_expression" | "composite_literal" => {
                value.child_by_field_name("type").and_then(text)
            }
            "struct_expression" => value.child_by_field_name("name").and_then(text),
            "call_expression" | "call" => {
                let function = value.child_by_field_name("function").and_then(text)?;
                match language {
                    Language::Go if function == "new" => value
                        .child_by_field_name("arguments")
                        .and_then(|arguments| arguments.named_child(0))
                        .and_then(text),
                    // A capitalised callee is a class by convention only; the caller checks
                    // that it is the type it is looking for.
                    Language::Python => last_segment(function)
                        .starts_with(char::is_uppercase)
                        .then_some(function),
                    Language::Rust => function
                        .strip_suffix("::new")
                        .or_else(|| function.strip_suffix("::default")),
                    _ => None,
                }
            }
            _ => None,
        }
        .and_then(clean_type)
    };
    // The value a binding is initialised with, and which of its results the binding takes.
    let valued = |value: Node<'_>, result: usize| -> Option<Receiver> {
        let mut value = value;
        while matches!(
            value.kind(),
            "unary_expression" | "reference_expression" | "parenthesized_expression"
        ) {
            value = value
                .child_by_field_name("operand")
                .or_else(|| value.child_by_field_name("value"))
                .or_else(|| value.named_child(0))?;
        }
        if result == 0 {
            if let Some(ty) = constructed(value) {
                return Some(Receiver::Typed(ty));
            }
        }
        matches!(
            value.kind(),
            "call_expression" | "call" | "method_invocation" | "invocation_expression"
        )
        .then(|| call_receiver(language, value, source, depth + 1, result))
        .flatten()
    };
    match (language, parent.kind()) {
        (Language::Go, "parameter_declaration" | "var_spec") => written(parent).or_else(|| {
            let values = parent.child_by_field_name("value")?;
            (values.named_child_count() == 1)
                .then(|| valued(values.named_child(0)?, 0))
                .flatten()
        }),
        (Language::Go, "expression_list") => {
            let declaration = parent.parent()?;
            if declaration.kind() != "short_var_declaration"
                || declaration.child_by_field_name("left") != Some(parent)
            {
                return None;
            }
            let right = declaration.child_by_field_name("right")?;
            let index = (0..parent.named_child_count())
                .find(|index| parent.named_child(*index) == Some(binding))?;
            if right.named_child_count() == parent.named_child_count() {
                valued(right.named_child(index)?, 0)
            } else if right.named_child_count() == 1 {
                // `r, err := open()`: the call's results in order.
                valued(right.named_child(0)?, index)
            } else {
                None
            }
        }
        (Language::Python, "typed_parameter" | "typed_default_parameter") => written(parent),
        (Language::Python, "assignment") => written(parent).or_else(|| {
            parent
                .child_by_field_name("right")
                .and_then(|value| valued(value, 0))
        }),
        (
            Language::TypeScript | Language::JavaScript,
            "required_parameter" | "optional_parameter" | "variable_declarator",
        ) => written(parent).or_else(|| {
            parent
                .child_by_field_name("value")
                .and_then(|value| valued(value, 0))
        }),
        (Language::Java, "formal_parameter" | "enhanced_for_statement" | "resource") => {
            written(parent)
        }
        (Language::Java | Language::CSharp, "variable_declarator") => {
            let declaration = parent.parent()?;
            let declared = written(declaration).filter(|ty| *ty != Receiver::Typed("var".into()));
            declared.or_else(|| {
                let mut cursor = parent.walk();
                let value = parent.child_by_field_name("value").or_else(|| {
                    parent
                        .named_children(&mut cursor)
                        .filter(|child| *child != binding)
                        .last()
                })?;
                let value = if value.kind() == "equals_value_clause" {
                    value.named_child(0)?
                } else {
                    value
                };
                valued(value, 0)
            })
        }
        (Language::CSharp, "parameter" | "foreach_statement") => written(parent),
        (Language::Rust, "parameter") => written(parent),
        (Language::Rust, "let_declaration") => written(parent).or_else(|| {
            parent
                .child_by_field_name("value")
                .and_then(|value| valued(value, 0))
        }),
        _ => None,
    }
}

/// A type as a receiver's declaration writes it, down to the name a member's owner is compared
/// with: no pointer, reference, lifetime or type arguments. `None` for what names no one type
/// (a slice, a map, a function, a tuple, a union).
fn clean_type(text: &str) -> Option<String> {
    let mut rest = text.trim().trim_start_matches(':').trim();
    loop {
        let before = rest;
        for prefix in ["*", "&", "mut ", "dyn ", "impl ", "readonly ", "final "] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest).trim_start();
        }
        if let Some(after) = rest.strip_prefix('\'') {
            // A Rust lifetime: `&'a T`.
            rest = after
                .split_once(' ')
                .map(|(_, ty)| ty)
                .unwrap_or("")
                .trim_start();
        }
        if rest == before {
            break;
        }
    }
    let head = rest.split(['<', '[']).next().unwrap_or(rest).trim();
    let head = head.trim_end_matches(['?', '!']);
    let named = !head.is_empty()
        && head
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | ':' | '$'))
        && head.starts_with(|ch: char| ch.is_alphabetic() || ch == '_');
    named.then(|| head.to_string())
}

fn arguments_of(language: &Language, arguments: Option<Node<'_>>, source: &[u8]) -> CallArguments {
    let mut out = CallArguments::default();
    let Some(arguments) = arguments else {
        return out;
    };
    // Python `f(x for x in xs)`: the generator is the one argument.
    if arguments.kind() == "generator_expression" {
        out.positional = 1;
        return out;
    }
    let text = |node: Node<'_>| node.utf8_text(source).unwrap_or_default().to_string();
    let mut only = None;
    for index in 0..arguments.named_child_count() {
        let Some(argument) = arguments.named_child(index) else {
            continue;
        };
        match argument.kind() {
            kind if kind.contains("comment") => {}
            "keyword_argument" => {
                out.keywords.push(
                    argument
                        .child_by_field_name("name")
                        .map(text)
                        .unwrap_or_default(),
                );
            }
            "list_splat" | "dictionary_splat" | "spread_element" | "variadic_argument" => {
                out.spread = true;
            }
            // C# `f(limit: 3)`: the label is the argument's `name`, written before a colon.
            "argument" if *language == Language::CSharp => {
                let label = argument.child_by_field_name("name").or_else(|| {
                    (0..argument.named_child_count())
                        .filter_map(|i| argument.named_child(i))
                        .find(|child| child.kind() == "name_colon")
                        .and_then(|colon| colon.named_child(0))
                });
                match label {
                    Some(label) => out.keywords.push(text(label)),
                    None => out.positional += 1,
                }
            }
            kind => {
                out.positional += 1;
                only = Some(kind);
            }
        }
    }
    out.single_call = out.positional == 1
        && out.keywords.is_empty()
        && only.is_some_and(|kind| kind == "call_expression");
    out
}

fn in_error(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(here) = current {
        if here.is_error() || here.is_missing() {
            return true;
        }
        current = here.parent();
    }
    false
}

/// The innermost callable and the innermost type around `node`, by name.
fn enclosing(node: Node<'_>, source: &[u8]) -> (Option<String>, Option<String>) {
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(str::to_string);
    let mut callable = None;
    let mut ty = None;
    let mut current = node.parent();
    while let Some(here) = current {
        match here.kind() {
            "function_definition"
            | "function_declaration"
            | "function_item"
            | "method_declaration"
            | "method_definition"
            | "constructor_declaration"
            | "local_function_statement"
            | "generator_function_declaration"
            | "function_signature_item" => {
                if callable.is_none() {
                    callable = here.child_by_field_name("name").and_then(text);
                }
                // A Go method's type is its receiver's.
                if ty.is_none() && here.kind() == "method_declaration" {
                    if let Some(receiver) = here.child_by_field_name("receiver") {
                        ty = text(receiver).map(|receiver| {
                            let inner = receiver.trim_matches(['(', ')']);
                            let named = inner.split_whitespace().last().unwrap_or(inner);
                            last_segment(named.trim_start_matches('*'))
                        });
                    }
                }
            }
            // `const name = () => ...` and `name = function () {}`.
            "variable_declarator" if callable.is_none() => {
                let is_function = here.child_by_field_name("value").is_some_and(|value| {
                    matches!(
                        value.kind(),
                        "arrow_function" | "function_expression" | "function"
                    )
                });
                if is_function {
                    callable = here.child_by_field_name("name").and_then(text);
                }
            }
            "class_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "trait_item"
            | "struct_item"
            | "enum_item"
            | "class" => {
                if ty.is_none() {
                    ty = here.child_by_field_name("name").and_then(text);
                }
            }
            "impl_item" if ty.is_none() => {
                ty = here
                    .child_by_field_name("type")
                    .and_then(text)
                    .map(|name| last_segment(&name));
            }
            _ => {}
        }
        if callable.is_some() && ty.is_some() {
            break;
        }
        current = here.parent();
    }
    (callable, ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses(language: Language, source: &str, name: &str) -> (Vec<NameUse>, FileNameUses) {
        let names = [name.to_string()].into_iter().collect();
        let all = name_uses(&language, source, &names).expect("a grammar for the language");
        (all.uses[name].clone(), all)
    }

    fn calls(language: Language, source: &str, name: &str) -> Vec<(u32, usize, Vec<String>, bool)> {
        uses(language, source, name)
            .0
            .into_iter()
            .filter_map(|found| match found.kind {
                NameUseKind::Call(arguments) => Some((
                    found.line,
                    arguments.positional,
                    arguments.keywords,
                    arguments.spread,
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn arguments_are_counted_from_the_tree_whatever_they_hold() {
        // A shift, a comment between arguments and a call spread over lines.
        let go = "package m\n\nfunc Use() {\n\tAlloc(1<<10, 8)\n\tAlloc(\n\t\t1,\n\t\t// retries: three, as agreed with ops\n\t\t2,\n\t)\n\tAlloc(xs...)\n}\n";
        assert_eq!(
            calls(Language::Go, go, "Alloc"),
            vec![
                (4, 2, vec![], false),
                (5, 2, vec![], false),
                (10, 0, vec![], true)
            ]
        );
        // Prefixed, long and bracket-holding single-quoted strings; a lambda; keywords.
        let python = "warn(f'bad value {x}, expected {y}')\nwarn('(', 1)\nwarn(')', key=lambda a, b: a, *rest)\nwarn('The request could not be completed in time, so it will be retried shortly', 2)\nprint(\"warn(3) ->\", warn(3, 2))\n";
        assert_eq!(
            calls(Language::Python, python, "warn"),
            vec![
                (1, 1, vec![], false),
                (2, 2, vec![], false),
                (3, 1, vec!["key".to_string()], true),
                (4, 2, vec![], false),
                // The `warn(3)` inside the string is not a call.
                (5, 2, vec![], false),
            ]
        );
        let typescript = "report('It could not be completed in time, so it will be retried', 2);\nreport(new Map<string, number>(), `a, ${report(1)}`);\n";
        assert_eq!(
            calls(Language::TypeScript, typescript, "report"),
            vec![
                (1, 2, vec![], false),
                (2, 2, vec![], false),
                (2, 1, vec![], false)
            ]
        );
        let rust =
            "fn f() { owed(|a, b| a + b, Vec::<(u8, u8)>::new(), ')'); crate::m::owed(1) }\n";
        assert_eq!(
            calls(Language::Rust, rust, "owed"),
            vec![(1, 3, vec![], false), (1, 1, vec![], false)]
        );
        let csharp =
            "class C { void M() { Convert(value, toType: typeof(int)); new Entry<int>(1, 2); } }\n";
        assert_eq!(
            calls(Language::CSharp, csharp, "Convert"),
            vec![(1, 1, vec!["toType".to_string()], false)]
        );
        assert_eq!(
            calls(Language::CSharp, csharp, "Entry"),
            vec![(1, 2, vec![], false)]
        );
        let java = "class C { void m() { this.send(a, /* b, c */ d); Helper.send(1); } }\n";
        assert_eq!(
            calls(Language::Java, java, "send"),
            vec![(1, 2, vec![], false), (1, 1, vec![], false)]
        );
    }

    #[test]
    fn declarations_bindings_imports_and_labels_are_not_calls() {
        // An interface method, an enum variant and a method on a receiver are declarations.
        let (found, _) = uses(
            Language::TypeScript,
            "import { fetchUser } from './api';\ninterface Deps { fetchUser(id: string): string; }\nexport { fetchEvery as fetchUser };\n",
            "fetchUser",
        );
        assert_eq!(
            found.iter().map(|u| u.kind.clone()).collect::<Vec<_>>(),
            vec![
                NameUseKind::Import,
                NameUseKind::Declaration { binding: false },
                NameUseKind::Import,
            ]
        );
        let (found, _) = uses(
            Language::Go,
            "package s\n\ntype Store interface {\n\tLookup(key string) string\n}\n\nfunc (m *Mem) Lookup(key string) string { return key }\n",
            "Lookup",
        );
        assert!(
            found
                .iter()
                .all(|u| u.kind == NameUseKind::Declaration { binding: false }),
            "{found:?}"
        );
        let (found, _) = uses(
            Language::Rust,
            "enum E { Timeout(u64), Other }\nfn f(e: E) -> bool { matches!(e, E::Other) }\n",
            "Timeout",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, NameUseKind::Declaration { binding: false });

        // Variables, parameters and loop targets are bound in the file.
        let (_, go) = uses(
            Language::Go,
            "package c\n\nfunc run(registry int) {\n\twatcher, err := newWatcher()\n\tfor _, item := range items {\n\t}\n\tvar engine = 1\n}\n",
            "x",
        );
        for name in ["registry", "watcher", "err", "item", "engine"] {
            assert!(go.bound.contains(name), "{name} in {:?}", go.bound);
        }
        let (found, python) = uses(
            Language::Python,
            "cache = redis.Redis()\ndef run(engine, *rest, limit=3):\n    for item in rest:\n        cache.lookup(key=item)\n",
            "key",
        );
        for name in ["cache", "engine", "rest", "limit", "item"] {
            assert!(python.bound.contains(name), "{name} in {:?}", python.bound);
        }
        // A keyword argument's label binds nothing and is not a use.
        assert!(!python.bound.contains("key"));
        assert!(
            found.iter().all(|u| u.kind == NameUseKind::Label),
            "{found:?}"
        );
    }

    #[test]
    fn a_use_says_how_it_is_reached_and_where_it_is() {
        let (found, file) = uses(
            Language::Python,
            "from store import cache\n\nclass Worker(Thread):\n    def run(self):\n        self.start()\n        super().start()\n        cache.start(1)\n        start()\n",
            "start",
        );
        assert_eq!(
            found
                .iter()
                .map(|u| (u.reached.clone(), u.within()))
                .collect::<Vec<_>>(),
            vec![
                (Reached::Member("self".into()), Some("Worker.run".into())),
                (Reached::Member("super()".into()), Some("Worker.run".into())),
                (Reached::Member("cache".into()), Some("Worker.run".into())),
                (Reached::Bare, Some("Worker.run".into())),
            ]
        );
        assert!(file.types_with_bases.contains("Worker"));
        assert_eq!(file.imports, vec!["from store import cache".to_string()]);

        let (found, file) = uses(
            Language::Rust,
            "use crate::clearing;\nmod other;\nimpl Display for Entry {\n    fn fmt(&self) { clearing::owed(1); Self::owed(2); self.owed(3) }\n}\n",
            "owed",
        );
        assert_eq!(
            found.iter().map(|u| u.reached.clone()).collect::<Vec<_>>(),
            vec![
                Reached::Path("clearing".into()),
                Reached::Path("Self".into()),
                Reached::Member("self".into()),
            ]
        );
        assert_eq!(found[0].within(), Some("Entry.fmt".into()));
        assert!(file.types_with_bases.contains("Entry"));
        assert_eq!(file.imports.len(), 2);

        let (found, _) = uses(
            Language::Go,
            "package m\n\nfunc (s *Server) Run() {\n\thttp.Get(url)\n\ts.Get(url)\n}\n",
            "Get",
        );
        assert_eq!(
            found
                .iter()
                .map(|u| (u.reached.clone(), u.within()))
                .collect::<Vec<_>>(),
            vec![
                (Reached::Member("http".into()), Some("Server.Run".into())),
                (Reached::Member("s".into()), Some("Server.Run".into())),
            ]
        );
    }

    #[test]
    fn a_name_in_a_comment_a_string_or_a_docstring_is_no_use() {
        let (found, _) = uses(
            Language::Go,
            "/*\nSchedule(\"nightly\") runs it.\n*/\npackage m\n\nconst help = `use Schedule(\"nightly\")`\n\nfunc f() { x := 0 // Schedule later\n\t_ = x }\n",
            "Schedule",
        );
        assert!(found.is_empty(), "{found:?}");
        let (found, _) = uses(
            Language::Python,
            "def show(path):\n    \"\"\"Uses\n    render(path) to print.\n    \"\"\"\n    return 'render'  # render\n",
            "render",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// The type of the name a member is reached through, where the enclosing function
    /// settles it, and nothing where it does not.
    #[test]
    fn a_receivers_type_is_read_from_its_declaration_in_the_function() {
        let types = |language: Language, source: &str, name: &str| {
            uses(language, source, name)
                .0
                .into_iter()
                .filter(|found| matches!(found.kind, NameUseKind::Call(_)))
                .map(|found| {
                    (
                        found.line,
                        match found.receiver {
                            Some(Receiver::Typed(ty)) => Some(ty),
                            Some(Receiver::Call {
                                function,
                                on,
                                result,
                            }) => Some(format!(
                                "call {function}{}{}",
                                if on.is_some() { " on a receiver" } else { "" },
                                if result > 0 {
                                    format!(" #{result}")
                                } else {
                                    String::new()
                                }
                            )),
                            None => None,
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let some = |ty: &str| Some(ty.to_string());
        // The method's own receiver, a typed parameter, a literal, `var`, `new`; not the result
        // of a call, a field, or a name bound twice with different types.
        assert_eq!(
            types(
                Language::Go,
                "package a\n\nfunc (c *Command) Run(o *lib.Other, n int) {\n\tc.Out()\n\to.Out()\n\tx := &Command{}\n\tx.Out()\n\tvar y Command\n\ty.Out()\n\tz := NewCommand()\n\tz.Out()\n\tc.parent.Out()\n\tw := new(Command)\n\tw = pick()\n\tw.Out()\n\tfor _, r := range rs {\n\t\tr.Out()\n\t}\n}\n\nfunc two() {\n\tv := Command{}\n\tv.Out()\n\tif true {\n\t\tv := Other{}\n\t\tv.Out()\n\t}\n}\n",
                "Out"
            ),
            vec![
                (4, some("Command")),
                (5, some("lib.Other")),
                (7, some("Command")),
                (9, some("Command")),
                (11, some("call NewCommand")),
                (12, None),
                (15, some("Command")),
                (17, None),
                (23, None),
                (26, None),
            ]
        );
        assert_eq!(
            types(
                Language::Python,
                "class A:\n    def run(self, cmd: Command, o, p: 'Other' = None):\n        self.out()\n        cmd.out()\n        x = Command()\n        x.out()\n        y: pkg.Command = make()\n        y.out()\n        o.out()\n        z = make()\n        z.out()\n",
                "out"
            ),
            vec![
                (3, None),
                (4, some("Command")),
                (6, some("Command")),
                (8, some("pkg.Command")),
                (9, None),
                (11, some("call make")),
            ]
        );
        assert_eq!(
            types(
                Language::TypeScript,
                "class A {\n  run(c: Command, o: any, q?: Map<string, Command>) {\n    c.out();\n    const x = new Command();\n    x.out();\n    this.out();\n    q.out();\n    const [d] = list;\n    d.out();\n  }\n}\n",
                "out"
            ),
            vec![
                (3, some("Command")),
                (5, some("Command")),
                (6, None),
                (7, some("Map")),
                (9, None),
            ]
        );
        assert_eq!(
            types(
                Language::Java,
                "class A {\n  void run(Command c, List<Command> all) {\n    c.out();\n    Command x = make();\n    x.out();\n    var y = new Command();\n    y.out();\n    for (Command each : all) {\n      each.out();\n    }\n    out();\n  }\n}\n",
                "out"
            ),
            vec![
                (3, some("Command")),
                (5, some("Command")),
                (7, some("Command")),
                (9, some("Command")),
                (11, None),
            ]
        );
        assert_eq!(
            types(
                Language::CSharp,
                "class A {\n  void Run(Command c) {\n    c.Out();\n    var y = new Command();\n    y.Out();\n    Command? z = Make();\n    z.Out();\n    var w = Make();\n    w.Out();\n  }\n}\n",
                "Out"
            ),
            vec![
                (3, some("Command")),
                (5, some("Command")),
                (7, some("Command")),
                (9, some("call Make")),
            ]
        );
        assert_eq!(
            types(
                Language::Rust,
                "impl A {\n    fn run(&self, c: &mut Command, d: &'a dyn Shape) {\n        c.out();\n        let x = Command::new();\n        x.out();\n        let y: Command = make();\n        y.out();\n        self.out();\n        let z = Command::parse(s);\n        z.out();\n        d.out();\n        let w = Command { a: 1 };\n        w.out();\n    }\n}\n",
                "out"
            ),
            vec![
                (3, some("Command")),
                (5, some("Command")),
                (7, some("Command")),
                (8, None),
                (10, some("call Command::parse")),
                (11, some("Shape")),
                (13, some("Command")),
            ]
        );
    }

    /// A call's result as a receiver: directly, through a name, through a package, and in a
    /// chain.
    #[test]
    fn a_call_result_is_a_receiver_too() {
        let (found, _) = uses(
            Language::Go,
            "package a\n\nfunc run(s *Server) {\n\tr := mux.NewRouter()\n\tr.Get()\n\tNewRouter().Get()\n\tv, err := open()\n\terr.Get()\n\ts.Route().Get()\n\tv.Get()\n\thandle(func(w Writer, r *Request) {\n\t\tr.Get()\n\t\ts.Get()\n\t})\n\tnew(Route).Get()\n}\n",
            "Get",
        );
        let call = |function: &str, on: Option<Receiver>, result: usize| {
            Some(Receiver::Call {
                function: function.to_string(),
                on: on.map(Box::new),
                result,
            })
        };
        assert_eq!(
            found
                .into_iter()
                .map(|u| (u.line, u.receiver))
                .collect::<Vec<_>>(),
            vec![
                (5, call("mux.NewRouter", None, 0)),
                (6, call("NewRouter", None, 0)),
                (8, call("open", None, 1)),
                (9, call("Route", Some(Receiver::Typed("Server".into())), 0)),
                (10, call("open", None, 0)),
                // The closure's own `r` inside it; the function's `s` seen from it.
                (12, Some(Receiver::Typed("Request".into()))),
                (13, Some(Receiver::Typed("Server".into()))),
                (15, Some(Receiver::Typed("Route".into()))),
            ]
        );
    }
}
