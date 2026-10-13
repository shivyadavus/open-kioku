//! C# calls, inheritance and type uses (#682), resolved over the name lookup the `using` pass
//! runs ([`super::Lookup`]) and the .NET project model it reads.
//!
//! A C# type name means what C#'s lookup finds for it, out from the namespaces enclosing the use
//! through the file's directives and its project's global ones, in the projects the file's
//! project references; a member name means what member lookup finds in that type, its base
//! classes or interfaces, and overload resolution among what it finds. This pass reads the
//! parser's facts ([`CSharpNameFacts::members`], `calls`, `locals`, `types`) and follows both:
//!
//! - `CALLS` from the calling member: a bare `M()` (a local function, a method of the enclosing
//!   types inside out, then of a type a `using static` imports), `this.M()`, `base.M()`,
//!   `x.M()` on a local, parameter, field or property of a written type (`var` only when
//!   `new T(..)`, a cast or `as` gives it one), `T.M()` and `Ns.T.M()` on a type, member-access
//!   chains of fields and properties, `new T(..)`, a target-typed `new()` whose declaration writes
//!   the type, `: base(..)` and `: this(..)`, and extension methods of a static class in scope.
//!   Overloads are told apart by argument count (optional and `params` parameters included),
//!   explicit type-argument count and static or instance; more than one left is ambiguous.
//! - `EXTENDS` and `IMPLEMENTS` from each base list entry: the kind of the type an entry names
//!   decides which (a class's interface is implemented, an interface's base interface extended).
//! - `USES_TYPE` from a member to each repository type its parameters, return type or declared
//!   type name, generic arguments included, and from a member to its locals' written types.
//!
//! An edge to a type goes to every part of a partial type. An answer is proven only when it is
//! settled: what makes a `using` edge unproven (see the parent module) makes these unproven, and
//! so does a call written under `#if`, a target declared there, a member found by name while a
//! base class outside the repository could declare it, and an extension method an outside
//! namespace imported nearer could shadow. An unproven answer is kept as a heuristic edge whose
//! message names the reason; an ambiguous one of at most four candidates as heuristic edges to
//! each.

use super::{
    join_namespace, namespace_levels, AliasTarget, Found, Index, Lookup, NamespaceState, TypeGroup,
};
use open_kioku_core::csharp::{
    CSharpCall, CSharpCallKind, CSharpLocal, CSharpLocalKind, CSharpMember, CSharpMemberKind,
    CSharpNameContext, CSharpNameFacts, CSharpNameUse, CSharpPlace, CSharpReceiverRoot,
    CSharpTypeKind, CSharpTypeRef, CSharpTypeSegment, CSharpTypeShape, CSharpUsingKind,
};
use open_kioku_core::{
    Confidence, EvidenceSourceType, File, FileId, FileRange, GraphEdgeType, LineRange,
    RelationshipProof, RelationshipProofKind, ResolutionEvidence, ResolutionEvidenceKind,
    SourceRange, SymbolId, SymbolKind,
};
use open_kioku_resolution::{ResolutionCandidate, ResolutionOutcome, ResolvedRelationship};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

/// Receives each occurrence's outcome, for the resolution quality report, with the time it took
/// in microseconds.
pub(crate) type OutcomeRecorder<'r> = dyn FnMut(&GraphEdgeType, &ResolutionOutcome, u64) + 'r;

/// Most candidates an ambiguous answer keeps as heuristic edges.
const MAX_AMBIGUOUS_EDGES: usize = 4;
/// Most types one walk up a base list or out through enclosing types reads.
const MAX_LEVELS: usize = 64;

/// What `System.Object`, and through it every type, declares; `System.Enum` adds the rest. A
/// call of one of these the repository's types do not declare is a call of the standard library.
const OBJECT_METHODS: &[&str] = &[
    "Equals",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "ReferenceEquals",
    "ToString",
    "Finalize",
    "HasFlag",
    "CompareTo",
    "GetTypeCode",
];

/// Resolves the calls, base lists and type uses of `files`, passing each occurrence's outcome to
/// `record`, and returns the relationships to emit.
pub(super) fn resolve(
    index: &Index<'_>,
    files: &[&File],
    record: &mut OutcomeRecorder<'_>,
) -> Vec<ResolvedRelationship> {
    let resolver = Resolver::new(index);
    let mut out = Vec::new();
    for file in files {
        let Some(facts) = index.names.get(&file.id) else {
            continue;
        };
        if facts.calls.is_empty() && facts.members.is_empty() && facts.types.is_empty() {
            continue;
        }
        let scopes = index
            .file_owner_keys(&file.id)
            .into_iter()
            .map(|owner| resolver.scope(file, facts, owner, true))
            .collect::<Vec<_>>();
        let mut emitter = Emitter {
            index,
            file,
            record: &mut *record,
            out: &mut out,
        };
        for call in &facts.calls {
            let Some(caller) = call
                .caller
                .and_then(|caller| facts.members.get(caller as usize))
            else {
                continue;
            };
            let started = Instant::now();
            let answer = agree(scopes.iter().map(|scope| scope.call(call)).collect());
            let site = Site::Call(&call.range);
            let what = describe_call(call);
            emitter.emit(
                GraphEdgeType::Calls,
                &caller.symbol_id,
                site,
                &what,
                answer,
                started,
            );
        }
        for declared in &facts.types {
            let Some(place) = &declared.base_place else {
                continue;
            };
            for written in &declared.base_types {
                let started = Instant::now();
                let answers = scopes
                    .iter()
                    .map(|scope| scope.base(declared.kind, &declared.symbol_id, written, place))
                    .collect::<Vec<_>>();
                let Some(edge_type) = answers.iter().find_map(|(edge_type, _)| edge_type.clone())
                else {
                    continue;
                };
                let answer = agree(answers.into_iter().map(|(_, answer)| answer).collect());
                let what = format!("base type `{}`", written_name(written));
                emitter.emit(
                    edge_type,
                    &declared.symbol_id,
                    Site::Line(place.line),
                    &what,
                    answer,
                    started,
                );
            }
        }
        let mut uses = BTreeMap::<(SymbolId, SymbolId), (u32, Answer, String)>::new();
        for (source, written, place) in type_uses(facts) {
            let started = Instant::now();
            let answer = agree(
                scopes
                    .iter()
                    .map(|scope| scope.type_use(written, place))
                    .collect(),
            );
            let what = format!("type `{}`", written_name(written));
            let targets = match &answer {
                Answer::Found { targets, .. } | Answer::Ambiguous { targets, .. } => {
                    targets.iter().map(|(target, _)| target.clone()).collect()
                }
                _ => Vec::new(),
            };
            if targets.is_empty() {
                // Nothing to emit; the outcome is still reported.
                emitter.emit(
                    GraphEdgeType::UsesType,
                    source,
                    Site::Line(place.line),
                    &what,
                    answer,
                    started,
                );
                continue;
            }
            for target in targets {
                if &target == source {
                    continue;
                }
                let key = (source.clone(), target.clone());
                let narrowed = answer.narrowed_to(&target);
                match uses.get(&key) {
                    // A proven use, or the first, decides the edge; it is recorded at its line.
                    Some((_, known, _)) if known.is_proven() || !narrowed.is_proven() => {}
                    _ => {
                        uses.insert(key, (place.line, narrowed, what.clone()));
                    }
                }
            }
        }
        for ((source, _), (line, answer, what)) in uses {
            emitter.emit(
                GraphEdgeType::UsesType,
                &source,
                Site::Line(line),
                &what,
                answer,
                Instant::now(),
            );
        }
    }
    out
}

/// Every written type a file's members and locals use, with the member that uses it: each
/// parameter, return, property, field and event type, the parameters of a primary constructor
/// (used by the type), and each local whose declaration writes its type (used by the member
/// declaring it). Generic arguments and element types are uses of their own.
fn type_uses(facts: &CSharpNameFacts) -> Vec<(&SymbolId, &CSharpTypeRef, &CSharpPlace)> {
    let mut uses = Vec::new();
    for member in &facts.members {
        for parameter in &member.parameters {
            if let Some(written) = &parameter.value_type {
                flatten(written, &member.symbol_id, &member.place, &mut uses);
            }
        }
        if let Some(written) = &member.value_type {
            flatten(written, &member.symbol_id, &member.place, &mut uses);
        }
    }
    for local in &facts.locals {
        if local.kind == CSharpLocalKind::Parameter || !local.declared {
            continue;
        }
        let (Some(written), Some(owner)) = (
            &local.value_type,
            local
                .owner
                .and_then(|owner| facts.members.get(owner as usize)),
        ) else {
            continue;
        };
        flatten(written, &owner.symbol_id, &local.place, &mut uses);
    }
    uses
}

fn flatten<'f>(
    written: &'f CSharpTypeRef,
    source: &'f SymbolId,
    place: &'f CSharpPlace,
    uses: &mut Vec<(&'f SymbolId, &'f CSharpTypeRef, &'f CSharpPlace)>,
) {
    if !written.path.is_empty() {
        uses.push((source, written, place));
    }
    for argument in &written.arguments {
        flatten(argument, source, place, uses);
    }
}

fn written_name(written: &CSharpTypeRef) -> String {
    let path = written
        .path
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join(".");
    if path.is_empty() {
        "(unnamed)".into()
    } else {
        path
    }
}

fn describe_call(call: &CSharpCall) -> String {
    match call.kind {
        CSharpCallKind::Invocation => {
            let receiver = receiver_text(call);
            if receiver.is_empty() {
                format!("call `{}()`", call.name)
            } else {
                format!("call `{receiver}.{}()`", call.name)
            }
        }
        CSharpCallKind::Creation => match &call.created {
            Some(created) => format!("`new {}()`", written_name(created)),
            None => "target-typed `new()`".into(),
        },
        CSharpCallKind::BaseConstructor => "`base(..)` constructor call".into(),
        CSharpCallKind::ThisConstructor => "`this(..)` constructor call".into(),
    }
}

fn receiver_text(call: &CSharpCall) -> String {
    let root = match &call.receiver.root {
        CSharpReceiverRoot::None => String::new(),
        CSharpReceiverRoot::This => "this".into(),
        CSharpReceiverRoot::Base => "base".into(),
        CSharpReceiverRoot::Name(segment) => segment.name.clone(),
        CSharpReceiverRoot::Typed(written) => format!("({})", written_name(written)),
        CSharpReceiverRoot::Unknown => "(..)".into(),
    };
    std::iter::once(root)
        .chain(
            call.receiver
                .members
                .iter()
                .map(|member| member.name.clone()),
        )
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// How a proven target was reached, which decides its proofs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// A member of the receiver's type; `receiver` names how the receiver's type was proven.
    Member {
        receiver: &'static str,
    },
    /// A constructor of the created type; `implicit` when the type declares none and the edge
    /// goes to the type.
    Constructor {
        receiver: &'static str,
        implicit: bool,
    },
    LocalFunction,
    /// An extension method of a static class in scope whose `this` parameter takes the receiver.
    Extension,
    /// A static method of a type a `using static` imports.
    UsingStatic,
    /// A type the name lookup found: through a directive (its strategy) or in an enclosing
    /// namespace or type (`None`).
    Type {
        directive: Option<&'static str>,
    },
    /// A base list entry, through a directive or not.
    Base {
        directive: Option<&'static str>,
        interface: bool,
    },
}

/// What one occurrence resolves to.
#[derive(Debug, Clone, PartialEq)]
enum Answer {
    /// The targets (several only for the parts of one partial type), each with why it may not
    /// hold, when it may not.
    Found {
        targets: Vec<(SymbolId, Option<String>)>,
        route: Route,
    },
    /// Candidates of which C# would pick one the index cannot tell.
    Ambiguous {
        targets: Vec<(SymbolId, Option<String>)>,
        reason: String,
    },
    /// Outside the repository.
    External(String),
    /// Nothing the index can name.
    Unresolved(String),
}

impl Answer {
    fn found(targets: Vec<SymbolId>, route: Route, doubt: Option<String>) -> Self {
        Self::Found {
            targets: targets
                .into_iter()
                .map(|target| (target, doubt.clone()))
                .collect(),
            route,
        }
    }

    fn is_proven(&self) -> bool {
        matches!(self, Self::Found { targets, .. } if targets.iter().all(|(_, doubt)| doubt.is_none()))
    }

    /// Adds `doubt` to every target that has none.
    fn doubted(self, doubt: Option<String>) -> Self {
        let Some(doubt) = doubt else {
            return self;
        };
        match self {
            Self::Found { targets, route } => Self::Found {
                targets: targets
                    .into_iter()
                    .map(|(target, known)| (target, known.or_else(|| Some(doubt.clone()))))
                    .collect(),
                route,
            },
            other => other,
        }
    }

    /// The answer for one of its targets alone.
    fn narrowed_to(&self, target: &SymbolId) -> Self {
        match self {
            Self::Found { targets, route } => Self::Found {
                targets: targets
                    .iter()
                    .filter(|(known, _)| known == target)
                    .cloned()
                    .collect(),
                route: *route,
            },
            other => other.clone(),
        }
    }
}

/// The answers of the projects compiling a file, merged: one they all give, or else the first
/// that names a target, unproven, since another project's build does not reach it.
fn agree(answers: Vec<Answer>) -> Answer {
    let Some(first) = answers.first().cloned() else {
        return Answer::Unresolved("the file is compiled by no project".into());
    };
    if answers.iter().all(|answer| *answer == first) {
        return first;
    }
    let found = answers
        .into_iter()
        .find(|answer| matches!(answer, Answer::Found { .. }))
        .unwrap_or(first);
    found.doubted(Some(
        "the projects compiling the file resolve it differently".into(),
    ))
}

/// Where an occurrence is written: a call's range, or a declaration's line.
#[derive(Clone, Copy)]
enum Site<'s> {
    Call(&'s SourceRange),
    Line(u32),
}

struct Emitter<'e, 'r, 'a> {
    index: &'e Index<'a>,
    file: &'e File,
    record: &'e mut OutcomeRecorder<'r>,
    out: &'e mut Vec<ResolvedRelationship>,
}

impl Emitter<'_, '_, '_> {
    /// Records the outcome `answer` amounts to and emits its edges: a proven target with the
    /// proofs its route gives, an unproven or ambiguous one as a heuristic edge naming why.
    fn emit(
        &mut self,
        edge_type: GraphEdgeType,
        source: &SymbolId,
        site: Site<'_>,
        what: &str,
        answer: Answer,
        started: Instant,
    ) {
        let lines = match site {
            Site::Call(range) => LineRange {
                start: range.start_line,
                end: range.end_line,
            },
            Site::Line(line) => LineRange::single(line),
        };
        let file_range = FileRange {
            path: self.file.path.as_path().into(),
            line_range: Some(lines),
        };
        let call_site = match site {
            Site::Call(range) => Some(range.clone()),
            Site::Line(_) => None,
        };
        let name = |target: &SymbolId| {
            self.index
                .symbols
                .get(target)
                .map_or_else(|| target.0.clone(), |symbol| symbol.qualified_name.clone())
        };
        let evidence = |target: &SymbolId, message: String| ResolutionEvidence {
            kind: match edge_type {
                GraphEdgeType::Extends | GraphEdgeType::Implements => {
                    ResolutionEvidenceKind::InheritanceGraph
                }
                _ => ResolutionEvidenceKind::TypedBinding,
            },
            source_type: EvidenceSourceType::TreeSitter,
            file_range: Some(file_range.clone()),
            symbol_id: Some(target.clone()),
            message,
        };
        let relationship =
            |target: &SymbolId,
             confidence: Confidence,
             evidence: ResolutionEvidence,
             proofs: Vec<RelationshipProof>| ResolvedRelationship {
                from: source.clone(),
                to: target.clone(),
                edge_type: edge_type.clone(),
                confidence,
                call_site: call_site.clone(),
                evidence: vec![evidence],
                proofs,
            };
        let outcome = match answer {
            Answer::Found { targets, route } => {
                let mut candidates = Vec::new();
                let mut unproven = Vec::new();
                for (target, doubt) in targets {
                    let reached = format!("C# {what} reaches `{}`", name(&target));
                    match doubt {
                        None => {
                            let message = format!("{reached} ({})", route_text(route));
                            let proofs = proofs(&edge_type, route, source, &target, &file_range);
                            let mut candidate =
                                ResolutionCandidate::new(target.clone(), Confidence::Exact);
                            candidate.proofs = proofs.clone();
                            candidate.evidence.push(evidence(&target, message.clone()));
                            self.out.push(relationship(
                                &target,
                                Confidence::Exact,
                                evidence(&target, message),
                                proofs,
                            ));
                            candidates.push(candidate);
                        }
                        Some(doubt) => {
                            let message = format!("{reached}; unproven: {doubt}");
                            self.out.push(relationship(
                                &target,
                                Confidence::Medium,
                                evidence(&target, message.clone()),
                                Vec::new(),
                            ));
                            let mut candidate =
                                ResolutionCandidate::new(target.clone(), Confidence::Medium);
                            candidate.evidence.push(evidence(&target, message));
                            unproven.push((candidate, doubt));
                        }
                    }
                }
                match (candidates.into_iter().next(), unproven.first()) {
                    (Some(candidate), _) => ResolutionOutcome::Proven { candidate },
                    (None, Some((_, doubt))) => ResolutionOutcome::Unresolved {
                        reason: format!("unproven: {doubt}"),
                        candidates_considered: unproven.len(),
                        candidates: unproven
                            .into_iter()
                            .map(|(candidate, _)| candidate)
                            .collect(),
                    },
                    (None, None) => ResolutionOutcome::Unresolved {
                        candidates: Vec::new(),
                        reason: "no target".into(),
                        candidates_considered: 0,
                    },
                }
            }
            Answer::Ambiguous { targets, reason } => {
                let names = targets
                    .iter()
                    .map(|(target, _)| name(target))
                    .collect::<Vec<_>>();
                // A few candidates are kept as possibilities; many (an overload family of one
                // count) would only spread the call over every member of the family.
                let emit = targets.len() <= MAX_AMBIGUOUS_EDGES;
                let candidates = targets
                    .iter()
                    .map(|(target, _)| {
                        let message = format!(
                            "C# {what} may reach `{}`; ambiguous: {reason} ({})",
                            name(target),
                            names.join(", ")
                        );
                        if emit {
                            self.out.push(relationship(
                                target,
                                Confidence::Low,
                                evidence(target, message.clone()),
                                Vec::new(),
                            ));
                        }
                        let mut candidate =
                            ResolutionCandidate::new(target.clone(), Confidence::Low);
                        candidate.evidence.push(evidence(target, message));
                        candidate
                    })
                    .collect();
                ResolutionOutcome::Ambiguous {
                    candidates,
                    reason,
                    candidates_considered: targets.len(),
                    candidate_cap_hit: false,
                }
            }
            Answer::External(identity) => ResolutionOutcome::External {
                identity,
                evidence: Vec::new(),
            },
            Answer::Unresolved(reason) => ResolutionOutcome::Unresolved {
                candidates: Vec::new(),
                reason,
                candidates_considered: 0,
            },
        };
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        (self.record)(&edge_type, &outcome, micros);
    }
}

fn route_text(route: Route) -> &'static str {
    match route {
        Route::Member { receiver } => match receiver {
            "csharp_enclosing_type" => "a member of the enclosing type",
            "csharp_base_type" => "a member of the base type",
            "csharp_type_receiver" => "a static member of the named type",
            _ => "a member of the receiver's declared type",
        },
        Route::Constructor {
            implicit: false, ..
        } => "a constructor of the created type",
        Route::Constructor { implicit: true, .. } => {
            "the created type, which declares no constructor"
        }
        Route::LocalFunction => "a local function of the calling member",
        Route::Extension => {
            "an extension method in scope whose `this` parameter takes the receiver"
        }
        Route::UsingStatic => "a static method of a type a `using static` imports",
        Route::Type { directive: Some(_) } => "the type a `using` directive imports",
        Route::Type { directive: None } => "a type of an enclosing namespace or type",
        Route::Base { .. } => "the type the base list names",
    }
}

/// The proofs a proven target carries: a unique proof of each kind its route establishes.
fn proofs(
    edge_type: &GraphEdgeType,
    route: Route,
    source: &SymbolId,
    target: &SymbolId,
    range: &FileRange,
) -> Vec<RelationshipProof> {
    let proof = |kind: RelationshipProofKind, strategy: &str| {
        let mut proof = RelationshipProof::new(kind, strategy, 1);
        proof.source_range = Some(range.clone());
        proof.source_symbol_id = Some(source.clone());
        proof.target_symbol_id = Some(target.clone());
        proof
    };
    let mut proofs = Vec::new();
    if *edge_type == GraphEdgeType::Calls {
        proofs.push(proof(
            RelationshipProofKind::ExactCallSite,
            "csharp_call_site",
        ));
    }
    match route {
        Route::Member { receiver } => {
            proofs.push(proof(RelationshipProofKind::ReceiverType, receiver));
            proofs.push(proof(
                RelationshipProofKind::ContainingType,
                "csharp_member_of_receiver_type",
            ));
        }
        Route::Constructor { receiver, implicit } => {
            proofs.push(proof(RelationshipProofKind::ReceiverType, receiver));
            proofs.push(proof(
                RelationshipProofKind::ContainingType,
                if implicit {
                    "csharp_implicit_constructor"
                } else {
                    "csharp_constructor_of_created_type"
                },
            ));
        }
        Route::LocalFunction => proofs.push(proof(
            RelationshipProofKind::SameScopeDefinition,
            "csharp_local_function",
        )),
        Route::Extension => {
            proofs.push(proof(
                RelationshipProofKind::ImportBinding,
                "csharp_extension_class_in_scope",
            ));
            proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "csharp_extension_method",
            ));
            proofs.push(proof(
                RelationshipProofKind::ReceiverType,
                "csharp_extension_receiver",
            ));
        }
        Route::UsingStatic => {
            proofs.push(proof(
                RelationshipProofKind::ImportBinding,
                "csharp-using-static",
            ));
            proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "csharp_static_member",
            ));
        }
        Route::Type { directive } => {
            proofs.push(proof(
                RelationshipProofKind::ExactReference,
                "csharp_declared_type",
            ));
            if let Some(directive) = directive {
                proofs.push(proof(RelationshipProofKind::ImportBinding, directive));
            }
        }
        Route::Base {
            directive,
            interface,
        } => {
            proofs.push(proof(
                RelationshipProofKind::InheritanceBinding,
                "csharp_base_list",
            ));
            proofs.push(proof(
                RelationshipProofKind::QualifiedName,
                "csharp_base_type_lookup",
            ));
            if interface {
                proofs.push(proof(
                    RelationshipProofKind::TraitOrInterfaceBinding,
                    "csharp_interface_base",
                ));
            }
            if let Some(directive) = directive {
                proofs.push(proof(RelationshipProofKind::ImportBinding, directive));
            }
        }
    }
    proofs
}

/// One member of a type part.
#[derive(Clone, Copy)]
struct MemberRef<'a> {
    fact: &'a CSharpMember,
    file: &'a FileId,
}

/// What a type name resolves to.
#[derive(Debug, Clone, PartialEq)]
enum TypeAnswer {
    Group {
        group: usize,
        directive: Option<&'static str>,
        doubt: Option<String>,
    },
    Namespace(String),
    TypeParameter,
    Outside(String),
    Predefined,
    Ambiguous(Vec<usize>),
    Nothing,
}

/// A type's base class, when its base list names one.
#[derive(Debug, Clone, PartialEq)]
enum BaseClass {
    Repository(usize, Option<String>),
    /// An entry no repository type answers, which may be a class outside the repository.
    Outside,
}

#[derive(Debug, Default)]
struct Bases {
    class: Option<BaseClass>,
    interfaces: Vec<usize>,
    /// Some entry may be an interface outside the repository.
    outside_interfaces: bool,
}

/// The levels member lookup reads on a type, most derived first, and whether they are every
/// level C# reads (no base class or interface outside the repository).
struct Levels {
    groups: Vec<usize>,
    known: bool,
}

/// What a member lookup by name found on a type.
enum MemberFind<'a> {
    /// A property, field or event, of the type at `group`.
    Value(MemberRef<'a>, usize),
    /// A nested type.
    Type(usize),
    /// A method group, which a member access cannot continue through.
    Method,
    /// Nothing; `known` when every level was read.
    None { known: bool },
}

/// What a method lookup found.
enum MethodFind<'a> {
    /// The methods overload resolution chooses among, with the group declaring each.
    Found(Vec<(MemberRef<'a>, usize)>),
    /// The nearest member of the name is no method: invoking it invokes a delegate.
    NonMethod,
    /// No method of the name takes the call; `known` when every level was read, `named` when
    /// some method of the name exists.
    NotFound { known: bool, named: bool },
}

/// Which methods a receiver can call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Calls {
    Static,
    Instance,
    Any,
}

/// The receiver of a call, read so far.
#[derive(Debug, Clone)]
enum Receiver {
    /// A value of a repository type.
    Value {
        group: usize,
        how: &'static str,
        doubt: Option<String>,
    },
    /// A repository type, whose static members the call names.
    Type {
        group: usize,
        doubt: Option<String>,
    },
    Namespace(String),
    Outside(String),
    Unknown(String),
}

/// A file as one project (or none) compiles it.
type ProjectFile = (FileId, Option<usize>);
/// A member's declared type (or with `true`, its `this` parameter's) as a project compiles it.
type DeclaredTypeKey = (SymbolId, Option<usize>, bool);

struct Resolver<'i, 'a> {
    index: &'i Index<'a>,
    /// Members of each type group by name, every part's; constructors under the type's name.
    members: Vec<HashMap<&'a str, Vec<MemberRef<'a>>>>,
    /// Each type group's kind, from the parts the parser read.
    kinds: Vec<Option<CSharpTypeKind>>,
    /// Each type group by (part symbol, owning project).
    group_of: HashMap<(&'i SymbolId, Option<usize>), usize>,
    /// Extension methods of top-level, non-generic static classes, by name.
    extensions: HashMap<&'a str, Vec<(usize, MemberRef<'a>)>>,
    /// Local functions by file and name.
    local_functions: HashMap<(&'a FileId, &'a str), Vec<&'a CSharpMember>>,
    lookups: RefCell<HashMap<ProjectFile, Rc<Lookup<'i, 'a>>>>,
    bases: RefCell<HashMap<usize, Rc<Bases>>>,
    levels: RefCell<HashMap<usize, Rc<Levels>>>,
    /// Resolved member and `this` parameter types by member symbol and owning project.
    value_types: RefCell<HashMap<DeclaredTypeKey, TypeAnswer>>,
}

impl<'i, 'a> Resolver<'i, 'a> {
    fn new(index: &'i Index<'a>) -> Self {
        let mut member_facts = HashMap::<&SymbolId, MemberRef<'a>>::new();
        let mut primaries = HashMap::<&SymbolId, MemberRef<'a>>::new();
        let mut local_functions = HashMap::<(&'a FileId, &'a str), Vec<&'a CSharpMember>>::new();
        for (file, facts) in index.names.iter() {
            for member in &facts.members {
                let reference = MemberRef { fact: member, file };
                match member.kind {
                    CSharpMemberKind::PrimaryConstructor => {
                        primaries.insert(&member.symbol_id, reference);
                    }
                    CSharpMemberKind::LocalFunction => {
                        local_functions
                            .entry((file, member.name.as_str()))
                            .or_default()
                            .push(member);
                    }
                    _ => {
                        member_facts.insert(&member.symbol_id, reference);
                    }
                }
            }
        }
        let mut members = Vec::with_capacity(index.groups.len());
        let mut kinds = Vec::with_capacity(index.groups.len());
        let mut group_of = HashMap::new();
        let mut extensions = HashMap::<&'a str, Vec<(usize, MemberRef<'a>)>>::new();
        for (id, group) in index.groups.iter().enumerate() {
            let mut by_name = HashMap::<&'a str, Vec<MemberRef<'a>>>::new();
            let mut kind = None;
            let mut is_static = false;
            let mut arity = 0;
            for (part, _) in &group.parts {
                group_of.insert((part, group.owner), id);
                if let Some(declared) = index.declarations.get(part) {
                    kind.get_or_insert(declared.kind);
                    is_static |= declared.is_static;
                    arity = declared.arity;
                }
                if let Some(primary) = primaries.get(part) {
                    by_name
                        .entry(primary.fact.name.as_str())
                        .or_default()
                        .push(*primary);
                }
                for child in index.children.get(part).into_iter().flatten() {
                    if let Some(member) = member_facts.get(&child.id) {
                        by_name
                            .entry(member.fact.name.as_str())
                            .or_default()
                            .push(*member);
                    }
                }
            }
            let top_level = group.parts.iter().all(|(part, _)| {
                index
                    .symbols
                    .get(part)
                    .and_then(|symbol| symbol.parent_symbol_id.as_ref())
                    .and_then(|parent| index.symbols.get(parent))
                    .is_none_or(|parent| parent.kind == SymbolKind::Package)
            });
            if is_static && arity == 0 && top_level {
                for (name, list) in &by_name {
                    for member in list {
                        let extension = member.fact.kind == CSharpMemberKind::Method
                            && member.fact.is_static
                            && member
                                .fact
                                .parameters
                                .first()
                                .is_some_and(|first| first.is_this);
                        if extension {
                            extensions.entry(name).or_default().push((id, *member));
                        }
                    }
                }
            }
            members.push(by_name);
            kinds.push(kind);
        }
        Self {
            index,
            members,
            kinds,
            group_of,
            extensions,
            local_functions,
            lookups: RefCell::new(HashMap::new()),
            bases: RefCell::new(HashMap::new()),
            levels: RefCell::new(HashMap::new()),
            value_types: RefCell::new(HashMap::new()),
        }
    }

    fn lookup(
        &self,
        file: &'a File,
        facts: &'a CSharpNameFacts,
        owner: Option<usize>,
    ) -> Rc<Lookup<'i, 'a>> {
        let key = (file.id.clone(), owner);
        if let Some(known) = self.lookups.borrow().get(&key) {
            return known.clone();
        }
        let lookup = Rc::new(Lookup::new(self.index, file, facts, owner));
        self.lookups.borrow_mut().insert(key, lookup.clone());
        lookup
    }

    /// The lookups of `file` as `owner` compiles it; with `locals`, its variables are read too,
    /// for the calls written in it.
    fn scope<'s>(
        &'s self,
        file: &'a File,
        facts: &'a CSharpNameFacts,
        owner: Option<usize>,
        locals: bool,
    ) -> Scope<'s, 'i, 'a> {
        let mut by_name = HashMap::<&'a str, Vec<&'a CSharpLocal>>::new();
        if locals {
            for local in &facts.locals {
                by_name.entry(local.name.as_str()).or_default().push(local);
            }
        }
        let mut doubts = Vec::new();
        if owner.is_none() {
            doubts.push("the file belongs to no project file the index read");
        }
        if self.index.conditionally_owned.contains(&file.id) {
            doubts.push(
                "a `Compile` item a `Condition` guards decides whether a project compiles the file",
            );
        }
        if facts.hidden_directives {
            doubts.push("an error node of the file may hide a `using` directive");
        }
        Scope {
            resolver: self,
            file,
            facts,
            owner,
            lookup: self.lookup(file, facts, owner),
            locals: by_name,
            file_doubt: (!doubts.is_empty()).then(|| doubts.join("; ")),
            types: RefCell::new(HashMap::new()),
        }
    }

    fn file_of(&self, id: &FileId) -> Option<(&'a File, &'a CSharpNameFacts)> {
        let file = *self.index.files.get(id)?;
        let facts = self.index.names.get(id)?;
        Some((file, facts))
    }

    /// The group of `group`'s type as `owner` compiles it: the same group when `owner` owns it.
    fn kind(&self, group: usize) -> Option<CSharpTypeKind> {
        self.kinds.get(group).copied().flatten()
    }

    /// The identity of a type across the projects compiling it: its least part symbol, which
    /// every project's group of the type holds.
    fn identity(&self, group: usize) -> Option<&'i SymbolId> {
        self.index.groups[group]
            .parts
            .iter()
            .map(|(part, _)| part)
            .min()
    }

    fn parts(&self, group: usize) -> Vec<SymbolId> {
        let mut parts = self.index.groups[group]
            .parts
            .iter()
            .map(|(part, _)| part.clone())
            .collect::<Vec<_>>();
        parts.sort();
        parts.dedup();
        parts
    }

    /// The type enclosing `group`'s type, as the same project compiles it.
    fn outer(&self, group: usize) -> Option<usize> {
        let owner = self.index.groups[group].owner;
        self.index.groups[group].parts.iter().find_map(|(part, _)| {
            let parent = self.index.symbols.get(part)?.parent_symbol_id.as_ref()?;
            self.group_of.get(&(parent, owner)).copied()
        })
    }

    /// The bases a type's base lists name, read in each part's file as the type's project
    /// compiles it.
    fn bases(&self, group: usize) -> Rc<Bases> {
        if let Some(known) = self.bases.borrow().get(&group) {
            return known.clone();
        }
        // A cycle reads as no base while it is being read.
        self.bases
            .borrow_mut()
            .insert(group, Rc::new(Bases::default()));
        let kind = self.kind(group);
        let class_like = matches!(kind, Some(CSharpTypeKind::Class | CSharpTypeKind::Record));
        let owner = self.index.groups[group].owner;
        let mut bases = Bases::default();
        for (part, file) in &self.index.groups[group].parts {
            let Some(declared) = self.index.declarations.get(part) else {
                continue;
            };
            let Some(place) = &declared.base_place else {
                continue;
            };
            let Some((file, facts)) = self.file_of(file) else {
                continue;
            };
            let scope = self.scope(file, facts, owner, false);
            for (position, written) in declared.base_types.iter().enumerate() {
                match scope.resolve_type(written, place) {
                    TypeAnswer::Group {
                        group: base, doubt, ..
                    } if base != group => {
                        if self.kind(base) == Some(CSharpTypeKind::Interface) {
                            if !bases.interfaces.contains(&base) {
                                bases.interfaces.push(base);
                            }
                        } else if class_like && position == 0 {
                            bases.class = Some(BaseClass::Repository(base, doubt));
                        }
                    }
                    _ if class_like && position == 0 => {
                        if bases.class.is_none() {
                            bases.class = Some(BaseClass::Outside);
                        }
                    }
                    _ => bases.outside_interfaces = true,
                }
            }
        }
        let bases = Rc::new(bases);
        self.bases.borrow_mut().insert(group, bases.clone());
        bases
    }

    /// The types member lookup reads on `group`, most derived first.
    fn levels(&self, group: usize) -> Rc<Levels> {
        if let Some(known) = self.levels.borrow().get(&group) {
            return known.clone();
        }
        let levels = Rc::new(self.read_levels(group));
        self.levels.borrow_mut().insert(group, levels.clone());
        levels
    }

    fn read_levels(&self, group: usize) -> Levels {
        let mut groups = vec![group];
        let mut known = true;
        match self.kind(group) {
            Some(CSharpTypeKind::Interface) => {
                let mut index = 0;
                while index < groups.len() && groups.len() < MAX_LEVELS {
                    let bases = self.bases(groups[index]);
                    known &= !bases.outside_interfaces;
                    for base in &bases.interfaces {
                        if !groups.contains(base) {
                            groups.push(*base);
                        }
                    }
                    index += 1;
                }
            }
            Some(CSharpTypeKind::Class | CSharpTypeKind::Record) => {
                let mut current = group;
                while groups.len() < MAX_LEVELS {
                    match &self.bases(current).class {
                        Some(BaseClass::Repository(base, _)) if !groups.contains(base) => {
                            groups.push(*base);
                            current = *base;
                        }
                        Some(BaseClass::Outside) => {
                            known = false;
                            break;
                        }
                        _ => break,
                    }
                }
            }
            Some(_) => {}
            None => known = false,
        }
        Levels { groups, known }
    }

    /// Every repository type a value of `group`'s type converts to by reference: itself, its
    /// base classes and every interface they implement, by identity.
    fn ancestors(&self, group: usize) -> HashSet<&'i SymbolId> {
        let mut seen = HashSet::new();
        let mut identities = HashSet::new();
        let mut pending = vec![group];
        while let Some(current) = pending.pop() {
            if !seen.insert(current) || seen.len() > MAX_LEVELS {
                continue;
            }
            if let Some(identity) = self.identity(current) {
                identities.insert(identity);
            }
            let bases = self.bases(current);
            if let Some(BaseClass::Repository(base, _)) = &bases.class {
                pending.push(*base);
            }
            pending.extend(bases.interfaces.iter().copied());
        }
        identities
    }

    /// Finds a member named `name` on `group` for a member access: a property, field or event
    /// gives its type, a nested type itself.
    fn find_member(&self, group: usize, name: &str) -> MemberFind<'a> {
        let levels = self.levels(group);
        for &level in &levels.groups {
            if let Some(list) = self.members[level].get(name) {
                if let Some(value) = list.iter().find(|member| member.fact.kind.holds_value()) {
                    return MemberFind::Value(*value, level);
                }
                if list.iter().any(|member| member.fact.kind.is_method()) {
                    return MemberFind::Method;
                }
            }
            if let Some(nested) = self.nested_type(level, name, 0) {
                return MemberFind::Type(nested);
            }
        }
        MemberFind::None {
            known: levels.known,
        }
    }

    /// A type nested in `group`'s type named `name` with `arity` type parameters.
    fn nested_type(&self, group: usize, name: &str, arity: u16) -> Option<usize> {
        let owner = self.index.groups[group].owner;
        self.index.groups[group].parts.iter().find_map(|(part, _)| {
            self.index
                .children
                .get(part)
                .into_iter()
                .flatten()
                .filter(|child| {
                    matches!(child.kind, SymbolKind::Class | SymbolKind::Interface)
                        && child.name == name
                        && self
                            .index
                            .declarations
                            .get(&child.id)
                            .is_some_and(|declared| declared.arity == arity)
                })
                .find_map(|child| self.group_of.get(&(&child.id, owner)).copied())
        })
    }

    /// Finds the methods named `name` a call with `arguments` arguments can bind to on
    /// `group`'s type: across its levels, those overload resolution can pick, less any a more
    /// derived `override` of the same parameter count takes the place of.
    fn find_methods(
        &self,
        group: usize,
        name: &str,
        arguments: u16,
        type_arguments: Option<u16>,
        calls: Calls,
        from: Option<usize>,
    ) -> MethodFind<'a> {
        let levels = self.levels(group);
        let mut found = Vec::new();
        let mut overridden = HashSet::<usize>::new();
        let mut named = false;
        for &level in &levels.groups {
            let Some(list) = self.members[level].get(name) else {
                continue;
            };
            let methods = list
                .iter()
                .filter(|member| member.fact.kind == CSharpMemberKind::Method)
                .collect::<Vec<_>>();
            if methods.is_empty() {
                // The nearest member of the name is a property, field or event.
                if !named && list.iter().any(|member| member.fact.kind.holds_value()) {
                    return MethodFind::NonMethod;
                }
                continue;
            }
            named = true;
            let mut overrides = Vec::new();
            for member in methods {
                let fact = member.fact;
                if fact.is_override {
                    overrides.push(fact.parameters.len());
                }
                if overridden.contains(&fact.parameters.len()) {
                    continue;
                }
                let kind_matches = match calls {
                    Calls::Static => fact.is_static,
                    Calls::Instance => !fact.is_static,
                    Calls::Any => true,
                };
                if kind_matches
                    && applicable(fact, arguments, type_arguments, false)
                    && self.reachable(fact, level, from)
                {
                    found.push((*member, level));
                }
            }
            overridden.extend(overrides);
        }
        if found.is_empty() {
            MethodFind::NotFound {
                known: levels.known,
                named,
            }
        } else {
            MethodFind::Found(without_partial_declarations(found))
        }
    }

    /// Whether code in `from`'s type can call `member` of `level`'s type: a `private` member
    /// only from inside the type declaring it, nested types included, and an `internal` one
    /// only from its own project and its friends. Overload resolution reads only the members the
    /// call can reach.
    fn reachable(&self, member: &CSharpMember, level: usize, from: Option<usize>) -> bool {
        let visibility = self
            .index
            .symbols
            .get(&member.symbol_id)
            .map(|symbol| symbol.visibility);
        if visibility == Some(open_kioku_core::Visibility::Crate) {
            let declaring = self.index.groups[level].owner;
            let calling = from.and_then(|group| self.index.groups[group].owner);
            return match (declaring, calling) {
                (Some(declaring), Some(calling)) => {
                    declaring == calling
                        || self.index.projects[declaring]
                            .friends
                            .contains(&self.index.projects[calling].model.assembly_name)
                }
                _ => true,
            };
        }
        if visibility != Some(open_kioku_core::Visibility::Private) {
            return true;
        }
        let Some(declaring) = self.identity(level) else {
            return false;
        };
        let mut current = from;
        for _ in 0..MAX_LEVELS {
            let Some(group) = current else {
                return false;
            };
            if self.identity(group) == Some(declaring) {
                return true;
            }
            current = self.outer(group);
        }
        false
    }

    /// The constructors of `group`'s type a creation with `arguments` arguments can call, or the
    /// type itself when it declares none (or, for a struct, none without parameters) and the
    /// creation passes none.
    fn constructors(
        &self,
        group: usize,
        arguments: u16,
        receiver: &'static str,
        doubt: Option<String>,
        from: Option<usize>,
    ) -> Answer {
        let kind = self.kind(group);
        if matches!(
            kind,
            Some(CSharpTypeKind::Interface | CSharpTypeKind::Enum | CSharpTypeKind::Delegate)
        ) {
            return Answer::Unresolved(
                "an interface, enum or delegate is created by no constructor of the repository"
                    .into(),
            );
        }
        let qualified = &self.index.groups[group].qualified;
        let name = qualified.rsplit("::").next().unwrap_or(qualified);
        let constructors = self.members[group]
            .get(name)
            .into_iter()
            .flatten()
            .filter(|member| {
                matches!(
                    member.fact.kind,
                    CSharpMemberKind::Constructor | CSharpMemberKind::PrimaryConstructor
                ) && !member.fact.is_static
            })
            .collect::<Vec<_>>();
        let applicable = constructors
            .iter()
            .filter(|member| {
                applicable(member.fact, arguments, None, false)
                    && self.reachable(member.fact, group, from)
            })
            .collect::<Vec<_>>();
        let doubt = doubt.or_else(|| self.group_doubt(group));
        match applicable.as_slice() {
            [member] => Answer::found(
                vec![member.fact.symbol_id.clone()],
                Route::Constructor {
                    receiver,
                    implicit: false,
                },
                doubt.or_else(|| member_doubt(member.fact)),
            ),
            [] if arguments == 0
                && (constructors.is_empty() || kind.is_some_and(CSharpTypeKind::is_value_type)) =>
            {
                Answer::found(
                    self.parts(group),
                    Route::Constructor {
                        receiver,
                        implicit: true,
                    },
                    doubt,
                )
            }
            [] => Answer::Unresolved(format!(
                "no constructor of `{}` takes {arguments} arguments",
                qualified.replace("::", ".")
            )),
            several => Answer::Ambiguous {
                targets: several
                    .iter()
                    .map(|member| (member.fact.symbol_id.clone(), None))
                    .collect(),
                reason: format!(
                    "{} constructors of `{}` take {arguments} arguments",
                    several.len(),
                    qualified.replace("::", ".")
                ),
            },
        }
    }

    /// Why an edge to a member or part of `group`'s type may not hold on every build: its parts
    /// may not be the type the compiler sees.
    fn group_doubt(&self, group: usize) -> Option<String> {
        self.index.groups[group].inexact.map(str::to_string)
    }

    /// The type a member holds (a property, field or event), resolved where the member is
    /// declared, as `owner` compiles it.
    fn member_type(&self, member: MemberRef<'a>, owner: Option<usize>) -> TypeAnswer {
        self.declared_type(member, owner, false)
    }

    /// The type a member declares, or with `this_parameter` the type of an extension method's
    /// `this` parameter.
    fn declared_type(
        &self,
        member: MemberRef<'a>,
        owner: Option<usize>,
        this_parameter: bool,
    ) -> TypeAnswer {
        let key = (member.fact.symbol_id.clone(), owner, this_parameter);
        if let Some(known) = self.value_types.borrow().get(&key) {
            return known.clone();
        }
        let written = if this_parameter {
            member
                .fact
                .parameters
                .first()
                .and_then(|first| first.value_type.as_ref())
        } else {
            member.fact.value_type.as_ref()
        };
        let answer = match (written, self.file_of(member.file)) {
            (Some(written), Some((file, facts))) => {
                let scope = self.scope(file, facts, owner, false);
                let answer = scope.resolve_type(written, &member.fact.place);
                // `T?` of a struct is `Nullable<T>`, whose members are not the struct's.
                match answer {
                    TypeAnswer::Group { group, .. }
                        if written.shape == CSharpTypeShape::Nullable
                            && self.kind(group).is_some_and(CSharpTypeKind::is_value_type) =>
                    {
                        TypeAnswer::Nothing
                    }
                    other => other,
                }
            }
            _ => TypeAnswer::Nothing,
        };
        self.value_types.borrow_mut().insert(key, answer.clone());
        answer
    }
}

/// Keeps one of a `partial` method's declaration and implementation: the one with a body.
fn without_partial_declarations<'a>(
    found: Vec<(MemberRef<'a>, usize)>,
) -> Vec<(MemberRef<'a>, usize)> {
    let implemented = found
        .iter()
        .filter(|(member, _)| member.fact.has_body)
        .map(|(member, _)| (member.fact.name.as_str(), member.fact.parameters.len()))
        .collect::<HashSet<_>>();
    found
        .iter()
        .filter(|(member, _)| {
            member.fact.has_body
                || !implemented.contains(&(member.fact.name.as_str(), member.fact.parameters.len()))
        })
        .copied()
        .collect()
}

/// Why an edge to `member` may not hold on every build.
fn member_doubt(member: &CSharpMember) -> Option<String> {
    member
        .place
        .conditional
        .then(|| "the target is declared under `#if`".to_string())
}

/// Whether a call passing `arguments` arguments (and `type_arguments`, when it writes them) can
/// bind to `member`, counting the receiver of an extension call as its `this` argument.
fn applicable(
    member: &CSharpMember,
    arguments: u16,
    type_arguments: Option<u16>,
    extension: bool,
) -> bool {
    if type_arguments.is_some_and(|count| count != member.type_parameters) {
        return false;
    }
    let parameters = if extension {
        member.parameters.get(1..).unwrap_or_default()
    } else {
        member.parameters.as_slice()
    };
    let required = parameters
        .iter()
        .filter(|parameter| !parameter.optional && !parameter.is_params)
        .count();
    let params = parameters.iter().any(|parameter| parameter.is_params);
    let count = usize::from(arguments);
    count >= required && (params || count <= parameters.len())
}

fn contains(scope: &SourceRange, at: &SourceRange) -> bool {
    (scope.start_line, scope.start_column) <= (at.start_line, at.start_column)
        && (at.start_line, at.start_column) <= (scope.end_line, scope.end_column)
}

fn span(range: &SourceRange) -> (u32, u32) {
    (
        range.end_line - range.start_line,
        if range.end_line == range.start_line {
            range.end_column.saturating_sub(range.start_column)
        } else {
            range.end_column
        },
    )
}

/// One file's lookups as one project compiles it.
struct Scope<'s, 'i, 'a> {
    resolver: &'s Resolver<'i, 'a>,
    file: &'a File,
    facts: &'a CSharpNameFacts,
    owner: Option<usize>,
    lookup: Rc<Lookup<'i, 'a>>,
    locals: HashMap<&'a str, Vec<&'a CSharpLocal>>,
    /// Why no name lookup in the file is settled, when one is not.
    file_doubt: Option<String>,
    /// What each written type name answers where it is written, read once.
    types: RefCell<HashMap<TypeKey, TypeAnswer>>,
}

/// A written type's name and where it is written: all its answer depends on.
type TypeKey = (u32, Option<u32>, bool, bool, Vec<CSharpTypeSegment>);

impl<'a> Scope<'_, '_, 'a> {
    fn index(&self) -> &Index<'a> {
        self.resolver.index
    }

    /// The type group of the type declaration `place` is written in.
    fn enclosing(&self, place: &CSharpPlace) -> Option<usize> {
        let declared = self.facts.types.get(place.enclosing? as usize)?;
        self.resolver
            .group_of
            .get(&(&declared.symbol_id, self.owner))
            .copied()
    }

    fn call(&self, call: &CSharpCall) -> Answer {
        let answer = match call.kind {
            CSharpCallKind::Creation => self.creation(call),
            CSharpCallKind::BaseConstructor | CSharpCallKind::ThisConstructor => {
                self.initializer(call)
            }
            CSharpCallKind::Invocation => self.invocation(call),
        };
        answer.doubted(
            call.place
                .conditional
                .then(|| "the call is written under `#if`".to_string()),
        )
    }

    fn creation(&self, call: &CSharpCall) -> Answer {
        let Some(written) = &call.created else {
            return Answer::Unresolved(
                "a target-typed `new()` whose declaration writes no type".into(),
            );
        };
        match self.resolve_type(written, &call.place) {
            TypeAnswer::Group { group, doubt, .. } => self.resolver.constructors(
                group,
                call.arguments,
                "csharp_created_type",
                doubt.or_else(|| self.file_doubt.clone()),
                self.enclosing(&call.place),
            ),
            other => type_miss(other),
        }
    }

    fn initializer(&self, call: &CSharpCall) -> Answer {
        let Some(group) = self.enclosing(&call.place) else {
            return Answer::Unresolved("the constructor's type is not one the index read".into());
        };
        if call.kind == CSharpCallKind::ThisConstructor {
            return self.resolver.constructors(
                group,
                call.arguments,
                "csharp_enclosing_type",
                None,
                Some(group),
            );
        }
        match &self.resolver.bases(group).class {
            Some(BaseClass::Repository(base, doubt)) => self.resolver.constructors(
                *base,
                call.arguments,
                "csharp_base_type",
                doubt.clone().or_else(|| self.file_doubt.clone()),
                Some(group),
            ),
            _ => Answer::External("a constructor of a base type outside the repository".into()),
        }
    }

    fn invocation(&self, call: &CSharpCall) -> Answer {
        let receiver = &call.receiver;
        let plain = receiver.members.is_empty();
        match &receiver.root {
            CSharpReceiverRoot::None if plain => self.bare(call),
            CSharpReceiverRoot::This if plain => match self.enclosing(&call.place) {
                Some(group) => self.on_value(group, call, "csharp_enclosing_type", None, true),
                None => Answer::Unresolved("`this` of a type the index did not read".into()),
            },
            CSharpReceiverRoot::Base if plain => match self.base_class(&call.place) {
                Ok((group, doubt)) => self.on_value(group, call, "csharp_base_type", doubt, false),
                Err(answer) => answer,
            },
            _ => match self.receiver(call) {
                Receiver::Value { group, how, doubt } => {
                    self.on_value(group, call, how, doubt, true)
                }
                Receiver::Type { group, doubt } => self.on_type(group, call, doubt),
                Receiver::Namespace(namespace) => {
                    Answer::Unresolved(format!("`{namespace}` is a namespace, not a type"))
                }
                Receiver::Outside(identity) => Answer::External(identity),
                Receiver::Unknown(reason) => Answer::Unresolved(reason),
            },
        }
    }

    /// The base class of the type `place` is written in, when the repository declares it.
    fn base_class(&self, place: &CSharpPlace) -> Result<(usize, Option<String>), Answer> {
        let Some(group) = self.enclosing(place) else {
            return Err(Answer::Unresolved(
                "`base` of a type the index did not read".into(),
            ));
        };
        match &self.resolver.bases(group).class {
            Some(BaseClass::Repository(base, doubt)) => Ok((*base, doubt.clone())),
            Some(BaseClass::Outside) => Err(Answer::External(
                "a member of a base type outside the repository".into(),
            )),
            None => Err(Answer::External("a member of `object`".into())),
        }
    }

    /// A bare `M(..)`: a local function of the calling member, then a method of the enclosing
    /// types from the innermost out, then a static method a `using static` imports.
    fn bare(&self, call: &CSharpCall) -> Answer {
        let name = call.name.as_str();
        if self.visible_local(name, &call.range).is_some() {
            return Answer::Unresolved(format!(
                "`{name}` is a variable; the call invokes the delegate it holds"
            ));
        }
        let caller = call
            .caller
            .and_then(|caller| self.facts.members.get(caller as usize));
        if let Some(caller) = caller {
            let functions = self.local_functions(name, &caller.symbol_id);
            match functions.as_slice() {
                [function] => {
                    return Answer::found(
                        vec![function.symbol_id.clone()],
                        Route::LocalFunction,
                        member_doubt(function),
                    )
                }
                [] => {}
                several => {
                    return Answer::Ambiguous {
                        targets: several
                            .iter()
                            .map(|function| (function.symbol_id.clone(), None))
                            .collect(),
                        reason: format!("{} local functions are named `{name}`", several.len()),
                    }
                }
            }
        }
        let mut current = self.enclosing(&call.place);
        let mut levels = 0;
        while let Some(group) = current {
            levels += 1;
            if levels > MAX_LEVELS {
                break;
            }
            match self.resolver.find_methods(
                group,
                name,
                call.arguments,
                call.type_arguments,
                Calls::Any,
                self.enclosing(&call.place),
            ) {
                MethodFind::Found(found) => {
                    return self.overloads(
                        found,
                        Route::Member {
                            receiver: "csharp_enclosing_type",
                        },
                        None,
                        name,
                        call.arguments,
                    )
                }
                MethodFind::NonMethod => {
                    return Answer::Unresolved(format!(
                        "`{name}` is a property, field or event; the call invokes the delegate it holds"
                    ))
                }
                MethodFind::NotFound { known: false, .. } => {
                    return Answer::Unresolved(format!(
                        "a base type outside the repository may declare `{name}`"
                    ))
                }
                MethodFind::NotFound { named: true, .. } => {
                    return Answer::Unresolved(format!(
                        "no method `{name}` of the enclosing type takes {} arguments",
                        call.arguments
                    ))
                }
                MethodFind::NotFound { .. } => current = self.resolver.outer(group),
            }
        }
        if OBJECT_METHODS.contains(&name) {
            return Answer::External(format!("System.Object.{name}"));
        }
        self.using_static(call)
    }

    /// The local functions named `name` declared in the member `caller` is, or is inside.
    fn local_functions(&self, name: &str, caller: &SymbolId) -> Vec<&'a CSharpMember> {
        let outermost = |id: &SymbolId| -> SymbolId {
            let mut current = id.clone();
            for _ in 0..MAX_LEVELS {
                let Some(symbol) = self.index().symbols.get(&current) else {
                    break;
                };
                match (&symbol.kind, &symbol.parent_symbol_id) {
                    (SymbolKind::Function, Some(parent)) => current = parent.clone(),
                    _ => break,
                }
            }
            current
        };
        let member = outermost(caller);
        self.resolver
            .local_functions
            .get(&(&self.file.id, name))
            .into_iter()
            .flatten()
            .filter(|function| outermost(&function.symbol_id) == member)
            .copied()
            .collect()
    }

    /// A static method of a type a `using static` in scope imports.
    fn using_static(&self, call: &CSharpCall) -> Answer {
        let name = call.name.as_str();
        let lookup = &self.lookup;
        let mut groups = Vec::new();
        let mut outside = None;
        let mut conditional = false;
        let mut consider =
            |target: &str, arity: u16, block_namespace: &str, is_conditional: bool| match lookup
                .alias_target(target, arity, block_namespace)
            {
                AliasTarget::Type(found) => {
                    conditional |= is_conditional;
                    groups.extend(found);
                }
                AliasTarget::Outside(namespace) => {
                    outside.get_or_insert(namespace);
                }
                AliasTarget::Namespace(_) | AliasTarget::Hidden => {}
            };
        for block in lookup.block_chain(call.place.block) {
            let block_namespace = self.facts.blocks[block].namespace.as_str();
            for using in &lookup.locals[block] {
                if using.kind == CSharpUsingKind::Static && using.extern_alias.is_none() {
                    consider(
                        &using.target,
                        using.arity,
                        block_namespace,
                        using.conditional,
                    );
                }
            }
        }
        for using in lookup.globals.iter() {
            if using.kind == CSharpUsingKind::Static && !using.extern_alias {
                consider(&using.target, using.arity, "", using.conditional);
            }
        }
        let mut found = Vec::new();
        for group in groups {
            if let MethodFind::Found(methods) = self.resolver.find_methods(
                group,
                name,
                call.arguments,
                call.type_arguments,
                Calls::Static,
                self.enclosing(&call.place),
            ) {
                for method in methods {
                    if !found.iter().any(|(known, _): &(MemberRef<'a>, usize)| {
                        known.fact.symbol_id == method.0.fact.symbol_id
                    }) {
                        found.push(method);
                    }
                }
            }
        }
        if found.is_empty() {
            return match outside {
                Some(namespace) => Answer::External(format!("a static member of `{namespace}`")),
                None => Answer::Unresolved(format!("no method `{name}` is in scope")),
            };
        }
        let doubt = outside
            .map(|namespace| {
                format!(
                    "a `using static` of `{namespace}`, outside the repository, may supply `{name}` too"
                )
            })
            .or_else(|| {
                conditional.then(|| "the `using static` that imports it is under `#if` or a `Condition`".to_string())
            })
            .or_else(|| self.file_doubt.clone());
        self.overloads(found, Route::UsingStatic, doubt, name, call.arguments)
    }

    /// What a member lookup on a value's type gives a call, extension methods last.
    fn on_value(
        &self,
        group: usize,
        call: &CSharpCall,
        how: &'static str,
        doubt: Option<String>,
        extensions: bool,
    ) -> Answer {
        let name = call.name.as_str();
        let doubt = doubt.or_else(|| self.lookup_doubt(how));
        match self.resolver.find_methods(
            group,
            name,
            call.arguments,
            call.type_arguments,
            Calls::Instance,
            self.enclosing(&call.place),
        ) {
            MethodFind::Found(found) => self.overloads(
                found,
                Route::Member { receiver: how },
                doubt,
                name,
                call.arguments,
            ),
            MethodFind::NonMethod => Answer::Unresolved(format!(
                "`{name}` is a property, field or event; the call invokes the delegate it holds"
            )),
            MethodFind::NotFound { known, .. } => {
                if OBJECT_METHODS.contains(&name) {
                    return Answer::External(format!("System.Object.{name}"));
                }
                if !extensions {
                    return Answer::Unresolved(format!("no method `{name}` takes the call"));
                }
                let doubt = doubt.or_else(|| {
                    (!known).then(|| {
                        format!(
                            "the receiver's type has a base type outside the repository, which may declare `{name}`"
                        )
                    })
                });
                self.extension(group, call, doubt)
            }
        }
    }

    /// A doubt every answer through a name lookup carries, which a member of the enclosing type
    /// itself does not need.
    fn lookup_doubt(&self, how: &str) -> Option<String> {
        (how != "csharp_enclosing_type")
            .then(|| self.file_doubt.clone())
            .flatten()
    }

    fn on_type(&self, group: usize, call: &CSharpCall, doubt: Option<String>) -> Answer {
        let name = call.name.as_str();
        let doubt = doubt.or_else(|| self.file_doubt.clone());
        match self.resolver.find_methods(
            group,
            name,
            call.arguments,
            call.type_arguments,
            Calls::Static,
            self.enclosing(&call.place),
        ) {
            MethodFind::Found(found) => self.overloads(
                found,
                Route::Member {
                    receiver: "csharp_type_receiver",
                },
                doubt,
                name,
                call.arguments,
            ),
            MethodFind::NonMethod => Answer::Unresolved(format!(
                "`{name}` is a property, field or event; the call invokes the delegate it holds"
            )),
            MethodFind::NotFound { known: false, .. } => Answer::Unresolved(format!(
                "a base type outside the repository may declare `{name}`"
            )),
            MethodFind::NotFound { .. } if OBJECT_METHODS.contains(&name) => {
                Answer::External(format!("System.Object.{name}"))
            }
            MethodFind::NotFound { .. } => {
                Answer::Unresolved(format!("no static method `{name}` takes the call"))
            }
        }
    }

    /// Overload resolution's answer among `found`.
    fn overloads(
        &self,
        found: Vec<(MemberRef<'a>, usize)>,
        route: Route,
        doubt: Option<String>,
        name: &str,
        arguments: u16,
    ) -> Answer {
        match found.as_slice() {
            [(member, group)] => Answer::found(
                vec![member.fact.symbol_id.clone()],
                route,
                doubt
                    .or_else(|| self.resolver.group_doubt(*group))
                    .or_else(|| member_doubt(member.fact))
                    .or_else(|| self.conditional_reference(*group)),
            ),
            several => Answer::Ambiguous {
                targets: several
                    .iter()
                    .map(|(member, _)| (member.fact.symbol_id.clone(), None))
                    .collect(),
                reason: format!(
                    "{} overloads of `{name}` take {arguments} arguments",
                    several.len()
                ),
            },
        }
    }

    /// The type's project is referenced only under an MSBuild `Condition`.
    fn conditional_reference(&self, group: usize) -> Option<String> {
        let owner = self.index().groups[group].owner?;
        let project = self.owner?;
        self.index().projects[project]
            .conditionally_visible
            .contains(&owner)
            .then(|| "the type's project is referenced only under a `Condition`".to_string())
    }

    /// An extension method a static class in scope declares whose `this` parameter takes a
    /// value of `group`'s type. C# reads the namespaces enclosing the call from the innermost
    /// out, and at each the static classes of that namespace and of the namespaces its
    /// directives import; the first that has applicable candidates decides.
    fn extension(&self, group: usize, call: &CSharpCall, doubt: Option<String>) -> Answer {
        let name = call.name.as_str();
        let candidates = self
            .resolver
            .extensions
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(class, member)| {
                applicable(member.fact, call.arguments, call.type_arguments, true)
                    && self
                        .resolver
                        .reachable(member.fact, *class, self.enclosing(&call.place))
                    && self.index().accessible(
                        &self.index().groups[*class],
                        self.owner,
                        &self.file.id,
                    )
                    && self.index().groups[*class].owner.is_none_or(|owner| {
                        self.owner.is_none_or(|project| {
                            self.index().projects[project].visible.contains(&owner)
                        })
                    })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Answer::Unresolved(format!(
                "no method or extension method `{name}` takes {} arguments",
                call.arguments
            ));
        }
        let ancestors = self.resolver.ancestors(group);
        let lookup = &self.lookup;
        let chain = lookup.block_chain(call.place.block);
        let innermost = chain
            .first()
            .map(|&block| self.facts.blocks[block].namespace.as_str())
            .unwrap_or_default();
        let mut nearer_outside: Option<String> = None;
        for level in namespace_levels(innermost) {
            let block = chain
                .iter()
                .copied()
                .find(|&block| self.facts.blocks[block].namespace == level);
            let mut imported = HashSet::new();
            let mut static_types = HashSet::new();
            let mut outside_here = None;
            let mut directives = Vec::new();
            if let Some(block) = block {
                for using in &lookup.locals[block] {
                    directives.push((
                        using.kind,
                        using.target.as_str(),
                        using.arity,
                        using.extern_alias.is_some(),
                    ));
                }
            }
            if level.is_empty() {
                for using in lookup.globals.iter() {
                    directives.push((
                        using.kind,
                        using.target.as_str(),
                        using.arity,
                        using.extern_alias,
                    ));
                }
            }
            for (kind, target, arity, extern_alias) in directives {
                if extern_alias {
                    outside_here.get_or_insert_with(|| target.to_string());
                    continue;
                }
                match kind {
                    CSharpUsingKind::Namespace => {
                        let namespace = lookup.directive_namespace(target, &level);
                        match self.index().namespace_state(&namespace, self.owner) {
                            NamespaceState::Visible => {
                                imported.insert(namespace);
                            }
                            NamespaceState::Outside => {
                                outside_here.get_or_insert(namespace);
                            }
                            NamespaceState::Hidden => {}
                        }
                    }
                    CSharpUsingKind::Static => match lookup.alias_target(target, arity, &level) {
                        AliasTarget::Type(found) => {
                            static_types.extend(
                                found
                                    .iter()
                                    .filter_map(|&found| self.resolver.identity(found)),
                            );
                        }
                        AliasTarget::Outside(namespace) => {
                            outside_here.get_or_insert(namespace);
                        }
                        _ => {}
                    },
                    CSharpUsingKind::Alias => {}
                }
            }
            let in_scope = candidates
                .iter()
                .filter(|(class, _)| {
                    let class_group = &self.index().groups[*class];
                    class_group.namespace == level
                        || imported.contains(&class_group.namespace)
                        || self
                            .resolver
                            .identity(*class)
                            .is_some_and(|identity| static_types.contains(identity))
                })
                .collect::<Vec<_>>();
            let mut compatible = Vec::new();
            let mut maybe = 0usize;
            for (class, member) in in_scope {
                let owner = self.index().groups[*class].owner;
                match self.resolver.declared_type(*member, owner, true) {
                    TypeAnswer::Group { group: this, .. } => {
                        if self
                            .resolver
                            .identity(this)
                            .is_some_and(|identity| ancestors.contains(identity))
                        {
                            compatible.push((*member, *class));
                        }
                    }
                    _ => maybe += 1,
                }
            }
            if compatible.is_empty() && maybe == 0 {
                if let Some(namespace) = outside_here {
                    nearer_outside.get_or_insert(namespace);
                }
                continue;
            }
            if maybe > 0 {
                let targets = compatible
                    .iter()
                    .map(|(member, _)| (member.fact.symbol_id.clone(), None))
                    .collect::<Vec<_>>();
                let reason = format!(
                    "{maybe} extension methods `{name}` in scope take a generic or outside `this` type the index cannot match to the receiver"
                );
                return if targets.is_empty() {
                    Answer::Unresolved(reason)
                } else {
                    Answer::Ambiguous { targets, reason }
                };
            }
            let doubt = doubt.or_else(|| {
                nearer_outside.map(|namespace| {
                    format!(
                        "a nearer `using` imports `{namespace}`, outside the repository, which may declare an extension method `{name}`"
                    )
                })
            }).or_else(|| self.file_doubt.clone());
            return self.overloads(compatible, Route::Extension, doubt, name, call.arguments);
        }
        Answer::Unresolved(format!(
            "no extension method `{name}` in scope takes the receiver's type"
        ))
    }

    /// The variable named `name` in scope at `at`: the one whose scope is innermost.
    fn visible_local(&self, name: &str, at: &SourceRange) -> Option<&'a CSharpLocal> {
        self.locals
            .get(name)?
            .iter()
            .filter(|local| contains(&local.scope, at))
            .min_by_key(|local| span(&local.scope))
            .copied()
    }

    /// The receiver of an invocation: its root, then each member access after it.
    fn receiver(&self, call: &CSharpCall) -> Receiver {
        let mut current = match &call.receiver.root {
            CSharpReceiverRoot::Name(segment) => self.simple_receiver(segment, call),
            CSharpReceiverRoot::Typed(written) => match self.resolve_type(written, &call.place) {
                TypeAnswer::Group { group, doubt, .. } => {
                    if written.shape == CSharpTypeShape::Nullable
                        && self
                            .resolver
                            .kind(group)
                            .is_some_and(CSharpTypeKind::is_value_type)
                    {
                        Receiver::Unknown(
                            "a nullable value type's members are `Nullable<T>`'s".into(),
                        )
                    } else {
                        Receiver::Value {
                            group,
                            how: "csharp_written_receiver_type",
                            doubt,
                        }
                    }
                }
                other => miss_receiver(other),
            },
            CSharpReceiverRoot::This => match self.enclosing(&call.place) {
                Some(group) => Receiver::Value {
                    group,
                    how: "csharp_enclosing_type",
                    doubt: None,
                },
                None => Receiver::Unknown("`this` of a type the index did not read".into()),
            },
            CSharpReceiverRoot::Base => match self.base_class(&call.place) {
                Ok((group, doubt)) => Receiver::Value {
                    group,
                    how: "csharp_base_type",
                    doubt,
                },
                Err(Answer::External(identity)) => Receiver::Outside(identity),
                Err(_) => Receiver::Unknown("`base` of a type the index did not read".into()),
            },
            CSharpReceiverRoot::None | CSharpReceiverRoot::Unknown => Receiver::Unknown(
                "the receiver is an expression whose type the index does not read".into(),
            ),
        };
        for member in &call.receiver.members {
            current = self.member_step(current, member);
        }
        current
    }

    /// A simple name before a member access: a variable in scope, a member of the enclosing
    /// types (their repository bases' included), then a type or namespace the name lookup finds.
    fn simple_receiver(&self, segment: &CSharpTypeSegment, call: &CSharpCall) -> Receiver {
        let name = segment.name.as_str();
        if segment.arity == 0 {
            if let Some(local) = self.visible_local(name, &call.range) {
                let Some(written) = &local.value_type else {
                    return Receiver::Unknown(format!(
                        "`{name}` is a variable whose declaration writes no type"
                    ));
                };
                let how = if local.declared {
                    "csharp_declared_variable_type"
                } else {
                    "csharp_initialized_variable_type"
                };
                return match self.resolve_type(written, &local.place) {
                    TypeAnswer::Group { group, doubt, .. } => {
                        if written.shape == CSharpTypeShape::Nullable
                            && self
                                .resolver
                                .kind(group)
                                .is_some_and(CSharpTypeKind::is_value_type)
                        {
                            Receiver::Unknown(
                                "a nullable value type's members are `Nullable<T>`'s".into(),
                            )
                        } else {
                            Receiver::Value { group, how, doubt }
                        }
                    }
                    other => miss_receiver(other),
                };
            }
        }
        let mut current = self.enclosing(&call.place);
        let mut outside_member = false;
        let mut levels = 0;
        while let Some(group) = current {
            levels += 1;
            if levels > MAX_LEVELS {
                break;
            }
            match self.resolver.find_member(group, name) {
                MemberFind::Value(member, level) => {
                    return self.value_of_member(member, level, None);
                }
                MemberFind::Type(nested) => {
                    return Receiver::Type {
                        group: nested,
                        doubt: None,
                    }
                }
                MemberFind::Method => {
                    return Receiver::Unknown(format!("`{name}` is a method group"))
                }
                MemberFind::None { known: false } => {
                    outside_member = true;
                    break;
                }
                MemberFind::None { known: true } => current = self.resolver.outer(group),
            }
        }
        let inherited = outside_member.then(|| {
            format!(
                "a member `{name}` inherited from a base type outside the repository would take the name first"
            )
        });
        match self.resolve_simple(segment, &call.place) {
            TypeAnswer::Group { group, doubt, .. } => Receiver::Type {
                group,
                doubt: inherited.or(doubt),
            },
            TypeAnswer::Namespace(namespace) if !outside_member => Receiver::Namespace(namespace),
            TypeAnswer::Outside(identity) if !outside_member => Receiver::Outside(identity),
            _ if outside_member => Receiver::Unknown(format!(
                "`{name}` may be a member inherited from a base type outside the repository"
            )),
            other => miss_receiver(other),
        }
    }

    fn value_of_member(
        &self,
        member: MemberRef<'a>,
        level: usize,
        doubt: Option<String>,
    ) -> Receiver {
        let owner = self.index().groups[level].owner;
        let doubt = doubt.or_else(|| member_doubt(member.fact));
        match self.resolver.member_type(member, owner) {
            TypeAnswer::Group {
                group,
                doubt: found,
                ..
            } => Receiver::Value {
                group,
                how: "csharp_member_declared_type",
                doubt: doubt.or(found),
            },
            other => miss_receiver(other),
        }
    }

    /// One member access after a receiver read so far.
    fn member_step(&self, current: Receiver, segment: &CSharpTypeSegment) -> Receiver {
        let name = segment.name.as_str();
        match current {
            Receiver::Value { group, doubt, .. } | Receiver::Type { group, doubt } => {
                match self.resolver.find_member(group, name) {
                    MemberFind::Value(member, level) => self.value_of_member(member, level, doubt),
                    MemberFind::Type(nested) => Receiver::Type {
                        group: nested,
                        doubt,
                    },
                    MemberFind::Method => Receiver::Unknown(format!("`{name}` is a method group")),
                    MemberFind::None { known: false } => Receiver::Unknown(format!(
                        "a base type outside the repository may declare `{name}`"
                    )),
                    MemberFind::None { known: true } => Receiver::Unknown(format!(
                        "no member `{name}` of the receiver's type holds a value"
                    )),
                }
            }
            Receiver::Namespace(namespace) => match self.in_namespace(&namespace, segment) {
                TypeAnswer::Group { group, doubt, .. } => Receiver::Type { group, doubt },
                TypeAnswer::Namespace(inner) => Receiver::Namespace(inner),
                TypeAnswer::Outside(identity) => Receiver::Outside(identity),
                other => miss_receiver(other),
            },
            other => other,
        }
    }

    /// The type a written type names, as C#'s lookup finds it where `place` is.
    fn resolve_type(&self, written: &CSharpTypeRef, place: &CSharpPlace) -> TypeAnswer {
        if !matches!(
            written.shape,
            CSharpTypeShape::Named | CSharpTypeShape::Nullable
        ) || written.extern_alias
        {
            return self.read_type(written, place);
        }
        let key = (
            place.block,
            place.enclosing,
            place.conditional,
            written.global,
            written.path.clone(),
        );
        if let Some(known) = self.types.borrow().get(&key) {
            return known.clone();
        }
        let answer = self.read_type(written, place);
        self.types.borrow_mut().insert(key, answer.clone());
        answer
    }

    fn read_type(&self, written: &CSharpTypeRef, place: &CSharpPlace) -> TypeAnswer {
        if written.extern_alias {
            return TypeAnswer::Outside(written_name(written));
        }
        match written.shape {
            CSharpTypeShape::Predefined => return TypeAnswer::Predefined,
            CSharpTypeShape::Other => return TypeAnswer::Nothing,
            CSharpTypeShape::Named | CSharpTypeShape::Nullable => {}
        }
        let Some((first, rest)) = written.path.split_first() else {
            return TypeAnswer::Nothing;
        };
        let mut current = if written.global {
            self.in_namespace("", first)
        } else {
            self.resolve_simple(first, place)
        };
        for segment in rest {
            current = match current {
                TypeAnswer::Namespace(namespace) => self.in_namespace(&namespace, segment),
                TypeAnswer::Group {
                    group,
                    directive,
                    doubt,
                } => match self
                    .resolver
                    .nested_type(group, &segment.name, segment.arity)
                {
                    Some(nested) => TypeAnswer::Group {
                        group: nested,
                        directive,
                        doubt,
                    },
                    None => TypeAnswer::Nothing,
                },
                other => other,
            };
        }
        match current {
            TypeAnswer::Namespace(_) => TypeAnswer::Nothing,
            TypeAnswer::Group {
                group,
                directive,
                doubt,
            } => TypeAnswer::Group {
                group,
                directive,
                doubt: doubt.or_else(|| {
                    place
                        .conditional
                        .then(|| "the name is written under `#if`".to_string())
                }),
            },
            other => other,
        }
    }

    /// The first segment of a written type: a type nested in an enclosing type (or its
    /// repository bases), then what the name lookup finds.
    fn resolve_simple(&self, segment: &CSharpTypeSegment, place: &CSharpPlace) -> TypeAnswer {
        let mut current = self.enclosing(place);
        let mut levels = 0;
        while let Some(group) = current {
            levels += 1;
            if levels > MAX_LEVELS {
                break;
            }
            for &level in &self.resolver.levels(group).groups {
                if let Some(nested) = self
                    .resolver
                    .nested_type(level, &segment.name, segment.arity)
                {
                    return TypeAnswer::Group {
                        group: nested,
                        directive: None,
                        doubt: None,
                    };
                }
            }
            current = self.resolver.outer(group);
        }
        let use_ = CSharpNameUse {
            block: place.block,
            enclosing_type: place
                .enclosing
                .and_then(|index| self.facts.types.get(index as usize))
                .map(|declared| declared.symbol_id.clone()),
            name: segment.name.clone(),
            arity: segment.arity,
            next: None,
            next_arity: 0,
            context: CSharpNameContext::Type,
            line: place.line,
            conditional: place.conditional,
        };
        match self.lookup.find(&use_) {
            Found::Declared(Some(group)) => TypeAnswer::Group {
                group,
                directive: None,
                doubt: self.file_doubt.clone(),
            },
            Found::Declared(None) => TypeAnswer::Ambiguous(Vec::new()),
            Found::Imported { group, via, doubt } => TypeAnswer::Group {
                group,
                directive: Some(via.strategy()),
                doubt: doubt.or_else(|| self.file_doubt.clone()),
            },
            Found::Namespace(namespace) => TypeAnswer::Namespace(namespace),
            Found::Ambiguous { candidates } => {
                TypeAnswer::Ambiguous(candidates.into_iter().map(|(group, _)| group).collect())
            }
            Found::Outside { namespace } => TypeAnswer::Outside(namespace),
            Found::Shadowed => TypeAnswer::TypeParameter,
            Found::Nothing => TypeAnswer::Nothing,
        }
    }

    /// A segment read inside a namespace: a namespace nested in it, or a type it declares.
    fn in_namespace(&self, namespace: &str, segment: &CSharpTypeSegment) -> TypeAnswer {
        let full = join_namespace(namespace, &segment.name);
        if segment.arity == 0
            && self.index().namespace_state(&full, self.owner) == NamespaceState::Visible
        {
            return TypeAnswer::Namespace(full);
        }
        let groups = self.index().types_in(
            namespace,
            &segment.name,
            segment.arity,
            self.owner,
            &self.file.id,
        );
        match groups.as_slice() {
            [group] => TypeAnswer::Group {
                group: *group,
                directive: None,
                doubt: self.file_doubt.clone(),
            },
            [] if namespace.is_empty()
                || self.index().namespace_state(namespace, self.owner)
                    == NamespaceState::Outside =>
            {
                TypeAnswer::Outside(full)
            }
            [] => TypeAnswer::Nothing,
            several => TypeAnswer::Ambiguous(several.to_vec()),
        }
    }

    /// A written type a member or local uses: an edge to each part of the type it names.
    fn type_use(&self, written: &CSharpTypeRef, place: &CSharpPlace) -> Answer {
        match self.resolve_type(written, place) {
            TypeAnswer::Group {
                group,
                directive,
                doubt,
            } => self.type_targets(group, Route::Type { directive }, doubt),
            other => type_miss(other),
        }
    }

    /// Each part of `group`'s type with why an edge to it may not hold.
    fn type_targets(&self, group: usize, route: Route, doubt: Option<String>) -> Answer {
        let group_ref: &TypeGroup = &self.index().groups[group];
        let targets = self
            .resolver
            .parts(group)
            .into_iter()
            .map(|part| {
                let file = group_ref
                    .parts
                    .iter()
                    .find(|(symbol, _)| *symbol == part)
                    .map(|(_, file)| file.clone());
                let target_doubt = doubt
                    .clone()
                    .or_else(|| file.and_then(|file| self.lookup.target_doubt(group_ref, &file)));
                (part, target_doubt)
            })
            .collect();
        Answer::Found { targets, route }
    }

    /// The edge a base list entry gives the type `child` declares: `EXTENDS` to a base class or
    /// an interface's base interface, `IMPLEMENTS` to a class's or struct's interface.
    fn base(
        &self,
        kind: CSharpTypeKind,
        child: &SymbolId,
        written: &CSharpTypeRef,
        place: &CSharpPlace,
    ) -> (Option<GraphEdgeType>, Answer) {
        let answer = self.resolve_type(written, place);
        let TypeAnswer::Group {
            group,
            directive,
            doubt,
        } = answer
        else {
            return (None, type_miss(answer));
        };
        let target = self.resolver.kind(group);
        let edge = match (kind, target) {
            (CSharpTypeKind::Interface, Some(CSharpTypeKind::Interface)) => GraphEdgeType::Extends,
            (
                CSharpTypeKind::Class
                | CSharpTypeKind::Record
                | CSharpTypeKind::Struct
                | CSharpTypeKind::RecordStruct,
                Some(CSharpTypeKind::Interface),
            ) => GraphEdgeType::Implements,
            (CSharpTypeKind::Class, Some(CSharpTypeKind::Class))
            | (CSharpTypeKind::Record, Some(CSharpTypeKind::Record)) => GraphEdgeType::Extends,
            _ => return (None, Answer::Unresolved("no base type C# allows".into())),
        };
        let interface = edge == GraphEdgeType::Implements;
        let mut answer = self.type_targets(
            group,
            Route::Base {
                directive,
                interface,
            },
            doubt,
        );
        if let Answer::Found { targets, .. } = &mut answer {
            targets.retain(|(target, _)| target != child);
        }
        (Some(edge), answer)
    }
}

/// The answer a type that is no repository type gives.
fn type_miss(answer: TypeAnswer) -> Answer {
    match answer {
        TypeAnswer::Outside(identity) => Answer::External(identity),
        TypeAnswer::Predefined => Answer::External("a predefined type".into()),
        TypeAnswer::TypeParameter => Answer::Unresolved("a type parameter".into()),
        TypeAnswer::Ambiguous(groups) => Answer::Unresolved(format!(
            "{} types of the name are in scope; C# would pick none",
            groups.len().max(2)
        )),
        TypeAnswer::Namespace(namespace) => {
            Answer::Unresolved(format!("`{namespace}` is a namespace"))
        }
        TypeAnswer::Nothing | TypeAnswer::Group { .. } => {
            Answer::Unresolved("no type of the repository or outside it answers the name".into())
        }
    }
}

fn miss_receiver(answer: TypeAnswer) -> Receiver {
    match type_miss(answer) {
        Answer::External(identity) => Receiver::Outside(identity),
        Answer::Unresolved(reason) => Receiver::Unknown(reason),
        _ => Receiver::Unknown("the receiver's type is not one the index read".into()),
    }
}

#[cfg(test)]
mod tests {
    use crate::{IndexSnapshot, Indexer};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{
        relationship_authority, GraphEdgeType, IndexMode, Language, RelationshipAuthority,
    };

    const SDK: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";

    fn index(files: &[(&str, &str)]) -> IndexSnapshot {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for (path, content) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, content).expect("write");
        }
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Full)
            .expect("index")
    }

    /// An SDK project at the root holding `files`.
    fn project(files: &[(&'static str, &'static str)]) -> IndexSnapshot {
        let mut all = vec![("App.csproj", SDK)];
        all.extend_from_slice(files);
        index(&all)
    }

    /// `from -> to` of every relationship of `edge_type`, by qualified name with `::` read as
    /// `.`, then `proven`, `unproven: <reason>` or `ambiguous`.
    fn edges(snapshot: &IndexSnapshot, edge_type: GraphEdgeType) -> Vec<String> {
        let name = |id: &open_kioku_core::SymbolId| {
            let symbol = snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.id == *id)
                .expect("an edge joins indexed symbols");
            let line = symbol.range.as_ref().map_or(0, |range| range.start);
            format!("{}@{line}", symbol.qualified_name.replace("::", "."))
        };
        let mut edges = snapshot
            .resolved_relationships
            .iter()
            .filter(|relationship| relationship.edge_type == edge_type)
            .filter(|relationship| {
                snapshot.symbols.iter().any(|symbol| {
                    symbol.id == relationship.from && symbol.language == Language::CSharp
                })
            })
            .map(|relationship| {
                let message = relationship
                    .evidence
                    .first()
                    .map(|evidence| evidence.message.as_str())
                    .unwrap_or_default();
                let status = if relationship_authority(&edge_type, &relationship.proofs)
                    == RelationshipAuthority::Authoritative
                {
                    "proven".to_string()
                } else if let Some((_, reason)) = message.split_once("; unproven: ") {
                    format!("unproven: {reason}")
                } else if message.contains("ambiguous") {
                    "ambiguous".to_string()
                } else {
                    format!("heuristic: {message}")
                };
                format!(
                    "{} -> {} {status}",
                    name(&relationship.from),
                    name(&relationship.to)
                )
            })
            .collect::<Vec<_>>();
        edges.sort();
        edges.dedup();
        edges
    }

    fn calls(snapshot: &IndexSnapshot) -> Vec<String> {
        edges(snapshot, GraphEdgeType::Calls)
    }

    /// The calls of every type but `STORE`'s `Entry`.
    fn calls_into_store(snapshot: &IndexSnapshot) -> Vec<String> {
        calls(snapshot)
            .into_iter()
            .filter(|edge| !edge.starts_with("Acme.Ledger.Entry."))
            .collect()
    }

    const STORE: &str = "namespace Acme.Ledger;\n\
        public class Entry\n\
        {\n\
        \x20   public void Save() { }\n\
        \x20   public void Save(int times) { }\n\
        \x20   public Entry Next { get; set; }\n\
        \x20   public static Entry Create() => new Entry();\n\
        }\n";

    #[test]
    fn calls_through_typed_receivers_reach_the_declared_types_member() {
        let snapshot = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Book.cs",
                "namespace Acme.Ledger;\n\
                 public class Book\n\
                 {\n\
                 \x20   private readonly Entry _first = new();\n\
                 \x20   public Entry Last { get; set; }\n\
                 \x20   public void Post(Entry entry)\n\
                 \x20   {\n\
                 \x20       entry.Save();\n\
                 \x20       _first.Save(2);\n\
                 \x20       Last.Next.Save();\n\
                 \x20       this.Last.Save();\n\
                 \x20       var made = new Entry();\n\
                 \x20       made.Save();\n\
                 \x20       Entry typed = Entry.Create();\n\
                 \x20       typed.Save();\n\
                 \x20       var unknown = Entry.Create();\n\
                 \x20       unknown.Save();\n\
                 \x20       ((Entry)entry).Save(1);\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Ledger.Book.Post@6 -> Acme.Ledger.Entry.Create@7 proven",
                "Acme.Ledger.Book.Post@6 -> Acme.Ledger.Entry.Save@4 proven",
                "Acme.Ledger.Book.Post@6 -> Acme.Ledger.Entry.Save@5 proven",
                "Acme.Ledger.Book.Post@6 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Book._first@4 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Entry.Create@7 -> Acme.Ledger.Entry@2 proven",
            ]
        );
        // `unknown`, typed by what `Create()` returns, and `entry.Save()` beside it are not
        // told apart by the edge set: check the `var` from a call proves nothing on its own.
        let unknown = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Book.cs",
                "namespace Acme.Ledger;\n\
                 public class Book\n\
                 {\n\
                 \x20   public void Post()\n\
                 \x20   {\n\
                 \x20       var unknown = Entry.Create();\n\
                 \x20       unknown.Save();\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&unknown),
            vec![
                "Acme.Ledger.Book.Post@4 -> Acme.Ledger.Entry.Create@7 proven",
                "Acme.Ledger.Entry.Create@7 -> Acme.Ledger.Entry@2 proven",
            ]
        );
    }

    #[test]
    fn this_base_and_bare_calls_reach_the_enclosing_types_and_their_partial_parts() {
        let snapshot = project(&[
            (
                "src/Base.cs",
                "namespace Acme;\n\
                 public class Base\n\
                 {\n\
                 \x20   protected Base(int seed) { }\n\
                 \x20   public virtual void Flush() { }\n\
                 \x20   public void Reset() { }\n\
                 }\n",
            ),
            (
                "src/Book.cs",
                "namespace Acme;\n\
                 public partial class Book : Base\n\
                 {\n\
                 \x20   public Book() : this(1) { }\n\
                 \x20   public Book(int seed) : base(seed) { }\n\
                 \x20   public override void Flush()\n\
                 \x20   {\n\
                 \x20       base.Flush();\n\
                 \x20       Audit();\n\
                 \x20       this.Reset();\n\
                 \x20       Flush();\n\
                 \x20   }\n\
                 }\n",
            ),
            (
                "src/Book.Audit.cs",
                "namespace Acme;\n\
                 public partial class Book\n\
                 {\n\
                 \x20   private void Audit() { }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Book.Book@4 -> Acme.Book.Book@5 proven",
                "Acme.Book.Book@5 -> Acme.Base.Base@4 proven",
                "Acme.Book.Flush@6 -> Acme.Base.Flush@5 proven",
                "Acme.Book.Flush@6 -> Acme.Base.Reset@6 proven",
                "Acme.Book.Flush@6 -> Acme.Book.Audit@4 proven",
                "Acme.Book.Flush@6 -> Acme.Book.Flush@6 proven",
            ]
        );
    }

    #[test]
    fn static_calls_through_simple_qualified_and_nested_type_names() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            (
                "src/Ledger/Rules.cs",
                "namespace Acme.Ledger;\n\
                 public static class Rules\n\
                 {\n\
                 \x20   public static int Apply() => 1;\n\
                 \x20   public static class Limits { public static int Max() => 2; }\n\
                 }\n\
                 public class Tally\n\
                 {\n\
                 \x20   public static int Count(int seed) => seed;\n\
                 \x20   public int Count(string label) => 0;\n\
                 }\n",
            ),
            (
                "src/Web/Web.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"..\\Ledger\\Ledger.csproj\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Web/Page.cs",
                "using Acme.Ledger;\n\
                 namespace Acme.Web;\n\
                 public class Page\n\
                 {\n\
                 \x20   public int Render() => Rules.Apply() + Acme.Ledger.Rules.Apply() + Rules.Limits.Max() + Tally.Count(1);\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Web.Page.Render@5 -> Acme.Ledger.Rules.Apply@4 proven",
                "Acme.Web.Page.Render@5 -> Acme.Ledger.Rules.Limits.Max@5 proven",
                "Acme.Web.Page.Render@5 -> Acme.Ledger.Tally.Count@9 proven",
            ]
        );
    }

    #[test]
    fn constructors_are_chosen_by_argument_count_and_target_typed_new_by_the_declaration() {
        let snapshot = project(&[
            (
                "src/Entry.cs",
                "namespace Acme;\n\
                 public class Entry\n\
                 {\n\
                 \x20   public Entry() { }\n\
                 \x20   public Entry(int amount, string note = \"\") { }\n\
                 }\n\
                 public record Money(decimal Amount);\n\
                 public class Plain { }\n",
            ),
            (
                "src/Book.cs",
                "namespace Acme;\n\
                 public class Book\n\
                 {\n\
                 \x20   private Entry _empty = new();\n\
                 \x20   public void Post()\n\
                 \x20   {\n\
                 \x20       var one = new Entry(1);\n\
                 \x20       Entry two = new(1, \"n\");\n\
                 \x20       var money = new Money(2m);\n\
                 \x20       var plain = new Plain();\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Book.Post@5 -> Acme.Entry.Entry@5 proven",
                "Acme.Book.Post@5 -> Acme.Money@7 proven",
                "Acme.Book.Post@5 -> Acme.Plain@8 proven",
                "Acme.Book._empty@4 -> Acme.Entry.Entry@4 proven",
            ]
        );
    }

    #[test]
    fn overloads_the_argument_count_cannot_tell_apart_are_ambiguous() {
        let snapshot = project(&[
            (
                "src/Entry.cs",
                "namespace Acme;\n\
                 public class Entry\n\
                 {\n\
                 \x20   public void Save(int times) { }\n\
                 \x20   public void Save(string note) { }\n\
                 \x20   private void Save(int a, int b) { }\n\
                 \x20   public void Save(long a, long b) { }\n\
                 }\n",
            ),
            (
                "src/Book.cs",
                "namespace Acme;\n\
                 public class Book\n\
                 {\n\
                 \x20   public void Post(Entry entry)\n\
                 \x20   {\n\
                 \x20       entry.Save(1);\n\
                 \x20       entry.Save(1, 2);\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        // The private two-argument overload is out of the caller's reach.
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Book.Post@4 -> Acme.Entry.Save@4 ambiguous",
                "Acme.Book.Post@4 -> Acme.Entry.Save@5 ambiguous",
                "Acme.Book.Post@4 -> Acme.Entry.Save@7 proven",
            ]
        );
    }

    #[test]
    fn extension_methods_need_their_class_in_scope_and_a_matching_receiver() {
        let extensions = "namespace Acme.Extensions;\n\
            public static class EntryExtensions\n\
            {\n\
            \x20   public static void Archive(this Acme.Ledger.Entry entry) { }\n\
            \x20   public static void Stamp(this Acme.Ledger.Other other) { }\n\
            }\n";
        let types = "namespace Acme.Ledger;\n\
            public class Entry { }\n\
            public class Special : Entry { }\n\
            public class Other { }\n";
        let in_scope = project(&[
            ("src/Types.cs", types),
            ("src/Extensions.cs", extensions),
            (
                "src/Book.cs",
                "using Acme.Extensions;\n\
                 using Acme.Ledger;\n\
                 namespace Acme.Web;\n\
                 public class Book\n\
                 {\n\
                 \x20   public void Post(Entry entry, Special special)\n\
                 \x20   {\n\
                 \x20       entry.Archive();\n\
                 \x20       special.Archive();\n\
                 \x20       entry.Stamp();\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls(&in_scope),
            vec!["Acme.Web.Book.Post@6 -> Acme.Extensions.EntryExtensions.Archive@4 proven"]
        );
        let out_of_scope = project(&[
            ("src/Types.cs", types),
            ("src/Extensions.cs", extensions),
            (
                "src/Book.cs",
                "using Acme.Ledger;\n\
                 namespace Acme.Web;\n\
                 public class Book\n\
                 {\n\
                 \x20   public void Post(Entry entry) => entry.Archive();\n\
                 }\n",
            ),
        ]);
        assert_eq!(calls(&out_of_scope), Vec::<String>::new());
    }

    #[test]
    fn local_functions_take_a_bare_call_before_the_types_methods() {
        let snapshot = project(&[(
            "src/Book.cs",
            "namespace Acme;\n\
             public class Book\n\
             {\n\
             \x20   private void Total() { }\n\
             \x20   public void Post()\n\
             \x20   {\n\
             \x20       Total();\n\
             \x20       void Total() { }\n\
             \x20   }\n\
             }\n",
        )]);
        assert_eq!(
            calls(&snapshot),
            vec!["Acme.Book.Post@5 -> Acme.Book.Post.Total@8 proven"]
        );
    }

    #[test]
    fn variables_shadow_members_and_types_of_their_name() {
        let snapshot = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Book.cs",
                "namespace Acme.Ledger;\n\
                 public class Book\n\
                 {\n\
                 \x20   public void Save() { }\n\
                 \x20   public static void Create() { }\n\
                 \x20   public void Post(Book Entry, System.Action Save)\n\
                 \x20   {\n\
                 \x20       Entry.Create();\n\
                 \x20       Save();\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        // `Entry` is the parameter, a `Book`; `Create` is static, which C# does not call through
        // a value, and `Save` is the delegate parameter.
        assert_eq!(calls_into_store(&snapshot), Vec::<String>::new());
    }

    #[test]
    fn members_a_base_type_outside_the_repository_may_declare_prove_nothing() {
        let snapshot = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Page.cs",
                "using Acme.Ledger;\n\
                 using Vendor.Web;\n\
                 namespace Acme.Web;\n\
                 public class Page : Controller\n\
                 {\n\
                 \x20   public void Render()\n\
                 \x20   {\n\
                 \x20       Redirect();\n\
                 \x20       Entry.Create();\n\
                 \x20       Own();\n\
                 \x20   }\n\
                 \x20   private void Own() { }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            calls_into_store(&snapshot),
            vec![
                "Acme.Web.Page.Render@6 -> Acme.Ledger.Entry.Create@7 unproven: a member `Entry` inherited from a base type outside the repository would take the name first",
                "Acme.Web.Page.Render@6 -> Acme.Web.Page.Own@12 proven",
            ]
        );
    }

    #[test]
    fn a_call_under_if_or_into_a_delegate_field_proves_nothing() {
        let snapshot = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Book.cs",
                "namespace Acme.Ledger;\n\
                 public class Journal\n\
                 {\n\
                 \x20   public void Changed() { }\n\
                 #if LEGACY\n\
                 \x20   public void Close() { }\n\
                 #endif\n\
                 }\n\
                 public class Book : Journal\n\
                 {\n\
                 \x20   public new System.Action Changed { get; set; }\n\
                 \x20   public void Post(Entry entry)\n\
                 \x20   {\n\
                 #if DEBUG\n\
                 \x20       entry.Save();\n\
                 #endif\n\
                 \x20       Changed();\n\
                 \x20       Close();\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        // `Changed` is the delegate property, which hides the base class's method.
        assert_eq!(
            calls_into_store(&snapshot),
            vec![
                "Acme.Ledger.Book.Post@12 -> Acme.Ledger.Entry.Save@4 unproven: the call is written under `#if`",
                "Acme.Ledger.Book.Post@12 -> Acme.Ledger.Journal.Close@6 unproven: the target is declared under `#if`",
            ]
        );
    }

    #[test]
    fn base_lists_extend_classes_and_interfaces_and_implement_interfaces() {
        let snapshot = project(&[
            (
                "src/Types.cs",
                "namespace Acme;\n\
                 public interface IStore { }\n\
                 public interface IAudited : IStore { }\n\
                 public abstract class Base<T> { }\n\
                 public class Book : Base<Entry>, IAudited, System.IDisposable { public void Dispose() { } }\n\
                 public struct Point : IStore { }\n\
                 public class Entry : Vendor.Model { }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot, GraphEdgeType::Extends),
            vec![
                "Acme.Book@5 -> Acme.Base@4 proven",
                "Acme.IAudited@3 -> Acme.IStore@2 proven",
            ]
        );
        assert_eq!(
            edges(&snapshot, GraphEdgeType::Implements),
            vec![
                "Acme.Book@5 -> Acme.IAudited@3 proven",
                "Acme.Point@6 -> Acme.IStore@2 proven",
            ]
        );
    }

    #[test]
    fn declared_types_are_used_by_the_member_or_local_that_writes_them() {
        let snapshot = project(&[
            ("src/Entry.cs", STORE),
            (
                "src/Book.cs",
                "using System.Collections.Generic;\n\
                 namespace Acme.Ledger;\n\
                 public class Book\n\
                 {\n\
                 \x20   private List<Entry> _entries;\n\
                 \x20   public Entry Last { get; }\n\
                 \x20   public Book Post(Entry entry, int count)\n\
                 \x20   {\n\
                 \x20       Book other = null;\n\
                 \x20       var inferred = new Entry();\n\
                 \x20       return other;\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot, GraphEdgeType::UsesType),
            vec![
                "Acme.Ledger.Book.Last@6 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Book.Post@7 -> Acme.Ledger.Book@3 proven",
                "Acme.Ledger.Book.Post@7 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Book._entries@5 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Entry.Create@7 -> Acme.Ledger.Entry@2 proven",
                "Acme.Ledger.Entry.Next@6 -> Acme.Ledger.Entry@2 proven",
            ]
        );
    }

    #[test]
    fn a_type_only_an_unreferenced_project_declares_gives_no_edge() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", STORE),
            ("src/Web/Web.csproj", SDK),
            (
                "src/Web/Page.cs",
                "using Acme.Ledger;\n\
                 namespace Acme.Web;\n\
                 public class Page\n\
                 {\n\
                 \x20   public void Render(Entry entry) => entry.Save();\n\
                 }\n",
            ),
        ]);
        assert_eq!(calls_into_store(&snapshot), Vec::<String>::new());
        assert_eq!(
            edges(&snapshot, GraphEdgeType::UsesType)
                .into_iter()
                .filter(|edge| edge.starts_with("Acme.Web"))
                .collect::<Vec<_>>(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_partial_methods_call_reaches_its_implementing_part() {
        let snapshot = project(&[
            (
                "src/Book.cs",
                "namespace Acme;\n\
                 public partial class Book\n\
                 {\n\
                 \x20   partial void OnPosted(int amount);\n\
                 \x20   public void Post() => OnPosted(1);\n\
                 }\n",
            ),
            (
                "src/Book.Hooks.cs",
                "namespace Acme;\n\
                 public partial class Book\n\
                 {\n\
                 \x20   partial void OnPosted(int amount) { }\n\
                 }\n",
            ),
        ]);
        let calls = calls(&snapshot);
        assert_eq!(
            calls,
            vec!["Acme.Book.Post@5 -> Acme.Book.OnPosted@4 proven"]
        );
        let target = snapshot
            .resolved_relationships
            .iter()
            .find(|relationship| relationship.edge_type == GraphEdgeType::Calls)
            .map(|relationship| relationship.to.clone())
            .expect("a call");
        let file = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.id == target)
            .map(|symbol| symbol.file_id.clone())
            .expect("the target");
        let path = snapshot
            .files
            .iter()
            .find(|candidate| candidate.id == file)
            .map(|candidate| candidate.path.to_string_lossy().replace('\\', "/"))
            .expect("the target's file");
        assert_eq!(path, "src/Book.Hooks.cs");
    }

    #[test]
    fn a_name_a_base_class_outside_the_repository_may_declare_is_not_a_using_static_method() {
        let snapshot = project(&[
            (
                "src/Helpers.cs",
                "namespace Acme.Shared;\n\
                 public static class Helpers\n\
                 {\n\
                 \x20   public static void Redirect() { }\n\
                 \x20   public static void Audit() { }\n\
                 }\n",
            ),
            (
                "src/Page.cs",
                "using static Acme.Shared.Helpers;\n\
                 namespace Acme.Web;\n\
                 public class Page : Vendor.Web.Controller\n\
                 {\n\
                 \x20   public void Render() => Redirect();\n\
                 }\n\
                 public class Plain\n\
                 {\n\
                 \x20   public void Render() => Audit();\n\
                 }\n",
            ),
            (
                "src/Calc.cs",
                "using static Acme.Shared.Helpers;\n\
                 using static System.Math;\n\
                 namespace Acme.Web;\n\
                 public class Calc\n\
                 {\n\
                 \x20   public void Run() => Audit();\n\
                 }\n",
            ),
        ]);
        // `Controller` may declare `Redirect`, which C# would call first; `Plain` derives from
        // nothing outside, so its bare `Audit()` is the imported one.
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Web.Calc.Run@6 -> Acme.Shared.Helpers.Audit@5 unproven: a `using static` of `System.Math`, outside the repository, may supply `Audit` too",
                "Acme.Web.Plain.Render@9 -> Acme.Shared.Helpers.Audit@5 proven",
            ]
        );
    }

    #[test]
    fn extension_methods_others_may_take_prove_nothing() {
        let snapshot = project(&[
            (
                "src/Types.cs",
                "namespace Acme.Ledger;\n\
                 public class Entry { }\n\
                 public class Remote : Vendor.Data.Record { }\n",
            ),
            (
                "src/Extensions.cs",
                "namespace Acme.Extensions;\n\
                 public static class EntryExtensions\n\
                 {\n\
                 \x20   public static void Archive(this Acme.Ledger.Entry entry, int times) { }\n\
                 \x20   public static void Archive<T>(this T value, int times) { }\n\
                 \x20   public static void Sync(this Acme.Ledger.Remote remote) { }\n\
                 \x20   public static void Seal(this Acme.Ledger.Entry entry) { }\n\
                 }\n",
            ),
            (
                "src/Book.cs",
                "using Acme.Extensions;\n\
                 using Acme.Ledger;\n\
                 namespace Acme.Web\n\
                 {\n\
                 \x20   using Vendor.Extensions;\n\
                 \x20   public class Book\n\
                 \x20   {\n\
                 \x20       public void Post(Acme.Ledger.Entry entry, Acme.Ledger.Remote remote)\n\
                 \x20       {\n\
                 \x20           entry.Archive(2);\n\
                 \x20           remote.Sync();\n\
                 \x20           entry.Seal();\n\
                 \x20       }\n\
                 \x20   }\n\
                 }\n",
            ),
        ]);
        // A generic `this` may take the call; a receiver whose base class is outside may declare
        // `Sync` itself; and `Vendor.Extensions`, imported nearer, may declare `Seal`. The
        // parameters' types are written whole, so no nearer import may declare them.
        assert_eq!(
            calls(&snapshot),
            vec![
                "Acme.Web.Book.Post@8 -> Acme.Extensions.EntryExtensions.Archive@4 ambiguous",
                "Acme.Web.Book.Post@8 -> Acme.Extensions.EntryExtensions.Seal@7 unproven: a nearer `using` imports `Vendor.Extensions`, outside the repository, which may declare an extension method `Seal`",
                "Acme.Web.Book.Post@8 -> Acme.Extensions.EntryExtensions.Sync@6 unproven: the receiver's type has a base type outside the repository, which may declare `Sync`",
            ]
        );
    }

    #[test]
    fn a_file_two_projects_resolve_differently_or_no_project_compiles_proves_no_call() {
        let shared = "namespace Acme.Web;\n\
            public class Page\n\
            {\n\
            \x20   public void Render(Acme.Ledger.Entry entry) => entry.Save();\n\
            \x20   public void Again() => this.Render(null);\n\
            }\n";
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", STORE),
            (
                "src/Web/Web.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"..\\Ledger\\Ledger.csproj\" />\n    <Compile Include=\"..\\Shared\\*.cs\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Tool/Tool.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Compile Include=\"..\\Shared\\*.cs\" />\n  </ItemGroup>\n</Project>\n",
            ),
            ("src/Shared/Page.cs", shared),
            ("loose/Page.cs", shared),
        ]);
        let calls = calls_into_store(&snapshot);
        assert!(
            calls.contains(&"Acme.Web.Page.Render@4 -> Acme.Ledger.Entry.Save@4 unproven: the projects compiling the file resolve it differently".to_string()),
            "{calls:#?}"
        );
        assert!(
            calls.contains(&"Acme.Web.Page.Render@4 -> Acme.Ledger.Entry.Save@4 unproven: the file belongs to no project file the index read".to_string()),
            "{calls:#?}"
        );
        // A call of the file's own type needs no lookup of a name.
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("Acme.Web.Page.Again"))
                .count(),
            1,
            "{calls:#?}"
        );
        assert!(
            calls
                .iter()
                .filter(|call| call.starts_with("Acme.Web.Page.Again"))
                .all(|call| call.ends_with("proven")),
            "{calls:#?}"
        );
    }

    #[test]
    fn a_name_is_read_by_arity_and_nested_types_before_namespaces() {
        let snapshot = project(&[(
            "src/Types.cs",
            "namespace Acme;\n\
                 public class Base { }\n\
                 public class Base<T> { }\n\
                 public class Inner { public static void Make() { } }\n\
                 public class Outer\n\
                 {\n\
                 \x20   public class Inner { public static void Make() { } }\n\
                 \x20   public void Run() => Inner.Make();\n\
                 \x20   public Inner Last { get; }\n\
                 }\n\
                 public class Book : Base<Inner> { }\n",
        )]);
        assert_eq!(
            calls(&snapshot),
            vec!["Acme.Outer.Run@8 -> Acme.Outer.Inner.Make@7 proven"]
        );
        assert_eq!(
            edges(&snapshot, GraphEdgeType::Extends),
            vec!["Acme.Book@11 -> Acme.Base@3 proven"]
        );
        assert_eq!(
            edges(&snapshot, GraphEdgeType::UsesType),
            vec!["Acme.Outer.Last@9 -> Acme.Outer.Inner@7 proven"]
        );
    }
}
