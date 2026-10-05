use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const ANALYSIS_SEMANTICS_DESCRIPTOR_VERSION: u32 = 1;
/// v2: an analysis-fact target's node id hashes its label as spelled (only SQL table names fold
/// case), so case-variant names such as a `TempDir` type and a `tempdir` function are separate
/// nodes; a v1 index merged them into one node and attached both items' edges to it.
pub const STABLE_IDENTITY_SEMANTICS_VERSION: &str = "stable-identity-v2";
/// v2: the project model's manifest walk and the import resolver's manifest index skip the
/// directories discovery prunes, and only those (`[index] keep_dirs` included): a crate
/// directory named `target` is a project root, and a manifest under pruned build output (a
/// `package.json` copied into a bundle's `dist/`) is not read. v1 skipped every `target` by name
/// and read manifests under `build/` and `dist/`, so a member crate in a `target` directory had
/// no module tree and its `crate::` paths proved no edge.
/// v3: in a Git repository, discovery prunes every nested work tree (a checked-out submodule, a
/// clone or a linked worktree inside the repository) as `submodule`, so neither its files nor its
/// manifests are read. v2 indexed a nested clone or worktree as this repository's source, its
/// manifests included, and failed outright on a checked-out submodule (#677).
/// v5: discovery prunes MSBuild output as `msbuild_output`: a `bin` beside an MSBuild project
/// file (`*.csproj`, `*.fsproj`, `*.vbproj`) holding build artifacts, and an `obj` holding a
/// NuGet restore's output or, beside a project file, a `Debug`/`Release` directory, so neither
/// its files nor its manifests are read; earlier versions indexed the JSON and other supported
/// files MSBuild copies there (#684). v4 was left to #676, which landed as v6.
/// v6 (#676; v4 was reserved for it): a file whose name starts with an SSH key stem but ends in
/// a programming-source extension (`id_rsa_loader.py`, `src/id_rsa_tool.rs`) is indexed, so it
/// can be a module, a crate root, or an import target; earlier indexes skipped it as
/// secret-like and hold no edge into it, and no history row for it. A private-key PEM body in
/// programming-language source is replaced before it is indexed; earlier versions stored it as
/// written.
pub const PROJECT_RESOLVER_SEMANTICS_VERSION: &str = "project-resolver-v6";
/// v3: a Rust `use` path inside the importing file's own crate resolves through that crate's
/// declared module tree, so its `IMPORTS` edge names the file declaring the module or the item;
/// v2 resolved such a path against the repository-root `src/` and fell back to the crate root,
/// which stored proven edges into a file that declares nothing the path names.
/// v4: a Rust bare name resolves to an item of this file only from the module declaring it, and
/// a nearer import or glob stops the lexical lookup; v3 let a call or receiver type inside a `mod`
/// block, or under a block-level import, prove an edge to an item in another module of the file.
/// v5: a Rust `self::` or `super::` path starts from the innermost module around the use site, an
/// inline `mod` block included; v4 climbed from the file's module path, so `super::f()` in
/// `mod tests` proved an edge to the parent module's `f` instead of the file's own.
/// v6: the symbol-registry pass applies the same Rust module scoping to its heuristic edges, so a
/// bare name no longer matches an item of its file that the use site cannot see; v5 indexes hold
/// such `CALLS` and `REFERENCES` edges.
/// v7: a Rust `crate::`, `self::` or `super::` path the resolver spells from file paths neither
/// starts nor ends in a file the declared module tree shows is not the module its path spells, such
/// as the default location of a `#[path]` module or a file a `#[path]` mounts; v6 indexes hold
/// such `CALLS` edges.
/// v8: a Rust `crate::` path is read against the caller's own crate (its package's module tree,
/// and the crate root whose tree holds it) instead of a top-level `src/`, a raw identifier
/// (`r#type`) names the module file `type.rs`, a `[lib] path` in `src/` is a crate root, and a
/// path from a file outside every package's module tree is not followed; v7 indexes hold `CALLS`
/// edges from workspace members into the root package and from `src/bin/` files into the library.
/// v9: a Rust file that no indexed crate root declares is read against no crate when a crate
/// root of its module tree was not indexed or is one the layout does not follow, and a crate root
/// a manifest's `[[bin]]`, `[[test]]`, `[[example]]` or `[[bench]]` table names (or that
/// `autobins = false` and its peers leave out) is followed; v8 indexes hold `CALLS` edges from a
/// skipped `main.rs`'s modules into the library, and none through a `[[bin]] path` root's modules.
/// v10: a `[lib] path` outside `src/` in a directory where another followed crate root keeps its
/// modules is a crate root of that module tree too, so a module both declare belongs to both
/// crates; and a module path in a package at the repository root reaches the modules below its
/// crate roots; v9 indexes hold `CALLS` edges from such a shared module into the other root's item,
/// none from a module only that library declares, and none through a nested module of a module
/// tree at the repository root.
/// v11: a manifest-named crate root path is placed by its `.`/`..`-normalised form inside the
/// package, and a module file another crate may also compile (declared beside a crate root the
/// index could not read, or mounted by another crate with `#[path]`) has no module path read from
/// it; v10 indexes hold `CALLS` edges from such files into one crate's item.
/// v12: the symbol-registry pass records a line's use of a target as `CALLS` when any use of it on
/// the line is a call (`let path = dir.path();`), and rules an item out of a whole line whichever
/// use comes first; v11 kept whichever edge the line spelled first.
/// v13: the symbol-registry pass reads no token inside a comment or string literal, and its
/// name-only strategies (same module, unique project name, suffix reachability, fuzzy) never match
/// a symbol of another language family; v12 indexes hold registry edges from comment and literal
/// words and across languages (a Rust `Utc::now()` to a JavaScript `now`).
/// v15 (v14 is taken by the symbol registry's unique-name precision change): a Rust path whose
/// first segment is a crate name the importer's package declares as a
/// dependency on a package of the repository (by `path`, directly or through
/// `[workspace.dependencies]`) is followed through that package's library module tree, and a
/// crate-name path, the importer's own package's included, follows the `pub use` re-exports of the
/// modules it passes through; `engine::run()` through such a name is a call into that crate. v13
/// indexes hold no `IMPORTS` edge, import binding or `CALLS` edge across crates, and no binding
/// for a path through the importer's own crate name. A call path or a call through an imported
/// module never ends in a module symbol.
/// v16 (the change v15 calls v14; it landed after v15): the symbol-registry pass matches by name
/// alone only a plain name: not an attribute (a Java annotation matches types only), a member
/// access (unless its receiver names the symbol's package or type), a field name or a local the
/// chunk binds, nor a path or import that leads elsewhere (`std::mem::take`, `use anyhow::Result;`);
/// and it reads Python f-string fields as code. v15 indexes hold unique-name edges from those
/// tokens and none from f-string fields.
/// v17: in a module file another crate may also compile, a `self::`/`super::` path that ends
/// below the file's own module is read when every crate compiles the file at the place its path
/// spells; a crate root skipped for size shares only the modules its `mod` items declare; and the
/// module files a `#[path]`-mounted file declares are shared with the mounting crate too. v16 indexes hold no `CALLS` edge
/// or binding from such a `self::`/`super::` path, none from a module file a size-skipped root
/// does not declare, and `CALLS` edges from a `crate::` path in a module file below a mounted
/// `mod.rs` into one crate's item.
/// v19: a `#[path]` attribute inside a `#[path]`-mounted file's module subtree, each alternative
/// of a `cfg_attr(.., path = ..)` included, mounts its file into the mounting crate too. Earlier
/// indexes hold `CALLS` edges from a `crate::` path in such a file into one crate's item.
/// v20: a Rust trait `impl` whose trait no repository symbol answers is an external resolution
/// when the index shows the trait is defined outside the repository: named through the standard
/// library, a dependency the package's manifest places outside the repository (registry, git, or a
/// `path` leaving it) and the workspace root's `[patch]` or `[replace]` does not point back into
/// it, or, unimported, the standard prelude. A method call on a Rust binding
/// annotated with a generic type or a reference (`w: &Wrapper<u8>`, `x: &mut Foo`) is typed by
/// the annotation's path without its generic arguments, and a call path whose last segment names
/// a type of the module the rest reaches (`crate::a::Engine::new()`, `engine::Engine::new()`)
/// reaches that type's associated function. Earlier indexes count those `IMPLEMENTS` as
/// unresolved and hold none of those `CALLS` edges.
/// v22: a Rust file a `#[path]` attribute mounts, or one below it, that was skipped for size has
/// its `mod` items read, so the module files it declares are shared with the mounting crate too;
/// one skipped and not read may mount any file of its package. Earlier indexes hold `CALLS`
/// edges from a `crate::` path in such a module file into one crate's item.
/// v24: a Rust module whose every `path` attribute is a `cfg_attr` is also placed at its default
/// location (`name.rs` or `name/mod.rs`), which it compiles from whenever no condition holds, in
/// its own crate's module tree and below a `#[path]`-mounted file, parsed or read off a file
/// skipped for size. Earlier indexes read no module path from that file, and hold `CALLS` edges
/// from a `crate::` path in it into one crate's item when another crate mounts its parent.
/// v26: a Rust path into a module whose file configuration selects (a `cfg_attr` path beside the
/// default location, or `#[cfg]`-gated `mod` items of one name naming more than one file), or
/// below one, keeps a `CALLS` edge to the item in each file that may hold it, none authoritative
/// and each naming the files; `cfg_attr(all(), ..)` and `X` beside `not(X)` leave no default
/// location. Earlier indexes hold one authoritative edge into the placed default file, including
/// one rustc never compiles. v25 is taken by an open change.
/// v27: the symbol-registry pass reads a member's receiver the file imports by its import: one
/// from outside the repository matches nothing, a Go receiver matches only in the package
/// directory its import path names (under a declared module, the module's directory and the rest
/// of the path), and a Java static import matches only a member of the class its path names.
/// v26 indexes hold unique-name edges from receivers imported from another module or package and
/// from static imports of library classes.
/// v29: the symbol-registry pass reads a Go type alias as the type it stands for, placed through
/// the alias file's imports: a member whose receiver's package declares the alias reaches the
/// aliased type, and an alias and its target matched by one name are one candidate. An alias whose
/// target the pass cannot place resolves to nothing, with a caveat naming the alias. v27 indexes
/// hold no edge for a member reached through an alias declared in another package. v28 is taken
/// by an open change.
/// v31: the symbol-registry pass reads a Java static import's class by the package the candidate's
/// file declares and the classes enclosing the member, not by the file's directories, and counts a
/// Java import inside a declared package (by whole segments) as the repository's. A Go file of an
/// external test package (`package store_test` in a `_test.go` file) is not of the package its
/// directory holds, and a Go `_test.go` declaration is ruled out for a use in another directory, or
/// outside its external test package. v29 indexes hold no edge for a static import of a class whose
/// directory does not mirror its package, one for a class whose directory spells the import's
/// package while its declaration names another, unique-name edges into `_test.go` declarations from
/// other directories, and no edge for a member made ambiguous by an alias its package's external
/// test package declares. v30 is taken by an open change.
/// v32: a call through a Rust `use` import whose path, or a `pub use` it is followed through,
/// passes through a module whose file configuration selects keeps a `CALLS` edge to the item in
/// each file that may hold that module, none authoritative and each naming the files, as the
/// path spelling does since v26. v31 indexes hold one authoritative edge into the placed default
/// file.
/// v34: a Rust path or `use` import written in a file inside one alternative of such a module,
/// a file a `path` attribute mounts included, reaches that alternative's files alone, and is
/// proven when that leaves one file for every choice on the path; a method call on a value of a
/// type imported through such a module, and a `USES_TYPE` or `IMPLEMENTS` relation through such
/// an import, keep one unproven edge per file that may hold the type. v32 indexes hold edges
/// from such a file into every alternative, and one authoritative edge into the placed default
/// file for the type's method, declared type and implemented trait. v33 is taken by an open
/// change.
/// v36: a Rust call path whose first segment is a module in scope at the call rather than
/// `crate`, `self` or `super` (`sys::imp::f()` beside `mod sys;`) starts from the module that
/// declares it, as a `self::` path does, and is proven as such a path is; one whose first segment
/// is also a crate the package can name is ambiguous. v34 indexes hold no edge for such a path.
/// v35 is taken by an open change.
/// v37: the symbol-registry pass reports each Go type alias it placed, and the indexed alias
/// symbol takes its target's kind and names it (`Symbol::alias_of`), which search ranks and
/// symbol listings order by. v36 indexes hold every alias with its syntax kind and no target, so
/// a one-line alias outranks the type it stands for. v33 and v35 were reserved for this change
/// and are left unused.
/// v38: a Rust `use` path whose first segment is a module the file declares in scope
/// (`use sys::imp::f;` beside `mod sys;`) is bound as its `self::` path in a package of the 2018
/// edition or later, and as its `crate::` path when the crate root declares that module in a 2015
/// package, for calls, types and the file-level `IMPORTS` edge; it is ambiguous when that segment
/// also names a crate a 2018 package can name; a type
/// written as a module path (`s: sys::imp::S`) is read through the path; a relative path written
/// in a file a `path` attribute mounts inside one alternative reads that alternative; and a file
/// `mod name;` declares inside an inline `mod` block is placed below the directory the blocks
/// spell. v37 indexes hold no edge for any of these.
/// v39: a Rust type written as a `crate::`, `self::` or `super::` path is read through the path
/// for `USES_TYPE` and for a receiver's type, with the configuration-selected module rules of a
/// call path; and a method call through a struct field (`self.store.save()`,
/// `entry.store.save()`) is read through the type the field declares. v38 indexes hold no edge
/// for either.
/// v40: a Rust path is followed through the `use` declarations of the module it names: `use
/// crate::f;` and `crate::f()` after `pub use auth::f;` in the crate root reach `auth::f`, through
/// named, aliased and glob re-exports, chains of them, and a private `use` from below its module;
/// a path from another crate follows `pub use` alone, also for a call path. A name a module
/// defines while a `use` beside it brings in another is ambiguous. v39 indexes hold no edge for
/// a path through an in-crate re-export.
/// v42: a Rust name is read in the namespace its path reads it in: a call path names a value
/// and a type path a type, so a braced `struct S` beside a glob that brings in a `fn S` no
/// longer answers `m::S()`, and a call path names no braced struct, enum, union, trait or type
/// alias. A path is followed through an enum's variants (`pub use Shape::*;`), a module a `use`
/// renames (`pub use inner as facade;`) and the named `use` declarations of an inline `mod`
/// block. v40 indexes hold the old edges. v41 is taken by an open change.
/// v43: a Rust method call through a struct field reads the field's type through a `type`
/// alias and through the standard library's `Box`, `Rc` and `Arc`, where no method of the
/// pointer may answer it, and an inherent method takes precedence over trait methods of the
/// same name that rustc tries no earlier; a local built by a tuple or unit struct's constructor
/// is an instance of it; an associated function is found through an alias; and two globs that
/// bring in one name in different namespaces settle each namespace, a path continuing through
/// a module a glob brings in. v42 indexes hold no edge for any of these.
/// v44: a symbol-registry fact made by a name-only strategy (unique project name, suffix import
/// reachability, fuzzy) records why it may name the wrong target in `AnalysisFact::ambiguity`;
/// v43 facts recorded it only in their message, so their edges did not read as ambiguous.
pub const RELATIONSHIP_RESOLVER_SEMANTICS_VERSION: &str = "ri3-relationship-resolver-v44";
pub const PROOF_POLICY_SEMANTICS_VERSION: &str = "ri3-proof-policy-v1";
/// v2: the static `use`-syntax `IMPORTS` edge asserts a module binding only where the syntax
/// proves one; a Rust in-crate item path carries none, and a glob names the module it opens.
/// v3: an edge several writes describe keeps the evidence at its earliest site among the
/// strongest writes, and a field that evidence's write leaves unset takes the smallest value any
/// write gave; v2 kept the evidence with the smallest id, a hash of the edge's target node, and
/// filled unset fields from whichever write was folded first.
/// v4: a symbol-registry `CALLS` or `REFERENCES` fact that resolved to an indexed symbol ends at
/// that symbol's node, and is dropped where a proven or corroborating edge already joins the same
/// two nodes with the same type; v3 drew it to an `analysis:<Kind>:<hash>` node made from the
/// symbol's label, which no symbol-keyed read reached, and v3 facts do not name the symbol.
/// v5: an analysis fact's recorded ambiguity is copied onto its edge, so a name-only
/// symbol-registry edge is ambiguous; and a similarity fact (`SIMILAR_TO`,
/// `SEMANTICALLY_RELATED`) ends at the similar symbol's node rather than at a node made from its
/// label. v4 indexes hold unambiguous registry edges and similarity edges no symbol read reaches.
pub const GRAPH_EMISSION_SEMANTICS_VERSION: &str = "ri3-graph-emission-v5";
pub const EXACT_INDEX_INGESTION_SEMANTICS_VERSION: &str = "exact-occurrence-v1";
pub const LANGUAGE_ADAPTER_SEMANTICS_VERSION: &str = "ri3-language-semantics-v1";
/// v2: test targets are callables with a test annotation in the attribute stack above them,
/// not every symbol of a file that mentions `#[test]`; indexes built with v1 persisted wrong
/// targets and must be rebuilt rather than partially refreshed.
/// v3: a Rust `use` declaration emits one import site per imported path, and `mod` items are
/// recorded as module declarations; v2 indexes lack both.
/// v4: a test file is one `is_test_path` recognises outside data-only directories, so JS/TS
/// `_test`, `.test.js`, `.spec.js`, `.test.tsx` and `__tests__/` files gain targets; only
/// callables are targets anywhere, and a JS/TS registration call such as `test("name", fn)` is
/// one too; a v3 index holds none of that and must be rebuilt.
/// v5: Rust and Java symbol visibility is read from the item's own visibility modifier, not
/// matched in its text; a v4 index records a private Rust function whose body spells `pub ` as
/// public, a package-private Java class holding a public member as public, and `pub(crate)`
/// items as private.
/// v6: a Rust trait item takes the trait's visibility and a trait `impl` member the narrower of
/// the trait's and the implementing type's, and a Java interface or annotation-type member with
/// no access keyword is public; a v5 index records them as private and package.
/// v7: a Rust `use` import site records whether it is a `pub use` re-export, and a Rust
/// `path::name()` call whose path starts with a lowercase segment has a module receiver (a
/// primitive type's, a type receiver), never a value one: `engine::run()` and `engine.run()` share
/// the receiver text `engine`. A v6 index's sites record no re-export, and its calls read such a
/// path as a value.
/// v8: the members of a Rust `impl` for a generic type belong to the type's path without its
/// generic arguments, so `impl<'a> Engine<'a>` and `impl<T> Wrapper<T>` link their methods and
/// trait implementations to the file's `Engine` and `Wrapper`. A v7 index keys them under the
/// written type, `Engine<'a>`, which names no symbol, so calls to them stay unresolved.
/// v9: a Rust trait `impl` member's bound also reads a `self::`, `super::` or `crate::` path to a
/// trait or type declared in the same file, and sees an implementing `&T`, `&mut T`, `Box<T>`,
/// `Rc<T>` or `Arc<T>` as `T`; a v8 index records `impl Store for &Hidden` and
/// `impl crate::Cache for Gen<u8>` members as public.
/// v10: a Rust closure is a scope, so its typed parameters bind in the closure alone; a v9 index
/// records `|ctx: &mut Ring|` in the enclosing block, where it types a `ctx` used after the
/// closure.
/// v11: a Rust `mod` declaration records whether its every `path` attribute is a `cfg_attr`
/// (`path_is_conditional`); a v10 index records none, which reads as a `path` that always applies.
/// v12: `path_is_conditional` is unset when the `cfg_attr` conditions hold on every build
/// (`all()`, or `X` beside `not(X)`); a v11 index records such a module as conditional.
/// v13: a Go type alias (`type Entry = store.Entry`, alone or in a `type ( .. )` group) is a type
/// symbol whose signature spells the alias, and records the package qualifier and name of the type
/// it stands for; a v12 index holds no symbol for it.
/// v15: a Java file records the package its `package` declaration names and a Go file its
/// `package` clause; a v13 index records neither. v14 is taken by an open change.
/// v16: each named field of a Rust `struct` is a binding in the struct's scope, with its written
/// type unless that names a type parameter of the struct; a v15 index records no field.
/// v17: a Rust file records whether its top level invokes a macro, which may expand to items and
/// `use` declarations; a v16 index does not.
/// v18: a Rust file records which of its structs, enums, unions and type aliases are types
/// alone, not also values, and the variants of each enum; a v17 index records neither.
/// v19: a Rust file records its unit structs, the type each `type` alias stands for and each
/// `impl` block's trait and generic coverage; a `let` initialized by `Name(..)` or a plain path
/// records it; and a struct field typed by `Box`, `Rc` or `Arc` of a type parameter has no
/// declared type. A v18 index records none of these.
/// v20: a callable in a test-path file is a test (`test_file_symbol`) only when its language's
/// runner discovers it as one, and a helper, fixture or lifecycle hook otherwise
/// (`test_file_helper`), which is not validation evidence; a v19 index records every callable of
/// a test file as a test, so a file of `setUp` and `withTempRepo` helpers reads as validation.
/// v21: a `.cs` file is C#, parsed by tree-sitter into namespace, type and member symbols
/// qualified by namespace and type nesting, and chunked with each symbol's `///` documentation,
/// read through syntax errors where recovery kept a declaration whole and in place; a v20
/// index holds `.cs` files as unknown text with no symbols.
pub const PARSER_SEMANTICS_VERSION: &str = "tier1-parser-semantics-v21";

const TIER1_LANGUAGES: [&str; 6] = ["go", "java", "javascript", "python", "rust", "typescript"];

/// Languages parsed into symbols that have no relationship adapter yet: they carry parser
/// semantics but no language adapter version, since no relationship is resolved for them.
const PARSED_ONLY_LANGUAGES: [&str; 1] = ["csharp"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsDescriptor {
    pub version: u32,
    pub stable_identity_version: String,
    pub parser_semantics: BTreeMap<String, String>,
    pub project_resolver_version: String,
    pub relationship_resolver_version: String,
    pub proof_policy_version: String,
    pub graph_emission_version: String,
    pub exact_index_ingestion_version: String,
    pub language_adapter_versions: BTreeMap<String, String>,
}

impl AnalysisSemanticsDescriptor {
    pub fn fingerprint(&self) -> String {
        // Struct field order is fixed and all maps are BTreeMap, so serde_json emits a stable
        // canonical representation for this descriptor. These field types are infallibly JSON
        // serializable; failure indicates a programming invariant violation.
        let canonical = serde_json::to_vec(self)
            .expect("analysis semantics descriptor must be canonically serializable");
        format!("{:x}", Sha256::digest(canonical))
    }
}

pub fn current_analysis_semantics_descriptor() -> AnalysisSemanticsDescriptor {
    let parser_semantics = TIER1_LANGUAGES
        .into_iter()
        .chain(PARSED_ONLY_LANGUAGES)
        .map(|language| (language.to_string(), PARSER_SEMANTICS_VERSION.to_string()))
        .collect();
    let language_adapter_versions = TIER1_LANGUAGES
        .into_iter()
        .map(|language| {
            (
                language.to_string(),
                LANGUAGE_ADAPTER_SEMANTICS_VERSION.to_string(),
            )
        })
        .collect();
    AnalysisSemanticsDescriptor {
        version: ANALYSIS_SEMANTICS_DESCRIPTOR_VERSION,
        stable_identity_version: STABLE_IDENTITY_SEMANTICS_VERSION.into(),
        parser_semantics,
        project_resolver_version: PROJECT_RESOLVER_SEMANTICS_VERSION.into(),
        relationship_resolver_version: RELATIONSHIP_RESOLVER_SEMANTICS_VERSION.into(),
        proof_policy_version: PROOF_POLICY_SEMANTICS_VERSION.into(),
        graph_emission_version: GRAPH_EMISSION_SEMANTICS_VERSION.into(),
        exact_index_ingestion_version: EXACT_INDEX_INGESTION_SEMANTICS_VERSION.into(),
        language_adapter_versions,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsState {
    pub descriptor: AnalysisSemanticsDescriptor,
    pub fingerprint: String,
}

impl AnalysisSemanticsState {
    pub fn new(descriptor: AnalysisSemanticsDescriptor) -> Self {
        let fingerprint = descriptor.fingerprint();
        Self {
            descriptor,
            fingerprint,
        }
    }

    pub fn current() -> Self {
        Self::new(current_analysis_semantics_descriptor())
    }

    pub fn fingerprint_is_valid(&self) -> bool {
        self.fingerprint == self.descriptor.fingerprint()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisSemanticsCompatibilityStatus {
    Compatible,
    RefreshRequired,
    RebuildRequired,
    FutureUnsupported,
}

impl AnalysisSemanticsCompatibilityStatus {
    pub fn allows_authoritative_relationships(self) -> bool {
        matches!(self, Self::Compatible)
    }

    pub fn allows_partial_index_update(self) -> bool {
        matches!(self, Self::Compatible)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisSemanticsCompatibility {
    pub status: AnalysisSemanticsCompatibilityStatus,
    pub stored_fingerprint: Option<String>,
    pub current_fingerprint: String,
    pub reasons: Vec<String>,
    pub affected_components: Vec<String>,
    pub affected_languages: Vec<String>,
    pub recommended_action: String,
}

impl AnalysisSemanticsCompatibility {
    pub fn compatible(current: &AnalysisSemanticsState) -> Self {
        Self {
            status: AnalysisSemanticsCompatibilityStatus::Compatible,
            stored_fingerprint: Some(current.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: Vec::new(),
            affected_components: Vec::new(),
            affected_languages: Vec::new(),
            recommended_action: "none".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisSemanticsCompatibilityPolicy {
    /// Components explicitly safe to refresh without rebuilding authoritative graph state.
    /// V1 intentionally leaves this empty: semantic changes fail closed until a component has
    /// a proven selective refresh path.
    pub refresh_compatible_components: BTreeSet<String>,
    pub legacy_manifest_requires_rebuild: bool,
}

impl Default for AnalysisSemanticsCompatibilityPolicy {
    fn default() -> Self {
        Self {
            refresh_compatible_components: BTreeSet::new(),
            legacy_manifest_requires_rebuild: true,
        }
    }
}

pub fn classify_analysis_semantics(
    stored: Option<&AnalysisSemanticsState>,
    current: &AnalysisSemanticsState,
) -> AnalysisSemanticsCompatibility {
    classify_analysis_semantics_with_policy(
        stored,
        current,
        &AnalysisSemanticsCompatibilityPolicy::default(),
    )
}

pub fn classify_analysis_semantics_with_policy(
    stored: Option<&AnalysisSemanticsState>,
    current: &AnalysisSemanticsState,
    policy: &AnalysisSemanticsCompatibilityPolicy,
) -> AnalysisSemanticsCompatibility {
    let Some(stored) = stored else {
        return AnalysisSemanticsCompatibility {
            status: if policy.legacy_manifest_requires_rebuild {
                AnalysisSemanticsCompatibilityStatus::RebuildRequired
            } else {
                AnalysisSemanticsCompatibilityStatus::RefreshRequired
            },
            stored_fingerprint: None,
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec!["legacy index has no analysis-semantics descriptor".into()],
            affected_components: vec!["analysis_semantics".into()],
            affected_languages: Vec::new(),
            recommended_action:
                "run `ok index .` to rebuild the index with current analysis semantics".into(),
        };
    };

    if stored.descriptor.version > current.descriptor.version {
        return AnalysisSemanticsCompatibility {
            status: AnalysisSemanticsCompatibilityStatus::FutureUnsupported,
            stored_fingerprint: Some(stored.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec![format!(
                "stored descriptor version {} is newer than supported version {}",
                stored.descriptor.version, current.descriptor.version
            )],
            affected_components: vec!["descriptor_version".into()],
            affected_languages: Vec::new(),
            recommended_action: "upgrade Open Kioku before reading this index".into(),
        };
    }

    if !stored.fingerprint_is_valid() {
        return AnalysisSemanticsCompatibility {
            status: AnalysisSemanticsCompatibilityStatus::RebuildRequired,
            stored_fingerprint: Some(stored.fingerprint.clone()),
            current_fingerprint: current.fingerprint.clone(),
            reasons: vec!["stored analysis-semantics fingerprint does not match its descriptor".into()],
            affected_components: vec!["fingerprint_integrity".into()],
            affected_languages: Vec::new(),
            recommended_action: "run `ok index .` to rebuild the index; do not trust persisted relationship authority"
                .into(),
        };
    }

    if stored == current {
        return AnalysisSemanticsCompatibility::compatible(current);
    }

    let mut reasons = Vec::new();
    let mut components = BTreeSet::new();
    let mut languages = BTreeSet::new();

    compare_component(
        "descriptor_version",
        &stored.descriptor.version.to_string(),
        &current.descriptor.version.to_string(),
        &mut components,
        &mut reasons,
    );
    compare_component(
        "stable_identity",
        &stored.descriptor.stable_identity_version,
        &current.descriptor.stable_identity_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "project_resolver",
        &stored.descriptor.project_resolver_version,
        &current.descriptor.project_resolver_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "relationship_resolver",
        &stored.descriptor.relationship_resolver_version,
        &current.descriptor.relationship_resolver_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "proof_policy",
        &stored.descriptor.proof_policy_version,
        &current.descriptor.proof_policy_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "graph_emission",
        &stored.descriptor.graph_emission_version,
        &current.descriptor.graph_emission_version,
        &mut components,
        &mut reasons,
    );
    compare_component(
        "exact_index_ingestion",
        &stored.descriptor.exact_index_ingestion_version,
        &current.descriptor.exact_index_ingestion_version,
        &mut components,
        &mut reasons,
    );
    compare_language_map(
        "parser_semantics",
        &stored.descriptor.parser_semantics,
        &current.descriptor.parser_semantics,
        &mut components,
        &mut languages,
        &mut reasons,
    );
    compare_language_map(
        "language_adapter",
        &stored.descriptor.language_adapter_versions,
        &current.descriptor.language_adapter_versions,
        &mut components,
        &mut languages,
        &mut reasons,
    );

    if components.is_empty() {
        // Descriptor equality with a different valid fingerprint cannot happen with the current
        // canonical encoding, so fail closed if a future encoding ever violates that invariant.
        components.insert("fingerprint".into());
        reasons.push(
            "analysis-semantics fingerprints differ without a classified component change".into(),
        );
    }

    let refresh_only = components
        .iter()
        .all(|component| policy.refresh_compatible_components.contains(component));
    let status = if refresh_only {
        AnalysisSemanticsCompatibilityStatus::RefreshRequired
    } else {
        AnalysisSemanticsCompatibilityStatus::RebuildRequired
    };
    AnalysisSemanticsCompatibility {
        status,
        stored_fingerprint: Some(stored.fingerprint.clone()),
        current_fingerprint: current.fingerprint.clone(),
        reasons,
        affected_components: components.into_iter().collect(),
        affected_languages: languages.into_iter().collect(),
        recommended_action: match status {
            AnalysisSemanticsCompatibilityStatus::RefreshRequired => {
                "refresh stale semantic components before relying on authoritative relationship evidence"
                    .into()
            }
            _ => "run `ok index .` to rebuild the index with current analysis semantics".into(),
        },
    }
}

fn compare_component(
    name: &str,
    stored: &str,
    current: &str,
    components: &mut BTreeSet<String>,
    reasons: &mut Vec<String>,
) {
    if stored != current {
        components.insert(name.into());
        reasons.push(format!("{name} changed from `{stored}` to `{current}`"));
    }
}

fn compare_language_map(
    component: &str,
    stored: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
    components: &mut BTreeSet<String>,
    languages: &mut BTreeSet<String>,
    reasons: &mut Vec<String>,
) {
    let keys = stored
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for language in keys {
        let before = stored.get(&language);
        let after = current.get(&language);
        if before != after {
            components.insert(format!("{component}:{language}"));
            languages.insert(language.clone());
            reasons.push(format!(
                "{component} for {language} changed from `{}` to `{}`",
                before.map(String::as_str).unwrap_or("<missing>"),
                after.map(String::as_str).unwrap_or("<missing>")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_across_map_insertion_order() {
        let first = current_analysis_semantics_descriptor();
        let mut second = first.clone();
        second.parser_semantics = first
            .parser_semantics
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        second.language_adapter_versions = first
            .language_adapter_versions
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn identical_semantics_are_compatible() {
        let state = AnalysisSemanticsState::current();
        let compatibility = classify_analysis_semantics(Some(&state), &state);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::Compatible
        );
    }

    #[test]
    fn legacy_manifest_fails_closed() {
        let current = AnalysisSemanticsState::current();
        let compatibility = classify_analysis_semantics(None, &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
    }

    #[test]
    fn proof_policy_change_requires_rebuild() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.proof_policy_version = "old-policy".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(compatibility.affected_components, vec!["proof_policy"]);
    }

    #[test]
    fn relationship_resolver_change_requires_rebuild() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.relationship_resolver_version = "old-resolver".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(
            compatibility.affected_components,
            vec!["relationship_resolver"]
        );
    }

    #[test]
    fn one_language_adapter_change_is_scoped() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored
            .descriptor
            .language_adapter_versions
            .insert("java".into(), "old-java-adapter".into());
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RebuildRequired
        );
        assert_eq!(compatibility.affected_languages, vec!["java"]);
        assert_eq!(
            compatibility.affected_components,
            vec!["language_adapter:java"]
        );
    }

    #[test]
    fn future_descriptor_is_unsupported() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.version += 1;
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let compatibility = classify_analysis_semantics(Some(&stored), &current);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::FutureUnsupported
        );
    }

    #[test]
    fn explicit_refresh_policy_can_classify_refresh_required() {
        let current = AnalysisSemanticsState::current();
        let mut stored = current.clone();
        stored.descriptor.project_resolver_version = "old-project-resolver".into();
        stored = AnalysisSemanticsState::new(stored.descriptor);
        let policy = AnalysisSemanticsCompatibilityPolicy {
            refresh_compatible_components: BTreeSet::from(["project_resolver".into()]),
            legacy_manifest_requires_rebuild: true,
        };
        let compatibility =
            classify_analysis_semantics_with_policy(Some(&stored), &current, &policy);
        assert_eq!(
            compatibility.status,
            AnalysisSemanticsCompatibilityStatus::RefreshRequired
        );
    }
}
