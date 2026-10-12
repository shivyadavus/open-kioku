//! What a C# file writes about the names it uses, read by the parser for the import resolver
//! (#681).
//!
//! A C# `using` names a namespace, never a file: `using Acme.Ledger;` makes every type of that
//! namespace nameable, wherever the repository declares it. Which file a `using` leads to is
//! therefore a question of which of those types the file names, and C#'s name lookup decides that:
//! from the innermost namespace a name is written in out to the global namespace, the types of
//! each namespace come before the `using` directives of its declaration. These facts keep what
//! that lookup reads and nothing else: the file's namespace declarations, its directives, the
//! type names it writes and where, and the declarations that can shadow them.

use crate::SymbolId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The C# name facts of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpNameFacts {
    /// The compilation unit, always first, then each namespace declaration in source order. A
    /// file-scoped namespace (`namespace Acme.Ledger;`) is a block holding the rest of the file.
    pub blocks: Vec<CSharpNamespaceBlock>,
    /// The file's `using` directives, `global using` included.
    pub usings: Vec<CSharpUsing>,
    /// The names `extern alias` directives declare.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extern_aliases: Vec<String>,
    /// The type names the file writes outside its `using` directives, once per block, enclosing
    /// type, spelling and context, at the first line each is written on.
    pub uses: Vec<CSharpNameUse>,
    /// The type declarations of the file, with what a name lookup needs beside the symbol.
    pub types: Vec<CSharpTypeDeclaration>,
    /// Every type parameter name the file declares, on a type, method, delegate or local
    /// function. A type name spelled like one of them may be that parameter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_parameters: Vec<String>,
    /// Every local, parameter, pattern, range and loop variable name the file declares. A
    /// member-access receiver spelled like one of them may be that variable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<String>,
    /// The assemblies an `[assembly: InternalsVisibleTo("Name")]` attribute of the file names,
    /// without a public key: they may name the project's `internal` types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub internals_visible_to: Vec<String>,
    /// The file parsed with syntax errors. What is read outside its error nodes is exact; what
    /// they hold is read only as far as `hidden_types` and `hidden_directives` say.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub syntax_errors: bool,
    /// The types an error node may hide, each once: every name its text spells after `class`,
    /// `struct`, `interface`, `enum`, `record` or `delegate`, in the namespace the node stands in
    /// and in each namespace its text declares (nested ones joined to their outer ones). A lookup
    /// of that name that passes through that namespace proves nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_types: Vec<CSharpHiddenType>,
    /// An error node in the compilation unit or a namespace body spells `using`: a directive of
    /// the file may be missing, so no lookup in the file proves anything.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden_directives: bool,
}

/// A type name an error node may declare, and a namespace it may be declared in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CSharpHiddenType {
    pub namespace: String,
    pub name: String,
}

/// One namespace declaration, or the compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpNamespaceBlock {
    /// The whole namespace the block declares, dotted: `Acme.Ledger` for `namespace Ledger`
    /// written inside `namespace Acme`. Empty for the compilation unit.
    pub namespace: String,
    /// The index of the enclosing block; `None` for the compilation unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
}

/// What a `using` directive brings into scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CSharpUsingKind {
    /// `using Acme.Ledger;`: the types of a namespace.
    Namespace,
    /// `using static Acme.Ledger.Rules;`: the static members and nested types of a type.
    Static,
    /// `using Rules = Acme.Ledger.Rules;`: one name for a namespace or a type.
    Alias,
}

/// One `using` directive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpUsing {
    /// The block whose declaration holds the directive; a `global using` is always in the
    /// compilation unit and applies to every file of its project.
    pub block: u32,
    pub kind: CSharpUsingKind,
    /// The dotted name the directive names, without `global::`, an extern alias or generic
    /// arguments: `Acme.Ledger`, `System.Collections.Generic.List`.
    pub target: String,
    /// The number of generic arguments written on the name's last segment.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub arity: u16,
    /// The name an alias directive declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub global: bool,
    /// The extern alias the name starts from (`Legacy::Acme.Old`): an assembly the project
    /// references under that alias, not a namespace of the repository's own types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extern_alias: Option<String>,
    pub line: u32,
    /// Written inside an `#if`, `#elif` or `#else` region: a build configuration decides
    /// whether it is compiled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
}

/// Where a type name is written, which decides what else its spelling may name.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CSharpNameContext {
    /// Where only a namespace or a type can stand: a declared, parameter, return, base, cast,
    /// pattern, `typeof`, `new` or generic argument type, or a constraint.
    Type,
    /// An attribute's name, which names the type `Name` or `NameAttribute`.
    Attribute,
    /// The receiver of a member access (`Rules.Apply()`), which a variable or an inherited
    /// member of the same name takes before any type.
    Receiver,
}

/// One type name a file writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpNameUse {
    /// The innermost block the name is written in.
    pub block: u32,
    /// The innermost type declaration the name is written in, whose nested types it may name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_type: Option<SymbolId>,
    /// The first segment as written: `Rules` of `Rules`, `Rules.Kind` or `Rules<T>.Kind`.
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub arity: u16,
    /// The segment after the first, when there is one: what an alias of a namespace names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next_arity: u16,
    pub context: CSharpNameContext,
    pub line: u32,
    /// Written inside an `#if`, `#elif` or `#else` region: a build configuration decides
    /// whether it is compiled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
}

/// One type declaration (class, struct, record, interface, enum or delegate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpTypeDeclaration {
    pub symbol_id: SymbolId,
    /// The number of type parameters it declares: `Entry` and `Entry<T>` are different types.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub arity: u16,
    /// The types its base list names, in order, read in the scope the declaration stands in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<CSharpNameUse>,
    /// Written inside an `#if`, `#elif` or `#else` region: a build configuration decides
    /// whether it is compiled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}
