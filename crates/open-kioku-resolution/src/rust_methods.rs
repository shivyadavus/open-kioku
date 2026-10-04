//! What a Rust method call reaches through what its receiver's declared type says beyond the
//! type's name (#639): a `type` alias stands for the type it names, `Box`, `Rc` and `Arc`
//! dereference to the type they hold, and an inherent method takes precedence over a trait
//! method of the same name, as rustc's method probe orders them.

use crate::context::{ResolutionContext, ScopedImport};
use crate::index::ScopeIndex;
use crate::typed_calls::{
    collect_type_candidate_set, rust_annotated_receiver_type, TypeCandidates,
};
use open_kioku_core::{FileId, ScopeId, Symbol, SymbolId, SymbolKind};
use open_kioku_semantic_model::{ImportBindingRule, SemanticRepository};
use std::collections::HashSet;

/// How many aliases and smart pointers a type is read through before the read gives up.
const MAX_TYPE_READ_DEPTH: usize = 8;

/// The Rust files where a trait the index does not know may be in scope (see
/// [`ScopeIndex::rust_trait_scope_is_open`]): a file with an import that names neither an item
/// or module of the repository nor a path of the standard library, or with a glob other than a
/// `use super::*;` written in an inline `mod` block, whose parent's imports are the file's own.
pub fn rust_open_trait_scope_files(
    repository: &SemanticRepository,
    scopes: &ScopeIndex,
    rust_files: &HashSet<FileId>,
) -> HashSet<FileId> {
    let mut open = HashSet::new();
    for binding in repository.imports.by_file_local_name.values().flatten() {
        if !rust_files.contains(&binding.file_id) || open.contains(&binding.file_id) {
            continue;
        }
        let source = binding.source_module.trim();
        let source = source.strip_prefix("::").unwrap_or(source);
        let first = source.split("::").next().unwrap_or_default().trim();
        let standard = matches!(first, "std" | "core" | "alloc");
        let closed = if binding.is_glob {
            standard
                || (source == "super::*"
                    && scopes
                        .get(&binding.scope_id)
                        .is_some_and(|scope| scope.kind == open_kioku_core::ScopeKind::Module))
        } else {
            standard
                || binding.target_symbol.is_some()
                || binding.configured_targets.is_some()
                || (binding.target_file.is_some()
                    && matches!(
                        binding.rule,
                        ImportBindingRule::RustModulePath | ImportBindingRule::RustReexport
                    ))
        };
        if !closed {
            open.insert(binding.file_id.clone());
        }
    }
    open
}

/// How a Rust receiver's place is reached, which decides the order rustc's method probe tries
/// receivers in: the place itself, then a reference to it, then a mutable one, each seen
/// through one more dereference only after all three fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustReceiverForm {
    /// A place of the type itself: `s: Store`, or what a smart pointer dereferences to.
    Place,
    /// A shared reference to it: `s: &'a Store`.
    Shared,
    /// A mutable reference to it: `s: &'a mut Store`.
    Mutable,
}

/// What a Rust type written in a declaration reaches for method calls (#639).
pub(crate) struct RustReadType {
    /// The types it names once aliases and smart pointers are read through.
    pub(crate) found: TypeCandidates,
    /// The aliases it was read through: a method an `impl` of one declares is a method of the
    /// type it stands for.
    pub(crate) aliases: Vec<SymbolId>,
    /// Read through `Box`, `Rc` or `Arc`.
    pub(crate) dereferenced: bool,
    /// How the type it names is reached, `None` through more than one reference.
    pub(crate) form: Option<RustReceiverForm>,
}

/// Reads the Rust type `written` at `scope` of `file`, the file declaring it: a `Box`, `Rc` or
/// `Arc` of the standard library is read as the type it holds, and an alias whose type is one
/// type of the repository as that type, read where the alias is declared. Any other type is
/// looked up as a receiver's type is. `None` for a form a receiver type never has.
pub(crate) fn rust_read_declared_type(
    ctx: &ResolutionContext<'_>,
    (file, scope): (&FileId, &ScopeId),
    written: &str,
) -> Option<RustReadType> {
    let mut read = RustReadType {
        found: TypeCandidates {
            targets: Vec::new(),
            configured: None,
        },
        aliases: Vec::new(),
        dereferenced: false,
        form: Some(RustReceiverForm::Place),
    };
    let mut at = (file.clone(), scope.clone());
    let mut written = written.trim().to_string();
    let mut references = Vec::new();
    for _ in 0..MAX_TYPE_READ_DEPTH {
        let declaring = declaring_context(ctx, &at.0);
        references.extend(rust_reference_prefix(&written));
        if let Some(inner) = rust_smart_pointer_inner(&declaring, &at.1, &written) {
            // Method calls see through the pointer: the form is that of the place it holds.
            references.clear();
            read.dereferenced = true;
            written = inner.to_string();
            continue;
        }
        let plain = rust_annotated_receiver_type(&written)?;
        let found = collect_type_candidate_set(&declaring, &at.1, plain);
        let alias = match (found.configured.as_ref(), found.targets.as_slice()) {
            (None, [(target, _)]) => ctx
                .scopes
                .rust_alias_target(target)
                .and_then(|aliased| Some((ctx.symbols.get(target)?, aliased))),
            _ => None,
        };
        let Some((alias, aliased)) = alias else {
            read.found = found;
            read.form = match references.as_slice() {
                [] => Some(RustReceiverForm::Place),
                [false] => Some(RustReceiverForm::Shared),
                [true] => Some(RustReceiverForm::Mutable),
                _ => None,
            };
            return Some(read);
        };
        read.aliases.push(alias.id.clone());
        at = (alias.file_id.clone(), alias.scope_id.clone()?);
        written = aliased.to_string();
    }
    None
}

/// The types of the repository that Rust aliases among `types` stand for, each read where its
/// alias is declared, through aliases of aliases: only an alias of one type, written without
/// a smart pointer, reaches one.
pub(crate) fn rust_aliased_types(ctx: &ResolutionContext<'_>, types: &[SymbolId]) -> Vec<SymbolId> {
    let mut reached = Vec::new();
    for alias in types {
        let Some(symbol) = ctx.symbols.get(alias) else {
            continue;
        };
        let (Some(aliased), Some(scope)) = (
            ctx.scopes.rust_alias_target(alias),
            symbol.scope_id.as_ref(),
        ) else {
            continue;
        };
        let Some(read) = rust_read_declared_type(ctx, (&symbol.file_id, scope), aliased) else {
            continue;
        };
        if read.dereferenced || read.found.configured.is_some() {
            continue;
        }
        if let [(target, _)] = read.found.targets.as_slice() {
            reached.push(target.clone());
            reached.extend(read.aliases);
        }
    }
    reached
}

/// A context that reads names where `file` declares them. Only type lookup runs in it, which
/// records no evidence, so the caller's path is kept.
fn declaring_context<'a>(ctx: &ResolutionContext<'a>, file: &'a FileId) -> ResolutionContext<'a> {
    ResolutionContext::new(
        file,
        ctx.file_path,
        None,
        ctx.language.clone(),
        ctx.repository,
        ctx.symbols,
        ctx.scopes,
        ctx.bindings,
        ctx.inheritance,
        ctx.semantics,
    )
}

/// The references `written` starts with, each `true` when mutable: `&'a mut &S` is
/// `[true, false]`.
fn rust_reference_prefix(written: &str) -> Vec<bool> {
    let mut references = Vec::new();
    let mut rest = written.trim();
    while let Some(referent) = rest.strip_prefix('&') {
        rest = referent.trim_start();
        if let Some(lifetime) = rest.strip_prefix('\'') {
            let end = lifetime.find(char::is_whitespace).unwrap_or(lifetime.len());
            rest = lifetime[end..].trim_start();
        }
        let mutable = rest.starts_with("mut ");
        if mutable {
            rest = rest[4..].trim_start();
        }
        references.push(mutable);
    }
    references
}

/// The type a `Box`, `Rc` or `Arc` of the standard library holds, when `written`, seen through
/// references, is one: `Arc<Inner>` is `Inner`. The pointer is named by a path of the standard
/// library (`std::sync::Arc`), by an import of one (`use std::rc::Rc;`), or, for `Box`, by the
/// prelude, when no item or import of the name is in scope. A pointer of another crate, or a
/// name a glob may bring in, is none.
fn rust_smart_pointer_inner<'t>(
    ctx: &ResolutionContext<'_>,
    scope: &ScopeId,
    written: &'t str,
) -> Option<&'t str> {
    let mut rest = written.trim();
    while let Some(referent) = rest.strip_prefix('&') {
        rest = referent.trim_start();
        if let Some(lifetime) = rest.strip_prefix('\'') {
            let end = lifetime.find(char::is_whitespace)?;
            rest = lifetime[end..].trim_start();
        }
        if let Some(referent) = rest.strip_prefix("mut ") {
            rest = referent.trim_start();
        }
    }
    let (head, arguments) = rest.split_once('<')?;
    let arguments = arguments.trim_end().strip_suffix('>')?;
    let inner = rust_first_type_argument(arguments)?;
    let head = head.trim();
    let path = head.strip_prefix("::").unwrap_or(head);
    if is_standard_smart_pointer(path) {
        let root = path.split("::").next().unwrap_or_default();
        // `std` is the standard library's unless a module of the name is in scope.
        return crate::context::rust_module_in_scope(ctx, scope, root)
            .is_none()
            .then_some(inner);
    }
    if path.contains("::") {
        return None;
    }
    if crate::context::nearest_lexical_items(ctx, scope, path, |_| true).is_some() {
        return None;
    }
    match ctx.scoped_import(scope, path, |_| true) {
        ScopedImport::NotImported => (path == "Box").then_some(inner),
        ScopedImport::Resolved(bindings) => match bindings.as_slice() {
            [binding] if !binding.is_glob => {
                let source = binding.source_module.trim();
                let source = source.strip_prefix("::").unwrap_or(source);
                is_standard_smart_pointer(source).then_some(inner)
            }
            _ => None,
        },
        ScopedImport::Unresolved => None,
    }
}

/// Whether `path` spells the standard library's `Box`, `Rc` or `Arc`.
fn is_standard_smart_pointer(path: &str) -> bool {
    matches!(
        path,
        "std::boxed::Box"
            | "alloc::boxed::Box"
            | "std::rc::Rc"
            | "alloc::rc::Rc"
            | "std::sync::Arc"
            | "alloc::sync::Arc"
    )
}

/// The first generic argument in `arguments`, the text between `<` and `>`, when it is a type:
/// `Inner` of `Inner, Global`.
fn rust_first_type_argument(arguments: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut end = arguments.len();
    for (index, ch) in arguments.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                end = index;
                break;
            }
            _ => {}
        }
    }
    let first = arguments[..end].trim();
    (!first.is_empty() && !first.starts_with('\'')).then_some(first)
}

/// Where a function's `self` parameter puts it in rustc's probe order for a receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfRank {
    /// Tried at this step, lower first.
    At(u8),
    /// No `self` parameter: a method call never reaches it.
    NoSelf,
    /// A `self` of another type (`self: Box<Self>`), or a signature the index cannot read.
    Unknown,
}

/// Where `symbol`'s `self` parameter puts it in rustc's probe order for a receiver of `form`.
fn rust_self_rank(symbol: &Symbol, form: RustReceiverForm) -> SelfRank {
    let Some(parameters) = symbol
        .signature
        .as_deref()
        .and_then(|signature| signature.strip_prefix("fn"))
        .and_then(|rest| rest.trim_start().strip_prefix('('))
    else {
        return SelfRank::Unknown;
    };
    let mut depth = 0usize;
    let mut end = parameters.len();
    for (index, ch) in parameters.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' if depth == 0 => {
                end = index;
                break;
            }
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                end = index;
                break;
            }
            _ => {}
        }
    }
    let first = parameters[..end].split_whitespace().collect::<Vec<_>>();
    #[derive(PartialEq)]
    enum Receiver {
        Value,
        Shared,
        Mutable,
    }
    let receiver = match first.as_slice() {
        ["self"] | ["mut", "self"] | ["self:", "Self"] | ["mut", "self:", "Self"] => {
            Receiver::Value
        }
        ["&self"] | ["self:", "&Self"] => Receiver::Shared,
        ["&mut", "self"] | ["self:", "&mut", "Self"] => Receiver::Mutable,
        [lifetime, "self"] if lifetime.starts_with("&'") => Receiver::Shared,
        [lifetime, "mut", "self"] if lifetime.starts_with("&'") => Receiver::Mutable,
        ["self:", lifetime, "Self"] if lifetime.starts_with("&'") => Receiver::Shared,
        ["self:", lifetime, "mut", "Self"] if lifetime.starts_with("&'") => Receiver::Mutable,
        words if words.iter().any(|word| word.contains("self")) => return SelfRank::Unknown,
        _ => return SelfRank::NoSelf,
    };
    let order: [Receiver; 3] = match form {
        RustReceiverForm::Place => [Receiver::Value, Receiver::Shared, Receiver::Mutable],
        RustReceiverForm::Shared => [Receiver::Shared, Receiver::Value, Receiver::Mutable],
        RustReceiverForm::Mutable => [Receiver::Mutable, Receiver::Value, Receiver::Shared],
    };
    order
        .iter()
        .position(|step| *step == receiver)
        .and_then(|rank| u8::try_from(rank).ok())
        .map_or(SelfRank::Unknown, SelfRank::At)
}

/// The one inherent method among `candidates`, methods named `name` of the types `owners`, that
/// rustc picks for a receiver of `form` over every trait method of the name the index knows:
/// the trait methods of an `impl` among the candidates and every Rust trait method of the name
/// the repository holds, as [`rust_trait_method`] reads one. It must be declared in an `impl` of the type
/// itself, not of an alias, that covers every instantiation of the type, and take `self` no
/// later in the probe order than each of them: an inherent method precedes a trait method
/// that takes `self` the same way, but not one taken in an earlier step (#639).
pub(crate) fn rust_inherent_precedence(
    ctx: &ResolutionContext<'_>,
    name: &str,
    owners: &[SymbolId],
    candidates: &[SymbolId],
    form: RustReceiverForm,
) -> Option<SymbolId> {
    let mut inherent = Vec::new();
    let mut traits = Vec::new();
    for id in candidates {
        let symbol = ctx.symbols.get(id)?;
        let block = ctx.scopes.rust_impl_block(symbol.scope_id.as_ref()?)?;
        if block.trait_name.is_some() {
            traits.push(symbol);
        } else {
            inherent.push((symbol, block));
        }
    }
    let [(chosen, block)] = inherent.as_slice() else {
        return None;
    };
    let own = chosen
        .parent_symbol_id
        .as_ref()
        .is_some_and(|parent| owners.contains(parent));
    if !own
        || !block.covers_every_instantiation
        || !matches!(chosen.kind, SymbolKind::Method | SymbolKind::Function)
    {
        return None;
    }
    let SelfRank::At(rank) = rust_self_rank(chosen, form) else {
        return None;
    };
    let declared_by_traits = ctx
        .symbols
        .by_name
        .get(name)
        .into_iter()
        .flatten()
        .filter_map(|id| ctx.symbols.get(id))
        .filter(|symbol| rust_trait_method(ctx, symbol));
    for other in traits.into_iter().chain(declared_by_traits) {
        match rust_self_rank(other, form) {
            SelfRank::At(earlier) if earlier < rank => return None,
            SelfRank::At(_) | SelfRank::NoSelf => {}
            SelfRank::Unknown => return None,
        }
    }
    Some(chosen.id.clone())
}

/// Whether `symbol` is a Rust method of a trait: a default method a `trait` item declares, or
/// one an `impl` of a trait declares. A method a trait only declares, with no body, is no
/// indexed item, but every `impl` that could answer a call has one, a blanket `impl` included.
fn rust_trait_method(ctx: &ResolutionContext<'_>, symbol: &Symbol) -> bool {
    if symbol.language != open_kioku_core::Language::Rust {
        return false;
    }
    let in_trait = symbol
        .parent_symbol_id
        .as_ref()
        .and_then(|parent| ctx.symbols.get(parent))
        .is_some_and(|parent| matches!(parent.kind, SymbolKind::Trait | SymbolKind::Interface));
    in_trait
        || symbol
            .scope_id
            .as_ref()
            .and_then(|scope| ctx.scopes.rust_impl_block(scope))
            .is_some_and(|block| block.trait_name.is_some())
}

/// Whether a method call named `name`, in the caller's file, may reach a method of the type a
/// `Box`, `Rc` or `Arc` holds rather than one of the pointer (#639). rustc tries the pointer,
/// and references to it, before the type it holds, so the name must be no method the standard
/// library gives a pointer, directly or through a trait the pointer forwards to what it holds
/// (`Rc::clone`, `Arc::strong_count`, `Box`'s `Iterator::next`), and no trait method of the
/// repository (see [`rust_trait_method`]), whose trait a blanket `impl` may give the pointer;
/// and no trait the index does not know may be in scope in the caller's file.
pub(crate) fn rust_dereference_reaches_pointee(ctx: &ResolutionContext<'_>, name: &str) -> bool {
    !SMART_POINTER_METHODS.contains(&name)
        && !ctx
            .symbols
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|id| ctx.symbols.get(id))
            .any(|symbol| rust_trait_method(ctx, symbol))
        && !ctx.scopes.rust_trait_scope_is_open(ctx.file_id)
}

/// The names a method call on a `Box`, `Rc` or `Arc` of the standard library may reach on the
/// pointer itself: its associated functions, which a call path names (`Rc::clone(&a)`), the
/// methods of the traits the standard library implements for it, those forwarding to what it
/// holds (`Iterator`, `Read`, `Write`, `BufRead`, `Seek`, `Hasher`, `Error`, `Future` and the
/// `Fn` traits) included, and those of the traits implemented for every type.
const SMART_POINTER_METHODS: &[&str] = &[
    // Associated functions of `Box`, `Rc` and `Arc`.
    "allocator",
    "as_mut_ptr",
    "as_ptr",
    "assume_init",
    "decrement_strong_count",
    "downcast",
    "downcast_unchecked",
    "downgrade",
    "from_non_null",
    "from_raw",
    "from_raw_in",
    "get_mut",
    "get_mut_unchecked",
    "increment_strong_count",
    "into_boxed_slice",
    "into_inner",
    "into_non_null",
    "into_pin",
    "into_raw",
    "into_raw_with_allocator",
    "leak",
    "make_mut",
    "new",
    "new_cyclic",
    "new_in",
    "new_uninit",
    "new_uninit_slice",
    "new_zeroed",
    "pin",
    "pin_in",
    "ptr_eq",
    "strong_count",
    "try_new",
    "try_unwrap",
    "unwrap_or_clone",
    "weak_count",
    "write",
    // `Clone`, `ToOwned`, `ToString`, `Into`, `TryInto`, `From`, `TryFrom`, `Default`, `Drop`.
    "clone",
    "clone_from",
    "clone_into",
    "default",
    "drop",
    "from",
    "into",
    "to_owned",
    "to_string",
    "try_from",
    "try_into",
    // `PartialEq`, `PartialOrd`, `Ord`, `Hash`, `Debug`, `Display`, `Pointer`, `Any`.
    "clamp",
    "cmp",
    "eq",
    "fmt",
    "ge",
    "gt",
    "hash",
    "hash_slice",
    "le",
    "lt",
    "max",
    "min",
    "ne",
    "partial_cmp",
    "type_id",
    // `AsRef`, `AsMut`, `Borrow`, `BorrowMut`, `Deref`, `DerefMut`, the descriptor traits.
    "as_fd",
    "as_handle",
    "as_mut",
    "as_raw_fd",
    "as_raw_handle",
    "as_raw_socket",
    "as_ref",
    "as_socket",
    "borrow",
    "borrow_mut",
    "deref",
    "deref_mut",
    // `Error`, `Future`, `AsyncIterator`, the `Fn` traits, `IntoIterator`.
    "call",
    "call_mut",
    "call_once",
    "cause",
    "description",
    "into_iter",
    "poll",
    "poll_next",
    "provide",
    "source",
    // `Read`, `Write`, `BufRead`, `Seek`, `fmt::Write`.
    "bytes",
    "consume",
    "fill_buf",
    "flush",
    "has_data_left",
    "is_read_vectored",
    "is_write_vectored",
    "lines",
    "read",
    "read_buf",
    "read_buf_exact",
    "read_exact",
    "read_line",
    "read_to_end",
    "read_to_string",
    "read_until",
    "read_vectored",
    "rewind",
    "seek",
    "seek_relative",
    "skip_until",
    "split",
    "stream_len",
    "stream_position",
    "write_all",
    "write_all_vectored",
    "write_char",
    "write_fmt",
    "write_str",
    "write_vectored",
    // `Hasher`.
    "finish",
    "write_i128",
    "write_i16",
    "write_i32",
    "write_i64",
    "write_i8",
    "write_isize",
    "write_length_prefix",
    "write_u128",
    "write_u16",
    "write_u32",
    "write_u64",
    "write_u8",
    "write_usize",
    // `Iterator`, `DoubleEndedIterator`, `ExactSizeIterator`.
    "advance_back_by",
    "advance_by",
    "all",
    "any",
    "array_chunks",
    "by_ref",
    "chain",
    "cloned",
    "cmp_by",
    "collect",
    "collect_into",
    "copied",
    "count",
    "cycle",
    "enumerate",
    "eq_by",
    "filter",
    "filter_map",
    "find",
    "find_map",
    "flat_map",
    "flatten",
    "fold",
    "for_each",
    "fuse",
    "inspect",
    "intersperse",
    "intersperse_with",
    "is_empty",
    "is_partitioned",
    "is_sorted",
    "is_sorted_by",
    "is_sorted_by_key",
    "last",
    "len",
    "map",
    "map_while",
    "map_windows",
    "max_by",
    "max_by_key",
    "min_by",
    "min_by_key",
    "next",
    "next_back",
    "next_chunk",
    "nth",
    "nth_back",
    "partial_cmp_by",
    "partition",
    "partition_in_place",
    "peekable",
    "position",
    "product",
    "reduce",
    "rev",
    "rfind",
    "rfold",
    "rposition",
    "scan",
    "size_hint",
    "skip",
    "skip_while",
    "step_by",
    "sum",
    "take",
    "take_while",
    "try_collect",
    "try_find",
    "try_fold",
    "try_for_each",
    "try_reduce",
    "try_rfold",
    "unzip",
    "zip",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_prefixes_are_read_with_their_mutability() {
        assert!(rust_reference_prefix("Store").is_empty());
        assert_eq!(rust_reference_prefix("&'a mut &Store"), vec![true, false]);
        assert_eq!(rust_reference_prefix("&'static Store"), vec![false]);
    }

    #[test]
    fn the_first_type_argument_is_read_past_nested_generics() {
        assert_eq!(rust_first_type_argument("Inner"), Some("Inner"));
        assert_eq!(
            rust_first_type_argument("Map<u8, Vec<u8>>, Global"),
            Some("Map<u8, Vec<u8>>")
        );
        assert_eq!(rust_first_type_argument("'a"), None);
    }

    #[test]
    fn self_parameters_are_ranked_in_probe_order() {
        use open_kioku_core::{Confidence, EvidenceSourceType, Language, Visibility};
        let method = |signature: &str| Symbol {
            id: SymbolId::new("symbol:m"),
            name: "m".into(),
            qualified_name: "src::lib::m".into(),
            kind: SymbolKind::Method,
            file_id: FileId::new("file:src/lib.rs"),
            range: None,
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: Some(signature.to_string()),
            visibility: Visibility::Public,
            alias_of: None,
        };
        let rank = |signature: &str, form| rust_self_rank(&method(signature), form);
        let place = RustReceiverForm::Place;
        assert_eq!(rank("fn(self)", place), SelfRank::At(0));
        assert_eq!(rank("fn(mut self, x: u8)", place), SelfRank::At(0));
        assert_eq!(rank("fn(&self) -> u8", place), SelfRank::At(1));
        assert_eq!(rank("fn(&'a self)", place), SelfRank::At(1));
        assert_eq!(rank("fn(&mut self)", place), SelfRank::At(2));
        assert_eq!(rank("fn(self: &mut Self)", place), SelfRank::At(2));
        assert_eq!(rank("fn(self: Box<Self>)", place), SelfRank::Unknown);
        assert_eq!(rank("fn(x: u8)", place), SelfRank::NoSelf);
        assert_eq!(rank("fn()", place), SelfRank::NoSelf);
        let shared = RustReceiverForm::Shared;
        assert_eq!(rank("fn(&self)", shared), SelfRank::At(0));
        assert_eq!(rank("fn(self)", shared), SelfRank::At(1));
        let mutable = RustReceiverForm::Mutable;
        assert_eq!(rank("fn(&mut self)", mutable), SelfRank::At(0));
        assert_eq!(rank("fn(&self)", mutable), SelfRank::At(2));
    }
}
