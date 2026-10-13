//! What a C# member body and signature say about calls, receivers, parameters and the types
//! they write (#682): the readers [`super::NameReader`] calls while it walks a file.
//!
//! Each reader looks at one node and what it holds, never at what a name means: which type
//! `Entry` names, which `Save` a call reaches and which variable a receiver is are decided by the
//! C# relationship pass in ingest, from these facts and the name lookup.

use super::{has_modifier, squeeze, type_arity, type_parameter_count};
use open_kioku_core::csharp::{
    CSharpCallKind, CSharpMemberKind, CSharpParameter, CSharpReceiver, CSharpReceiverRoot,
    CSharpTypeRef, CSharpTypeSegment, CSharpTypeShape,
};
use open_kioku_core::SourceRange;
use tree_sitter::Node;

/// The member a declaration node declares, for the members a call can name or a body sits in.
pub(super) fn member_kind(node: Node<'_>) -> Option<CSharpMemberKind> {
    Some(match node.kind() {
        "method_declaration" => CSharpMemberKind::Method,
        "constructor_declaration" => CSharpMemberKind::Constructor,
        "destructor_declaration" => CSharpMemberKind::Destructor,
        "operator_declaration" | "conversion_operator_declaration" => CSharpMemberKind::Operator,
        "property_declaration" => CSharpMemberKind::Property,
        "indexer_declaration" => CSharpMemberKind::Indexer,
        "event_declaration" => CSharpMemberKind::Event,
        "local_function_statement" => CSharpMemberKind::LocalFunction,
        "parameter" if super::is_record_parameter(node) => CSharpMemberKind::Property,
        "variable_declarator" => {
            let declaration = super::declaration_of(node);
            match declaration.kind() {
                "field_declaration" => CSharpMemberKind::Field,
                "event_field_declaration" => CSharpMemberKind::Event,
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Whether a member declaration is `static`, read from its own modifiers (a field's from its
/// declaration).
pub(super) fn is_static(node: Node<'_>) -> bool {
    let declaration = super::declaration_of(node);
    has_modifier(declaration, "static") || has_modifier(declaration, "const")
}

pub(super) fn is_override(node: Node<'_>) -> bool {
    let declaration = super::declaration_of(node);
    has_modifier(declaration, "override") || has_modifier(declaration, "new")
}

/// Whether a member declaration has a body: a block or an expression body.
pub(super) fn has_body(declaration: Node<'_>) -> bool {
    if declaration.child_by_field_name("body").is_some() {
        return true;
    }
    let mut cursor = declaration.walk();
    let found = declaration
        .children(&mut cursor)
        .any(|child| matches!(child.kind(), "block" | "arrow_expression_clause"));
    found
}

/// The number of type parameters a method or local function declares.
pub(super) fn generic_arity(node: Node<'_>) -> u16 {
    type_parameter_count(node)
}

/// The type a member holds or returns: a method's return type (`None` for `void`), a property's,
/// indexer's or event's type, a field declarator's declared type.
pub(super) fn value_type(node: Node<'_>, source: &[u8]) -> Option<CSharpTypeRef> {
    let written = match node.kind() {
        "method_declaration" => node.child_by_field_name("returns"),
        "variable_declarator" => node
            .parent()
            .filter(|declaration| declaration.kind() == "variable_declaration")
            .and_then(|declaration| declaration.child_by_field_name("type")),
        // A positional record parameter is a property of its type.
        "parameter" => node.child_by_field_name("type"),
        "property_declaration"
        | "indexer_declaration"
        | "event_declaration"
        | "local_function_statement"
        | "operator_declaration"
        | "conversion_operator_declaration" => node.child_by_field_name("type"),
        _ => None,
    }?;
    if written.kind() == "predefined_type" && text(written, source) == "void" {
        return None;
    }
    type_ref(written, source)
}

/// The parameter list of a member, local function, lambda, anonymous method, delegate or a
/// type's primary constructor.
pub(super) fn parameter_list(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("parameters").or_else(|| {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "parameter_list" | "bracketed_parameter_list"));
        found
    })
}

/// The parameters a list declares. This grammar reads a `params` parameter as loose children of
/// the list (`params`, a `type` and a `name` field) rather than as a `parameter` node.
pub(super) fn parameters(list: Node<'_>, source: &[u8]) -> Vec<CSharpParameter> {
    let mut parameters = Vec::new();
    let mut pending: Option<CSharpParameter> = None;
    let mut cursor = list.walk();
    for (index, child) in list.children(&mut cursor).enumerate() {
        let field = u32::try_from(index)
            .ok()
            .and_then(|index| list.field_name_for_child(index));
        match (child.kind(), field) {
            ("parameter", _) => parameters.push(parameter(child, source)),
            ("params", None) => {
                pending = Some(CSharpParameter {
                    name: String::new(),
                    value_type: None,
                    is_this: false,
                    is_params: true,
                    optional: false,
                })
            }
            (_, Some("type")) => {
                if let Some(pending) = pending.as_mut() {
                    pending.value_type = type_ref(child, source);
                }
            }
            (_, Some("name")) => {
                if let Some(mut done) = pending.take() {
                    done.name = text(child, source);
                    parameters.push(done);
                }
            }
            _ => {}
        }
    }
    parameters
}

fn parameter(node: Node<'_>, source: &[u8]) -> CSharpParameter {
    let mut cursor = node.walk();
    let mut is_this = false;
    let mut is_params = false;
    let mut optional = false;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "modifier" => match text(child, source).as_str() {
                "this" => is_this = true,
                "params" => is_params = true,
                _ => {}
            },
            "=" => optional = true,
            _ => {}
        }
    }
    CSharpParameter {
        name: node
            .child_by_field_name("name")
            .map(|name| text(name, source))
            .unwrap_or_default(),
        value_type: node
            .child_by_field_name("type")
            .and_then(|written| type_ref(written, source)),
        is_this,
        is_params,
        optional,
    }
}

/// A type as written. `None` for `var` and for a node that is no type.
pub(super) fn type_ref(node: Node<'_>, source: &[u8]) -> Option<CSharpTypeRef> {
    let other = |arguments: Vec<CSharpTypeRef>| CSharpTypeRef {
        path: Vec::new(),
        global: false,
        extern_alias: false,
        arguments,
        shape: CSharpTypeShape::Other,
    };
    match node.kind() {
        "implicit_type" => None,
        "predefined_type" => Some(CSharpTypeRef {
            path: Vec::new(),
            global: false,
            extern_alias: false,
            arguments: Vec::new(),
            shape: CSharpTypeShape::Predefined,
        }),
        "identifier" | "generic_name" | "qualified_name" | "alias_qualified_name" => {
            let mut written = CSharpTypeRef {
                path: Vec::new(),
                global: false,
                extern_alias: false,
                arguments: Vec::new(),
                shape: CSharpTypeShape::Named,
            };
            name_path(node, source, &mut written).then_some(written)
        }
        "nullable_type" => {
            let inner = type_ref(node.child_by_field_name("type")?, source)?;
            Some(match inner.shape {
                CSharpTypeShape::Named => CSharpTypeRef {
                    shape: CSharpTypeShape::Nullable,
                    ..inner
                },
                _ => inner,
            })
        }
        "ref_type" | "scoped_type" => type_ref(node.child_by_field_name("type")?, source),
        "array_type" | "pointer_type" => Some(other(
            node.child_by_field_name("type")
                .and_then(|element| type_ref(element, source))
                .into_iter()
                .collect(),
        )),
        "tuple_type" => {
            let mut cursor = node.walk();
            let elements = node
                .named_children(&mut cursor)
                .filter_map(|element| element.child_by_field_name("type"))
                .filter_map(|element| type_ref(element, source))
                .collect();
            Some(other(elements))
        }
        _ => Some(other(Vec::new())),
    }
}

/// Appends the segments and type arguments of a name to `written`; `false` when the node is no
/// name this reads.
fn name_path(node: Node<'_>, source: &[u8], written: &mut CSharpTypeRef) -> bool {
    match node.kind() {
        "identifier" => {
            written.path.push(CSharpTypeSegment {
                name: text(node, source),
                arity: 0,
            });
            true
        }
        "generic_name" => {
            let mut cursor = node.walk();
            let Some(identifier) = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
            else {
                return false;
            };
            written.path.push(CSharpTypeSegment {
                name: text(identifier, source),
                arity: type_arity(node),
            });
            let mut cursor = node.walk();
            if let Some(arguments) = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "type_argument_list")
            {
                let mut cursor = arguments.walk();
                written.arguments.extend(
                    arguments
                        .named_children(&mut cursor)
                        .filter_map(|argument| type_ref(argument, source)),
                );
            }
            true
        }
        "qualified_name" => {
            let (Some(qualifier), Some(name)) = (
                node.child_by_field_name("qualifier"),
                node.child_by_field_name("name"),
            ) else {
                return false;
            };
            name_path(qualifier, source, written) && name_path(name, source, written)
        }
        "alias_qualified_name" => {
            let (Some(alias), Some(name)) = (
                node.child_by_field_name("alias"),
                node.child_by_field_name("name"),
            ) else {
                return false;
            };
            if text(alias, source) == "global" {
                written.global = true;
            } else {
                written.extern_alias = true;
            }
            name_path(name, source, written)
        }
        _ => false,
    }
}

/// One segment a receiver or a call names: an identifier, or a generic name with its arity.
fn segment(node: Node<'_>, source: &[u8]) -> Option<CSharpTypeSegment> {
    match node.kind() {
        "identifier" => Some(CSharpTypeSegment {
            name: text(node, source),
            arity: 0,
        }),
        "generic_name" => {
            let mut cursor = node.walk();
            let identifier = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")?;
            Some(CSharpTypeSegment {
                name: text(identifier, source),
                arity: type_arity(node),
            })
        }
        _ => None,
    }
}

/// What an invocation's `function` names: the method, the type arguments written on it and its
/// receiver. `None` for a call of anything but a name or a member access (a delegate a call
/// returns, an element access).
pub(super) fn invoked(
    function: Node<'_>,
    source: &[u8],
) -> Option<(String, Option<u16>, CSharpReceiver)> {
    let named = |name: Node<'_>| -> Option<(String, Option<u16>)> {
        let found = segment(name, source)?;
        let arguments = (name.kind() == "generic_name").then_some(found.arity);
        Some((found.name, arguments))
    };
    match function.kind() {
        "identifier" | "generic_name" => {
            let (name, arguments) = named(function)?;
            Some((name, arguments, CSharpReceiver::default()))
        }
        "member_access_expression" => {
            let (name, arguments) = named(function.child_by_field_name("name")?)?;
            let receiver = receiver(function.child_by_field_name("expression")?, source);
            Some((name, arguments, receiver))
        }
        "conditional_access_expression" => {
            let (name, arguments) = named(bound_name(function)?)?;
            let receiver = receiver(function.child_by_field_name("condition")?, source);
            Some((name, arguments, receiver))
        }
        _ => None,
    }
}

/// The name `?.Name` binds in a conditional access, when it binds one.
fn bound_name(access: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = access.walk();
    let binding = access
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "member_binding_expression")
        .last()?;
    binding.child_by_field_name("name")
}

/// What a receiver expression is: a root and the members accessed after it.
pub(super) fn receiver(node: Node<'_>, source: &[u8]) -> CSharpReceiver {
    let unknown = CSharpReceiver {
        root: CSharpReceiverRoot::Unknown,
        members: Vec::new(),
    };
    let rooted = |root: CSharpReceiverRoot| CSharpReceiver {
        root,
        members: Vec::new(),
    };
    let typed =
        |written: Option<Node<'_>>| match written.and_then(|written| type_ref(written, source)) {
            Some(written)
                if matches!(
                    written.shape,
                    CSharpTypeShape::Named | CSharpTypeShape::Nullable
                ) =>
            {
                rooted(CSharpReceiverRoot::Typed(written))
            }
            _ => unknown.clone(),
        };
    let accessed = |inner: Node<'_>, member: Option<Node<'_>>| {
        let mut found = receiver(inner, source);
        match member.and_then(|member| segment(member, source)) {
            Some(member) if found.root != CSharpReceiverRoot::Unknown => {
                found.members.push(member);
                found
            }
            _ => unknown.clone(),
        }
    };
    match node.kind() {
        "identifier" | "generic_name" => match segment(node, source) {
            Some(found) => rooted(CSharpReceiverRoot::Name(found)),
            None => unknown,
        },
        "this" => rooted(CSharpReceiverRoot::This),
        "base" => rooted(CSharpReceiverRoot::Base),
        "member_access_expression" => match node.child_by_field_name("expression") {
            Some(inner) => accessed(inner, node.child_by_field_name("name")),
            None => unknown,
        },
        "conditional_access_expression" => match node.child_by_field_name("condition") {
            Some(inner) => accessed(inner, bound_name(node)),
            None => unknown,
        },
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).next();
            match inner {
                Some(inner) => receiver(inner, source),
                None => unknown,
            }
        }
        "cast_expression" => typed(node.child_by_field_name("type")),
        "as_expression" => typed(node.child_by_field_name("right")),
        "object_creation_expression" => typed(node.child_by_field_name("type")),
        _ => unknown,
    }
}

/// The number of arguments an invocation, creation or constructor initializer passes.
pub(super) fn argument_count(node: Node<'_>) -> u16 {
    let list = node.child_by_field_name("arguments").or_else(|| {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "argument_list");
        found
    });
    let count = list.map_or(0, |list| {
        let mut cursor = list.walk();
        let count = list
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "argument")
            .count();
        count
    });
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// The kind of a constructor initializer: `: base(..)` or `: this(..)`.
pub(super) fn initializer_kind(node: Node<'_>) -> Option<CSharpCallKind> {
    let mut cursor = node.walk();
    let kind = node
        .children(&mut cursor)
        .find_map(|child| match child.kind() {
            "base" => Some(CSharpCallKind::BaseConstructor),
            "this" => Some(CSharpCallKind::ThisConstructor),
            _ => None,
        });
    kind
}

/// The type a target-typed `new(..)` creates, when the declaration it initializes writes one:
/// a local, field or property of a declared type (`Entry e = new();`). Anywhere else the target
/// type comes from what the reader does not track.
pub(super) fn target_type(creation: Node<'_>, source: &[u8]) -> Option<CSharpTypeRef> {
    let parent = creation.parent()?;
    let declared = match parent.kind() {
        "variable_declarator" => parent
            .parent()
            .filter(|declaration| declaration.kind() == "variable_declaration")?
            .child_by_field_name("type")?,
        "property_declaration" if parent.child_by_field_name("value") == Some(creation) => {
            parent.child_by_field_name("type")?
        }
        _ => return None,
    };
    let declared = type_ref(declared, source)?;
    matches!(
        declared.shape,
        CSharpTypeShape::Named | CSharpTypeShape::Nullable
    )
    .then_some(declared)
}

/// The type an initializer gives a `var`: `new T(..)`, `(T)x` or `x as T`. Anything else, a
/// call's result included, says nothing the reader can read.
pub(super) fn initializer_type(value: Node<'_>, source: &[u8]) -> Option<CSharpTypeRef> {
    let written = match value.kind() {
        "object_creation_expression" | "cast_expression" => value.child_by_field_name("type")?,
        "as_expression" => value.child_by_field_name("right")?,
        "parenthesized_expression" => {
            let mut cursor = value.walk();
            let inner = value.named_children(&mut cursor).next()?;
            return initializer_type(inner, source);
        }
        _ => return None,
    };
    let written = type_ref(written, source)?;
    matches!(
        written.shape,
        CSharpTypeShape::Named | CSharpTypeShape::Nullable
    )
    .then_some(written)
}

/// The value a variable declarator initializes it with.
pub(super) fn declarator_value(declarator: Node<'_>) -> Option<Node<'_>> {
    if let Some(value) = declarator.child_by_field_name("value") {
        return Some(value);
    }
    // `name = value`: the value is the named child after the `=`.
    let mut cursor = declarator.walk();
    let mut after_equals = false;
    for child in declarator.children(&mut cursor) {
        if child.kind() == "=" {
            after_equals = true;
        } else if after_equals && child.is_named() {
            return Some(child);
        }
    }
    None
}

/// The node a variable declared at `node` is in scope in: the nearest block, switch section,
/// `for`, `foreach`, `using`, `fixed` or `catch` around it, or the lambda, local function,
/// member or type declaring it. An expression variable of an `if` condition is in the block
/// around the `if`, as C# scopes it.
pub(super) fn scope_of(node: Node<'_>) -> Node<'_> {
    let mut current = node.parent();
    let mut last = node;
    while let Some(ancestor) = current {
        if matches!(
            ancestor.kind(),
            "block"
                | "switch_section"
                | "for_statement"
                | "foreach_statement"
                | "using_statement"
                | "fixed_statement"
                | "catch_clause"
                | "lambda_expression"
                | "anonymous_method_expression"
                | "local_function_statement"
                | "method_declaration"
                | "constructor_declaration"
                | "destructor_declaration"
                | "operator_declaration"
                | "conversion_operator_declaration"
                | "accessor_declaration"
                | "property_declaration"
                | "indexer_declaration"
                | "global_statement"
                | "compilation_unit"
        ) || super::is_type_declaration(ancestor.kind())
        {
            return ancestor;
        }
        last = ancestor;
        current = ancestor.parent();
    }
    last
}

pub(super) fn source_range(node: Node<'_>) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start_line: start.row as u32 + 1,
        start_column: start.column as u32 + 1,
        end_line: end.row as u32 + 1,
        end_column: end.column as u32 + 1,
    }
}

fn text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).map(squeeze).unwrap_or_default()
}
