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
    /// The members the file declares (#682): methods, constructors, a type's primary
    /// constructor, properties, indexers, events, fields, operators and local functions, with
    /// what overload resolution and a member access's type need.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<CSharpMember>,
    /// The calls the file writes outside error nodes: invocations, object creations and
    /// constructor initializers (#682).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CSharpCall>,
    /// Every parameter, local, pattern, `out`, `foreach`, `catch` and `using` variable the file
    /// declares, with its type when the declaration says it and where its name is in scope
    /// (#682).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<CSharpLocal>,
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
    /// What the declaration declares: a class, struct, record, interface, enum or delegate.
    #[serde(default)]
    pub kind: CSharpTypeKind,
    /// Declared `static` (a part of a partial type may carry it alone).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    /// Every type its base list names, in order and whole (`Acme.Ledger.Base<Entry>`), read in
    /// the scope the declaration stands in (#682). A C# base list does not say which entry is a
    /// class and which an interface; the type an entry names does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_types: Vec<CSharpTypeRef>,
    /// Where the base list is written: in the scope the declaration stands in, outside the
    /// type itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_place: Option<CSharpPlace>,
}

/// What a type declaration declares.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CSharpTypeKind {
    #[default]
    Class,
    Struct,
    /// `record` or `record class`.
    Record,
    RecordStruct,
    Interface,
    Enum,
    Delegate,
}

impl CSharpTypeKind {
    /// A value type: what it holds is no reference, and `T?` of it is `Nullable<T>`.
    pub fn is_value_type(self) -> bool {
        matches!(self, Self::Struct | Self::RecordStruct | Self::Enum)
    }
}

/// A type as written: `Acme.Ledger.Entry<Item>`, `Entry[]`, `Entry?`, `(Entry, int)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CSharpTypeRef {
    /// The segments of the name as written, outermost first, each with the number of type
    /// arguments written on it: `Acme`, `Ledger`, `Entry<1>`. Empty for a predefined type
    /// (`int`, `string`), an array, a tuple or a pointer, whose element types are `arguments`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<CSharpTypeSegment>,
    /// The name starts at `global::`: no enclosing namespace or `using` takes part.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub global: bool,
    /// The name starts at an extern alias: a type of an assembly the project references.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extern_alias: bool,
    /// The type arguments written on any segment, and the element types of an array, tuple,
    /// pointer or nullable value: each is a type the declaration uses too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<CSharpTypeRef>,
    #[serde(default)]
    pub shape: CSharpTypeShape,
}

/// One segment of a written type or receiver name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CSharpTypeSegment {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub arity: u16,
}

/// How a written type holds the type its path names.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CSharpTypeShape {
    /// The value is of the type the path names.
    #[default]
    Named,
    /// `T?`: the type the path names, or `Nullable<T>` when that is a value type.
    Nullable,
    /// A predefined type (`int`, `string`, `object`), never a type of the repository.
    Predefined,
    /// An array, a pointer, a tuple, a function pointer or a type the reader does not take
    /// apart: no member of a repository type is reached through it.
    Other,
}

/// Where a declaration or use is written, for the name lookup of the types it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpPlace {
    /// The innermost namespace block (see [`CSharpNameFacts::blocks`]).
    pub block: u32,
    /// The innermost type declaration, as an index into [`CSharpNameFacts::types`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing: Option<u32>,
    pub line: u32,
    /// Inside an `#if`, `#elif` or `#else` region.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conditional: bool,
}

/// What kind of member a [`CSharpMember`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CSharpMemberKind {
    Method,
    Constructor,
    /// The parameter list written on a record, class or struct declaration; the member's
    /// symbol is the type's own.
    PrimaryConstructor,
    Property,
    Indexer,
    Event,
    Field,
    Operator,
    Destructor,
    LocalFunction,
}

impl CSharpMemberKind {
    /// Invoked by a call that names it: a method or a local function.
    pub fn is_method(self) -> bool {
        matches!(self, Self::Method | Self::LocalFunction)
    }

    /// Holds a value of its declared type: a property, field or event.
    pub fn holds_value(self) -> bool {
        matches!(self, Self::Property | Self::Field | Self::Event)
    }
}

/// One member declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpMember {
    pub symbol_id: SymbolId,
    pub kind: CSharpMemberKind,
    /// The name calls and member accesses write: a method's or property's name, the type's
    /// name for a constructor.
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    /// Declared `override` or `new`: it takes the place of, or hides, a base type's member of
    /// its signature.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_override: bool,
    /// Written with a body (`{ .. }` or `=> ..`). A `partial` method declared without one is
    /// implemented by the part that has it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_body: bool,
    /// The number of type parameters a generic method declares.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub type_parameters: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<CSharpParameter>,
    /// The return type of a method, local function or operator, the type of a property,
    /// indexer, field or event; `None` for `void`, a constructor or `var`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_type: Option<CSharpTypeRef>,
    /// Where the signature's types are looked up: the declaring type's scope.
    pub place: CSharpPlace,
}

/// One parameter of a member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpParameter {
    pub name: String,
    /// `None` for a parameter written without a type (`__arglist`) or one the reader cannot
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_type: Option<CSharpTypeRef>,
    /// The `this` parameter of an extension method.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_this: bool,
    /// A `params` parameter, which takes any number of arguments.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_params: bool,
    /// Has a default value, so an argument may be left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// What a call does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CSharpCallKind {
    /// `M(..)`, `x.M(..)`, `x?.M(..)`.
    Invocation,
    /// `new T(..)`, or a target-typed `new(..)` whose type the declaration states.
    Creation,
    /// `: base(..)` on a constructor.
    BaseConstructor,
    /// `: this(..)` on a constructor.
    ThisConstructor,
}

/// One call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpCall {
    /// The member the call is written in, as an index into [`CSharpNameFacts::members`]: the
    /// innermost method, constructor, accessor's property, operator or local function, or the
    /// field whose initializer holds it. `None` outside every member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<u32>,
    pub kind: CSharpCallKind,
    /// The method's name for an invocation; empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The number of type arguments written on the method's name (`M<int>(..)`), when any are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_arguments: Option<u16>,
    pub arguments: u16,
    /// What an invocation is called on.
    #[serde(default, skip_serializing_if = "CSharpReceiver::is_none")]
    pub receiver: CSharpReceiver,
    /// The type a creation creates: written after `new`, or the declared type of the variable,
    /// field or property a target-typed `new(..)` initializes. `None` when the reader cannot
    /// tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<CSharpTypeRef>,
    pub range: crate::SourceRange,
    pub place: CSharpPlace,
}

/// What an invocation is called on: a root, then member accesses written after it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpReceiver {
    pub root: CSharpReceiverRoot,
    /// The member names accessed after the root, in order: `store` and `Default` of
    /// `this.store.Default.Save()`, or `Ledger` and `Rules` of `Acme.Ledger.Rules.Apply()`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<CSharpTypeSegment>,
}

impl CSharpReceiver {
    pub fn is_none(&self) -> bool {
        self.root == CSharpReceiverRoot::None && self.members.is_empty()
    }
}

/// Where a receiver starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CSharpReceiverRoot {
    /// A bare `M(..)`.
    #[default]
    None,
    This,
    Base,
    /// A simple name: a local, a member, a type or a namespace.
    Name(CSharpTypeSegment),
    /// A value of a written type: `new T(..)`, `(T)x`, `x as T`.
    Typed(CSharpTypeRef),
    /// Anything else: a call's result, an element access, a literal.
    Unknown,
}

/// What declares a [`CSharpLocal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CSharpLocalKind {
    /// A parameter of a member, a local function or a primary constructor.
    Parameter,
    /// A parameter of a lambda or anonymous method.
    LambdaParameter,
    /// A local, pattern, `out`, `foreach`, `catch`, `using` or query variable.
    Variable,
}

/// One variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CSharpLocal {
    pub name: String,
    pub kind: CSharpLocalKind,
    /// Its type: as declared, or for `var` the type `new T(..)`, `(T)x` or `x as T` gives it.
    /// `None` when neither says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_type: Option<CSharpTypeRef>,
    /// The type is the declaration's own (`Entry e`), not one an initializer gives `var`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declared: bool,
    /// The member it belongs to, as an index into [`CSharpNameFacts::members`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<u32>,
    /// Where the name is in scope: the block, statement, lambda or member that declares it.
    pub scope: crate::SourceRange,
    pub place: CSharpPlace,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}
