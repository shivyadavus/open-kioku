//! What a change breaks outside itself: the calls that no longer fit a function whose
//! parameters it changed, and the code that still names a declaration it removed.
//!
//! A diff shows what changed. It cannot show the caller in another file that still passes two
//! arguments to a function that now requires three. This module finds those callers.
//!
//! It needs no index. The files of the change are parsed as they were at the base revision and
//! as they are now, and the two sets of declarations are compared. The sites of each changed
//! declaration are found by its name in the working tree (`git grep`); each file with a hit is
//! parsed to tell a call from another declaration of the same name; and each call's arguments
//! are counted against the new parameter list. A call that still fits is not reported, whether
//! or not the change touched it. An index, when there is one, adds what a name cannot: the call
//! edges it proved, which settle a name several declarations share.

use open_kioku_core::{
    File, FileId, GitChangeKind, GraphEdgeType, Language, RelationshipAuthority, RepositoryId,
    Symbol, SymbolKind,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_git::DiffFile;
use open_kioku_languages::detect_language;
use open_kioku_storage::OkStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const RIPPLE_SCHEMA_VERSION: u32 = 1;

/// Changed declarations past this many are counted and not searched for.
const MAX_CONTRACTS: usize = 60;
/// Sites listed for one declaration; the rest are counted.
const MAX_SITES_LISTED: usize = 12;
/// A name with more hits than this is too common to attribute by name.
const MAX_NAME_HITS: usize = 1500;
/// Files with a hit that are parsed in one run; hits in files past it are `possible`.
const MAX_FILES_PARSED: usize = 400;
/// How many lines a call's arguments are read across.
const MAX_CALL_LINES: usize = 40;
const SITE_TEXT_CHARS: usize = 140;
/// How long the sites of changed declarations are searched for in one run. A hook waits on
/// this, and a very large tree can take a second for each name.
const SEARCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct RippleInput {
    /// The revision the working tree is compared with. `HEAD` when absent.
    pub base: Option<String>,
    /// Only declarations in these repository-relative files are examined. Every changed file
    /// when empty. Sites are looked for everywhere either way.
    pub only_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RippleReport {
    pub schema_version: u32,
    pub base: String,
    /// Changed files that hold code this module can read.
    pub files_examined: usize,
    /// Whether an index was read for proven call edges.
    pub index_used: bool,
    pub contracts: Vec<ChangedContract>,
    /// Changed declarations past the limit, which were not searched for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub contracts_omitted: usize,
    /// Sites that no longer fit a changed declaration or still name a removed one, attributed
    /// by proof or by an unshared name. This is the number a caller acts on.
    pub broken_sites: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContractChange {
    SignatureChanged,
    Removed,
}

/// What a changed declaration asks of the code that uses it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContractEffect {
    /// A call written for the old declaration may not fit the new one, or the declaration is
    /// gone. Its sites are checked.
    Breaking,
    /// Only parameters with a default were added: every existing call still fits.
    Compatible,
    /// The parameters are the same in number, order and (where calls name them) name; only
    /// their types or the return type changed. What a call passes cannot be checked from its
    /// text, so the sites are not searched.
    TypesOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SiteEvidence {
    /// The index holds a call or use edge from this site to the declaration, proven
    /// structurally (an import binding, an exact call site, a receiver type).
    Proven,
    /// The site names the declaration and no other declaration in the repository has the name.
    Name,
    /// The site uses the name, and the name alone does not settle what it refers to: another
    /// declaration shares it, or it is a method called on a receiver of unknown type; or the
    /// call's arguments could not be counted.
    Possible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedContract {
    pub path: String,
    pub name: String,
    /// The enclosing type or function, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub kind: String,
    pub change: ContractChange,
    pub effect: ContractEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The declaration that took a removed one's place under another name, when exactly one
    /// added declaration has its signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<String>,
    /// Line of the declaration: in the working tree, or at the base when it was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Calls that were checked and fit the declaration as it is now.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sites_fitting: usize,
    /// Sites that do not fit, or that name what was removed, listed up to a limit.
    pub broken: Vec<RippleSite>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broken_omitted: usize,
    /// Sites where the name alone does not settle the reference, or the call could not be
    /// checked. Not counted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possible: Vec<RippleSite>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub possible_omitted: usize,
    /// Lines of prose, configuration and string literals that still name a removed declaration.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub other_mentions: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RippleSite {
    pub path: String,
    pub line: u32,
    /// The function or type the site is inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    pub text: String,
    pub evidence: SiteEvidence,
    /// Why the site is listed.
    pub problem: String,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl RippleReport {
    /// The changed declarations with a broken site.
    pub fn open_contracts(&self) -> impl Iterator<Item = &ChangedContract> {
        self.contracts
            .iter()
            .filter(|contract| !contract.broken.is_empty())
    }

    /// A stable name for the set of broken sites, so a caller can tell a report it has already
    /// shown from one that changed.
    pub fn fingerprint(&self) -> String {
        let mut lines = Vec::new();
        for contract in self.open_contracts() {
            for site in &contract.broken {
                lines.push(format!(
                    "{}|{}|{}|{}",
                    contract.path,
                    contract.name,
                    site.path,
                    site.text.trim()
                ));
            }
            lines.push(format!(
                "{}|{}|+{}",
                contract.path, contract.name, contract.broken_omitted
            ));
        }
        lines.sort();
        crate::stable_id(&lines.join("\n"))
    }

    pub fn headline(&self) -> String {
        let breaking = self
            .contracts
            .iter()
            .filter(|contract| contract.effect == ContractEffect::Breaking)
            .count();
        if self.contracts.is_empty() {
            return format!(
                "Ripple: no signature changed and no declaration was removed in {} changed code \
                 file(s).",
                self.files_examined
            );
        }
        if self.broken_sites == 0 {
            let fitting: usize = self.contracts.iter().map(|c| c.sites_fitting).sum();
            return format!(
                "Ripple: {} changed declaration(s), {breaking} that a call could break on; no \
                 broken site found ({fitting} call(s) checked and fit).",
                self.contracts.len(),
            );
        }
        format!(
            "Ripple: {} site(s) no longer fit {} changed declaration(s).",
            self.broken_sites,
            self.open_contracts().count()
        )
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.headline());
        out.push('\n');
        let mut quiet = Vec::new();
        for contract in &self.contracts {
            let possible = contract.possible.len() + contract.possible_omitted;
            if contract.effect != ContractEffect::Breaking
                || (contract.broken.is_empty() && possible == 0 && contract.other_mentions == 0)
            {
                quiet.push(contract);
                continue;
            }
            out.push('\n');
            out.push_str(&contract.title());
            out.push('\n');
            if let Some(before) = &contract.before {
                out.push_str(&format!("  - {before}\n"));
            }
            if let Some(after) = &contract.after {
                out.push_str(&format!("  + {after}\n"));
            }
            if let Some(renamed) = &contract.renamed_to {
                out.push_str(&format!("  now `{renamed}`\n"));
            }
            if let Some(note) = &contract.note {
                out.push_str(&format!("  note: {note}\n"));
            }
            let broken = contract.broken.len() + contract.broken_omitted;
            if broken > 0 {
                out.push_str(&format!("  broken: {broken} site(s)\n"));
                for site in &contract.broken {
                    out.push_str(&format!("    {}\n", site.line_text()));
                }
                if contract.broken_omitted > 0 {
                    out.push_str(&format!(
                        "    ... {} more not listed\n",
                        contract.broken_omitted
                    ));
                }
            }
            if contract.sites_fitting > 0 {
                out.push_str(&format!(
                    "  {} other call(s) fit the new signature\n",
                    contract.sites_fitting
                ));
            }
            if possible > 0 {
                out.push_str(&format!(
                    "  possible: {possible} more site(s) that the name alone does not settle\n"
                ));
                for site in contract.possible.iter().take(4) {
                    out.push_str(&format!("    {}\n", site.line_text()));
                }
            }
            if contract.other_mentions > 0 {
                out.push_str(&format!(
                    "  also named on {} line(s) of prose, configuration or string literals\n",
                    contract.other_mentions
                ));
            }
        }
        if !quiet.is_empty() {
            out.push_str(&format!(
                "\n{} changed declaration(s) with nothing to act on:\n",
                quiet.len()
            ));
            for contract in quiet.iter().take(12) {
                let why = match contract.effect {
                    ContractEffect::Compatible => "only optional parameters added".to_string(),
                    ContractEffect::TypesOnly => "only types changed".to_string(),
                    ContractEffect::Breaking if contract.change == ContractChange::Removed => {
                        "removed; nothing names it".to_string()
                    }
                    ContractEffect::Breaking => {
                        format!("{} call(s) checked, all fit", contract.sites_fitting)
                    }
                };
                out.push_str(&format!(
                    "  {}  `{}`: {why}\n",
                    contract.place(),
                    contract.display_name()
                ));
            }
            if quiet.len() > 12 {
                out.push_str(&format!("  ... {} more\n", quiet.len() - 12));
            }
        }
        if self.contracts_omitted > 0 {
            out.push_str(&format!(
                "\n{} more changed declaration(s) were not searched for.\n",
                self.contracts_omitted
            ));
        }
        for caveat in &self.caveats {
            out.push_str(&format!("\ncaveat: {caveat}\n"));
        }
        out
    }

    /// The broken sites as a short instruction for whoever made the change.
    pub fn to_brief(&self, max_sites: usize) -> String {
        let mut out = String::new();
        let mut left = max_sites;
        let mut dropped = 0usize;
        for contract in self.open_contracts() {
            out.push_str(&format!("{}\n", contract.title()));
            if let (Some(before), Some(after)) = (&contract.before, &contract.after) {
                out.push_str(&format!("  was: {before}\n  now: {after}\n"));
            }
            if let Some(renamed) = &contract.renamed_to {
                out.push_str(&format!("  now named `{renamed}`\n"));
            }
            for site in &contract.broken {
                if left == 0 {
                    dropped += 1;
                    continue;
                }
                left -= 1;
                out.push_str(&format!("  {}\n", site.line_text()));
            }
            dropped += contract.broken_omitted;
        }
        if dropped > 0 {
            out.push_str(&format!("... and {dropped} more; `ok ripple` lists them\n"));
        }
        out
    }
}

impl ChangedContract {
    pub fn display_name(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{owner}.{}", self.name),
            None => self.name.clone(),
        }
    }

    fn place(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }

    fn title(&self) -> String {
        let what = match self.change {
            ContractChange::SignatureChanged => "signature changed",
            ContractChange::Removed if self.renamed_to.is_some() => "renamed",
            ContractChange::Removed => "removed",
        };
        format!(
            "{}  {} `{}`: {what}",
            self.place(),
            self.kind,
            self.display_name()
        )
    }
}

impl RippleSite {
    fn line_text(&self) -> String {
        let evidence = match self.evidence {
            SiteEvidence::Proven => "proven",
            SiteEvidence::Name => "by name",
            SiteEvidence::Possible => "possible",
        };
        let within = self
            .within
            .as_ref()
            .map(|name| format!(" (in {name})"))
            .unwrap_or_default();
        format!(
            "{}:{}{within}  {}\n        {} [{evidence}]",
            self.path, self.line, self.text, self.problem
        )
    }
}

/// One declaration as it stood on one side of the change.
#[derive(Debug, Clone)]
struct Declared {
    name: String,
    owner: Option<String>,
    kind: SymbolKind,
    signature: Option<String>,
    line: u32,
    end: u32,
    local: bool,
    /// Declared inside a type: reached through a receiver, not by its bare name.
    member: bool,
}

/// The languages whose declarations and calls this module reads. Prose and configuration that
/// name a declaration are counted apart.
fn is_supported_code(language: &Language) -> bool {
    matches!(
        language,
        Language::Rust
            | Language::Java
            | Language::TypeScript
            | Language::JavaScript
            | Language::Python
            | Language::Go
            | Language::CSharp
    )
}

/// Whether a change to `path` can change a declaration this module reads.
pub fn reads_path(path: &Path) -> bool {
    is_supported_code(&detect_language(path)) && !skips_path(&slashed(path))
}

fn parse_target(path: &Path) -> File {
    File {
        id: FileId(path.to_string_lossy().into_owned()),
        repository_id: RepositoryId("ripple".into()),
        path: path.to_path_buf(),
        language: detect_language(path),
        size_bytes: 0,
        content_hash: String::new(),
        is_generated: false,
        is_vendor: false,
        generated_by: None,
    }
}

fn declarations(path: &Path, text: &str) -> Option<Vec<Declared>> {
    let file = parse_target(path);
    if !is_supported_code(&file.language) {
        return None;
    }
    let symbols = open_kioku_tree_sitter::parse_symbols(&file, text).ok()?;
    let by_id = symbols
        .iter()
        .map(|symbol| (symbol.id.clone(), symbol))
        .collect::<BTreeMap<_, _>>();
    Some(
        symbols
            .iter()
            .filter_map(|symbol| {
                let range = symbol.range.as_ref()?;
                let parent = symbol
                    .parent_symbol_id
                    .as_ref()
                    .and_then(|id| by_id.get(id))
                    .copied();
                Some(Declared {
                    name: symbol.name.clone(),
                    owner: parent
                        .map(|parent| parent.name.clone())
                        .or_else(|| owner_from_qualified_name(symbol)),
                    kind: symbol.kind.clone(),
                    signature: symbol
                        .signature
                        .as_deref()
                        .map(|signature| tidy_signature(&file.language, signature)),
                    line: range.start,
                    end: range.end,
                    local: parent.is_some_and(|parent| is_callable(&parent.kind)),
                    member: symbol.kind == SymbolKind::Method
                        || parent.is_some_and(|parent| !is_callable(&parent.kind)),
                })
            })
            .collect(),
    )
}

/// A Rust method's parent is its `impl` owner, which the parser may leave unresolved in one
/// file; the qualified name still carries it.
fn owner_from_qualified_name(symbol: &Symbol) -> Option<String> {
    if !matches!(symbol.kind, SymbolKind::Method) {
        return None;
    }
    let mut parts = symbol.qualified_name.rsplit("::");
    let _name = parts.next()?;
    parts.next().map(str::to_string)
}

fn is_callable(kind: &SymbolKind) -> bool {
    matches!(kind, SymbolKind::Function | SymbolKind::Method)
}

/// Kinds whose removal or changed signature is a change to what other code relies on.
fn is_contract_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Class
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Constant
    )
}

fn kind_label(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "type",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Constant => "constant",
        _ => "declaration",
    }
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A signature on one line, as it would be written on one: a list broken over lines leaves a
/// space inside its brackets and a trailing comma.
fn tidy_signature(language: &Language, text: &str) -> String {
    // A comment inside a parameter list (`# type: ignore`) is not part of what a call matches.
    let marker = if *language == Language::Python {
        "#"
    } else {
        "//"
    };
    let code = text
        .lines()
        .map(|line| {
            let mut cut = line.len();
            for (offset, _) in line.match_indices(marker) {
                let before = line[..offset].chars().next_back();
                if before.is_none_or(char::is_whitespace) || before == Some(',') {
                    cut = offset;
                    break;
                }
            }
            &line[..cut]
        })
        .collect::<Vec<_>>()
        .join(" ");
    normalize_whitespace(&code)
        .replace("( ", "(")
        .replace(", )", ")")
        .replace(",)", ")")
        .replace(" )", ")")
}

/// Splits `inner` at its top-level commas: not inside brackets, strings, a Rust closure's
/// parameter bars, or a Python `lambda`'s parameters.
fn split_top_level(inner: &str) -> Vec<String> {
    let chars = inner.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut quote: Option<char> = None;
    let mut bars = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        let previous = if index > 0 { chars[index - 1] } else { ' ' };
        if let Some(open) = quote {
            current.push(ch);
            if ch == '\\' {
                if let Some(next) = chars.get(index + 1) {
                    current.push(*next);
                    index += 1;
                }
            } else if ch == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '"' | '`' => quote = Some(ch),
            // An apostrophe opens a string unless it follows a name, `&` or `<`, as a Rust
            // lifetime does, or nothing closes it nearby.
            '\'' if !(previous.is_alphanumeric() || previous == '&' || previous == '<')
                && chars[index + 1..].iter().take(60).any(|c| *c == '\'') =>
            {
                quote = Some(ch)
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            // `Map<K, V>` after a name, or a turbofish; not a comparison, which has spaces.
            '<' if previous.is_alphanumeric() || previous == ':' => angle += 1,
            '>' if angle > 0 && previous != '-' && previous != '=' => angle -= 1,
            '|' if depth == 0 => {
                let opens = current.trim().is_empty() || current.trim() == "move";
                if bars {
                    bars = false;
                } else if opens && chars.get(index + 1) != Some(&'|') {
                    bars = true;
                }
            }
            ',' if depth == 0 && angle == 0 && !bars => {
                // A Python `lambda a, b: ...` keeps its own commas until its colon.
                let lambda = word_offsets(&current, "lambda")
                    .last()
                    .is_some_and(|at| !current[*at..].contains(':'));
                if !lambda {
                    parts.push(current.trim().to_string());
                    current.clear();
                    index += 1;
                    continue;
                }
            }
            _ => {}
        }
        current.push(ch);
        index += 1;
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// The text between the parenthesis at `open` and its match, and what follows the match.
fn balanced(text: &str, open: usize) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, ch) in text[open..].char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '`' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[open + 1..open + offset], &text[open + offset + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Param {
    name: String,
    required: bool,
    keyword_only: bool,
    positional_only: bool,
}

/// What a call has to match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Shape {
    params: Vec<Param>,
    /// Takes any number of further positional arguments.
    varargs: bool,
    /// Takes any further named arguments.
    varkw: bool,
    /// The first parameter is the receiver (`self`, `cls`, `this`): written in the
    /// declaration, and passed by the call's receiver rather than in its argument list.
    receiver: bool,
}

fn positional(required: bool) -> Param {
    Param {
        name: String::new(),
        required,
        keyword_only: false,
        positional_only: true,
    }
}

/// `member` is a declaration inside a type: only there is a first parameter called `self` or
/// `cls` the receiver, and not an argument a caller passes.
fn shape(language: &Language, signature: &str, member: bool, name: &str) -> Option<Shape> {
    // The parameter list is the bracket that follows the declaration's name. A signature may
    // carry brackets before it: an attribute's arguments, a generic bound.
    let after_name = word_offsets(signature, name)
        .into_iter()
        .find_map(|offset| {
            let rest = &signature[offset + name.len()..];
            let trimmed = rest.trim_start();
            let generics = if trimmed.starts_with('<') {
                let mut depth = 0i32;
                trimmed
                    .char_indices()
                    .find_map(|(at, ch)| {
                        match ch {
                            '<' => depth += 1,
                            '>' => depth -= 1,
                            _ => {}
                        }
                        (depth == 0).then_some(at + 1)
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            let after = trimmed[generics..].trim_start();
            after
                .starts_with('(')
                .then(|| signature.len() - after.len())
        });
    let open = match after_name {
        Some(open) => open,
        None => signature.find('(')?,
    };
    let (inner, _) = balanced(signature, open)?;
    let mut out = Shape::default();
    let mut keyword_only = false;
    for (index, raw) in split_top_level(inner).into_iter().enumerate() {
        let name_of = |text: &str| {
            text.split([':', '=', '?'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        };
        match language {
            Language::Python => {
                if raw == "/" {
                    for param in &mut out.params {
                        param.positional_only = true;
                    }
                } else if raw == "*" {
                    keyword_only = true;
                } else if raw.starts_with("**") {
                    out.varkw = true;
                } else if raw.starts_with('*') {
                    out.varargs = true;
                    keyword_only = true;
                } else {
                    let name = name_of(&raw);
                    if member && index == 0 && matches!(name.as_str(), "self" | "cls" | "mcs") {
                        out.receiver = true;
                        continue;
                    }
                    out.params.push(Param {
                        name,
                        required: !raw.contains('='),
                        keyword_only,
                        positional_only: false,
                    });
                }
            }
            Language::TypeScript | Language::JavaScript => {
                if raw.starts_with("...") {
                    out.varargs = true;
                } else if index == 0 && name_of(&raw) == "this" {
                    continue;
                } else {
                    // JavaScript binds a missing argument to `undefined` and drops a surplus
                    // one, so only TypeScript holds a call to the count.
                    let head = raw.split(':').next().unwrap_or("");
                    out.params.push(positional(
                        *language == Language::TypeScript
                            && !(head.trim_end().ends_with('?') || raw.contains('=')),
                    ));
                }
            }
            Language::Rust => {
                let head = raw.split(':').next().unwrap_or("").trim();
                let is_self = index == 0
                    && head
                        .rsplit(|ch: char| ch.is_whitespace() || ch == '&')
                        .next()
                        == Some("self");
                if is_self {
                    out.receiver = true;
                } else {
                    out.params.push(positional(true));
                }
            }
            Language::Go | Language::Java => {
                if raw.contains("...") {
                    out.varargs = true;
                } else {
                    out.params.push(positional(true));
                }
            }
            Language::CSharp => {
                if raw.starts_with("params ") {
                    out.varargs = true;
                } else if index == 0 && raw.starts_with("this ") {
                    out.receiver = true;
                } else {
                    out.params.push(positional(!raw.contains('=')));
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Whether every call that fit `before` fits `after`: the parameters that were there are in
/// place and no more demanding, and whatever was added is optional.
fn is_compatible(before: &Shape, after: &Shape) -> bool {
    if before.receiver != after.receiver
        || (before.varargs && !after.varargs)
        || (before.varkw && !after.varkw)
    {
        return false;
    }
    // A parameter put in ahead of positional ones shifts them, so additions go last, or are
    // keyword-only.
    let mut after_positional = after.params.iter().filter(|param| !param.keyword_only);
    for param in before.params.iter().filter(|param| !param.keyword_only) {
        match after_positional.next() {
            Some(kept) if kept.name == param.name && (!kept.required || param.required) => {}
            _ => return false,
        }
    }
    if after_positional.any(|added| added.required) {
        return false;
    }
    let keyword = |shape: &Shape| {
        shape
            .params
            .iter()
            .filter(|param| param.keyword_only)
            .map(|param| (param.name.clone(), param.required))
            .collect::<BTreeMap<_, _>>()
    };
    let (before_keyword, after_keyword) = (keyword(before), keyword(after));
    after_keyword
        .iter()
        .all(|(name, required)| !required || before_keyword.get(name).copied().unwrap_or(false))
        && before_keyword
            .keys()
            .all(|name| after.varkw || after_keyword.contains_key(name))
}

/// The arguments of one call, as far as its text tells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CallArgs {
    positional: usize,
    keywords: Vec<String>,
    /// An argument is unpacked (`*args`, `**kwargs`, `...items`): the count is unknown.
    spread: bool,
    /// The only argument is itself a call, which in Go may supply several values.
    single_call: bool,
}

/// Reads the arguments of the call to `name` that starts on `line` (1-based).
fn call_args(language: &Language, lines: &[&str], line: u32, name: &str) -> Option<CallArgs> {
    let first = lines.get(line as usize - 1)?;
    let offset = word_offsets(first, name).into_iter().find(|offset| {
        let after = first[offset + name.len()..].trim_start();
        after.starts_with('(') || after.starts_with("::<")
    })?;
    let mut text = first[offset + name.len()..].to_string();
    for more in lines.iter().skip(line as usize).take(MAX_CALL_LINES) {
        if text
            .find('(')
            .and_then(|open| balanced(&text, open))
            .is_some()
        {
            break;
        }
        text.push('\n');
        // A line comment would hide the rest of the call from the bracket count.
        let code = match language {
            Language::Python => more.split(" #").next().unwrap_or(more),
            _ => more.split(" //").next().unwrap_or(more),
        };
        text.push_str(code);
    }
    let open = text.find('(')?;
    let (inner, _) = balanced(&text, open)?;
    let mut out = CallArgs::default();
    let parts = split_top_level(inner);
    for part in &parts {
        if (part.starts_with('*') && *language == Language::Python)
            || part.starts_with("...")
            || (part.ends_with("...") && *language == Language::Go)
        {
            out.spread = true;
            continue;
        }
        let keyword = match language {
            Language::Python => part.split_once('=').and_then(|(key, value)| {
                (is_identifier(key.trim()) && !value.starts_with('=')).then(|| key.trim())
            }),
            Language::CSharp => part.split_once(':').and_then(|(key, value)| {
                (is_identifier(key.trim()) && !value.starts_with(':')).then(|| key.trim())
            }),
            _ => None,
        };
        match keyword {
            Some(keyword) => out.keywords.push(keyword.to_string()),
            None => out.positional += 1,
        }
    }
    out.single_call = parts.len() == 1 && parts[0].ends_with(')');
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fit {
    Fits,
    Breaks(String),
    Unknown(&'static str),
}

/// Whether `call` fits `shape`. `explicit_receiver` is a call that names the owner type and so
/// may pass the receiver as its first argument.
fn fit(language: &Language, shape: &Shape, call: &CallArgs, explicit_receiver: bool) -> Fit {
    if call.spread {
        return Fit::Unknown("it unpacks an argument list");
    }
    if shape.receiver && explicit_receiver && *language == Language::Python {
        return Fit::Unknown("it is called on the type, which may pass the receiver itself");
    }
    let plural = |count: usize| if count == 1 { "" } else { "s" };
    let verb = |count: usize| if count == 1 { "is" } else { "are" };
    if *language == Language::Python {
        let positional = shape
            .params
            .iter()
            .filter(|param| !param.keyword_only)
            .collect::<Vec<_>>();
        if call.positional > positional.len() && !shape.varargs {
            return Fit::Breaks(format!(
                "passes {} positional argument{}; at most {} {} accepted now",
                call.positional,
                plural(call.positional),
                positional.len(),
                verb(positional.len())
            ));
        }
        let mut covered = positional
            .iter()
            .take(call.positional)
            .map(|param| param.name.as_str())
            .collect::<BTreeSet<_>>();
        for keyword in &call.keywords {
            match shape.params.iter().find(|param| &param.name == keyword) {
                Some(param) if !param.positional_only => {
                    covered.insert(param.name.as_str());
                }
                _ if shape.varkw => {}
                _ => {
                    return Fit::Breaks(format!(
                        "passes `{keyword}=`, which is not a parameter now"
                    ))
                }
            }
        }
        if let Some(missing) = shape
            .params
            .iter()
            .find(|param| param.required && !covered.contains(param.name.as_str()))
        {
            return Fit::Breaks(format!(
                "does not pass `{}`, which is required now",
                missing.name
            ));
        }
        return Fit::Fits;
    }
    let receiver_passed = shape.receiver && explicit_receiver && *language == Language::Rust;
    let passed =
        (call.positional + call.keywords.len()).saturating_sub(usize::from(receiver_passed));
    let required = shape.params.iter().filter(|param| param.required).count();
    let most = shape.params.len();
    if *language == Language::Go && call.single_call && passed == 1 && required > 1 {
        return Fit::Unknown("its one argument is a call that may return several values");
    }
    if passed < required {
        return Fit::Breaks(format!(
            "passes {passed} argument{}; {required} {} required now",
            plural(passed),
            verb(required)
        ));
    }
    // JavaScript ignores a surplus argument.
    if passed > most && !shape.varargs && *language != Language::JavaScript {
        return Fit::Breaks(format!(
            "passes {passed} argument{}; at most {most} {} accepted now",
            plural(passed),
            verb(most)
        ));
    }
    Fit::Fits
}

struct Hit {
    path: String,
    line: u32,
    text: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .output()
        .map_err(|err| OkError::Repository(format!("git {} failed: {err}", args[0])))
}

fn file_at(repo: &Path, rev: &str, path: &Path) -> Option<String> {
    let spec = format!("{rev}:{}", path.to_string_lossy());
    let output = git(repo, &["show", "--end-of-options", &spec]).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
        && !name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

/// Every line of the working tree, tracked or not and not ignored, that holds `name` as a word.
fn name_hits(repo: &Path, name: &str) -> Result<Vec<Hit>> {
    let output = git(
        repo,
        &[
            "grep",
            "-n",
            "-z",
            "-I",
            "-w",
            "-F",
            "--untracked",
            "--no-color",
            "-e",
            name,
            "--",
            ".",
        ],
    )?;
    // Exit status 1 is "no line matched".
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(OkError::Repository(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();
    for record in text.split('\n') {
        let mut fields = record.splitn(3, '\0');
        let (Some(path), Some(line), Some(body)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(line) = line.parse::<u32>() else {
            continue;
        };
        if path.starts_with(".ok/") || path.starts_with(".git/") {
            continue;
        }
        hits.push(Hit {
            path: path.to_string(),
            line,
            text: body.to_string(),
        });
    }
    Ok(hits)
}

fn is_comment_line(text: &str) -> bool {
    // A byte-order mark may stand before the first line's `#if` or `//`.
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    ["//", "#", "*", "/*", "--", "<!--", "\"\"\"", "'''"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
        && !trimmed.starts_with("#[")
        && !trimmed.starts_with("#!")
        && !trimmed.starts_with("#include")
}

fn is_import_line(text: &str) -> bool {
    let trimmed = text.trim_start();
    ["use ", "pub use ", "pub(crate) use ", "import ", "from ", "using "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
        // `export { a } from './b'` re-exports; `export const a = f()` is code.
        || (trimmed.starts_with("export ") && trimmed.contains(" from "))
        || trimmed.contains("require(")
}

/// Names a language provides itself: a call of one is not evidence of a use of a declaration
/// the repository once had under that name.
const BUILTIN_NAMES: &[&str] = &[
    "abs", "all", "any", "append", "bool", "bytes", "cap", "clear", "close", "copy", "delete",
    "dict", "dir", "filter", "float", "format", "hash", "id", "input", "int", "iter", "len",
    "list", "make", "map", "max", "min", "new", "next", "open", "panic", "print", "println",
    "range", "recover", "repr", "round", "set", "sorted", "str", "sum", "tuple", "type", "vars",
    "zip",
];

/// Whether `line` (1-based) of a Python file is inside a triple-quoted string that an earlier
/// line opened.
fn in_python_docstring(text: &str, line: u32) -> bool {
    let mut open: Option<&str> = None;
    for source in text.lines().take(line as usize - 1) {
        let mut rest = source;
        loop {
            match open {
                Some(quote) => match rest.find(quote) {
                    Some(at) => {
                        rest = &rest[at + 3..];
                        open = None;
                    }
                    None => break,
                },
                None => {
                    let double = rest.find("\"\"\"");
                    let single = rest.find("'''");
                    let (at, quote) = match (double, single) {
                        (Some(d), Some(s)) if s < d => (s, "'''"),
                        (Some(d), _) => (d, "\"\"\""),
                        (None, Some(s)) => (s, "'''"),
                        (None, None) => break,
                    };
                    rest = &rest[at + 3..];
                    open = Some(quote);
                }
            }
        }
    }
    open.is_some()
}

/// Byte offsets of `name` as a whole word in `text`.
fn word_offsets(text: &str, name: &str) -> Vec<usize> {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$';
    text.match_indices(name)
        .filter(|(offset, _)| {
            let before = text[..*offset].chars().next_back();
            let after = text[offset + name.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .map(|(offset, _)| offset)
        .collect()
}

/// Whether the text that follows `before` on its line is inside a string literal: an odd
/// number of double quotes (or backticks) stands before it, and it is not inside the braces of
/// an interpolation (`f"{name(x)}"`, `` `${name(x)}` ``).
fn in_string_literal(before: &str, single_quotes: bool) -> bool {
    let quotes: &[char] = if single_quotes {
        &['"', '`', '\'']
    } else {
        &['"', '`']
    };
    for quote in quotes.iter().copied() {
        let mut count = 0usize;
        let mut last = None;
        let mut escaped = false;
        for (offset, ch) in before.char_indices() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                count += 1;
                last = Some(offset);
            }
        }
        if count % 2 == 1 {
            let inside = &before[last.map(|offset| offset + 1).unwrap_or(0)..];
            let opened = inside.matches('{').count();
            let closed = inside.matches('}').count();
            if opened <= closed {
                return true;
            }
        }
    }
    false
}

/// The names that can stand for the module a file declares in an import path or a qualifier:
/// the file's stem and the directories above it, less the ones every project has, each also
/// with `-` read as `_`.
fn module_names(path: &str) -> BTreeSet<String> {
    const GENERIC_DIRECTORIES: &[&str] = &[
        "src", "lib", "crates", "packages", "pkg", "internal", "app", "test", "tests", "java",
        "kotlin", "python", "cmd",
    ];
    const GENERIC_STEMS: &[&str] = &["main", "mod", "lib", "index", "__init__"];
    let mut names = BTreeSet::new();
    let parts = path.split('/').collect::<Vec<_>>();
    for (position, part) in parts.iter().enumerate() {
        let is_file = position + 1 == parts.len();
        // `foo.test.js`, `foo.d.ts` stand for `foo`.
        let stem = part.split('.').next().unwrap_or(part);
        let generic = if is_file {
            GENERIC_STEMS
        } else {
            GENERIC_DIRECTORIES
        };
        if stem.is_empty() || generic.contains(&stem) {
            continue;
        }
        names.insert(stem.to_string());
        names.insert(stem.replace('-', "_"));
    }
    names
}

/// Whether `text` holds any of `words` as a whole word.
fn mentions_any(text: &str, words: &BTreeSet<String>) -> bool {
    words
        .iter()
        .any(|word| !word_offsets(text, word).is_empty())
}

/// What a file's import statements say about `name`: `Some(true)` when one brings it in from
/// the declaring module or by a relative path, or brings in everything the declaring module
/// has; `Some(false)` when one brings it in from somewhere else; `None` when none names it.
/// A statement that names it decides before one that imports everything.
fn import_evidence(text: &str, name: &str, modules: &BTreeSet<String>) -> Option<bool> {
    let mut named: Option<bool> = None;
    let mut everything = false;
    let lines = text.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        if !is_import_line(lines[index]) {
            index += 1;
            continue;
        }
        // An import may run over lines: until its brackets close.
        let mut block = String::new();
        let mut depth = 0i32;
        loop {
            let line = lines[index];
            block.push_str(line);
            block.push('\n');
            depth += line.matches(['(', '{']).count() as i32;
            depth -= line.matches([')', '}']).count() as i32;
            index += 1;
            if depth <= 0 || index >= lines.len() || block.len() > 4000 {
                break;
            }
        }
        let names_it = !word_offsets(&block, name).is_empty();
        let glob = block.contains("::*") || block.contains("import *") || block.contains(".*;");
        if !names_it && !glob {
            continue;
        }
        let relative = block.contains("from .")
            || block.contains("'./")
            || block.contains("\"./")
            || block.contains("'../")
            || block.contains("\"../")
            || block.contains("super::")
            || block.contains("self::");
        let from_module = mentions_any(&block, modules);
        if names_it {
            named = Some(named.unwrap_or(false) || relative || from_module);
        } else if from_module {
            everything = true;
        }
    }
    named.or(everything.then_some(true))
}

/// The line without a comment that trails its code: from `//` (or `#` in Python) that stands
/// outside a string. A name in such a comment is prose.
fn without_trailing_comment<'a>(text: &'a str, language: &Language) -> &'a str {
    let marker = if *language == Language::Python {
        "#"
    } else {
        "//"
    };
    let single_quotes = matches!(
        language,
        Language::Python | Language::JavaScript | Language::TypeScript
    );
    for (offset, _) in text.match_indices(marker) {
        let before = &text[..offset];
        // `://` is a URL, and `#` after a name is not a comment.
        if marker == "//" && before.ends_with(':') {
            continue;
        }
        if marker == "#" && !before.chars().next_back().is_none_or(char::is_whitespace) {
            continue;
        }
        if !in_string_literal(before, single_quotes) {
            return before;
        }
    }
    text
}

/// How one line uses a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Usage {
    called: bool,
    /// Reached through a receiver or a path: `x.name`, `x->name`, `m::name`.
    on_receiver: bool,
    /// Reached through a path rather than a value: `Type::name`.
    by_path: bool,
    /// The receiver is the instance itself: `self.name`, `this.name`, `Self::name`.
    by_self: bool,
    /// The path before it ends with the owner type's name: `Owner.name`, `Owner::name`.
    by_owner: bool,
    /// A declaration keyword stands before it: another declaration of the name.
    declares: bool,
    /// The qualifier is the crate or the enclosing module: `crate::name`, `super::name`.
    by_scope: bool,
}

/// The last segment of the path a line reaches `name` through, if any.
fn qualifier<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    word_offsets(text, name).into_iter().find_map(|offset| {
        let before = text[..offset].trim_end();
        let path = before
            .trim_end_matches("::")
            .trim_end_matches("?.")
            .trim_end_matches('.')
            .trim_end_matches("->");
        if path.len() == before.len() {
            return None;
        }
        path.rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .next()
            .filter(|segment| !segment.is_empty())
    })
}

/// The first call of `name` on the line, or failing one its first use outside a string
/// literal. `None` when every occurrence is quoted.
///
/// `single_quotes` is whether the language writes strings in single quotes (Python, JavaScript,
/// TypeScript), where Rust writes lifetimes and the others characters.
fn usage(text: &str, name: &str, owner: Option<&str>, single_quotes: bool) -> Option<Usage> {
    let mut reference = None;
    for offset in word_offsets(text, name) {
        let after_raw = &text[offset + name.len()..];
        let before_raw = &text[..offset];
        let quoted = |ch: Option<char>| ch.is_some_and(|ch| matches!(ch, '"' | '\'' | '`'));
        if quoted(before_raw.chars().next_back())
            || quoted(after_raw.chars().next())
            || in_string_literal(before_raw, single_quotes)
        {
            continue;
        }
        let after = after_raw.trim_start();
        let before = before_raw.trim_end();
        let path = before
            .trim_end_matches("::")
            .trim_end_matches("?.")
            .trim_end_matches('.')
            .trim_end_matches("->");
        let on_receiver = path.len() != before.len();
        let last = path
            .rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == ')' || ch == '('))
            .next()
            .unwrap_or("");
        let found = Usage {
            called: after.starts_with('(') || after.starts_with("::<"),
            on_receiver,
            by_path: on_receiver && before.ends_with("::"),
            by_self: on_receiver
                && matches!(last, "self" | "Self" | "this" | "cls" | "super()" | "super"),
            // `Owner.method` names the type only where a type can be called on, and a type is
            // told from a variable by its capital.
            by_owner: on_receiver
                && owner.is_some_and(|owner| {
                    last == owner && owner.chars().next().is_some_and(char::is_uppercase)
                }),
            by_scope: on_receiver && matches!(last, "crate" | "super"),
            declares: !on_receiver
                && matches!(
                    last,
                    "def"
                        | "fn"
                        | "function"
                        | "func"
                        | "class"
                        | "struct"
                        | "enum"
                        | "trait"
                        | "interface"
                        | "type"
                        | "const"
                        | "static"
                ),
        };
        if found.called || found.declares {
            return Some(found);
        }
        reference.get_or_insert(found);
    }
    reference
}

fn clip(text: &str) -> String {
    let text = normalize_whitespace(text);
    if text.chars().count() <= SITE_TEXT_CHARS {
        return text;
    }
    let mut out = text.chars().take(SITE_TEXT_CHARS - 1).collect::<String>();
    out.push('…');
    out
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Call and use edges into a declaration that the index proved, as `(path, line)` of the site.
fn proven_sites(
    store: &dyn OkStore,
    path: &Path,
    declared: &Declared,
    change: ContractChange,
) -> Result<Option<BTreeSet<(String, u32)>>> {
    let Some(file) = store.get_file_by_path(path)? else {
        return Ok(None);
    };
    let symbols = store.symbols_for_file(&file.id)?;
    let candidates = symbols
        .iter()
        .filter(|symbol| symbol.name == declared.name && symbol.kind == declared.kind)
        .collect::<Vec<_>>();
    // Overloads share a name; the index's line picks the one meant, when it is still there.
    let symbol = match candidates.as_slice() {
        [] => return Ok(None),
        [only] => *only,
        many => {
            let Some(found) = many.iter().find(|symbol| {
                symbol
                    .range
                    .as_ref()
                    .is_some_and(|range| range.start == declared.line)
            }) else {
                return Ok(None);
            };
            *found
        }
    };
    let edge_types: &[GraphEdgeType] = match change {
        ContractChange::SignatureChanged => &[GraphEdgeType::Calls],
        ContractChange::Removed => &[
            GraphEdgeType::Calls,
            GraphEdgeType::References,
            GraphEdgeType::UsesType,
            GraphEdgeType::Implements,
            GraphEdgeType::Extends,
        ],
    };
    let node = format!("symbol:{}", symbol.id.0);
    let mut sites = BTreeSet::new();
    for edge_type in edge_types {
        let edges = match store.edges_by_type_for_node(edge_type.clone(), &node, false, 500, 0) {
            Ok(edges) => edges,
            Err(OkError::Unsupported(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        for edge in edges {
            if edge.relationship_authority() != RelationshipAuthority::Authoritative {
                continue;
            }
            let Some(range) = edge.evidence.file_range.as_ref() else {
                continue;
            };
            let Some(lines) = range.line_range.as_ref() else {
                continue;
            };
            sites.insert((slashed(range.path.as_path()), lines.start));
        }
    }
    Ok(Some(sites))
}

/// The line a working-tree line had before the change, when the change did not write it.
fn line_before(entry: Option<&DiffFile>, line: u32) -> u32 {
    let Some(entry) = entry else {
        return line;
    };
    let mut delta = 0i64;
    for hunk in &entry.hunks {
        let removed = hunk
            .old_range
            .as_ref()
            .map(|range| i64::from(range.end) - i64::from(range.start) + 1)
            .unwrap_or(0);
        match hunk.new_range.as_ref() {
            Some(new) if new.start <= line => {
                delta += i64::from(new.end) - i64::from(new.start) + 1 - removed;
            }
            // A pure deletion sits between two working-tree lines; its old range says where.
            None => {
                if let Some(old) = hunk.old_range.as_ref() {
                    if i64::from(old.start) <= i64::from(line) - delta {
                        delta -= removed;
                    }
                }
            }
            _ => {}
        }
    }
    (i64::from(line) - delta).max(1) as u32
}

/// What is searched for, for one changed declaration.
struct Target {
    declared: Declared,
    /// The name its sites are written with: the type's name for a constructor.
    site_name: String,
    /// A constructor, called through its type's name.
    constructor: bool,
    shape: Option<Shape>,
    language: Language,
    index_path: PathBuf,
    /// The declaring file is gone.
    file_deleted: bool,
}

struct SiteSearch<'a> {
    repo: &'a Path,
    store: Option<&'a dyn OkStore>,
    diff: &'a [DiffFile],
    /// Parsed declarations of files with a hit, kept across the declarations of one run.
    parsed: BTreeMap<String, Option<Vec<Declared>>>,
    texts: BTreeMap<String, Option<String>>,
    /// Whether a dependency manifest names each module asked about.
    dependencies: BTreeMap<String, bool>,
}

/// Whether a dependency manifest of the working tree holds `name` as a word.
fn manifest_names(repo: &Path, name: &str) -> bool {
    git(
        repo,
        &[
            "grep",
            "-I",
            "-l",
            "-w",
            "-F",
            "--untracked",
            "-e",
            name,
            "--",
            "*Cargo.toml",
            "*package.json",
            "*go.mod",
            "*pyproject.toml",
            "*requirements*.txt",
            "*setup.py",
            "*setup.cfg",
            "*pom.xml",
            "*build.gradle",
            "*build.gradle.kts",
            "*.csproj",
        ],
    )
    .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

impl SiteSearch<'_> {
    fn text(&mut self, path: &str) -> Option<&str> {
        let repo = self.repo;
        self.texts
            .entry(path.to_string())
            .or_insert_with(|| fs::read_to_string(repo.join(path)).ok())
            .as_deref()
    }

    fn declared_in(&mut self, path: &str) -> Option<&Vec<Declared>> {
        if !self.parsed.contains_key(path) {
            let parsed = if self.parsed.len() >= MAX_FILES_PARSED {
                None
            } else {
                self.text(path)
                    .map(str::to_string)
                    .and_then(|text| declarations(Path::new(path), &text))
            };
            self.parsed.insert(path.to_string(), parsed);
        }
        self.parsed.get(path).and_then(Option::as_ref)
    }

    /// Whether something in the site's file leads to the declaring module: an import of the
    /// name from it, a qualifier that names it, or a language rule that needs neither.
    fn reaches(
        &mut self,
        hit: &Hit,
        usage: Usage,
        name: &str,
        modules: &BTreeSet<String>,
        language: &Language,
        declaring_path: &str,
    ) -> bool {
        if usage.on_receiver {
            return usage.by_scope
                || qualifier(&hit.text, name).is_some_and(|segment| modules.contains(segment));
        }
        let directory = |path: &str| path.rsplit_once('/').map(|(dir, _)| dir.to_string());
        let same_directory = directory(&hit.path) == directory(declaring_path);
        match language {
            // A namespace is opened whole; no statement names the declaration.
            Language::CSharp => true,
            // A package is a directory: its files see each other, and others must qualify. A
            // file that imports a package of the name means the package.
            Language::Go => {
                same_directory
                    && self
                        .text(&hit.path)
                        .and_then(|text| import_evidence(text, name, modules))
                        != Some(false)
            }
            _ => {
                let evidence = self
                    .text(&hit.path)
                    .and_then(|text| import_evidence(text, name, modules));
                match evidence {
                    Some(ours) => ours,
                    None => *language == Language::Java && same_directory,
                }
            }
        }
    }

    fn changed(&self, path: &str) -> Option<&DiffFile> {
        self.diff.iter().find(|entry| {
            entry
                .new_path
                .as_ref()
                .is_some_and(|new| slashed(new) == path)
        })
    }

    fn fill(&mut self, contract: &mut ChangedContract, target: &Target) -> Result<()> {
        let declared = &target.declared;
        let name = target.site_name.as_str();
        if !is_identifier(name) {
            contract.note =
                Some("the name is not an identifier; its sites were not searched".into());
            return Ok(());
        }
        let hits = name_hits(self.repo, name)?;
        let proven = match self.store {
            Some(store) if !target.constructor => {
                proven_sites(store, &target.index_path, declared, contract.change)?
            }
            _ => None,
        };
        let too_common = hits.len() > MAX_NAME_HITS;
        let removed = contract.change == ContractChange::Removed;
        // A member of a type, callable or not, is reached through the type or an instance.
        let method = declared.member && !target.constructor;
        let callable = is_callable(&declared.kind);
        let builtin = BUILTIN_NAMES.contains(&name);
        let owner = if target.constructor {
            None
        } else {
            declared.owner.as_deref()
        };

        let mut modules = module_names(&contract.path);
        if target.file_deleted {
            // A module that is gone may have given way to a dependency of the same name (a
            // vendored copy replaced by the package): an import by that name then resolves,
            // and is no evidence of a use of what was removed.
            let repo = self.repo;
            modules.retain(|module| {
                !*self.dependencies.entry(module.clone()).or_insert_with(|| {
                    // A package `proc-macro2` is the module `proc_macro2`.
                    manifest_names(repo, module) || manifest_names(repo, &module.replace('_', "-"))
                })
            });
        }
        // Another declaration of the name anywhere means the name does not settle a reference.
        let mut shared = false;
        let mut candidates = Vec::new();
        for hit in hits {
            let language = detect_language(Path::new(&hit.path));
            if !is_supported_code(&language) {
                if removed && !is_comment_line(&hit.text) {
                    contract.other_mentions += 1;
                }
                continue;
            }
            if skips_path(&hit.path) {
                continue;
            }
            let own_file = hit.path == contract.path;
            let declarations = if too_common {
                None
            } else {
                self.declared_in(&hit.path).cloned()
            };
            let declared_here = declarations.as_ref().and_then(|declarations| {
                declarations
                    .iter()
                    .find(|other| other.name == name && other.line == hit.line && !other.local)
            });
            if let Some(other) = declared_here {
                // The changed declaration itself, where it still stands, is not another one;
                // nor is the type whose constructor changed.
                let is_self = own_file
                    && !removed
                    && (target.constructor
                        || (contract.line == Some(hit.line) && other.owner == contract.owner));
                if !is_self {
                    shared = true;
                }
                continue;
            }
            if is_comment_line(&hit.text) {
                continue;
            }
            let single_quotes = matches!(
                language,
                Language::Python | Language::JavaScript | Language::TypeScript
            );
            let code = without_trailing_comment(&hit.text, &language);
            if word_offsets(code, name).is_empty() {
                // Named only in the comment after the code.
                continue;
            }
            let Some(usage) = usage(code, name, owner, single_quotes) else {
                if removed {
                    contract.other_mentions += 1;
                }
                continue;
            };
            if usage.declares {
                // A declaration the parser did not report (the file did not parse).
                if !(own_file && contract.line == Some(hit.line)) {
                    shared = true;
                }
                continue;
            }
            if !removed && (!usage.called || is_import_line(&hit.text)) {
                // A changed signature leaves an import, and a function passed by name, valid.
                continue;
            }
            if removed && callable && !usage.called && !is_import_line(&hit.text) {
                // The bare name of a removed function is as likely a field or a local variable
                // of that name as the function passed by value.
                continue;
            }
            if language == Language::Python
                && self
                    .text(&hit.path)
                    .is_some_and(|text| in_python_docstring(text, hit.line))
            {
                if removed {
                    contract.other_mentions += 1;
                }
                continue;
            }
            let within = declarations.as_ref().and_then(|declarations| {
                declarations
                    .iter()
                    .filter(|other| other.line <= hit.line && other.end >= hit.line)
                    .filter(|other| is_callable(&other.kind) || is_contract_kind(&other.kind))
                    .max_by_key(|other| other.line)
                    .map(|other| match &other.owner {
                        Some(owner) => format!("{owner}.{}", other.name),
                        None => other.name.clone(),
                    })
            });
            candidates.push((hit, usage, within, language));
        }

        let owner_imported = match (owner, self.text(&contract.path)) {
            (Some(owner), Some(text)) => import_evidence(text, owner, &BTreeSet::new()).is_some(),
            _ => false,
        };
        let mut seen = BTreeSet::new();
        let mut broken = Vec::new();
        let mut possible = Vec::new();
        for (hit, usage, within, language) in candidates {
            if !seen.insert((hit.path.clone(), hit.line)) {
                continue;
            }
            let before_line = line_before(self.changed(&hit.path), hit.line);
            let proven_here = proven.as_ref().is_some_and(|sites| {
                sites.contains(&(hit.path.clone(), hit.line))
                    || sites.contains(&(hit.path.clone(), before_line))
            });
            let mut evidence = if proven_here {
                SiteEvidence::Proven
            } else if too_common || shared || builtin {
                SiteEvidence::Possible
            } else if !method
                && !usage.on_receiver
                && self.text(&hit.path).is_some_and(|text| shadows(text, name))
            {
                // The file has a variable or a parameter of this name of its own.
                SiteEvidence::Possible
            } else if !method
                && (hit.path != contract.path || usage.on_receiver)
                && !self.reaches(&hit, usage, name, &modules, &language, &contract.path)
            {
                // Nothing leads from this file to the declaring module: no import of the name
                // from it, no qualifier that names it. Something else of the name is meant.
                SiteEvidence::Possible
            } else if method
                && usage.on_receiver
                && !(usage.by_owner && language != Language::Go)
                && !(usage.by_self
                    && within.as_ref().is_some_and(|within| {
                        owner.is_some_and(|owner| {
                            within
                                .strip_prefix(owner)
                                .is_some_and(|rest| rest.starts_with('.'))
                        })
                    }))
            {
                // A method reached through a receiver is anyone's method of that name, unless
                // the receiver is the owner type, or the instance inside the owner type.
                SiteEvidence::Possible
            } else if method && owner_imported {
                // The declaring file also brings its owner's name in from elsewhere: which
                // of the two a site means is not settled by the name.
                SiteEvidence::Possible
            } else if method && !usage.on_receiver && language != Language::Rust {
                // A bare name is a member only inside its own type, which Rust alone writes
                // without a receiver; elsewhere it may be inherited, or declared in another
                // part of the type, and the file that lost it says nothing about that.
                SiteEvidence::Possible
            } else {
                SiteEvidence::Name
            };
            let problem = if removed {
                match &contract.renamed_to {
                    Some(renamed) => format!("still names `{name}`, which is now `{renamed}`"),
                    None => format!("still names `{name}`, which this change removed"),
                }
            } else {
                let Some(shape) = target.shape.as_ref() else {
                    continue;
                };
                let call = match self.text(&hit.path) {
                    Some(text) => {
                        let lines = text.lines().collect::<Vec<_>>();
                        call_args(&language, &lines, hit.line, name)
                    }
                    None => continue,
                };
                let explicit_receiver = usage.by_owner || usage.by_path;
                match call.map(|call| fit(&target.language, shape, &call, explicit_receiver)) {
                    Some(Fit::Fits) => {
                        if evidence != SiteEvidence::Possible {
                            contract.sites_fitting += 1;
                        }
                        continue;
                    }
                    Some(Fit::Breaks(problem)) => problem,
                    Some(Fit::Unknown(why)) => {
                        evidence = SiteEvidence::Possible;
                        format!("could not be checked: {why}")
                    }
                    None => {
                        evidence = SiteEvidence::Possible;
                        "could not be checked: its arguments could not be read".to_string()
                    }
                }
            };
            let site = RippleSite {
                path: hit.path,
                line: hit.line,
                within,
                text: clip(&hit.text),
                evidence,
                problem,
            };
            if evidence == SiteEvidence::Possible {
                possible.push(site);
            } else {
                broken.push(site);
            }
        }

        // A use of a removed declaration that the index proved and the name did not find: made
        // through an alias.
        if let (Some(sites), true) = (&proven, removed) {
            for (path, line) in sites {
                // The index's line may be from either side of a changed file; without the name
                // on it there is nothing to anchor the site to.
                if seen.contains(&(path.clone(), *line))
                    || skips_path(path)
                    || self.changed(path).is_some()
                {
                    continue;
                }
                let Some(source) = self
                    .text(path)
                    .and_then(|text| text.lines().nth(*line as usize - 1))
                    .map(str::to_string)
                else {
                    continue;
                };
                if !word_offsets(&source, name).is_empty() {
                    continue;
                }
                seen.insert((path.clone(), *line));
                broken.push(RippleSite {
                    path: path.clone(),
                    line: *line,
                    within: None,
                    text: clip(&source),
                    evidence: SiteEvidence::Proven,
                    problem: format!("the index proved a use of `{name}` here, under another name"),
                });
            }
        }

        let order = |a: &RippleSite, b: &RippleSite| {
            a.evidence
                .cmp(&b.evidence)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.line.cmp(&b.line))
        };
        broken.sort_by(order);
        possible.sort_by(order);
        contract.broken_omitted = broken.len().saturating_sub(MAX_SITES_LISTED);
        broken.truncate(MAX_SITES_LISTED);
        contract.possible_omitted = possible.len().saturating_sub(MAX_SITES_LISTED);
        possible.truncate(MAX_SITES_LISTED);
        contract.broken = broken;
        contract.possible = possible;
        if too_common {
            contract.note = Some(format!(
                "`{name}` appears on more than {MAX_NAME_HITS} lines; only sites the index \
                 proved are counted"
            ));
        } else if shared {
            contract.note = Some(format!(
                "another declaration is also named `{name}`{}; sites the index did not prove \
                 are listed as possible",
                if removed { " (it may have moved)" } else { "" }
            ));
        }
        Ok(())
    }
}

/// Whether `text` still declares or binds `name`: `var name =`, `name := ...`,
/// `const name: T =`, `type name ...`, `func name(`. A declaration the parser reports under
/// another kind, or does not report, is still a declaration.
fn binds_name(text: &str, name: &str) -> bool {
    text.lines().any(|line| binding_of(line, name).is_some())
}

/// How a line binds `name`, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    /// `type name`, `class name`, `fn name`: a declaration.
    Declaration,
    /// `name := ...`, `let name = ...`, `for name in ...`, `name, err := ...`: a variable.
    Variable,
}

fn binding_of(line: &str, name: &str) -> Option<Binding> {
    // An attribute given a value on the instance (`self.name = ...`) binds the name as a
    // method of that name would.
    let mut rest = line.trim_start();
    for instance in ["self.", "this."] {
        rest = rest.strip_prefix(instance).unwrap_or(rest);
    }
    let mut declares = false;
    let mut introduces = false;
    loop {
        let mut stripped = false;
        for keyword in [
            "export ",
            "default ",
            "pub ",
            "pub(crate) ",
            "public ",
            "private ",
            "protected ",
            "static ",
            "final ",
            "abstract ",
            "async ",
            "unsafe ",
        ] {
            if let Some(after) = rest.strip_prefix(keyword) {
                rest = after.trim_start();
                stripped = true;
            }
        }
        for keyword in [
            "type ",
            "class ",
            "struct ",
            "enum ",
            "trait ",
            "interface ",
            "def ",
            "fn ",
            "func ",
            "function ",
        ] {
            if let Some(after) = rest.strip_prefix(keyword) {
                rest = after.trim_start();
                declares = true;
                stripped = true;
            }
        }
        for keyword in ["var ", "let ", "const ", "mut ", "for ", "with "] {
            if let Some(after) = rest.strip_prefix(keyword) {
                rest = after.trim_start();
                introduces = true;
                stripped = true;
            }
        }
        if !stripped {
            break;
        }
    }
    // `name, err := f()`: the name may stand anywhere in the list being bound.
    let targets = rest
        .split_once(":=")
        .or_else(|| rest.split_once(" = "))
        .or_else(|| rest.split_once(" in "))
        .map(|(targets, _)| targets);
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$';
    let starts = rest
        .strip_prefix(name)
        .filter(|after| !after.chars().next().is_some_and(is_word));
    if declares {
        return starts.map(|_| Binding::Declaration);
    }
    if let Some(after) = starts {
        let after = after.trim_start();
        let bound = (after.starts_with('=') && !after.starts_with("=="))
            || after.starts_with(":=")
            || after.starts_with(',')
            || (introduces && (after.starts_with(':') || after.starts_with(" in ")))
            || (after.starts_with(':') && !after.starts_with("::") && introduces);
        if bound {
            return Some(Binding::Variable);
        }
    }
    let listed = targets.is_some_and(|targets| {
        targets.contains(',')
            && targets
                .split(',')
                .any(|target| target.trim().trim_start_matches("mut ") == name)
    });
    (listed || (introduces && starts.is_some())).then_some(Binding::Variable)
}

/// Whether `text` gives `name` to a variable or a parameter of its own, which a bare use in
/// the file may then mean.
fn shadows(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        binding_of(line, name) == Some(Binding::Variable)
            || (line.contains("def ")
                || line.contains("fn ")
                || line.contains("func ")
                || line.contains("function ")
                || line.contains("=>"))
                && parameter_named(line, name)
    })
}

/// Whether a function header on `line` has a parameter called `name`.
fn parameter_named(line: &str, name: &str) -> bool {
    let Some(open) = line.find('(') else {
        return false;
    };
    let Some((inner, _)) = balanced(line, open) else {
        return false;
    };
    split_top_level(inner).iter().any(|param| {
        param
            .trim_start_matches(['*', '&', '.'])
            .trim_start_matches("mut ")
            .split([':', '=', ' ', '?'])
            .next()
            == Some(name)
    })
}

type DeclaredKey = (Option<String>, String, &'static str);

/// The one added declaration that has a removed one's kind, owner and signature under another
/// name: what it was renamed to.
fn renamed_to(
    removed: &Declared,
    before: &BTreeMap<DeclaredKey, Vec<&Declared>>,
    after: &[Declared],
) -> Option<String> {
    let shape = |declared: &Declared| {
        declared
            .signature
            .as_ref()
            .map(|signature| signature.replacen(&declared.name, "", 1))
    };
    let wanted = shape(removed)?;
    let mut candidates = after.iter().filter(|added| {
        added.kind == removed.kind
            && added.owner == removed.owner
            && !before.contains_key(&(
                added.owner.clone(),
                added.name.clone(),
                kind_label(&added.kind),
            ))
            && shape(added).as_ref() == Some(&wanted)
    });
    let first = candidates.next()?;
    candidates.next().is_none().then(|| first.name.clone())
}

/// Paths whose contents are not the repository's own code: dependencies and build output.
fn skips_path(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            "node_modules" | "vendor" | "third_party" | ".venv" | "target" | "dist" | "build"
        )
    })
}

/// Compares the working tree with `base` and reports the changed declarations and the sites
/// that no longer fit them.
///
/// `repo` is the top of a git working tree. `store` is an index of it, if one exists; it is
/// read for proven call edges and nothing else.
pub fn ripple(
    repo: &Path,
    store: Option<&dyn OkStore>,
    input: &RippleInput,
) -> Result<RippleReport> {
    let base = input.base.clone().unwrap_or_else(|| "HEAD".to_string());
    if !repo.join(".git").exists() {
        return Err(OkError::Repository(format!(
            "ripple compares the working tree with a git revision, and {} is not the top of a \
             git working tree",
            repo.display()
        )));
    }
    let diff = open_kioku_git::diff_unified_zero_since(repo, &base)?;
    let only = input
        .only_files
        .iter()
        .map(|path| slashed(path))
        .collect::<BTreeSet<_>>();

    let mut caveats = Vec::new();
    let mut files_examined = 0usize;
    let mut found: Vec<(ChangedContract, Target)> = Vec::new();
    let mut unparsed = Vec::new();
    for entry in &diff {
        if matches!(entry.status, GitChangeKind::Added | GitChangeKind::Copied) || entry.binary {
            continue;
        }
        let Some(old_path) = entry.old_path.as_ref() else {
            continue;
        };
        let old = slashed(old_path);
        let new = entry.new_path.as_ref().map(|path| slashed(path));
        if !only.is_empty()
            && !only.contains(&old)
            && !new.as_ref().is_some_and(|new| only.contains(new))
        {
            continue;
        }
        let language = detect_language(old_path);
        if !is_supported_code(&language) || skips_path(&old) {
            continue;
        }
        let Some(before_text) = file_at(repo, &base, old_path) else {
            continue;
        };
        let Some(before) = declarations(old_path, &before_text) else {
            unparsed.push(format!("{old} (at {base})"));
            continue;
        };
        let after_text = new
            .as_ref()
            .and_then(|path| fs::read_to_string(repo.join(path)).ok());
        let after = match (&new, &after_text) {
            (Some(path), Some(text)) => match declarations(Path::new(path), text) {
                Some(after) => after,
                None => {
                    unparsed.push(path.clone());
                    continue;
                }
            },
            _ => Vec::new(),
        };
        files_examined += 1;
        let current_path = new.clone().unwrap_or_else(|| old.clone());
        let key = |declared: &Declared| -> DeclaredKey {
            (
                declared.owner.clone(),
                declared.name.clone(),
                kind_label(&declared.kind),
            )
        };
        let mut after_by_key: BTreeMap<DeclaredKey, Vec<&Declared>> = BTreeMap::new();
        for declared in &after {
            after_by_key
                .entry(key(declared))
                .or_default()
                .push(declared);
        }
        let mut before_by_key: BTreeMap<DeclaredKey, Vec<&Declared>> = BTreeMap::new();
        for declared in &before {
            before_by_key
                .entry(key(declared))
                .or_default()
                .push(declared);
        }
        for (key, group) in &before_by_key {
            let declared = group[0];
            let special = matches!(declared.name.as_str(), "__init__" | "constructor");
            if !is_contract_kind(&declared.kind)
                || declared.local
                || declared.name.starts_with("test_")
                || declared.name == "main"
            {
                continue;
            }
            // A member of a type that is itself gone is reported once, as the type.
            let owner_gone = declared.owner.as_ref().is_some_and(|owner| {
                !after.iter().any(|other| &other.name == owner)
                    && before
                        .iter()
                        .any(|other| &other.name == owner && !is_callable(&other.kind))
            });
            if owner_gone {
                continue;
            }
            let now = after_by_key.get(key).map(Vec::as_slice).unwrap_or(&[]);
            let (change, after_declared) = match (group.as_slice(), now) {
                // A removed constructor leaves the default one; nothing names it.
                (_, []) if special => continue,
                // Still declared under the name, as another kind of thing (a function that
                // became a variable holding one): what uses it is not known to be broken.
                (_, [])
                    if after.iter().any(|other| {
                        other.name == declared.name && other.owner == declared.owner
                    }) || after_text
                        .as_deref()
                        .is_some_and(|text| binds_name(text, &declared.name)) =>
                {
                    continue
                }
                (_, []) => (ContractChange::Removed, None),
                ([before], [after]) => match (&before.signature, &after.signature) {
                    (Some(a), Some(b)) if a != b => {
                        (ContractChange::SignatureChanged, Some(*after))
                    }
                    _ => continue,
                },
                // Overloads: a changed set is not attributed to one of them.
                _ => continue,
            };
            let shapes = match (&declared.signature, after_declared) {
                (Some(before), Some(after)) => after.signature.as_deref().and_then(|after| {
                    Some((
                        shape(&language, before, declared.member, &declared.name)?,
                        shape(&language, after, declared.member, &declared.name)?,
                    ))
                }),
                _ => None,
            };
            let effect = match (change, &shapes) {
                (ContractChange::Removed, _) => ContractEffect::Breaking,
                (_, Some((before, after))) if before == after => ContractEffect::TypesOnly,
                (_, Some((before, after))) if is_compatible(before, after) => {
                    ContractEffect::Compatible
                }
                (_, Some(_)) => ContractEffect::Breaking,
                // A signature that could not be read is not claimed to break anything.
                (_, None) => ContractEffect::TypesOnly,
            };
            let constructor = special && declared.owner.is_some();
            let contract = ChangedContract {
                path: current_path.clone(),
                name: declared.name.clone(),
                owner: declared.owner.clone(),
                kind: kind_label(&declared.kind).to_string(),
                change,
                effect,
                before: declared.signature.clone(),
                after: after_declared.and_then(|after| after.signature.clone()),
                renamed_to: match change {
                    ContractChange::Removed => renamed_to(declared, &before_by_key, &after),
                    ContractChange::SignatureChanged => None,
                },
                line: Some(
                    after_declared
                        .map(|after| after.line)
                        .unwrap_or(declared.line),
                ),
                sites_fitting: 0,
                broken: Vec::new(),
                broken_omitted: 0,
                possible: Vec::new(),
                possible_omitted: 0,
                other_mentions: 0,
                note: None,
            };
            let target = Target {
                site_name: if constructor {
                    declared.owner.clone().unwrap_or_default()
                } else {
                    declared.name.clone()
                },
                constructor,
                shape: shapes.map(|(_, after)| after),
                language: language.clone(),
                index_path: old_path.clone(),
                file_deleted: new.is_none(),
                declared: after_declared.unwrap_or(declared).clone(),
            };
            found.push((contract, target));
        }
    }
    if !unparsed.is_empty() {
        unparsed.sort();
        caveats.push(format!(
            "{} changed file(s) could not be parsed and were not examined: {}",
            unparsed.len(),
            unparsed
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Breaking changes are searched for first, so the limit cuts the others.
    found.sort_by(|a, b| {
        a.0.effect
            .cmp(&b.0.effect)
            .then_with(|| a.0.path.cmp(&b.0.path))
            .then_with(|| a.0.line.cmp(&b.0.line))
    });
    let contracts_omitted = found.len().saturating_sub(MAX_CONTRACTS);
    found.truncate(MAX_CONTRACTS);

    let mut search = SiteSearch {
        repo,
        store,
        diff: &diff,
        parsed: BTreeMap::new(),
        texts: BTreeMap::new(),
        dependencies: BTreeMap::new(),
    };
    let mut contracts = Vec::new();
    let mut contracts_omitted = contracts_omitted;
    let started = std::time::Instant::now();
    let mut out_of_time = false;
    for (mut contract, target) in found {
        if contract.effect == ContractEffect::Breaking {
            if started.elapsed() > SEARCH_BUDGET {
                out_of_time = true;
                contracts_omitted += 1;
                continue;
            }
            search.fill(&mut contract, &target)?;
        }
        contracts.push(contract);
    }
    if out_of_time {
        caveats.push(format!(
            "the search stopped after {} seconds; the changed declarations it had not reached \
             are counted and were not searched for",
            SEARCH_BUDGET.as_secs()
        ));
    }
    let broken_sites = contracts
        .iter()
        .map(|contract| contract.broken.len() + contract.broken_omitted)
        .sum();
    if store.is_none()
        && contracts
            .iter()
            .any(|contract| contract.effect == ContractEffect::Breaking)
    {
        caveats.push(
            "no index was read: sites are attributed by name, and a use made through an alias \
             is not found"
                .into(),
        );
    }
    Ok(RippleReport {
        schema_version: RIPPLE_SCHEMA_VERSION,
        base,
        files_examined,
        index_used: store.is_some(),
        contracts,
        contracts_omitted,
        broken_sites,
        caveats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.email=ripple@test", "-c", "user.name=ripple"])
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// A committed repository of `files`, then `edits` written over it uncommitted.
    fn changed(files: &[(&str, &str)], edits: &[(&str, Option<&str>)]) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path();
        git_in(repo, &["init", "-q"]);
        for (path, text) in files {
            let target = repo.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, text).unwrap();
        }
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "base"]);
        for (path, text) in edits {
            let target = repo.join(path);
            match text {
                Some(text) => {
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(target, text).unwrap();
                }
                None => fs::remove_file(target).unwrap(),
            }
        }
        temp
    }

    fn report(temp: &tempfile::TempDir) -> RippleReport {
        ripple(temp.path(), None, &RippleInput::default()).expect("ripple should run")
    }

    fn broken(report: &RippleReport) -> Vec<String> {
        report
            .contracts
            .iter()
            .flat_map(|contract| contract.broken.iter())
            .map(|site| format!("{}:{} {}", site.path, site.line, site.problem))
            .collect()
    }

    #[test]
    fn a_python_call_that_misses_a_new_required_parameter_is_broken_and_one_that_fits_is_not() {
        let temp = changed(
            &[
                ("pkg/util.py", "def shorten(text, limit=45):\n    return text[:limit]\n"),
                (
                    "pkg/core.py",
                    "from .util import shorten\n\n\ndef title(text):\n    return shorten(text, 10)\n\n\ndef strict(text):\n    return shorten(text, 10, strict=True)\n",
                ),
            ],
            &[(
                "pkg/util.py",
                Some("def shorten(text, limit, strict):\n    return text[:limit]\n"),
            )],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 1, "{}", report.to_text());
        assert_eq!(
            broken(&report),
            vec!["pkg/core.py:5 does not pass `strict`, which is required now".to_string()]
        );
        let contract = &report.contracts[0];
        assert_eq!(contract.effect, ContractEffect::Breaking);
        assert_eq!(contract.sites_fitting, 1);
        assert_eq!(contract.broken[0].within.as_deref(), Some("title"));
        assert_eq!(contract.broken[0].evidence, SiteEvidence::Name);
        assert!(!report.index_used);
        assert!(report.to_text().contains("pkg/core.py:5 (in title)"));
        // Fixing the call clears it, whether or not the fix is committed.
        fs::write(
            temp.path().join("pkg/core.py"),
            "from .util import shorten\n\n\ndef title(text):\n    return shorten(text, 10, False)\n\n\ndef strict(text):\n    return shorten(text, 10, strict=True)\n",
        )
        .unwrap();
        let fixed = self::report(&temp);
        assert_eq!(fixed.broken_sites, 0, "{}", fixed.to_text());
        assert_eq!(fixed.contracts[0].sites_fitting, 2);
        assert_ne!(report.fingerprint(), fixed.fingerprint());
    }

    #[test]
    fn annotations_defaults_added_and_comments_in_a_signature_break_nothing() {
        let temp = changed(
            &[
                (
                    "pkg/util.py",
                    "def echo(message=None, nl=True):\n    pass\n\n\ndef join(options):\n    pass\n\n\nclass Group:\n    def command(self, *args, **kwargs):\n        pass\n",
                ),
                (
                    "pkg/core.py",
                    "from .util import echo, join\n\necho('x')\njoin([])\n",
                ),
            ],
            &[(
                "pkg/util.py",
                Some("def echo(message: object = None, nl: bool = True, color=None):\n    pass\n\n\ndef join(options: list[str]) -> str:\n    pass\n\n\nclass Group:\n    def command(  # type: ignore[override]\n        self, *args: object, **kwargs: object\n    ):\n        pass\n"),
            )],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        let effect = |name: &str| {
            report
                .contracts
                .iter()
                .find(|contract| contract.name == name)
                .map(|contract| contract.effect)
        };
        assert_eq!(effect("echo"), Some(ContractEffect::Compatible));
        assert_eq!(effect("join"), Some(ContractEffect::TypesOnly));
        assert_eq!(effect("command"), Some(ContractEffect::TypesOnly));
    }

    #[test]
    fn a_renamed_function_leaves_its_callers_and_imports_broken_and_strings_are_mentions() {
        let temp = changed(
            &[
                ("pkg/util.py", "def format_filename(name, shorten=False):\n    return name\n"),
                (
                    "pkg/core.py",
                    "from .util import format_filename\n\n\ndef show(path):\n    \"\"\"Uses\n    format_filename(path) to print.\n    \"\"\"\n    print(f\"at {format_filename(path)}\")\n    return 'format_filename'\n",
                ),
                ("README.md", "Call `format_filename` to format.\n"),
            ],
            &[(
                "pkg/util.py",
                Some("def format_file_name(name, shorten=False):\n    return name\n"),
            )],
        );
        let report = report(&temp);
        let contract = &report.contracts[0];
        assert_eq!(contract.renamed_to.as_deref(), Some("format_file_name"));
        assert_eq!(
            broken(&report),
            vec![
                "pkg/core.py:1 still names `format_filename`, which is now `format_file_name`"
                    .to_string(),
                "pkg/core.py:8 still names `format_filename`, which is now `format_file_name`"
                    .to_string(),
            ],
            "{}",
            report.to_text()
        );
        // The docstring line, the quoted name and the README are mentions, not broken code.
        assert_eq!(contract.other_mentions, 3, "{}", report.to_text());
    }

    #[test]
    fn a_name_that_comes_from_somewhere_else_is_possible_and_not_counted() {
        let temp = changed(
            &[
                ("app/host.py", "class Host:\n    pass\n\n\ndef create_comment(body):\n    pass\n"),
                (
                    "app/other.py",
                    "from headers import Host\n\n\ndef f(issue):\n    issue.create_comment('x')\n    return Host()\n",
                ),
                ("app/bare.py", "def g():\n    return create_comment('x')\n"),
            ],
            &[("app/host.py", Some("X = 1\n"))],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        let possible = report
            .contracts
            .iter()
            .map(|contract| contract.possible.len())
            .sum::<usize>();
        // `Host` twice in other.py (import and use), `create_comment` on a receiver, and the
        // bare call in a file that imports nothing from `host`.
        assert_eq!(possible, 4, "{}", report.to_text());
    }

    #[test]
    fn a_removed_functions_name_as_a_field_or_a_rebound_variable_is_not_a_use() {
        let temp = changed(
            &[
                (
                    "src/lib.rs",
                    "pub fn configured() -> bool {\n    true\n}\n\npub struct Options {\n    pub configured: bool,\n}\n\npub fn build() -> Options {\n    Options { configured: true }\n}\n",
                ),
                (
                    "web/site.js",
                    "function engine() {\n  return 1;\n}\n\nexport function run() {\n  return engine();\n}\n",
                ),
            ],
            &[
                (
                    "src/lib.rs",
                    Some("pub struct Options {\n    pub configured: bool,\n}\n\npub fn build() -> Options {\n    Options { configured: true }\n}\n"),
                ),
                (
                    "web/site.js",
                    Some("const engine = once(() => 1);\n\nexport function run() {\n  return engine();\n}\n"),
                ),
            ],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        assert!(
            !report
                .contracts
                .iter()
                .any(|contract| contract.name == "engine"),
            "a function that became a variable of the same name is not removed"
        );
    }

    #[test]
    fn a_free_function_takes_cls_as_an_argument_and_an_instance_attribute_keeps_a_methods_name() {
        let temp = changed(
            &[(
                "pkg/make.py",
                "def transform(cls, these, kw_only, force, mro):\n    return cls\n\n\nclass Builder:\n    def __init__(self, cls):\n        self.attrs = transform(cls, 1, 2, 3, 4)\n\n    def add_dunders(self, method):\n        return method\n\n    def build(self):\n        return self.add_dunders(1)\n",
            )],
            &[(
                "pkg/make.py",
                Some("def transform(cls, these, kw_only, mro):\n    return cls\n\n\nclass Builder:\n    def __init__(self, cls):\n        self.attrs = transform(cls, 1, 2, 4)\n        self.add_dunders = self.safe\n\n    def safe(self, method):\n        return method\n\n    def build(self):\n        return self.add_dunders(1)\n"),
            )],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        let transform = report
            .contracts
            .iter()
            .find(|contract| contract.name == "transform")
            .expect("the changed function is reported");
        assert_eq!(transform.sites_fitting, 1, "{}", report.to_text());
        assert!(
            !report
                .contracts
                .iter()
                .any(|contract| contract.name == "add_dunders"),
            "a method that became an instance attribute of the same name is not removed"
        );
    }

    #[test]
    fn a_quoted_path_an_attribute_before_the_name_and_a_vendored_copy_given_up_are_not_breaks() {
        // A name inside a single-quoted module path is a string where the language has them.
        assert!(usage(
            "const { runCommand } = require('./helpers/run-command.cjs');",
            "run",
            None,
            true
        )
        .is_none());
        assert!(
            usage("fn f<'a>(x: &'a str) { run(x) }", "run", None, false).is_some_and(|u| u.called)
        );
        // The parameter list is the one after the name, not an attribute's.
        let attributed = shape(
            &Language::CSharp,
            "#if NET5 [Requires(\"Finds accessors by name\")] #endif public static object? ConvertToType(string value, Type toType)",
            true,
            "ConvertToType",
        )
        .unwrap();
        assert_eq!(attributed.params.len(), 2);
        let generic = shape(
            &Language::Java,
            "<T extends Comparable<T>> T max(List<T> items, T floor)",
            true,
            "max",
        )
        .unwrap();
        assert_eq!(generic.params.len(), 2);

        // A vendored crate deleted in favour of the registry crate of the same name: the
        // imports resolve to the dependency, so they are not claimed.
        let temp = changed(
            &[
                ("Cargo.toml", "[workspace]\nmembers = [\"app\"]\n"),
                (
                    "app/Cargo.toml",
                    "[package]\nname = \"app\"\n\n[dependencies]\nproc-macro2 = \"1\"\n",
                ),
                (
                    "precompiled/proc-macro2/src/lib.rs",
                    "pub struct Span;\n\npub fn fallback() -> u8 {\n    1\n}\n",
                ),
                (
                    "app/src/lib.rs",
                    "use proc_macro2::Span;\n\npub fn at() -> Span {\n    Span\n}\n",
                ),
                ("app/src/old.rs", "pub fn gone() -> u8 {\n    1\n}\n"),
                (
                    "app/src/main.rs",
                    "mod old;\n\nfn main() {\n    let _ = old::gone();\n}\n",
                ),
            ],
            &[
                ("precompiled/proc-macro2/src/lib.rs", None),
                ("app/src/old.rs", None),
            ],
        );
        let report = report(&temp);
        // The module that simply went away still breaks its caller.
        assert_eq!(
            broken(&report),
            vec!["app/src/main.rs:4 still names `gone`, which this change removed".to_string()],
            "{}",
            report.to_text()
        );
    }

    #[test]
    fn a_trailing_comment_a_member_constant_and_an_owner_imported_too_are_not_breaks() {
        assert_eq!(
            without_trailing_comment("break // whatever context is reset to", &Language::Go),
            "break "
        );
        assert_eq!(
            without_trailing_comment("u := \"http://x\" // context", &Language::Go),
            "u := \"http://x\" "
        );
        assert_eq!(
            without_trailing_comment("x = d['#k']  # context", &Language::Python),
            "x = d['#k']  "
        );
        assert!(is_comment_line("\u{feff}#if !NET5_0_OR_GREATER"));

        let temp = changed(
            &[
                (
                    "src/Tests.cs",
                    "class IntegratedTests {\n    const int Port = 5001;\n\n    int Url() {\n        return Port;\n    }\n}\n",
                ),
                (
                    "src/Tools.cs",
                    "class Tools {\n    bool Basic(Uri url) {\n        return url is { Scheme: \"http\", Port: 80 };\n    }\n}\n",
                ),
                (
                    "src/backtrace.rs",
                    "pub(crate) use std::backtrace::Backtrace;\n\nstruct Local;\n\nimpl Backtrace {\n    fn capture() -> u8 {\n        1\n    }\n}\n\nfn make() -> u8 {\n    crate::backtrace::Backtrace::capture()\n}\n",
                ),
                (
                    "route.go",
                    "package mux\n\ntype context struct{}\n\nfunc match() int {\n\treturn 0 // fresh context is empty\n}\n",
                ),
            ],
            &[
                (
                    "src/Tests.cs",
                    Some("class IntegratedTests {\n    int Url() {\n        return 5001;\n    }\n}\n"),
                ),
                (
                    "src/backtrace.rs",
                    Some("pub(crate) use std::backtrace::Backtrace;\n\nstruct Local;\n\nfn make() -> u8 {\n    crate::backtrace::Backtrace::capture()\n}\n"),
                ),
                (
                    "route.go",
                    Some("package mux\n\nfunc match() int {\n\treturn 0 // fresh context is empty\n}\n"),
                ),
            ],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
    }

    #[test]
    fn a_variable_a_parameter_or_an_imported_package_of_the_name_is_not_the_declaration() {
        let temp = changed(
            &[
                (
                    "cfg/types.go",
                    "package cfg\n\ntype watcher struct{}\n\ntype finder struct{}\n\ntype logger struct{}\n\nfunc (l logger) Info(msg string) {}\n\nfunc registry() int {\n\treturn 1\n}\n",
                ),
                (
                    "cfg/use.go",
                    "package cfg\n\nimport (\n\t\"example.com/x/finder\"\n)\n\nfunc run(v *V) {\n\twatcher, err := newWatcher()\n\tdefer watcher.Close()\n\tnames := finder.Names()\n\tv.logger.Info(\"x\")\n\t_ = err\n\t_ = names\n}\n\nfunc keep(registry int) int {\n\treturn registry\n}\n\nfunc real() int {\n\treturn registry()\n}\n",
                ),
                ("cfg/other.go", "package cfg\n\nfunc real2() int {\n\treturn registry()\n}\n"),
            ],
            &[("cfg/types.go", Some("package cfg\n\ntype logger interface{}\n"))],
        );
        let report = report(&temp);
        // `registry()` in other.go is the removed function. In use.go the file has a parameter
        // of that name, so its call is not claimed; nor are the variable `watcher`, the
        // imported package `finder`, or `Info` on a field called `logger`.
        assert_eq!(
            broken(&report),
            vec!["cfg/other.go:4 still names `registry`, which this change removed".to_string()],
            "{}",
            report.to_text()
        );
        assert_eq!(
            binding_of("\twatcher, err := fsnotify.NewWatcher()", "watcher"),
            Some(Binding::Variable)
        );
        assert_eq!(
            binding_of("    let mut engine = build();", "engine"),
            Some(Binding::Variable)
        );
        assert_eq!(
            binding_of("for item in items:", "item"),
            Some(Binding::Variable)
        );
        assert_eq!(
            binding_of("type Handle = fs::Handle;", "Handle"),
            Some(Binding::Declaration)
        );
        assert_eq!(binding_of("    x = engine(1)", "engine"), None);
        assert_eq!(binding_of("if engine == other {", "engine"), None);
        assert!(parameter_named("def run(self, engine, *rest):", "engine"));
        assert!(!parameter_named("def run(self, motor):", "engine"));
    }

    #[test]
    fn rust_and_typescript_calls_are_held_to_the_parameter_count_and_javascript_is_not() {
        let temp = changed(
            &[
                (
                    "crates/core/src/clearing.rs",
                    "pub fn owed(root: &str, scope: u8) -> bool {\n    true\n}\n",
                ),
                (
                    "crates/core/src/lib.rs",
                    "mod clearing;\n\npub fn run() -> bool {\n    clearing::owed(\n        \"x\",\n        1,\n    )\n}\n",
                ),
                ("web/math.ts", "export function scale(value: number): number {\n  return value;\n}\n"),
                (
                    "web/use.ts",
                    "import { scale } from './math';\n\nexport const a = scale(1);\nexport const b = scale(1, 2);\n",
                ),
                ("web/loose.js", "export function open(path) {\n  return path;\n}\n"),
                ("web/caller.js", "import { open } from './loose.js';\n\nopen('a');\n"),
            ],
            &[
                (
                    "crates/core/src/clearing.rs",
                    Some("pub fn owed(root: &str, skipped: &[u8], scope: u8) -> bool {\n    true\n}\n"),
                ),
                (
                    "web/math.ts",
                    Some("export function scale(value: number, factor: number): number {\n  return value * factor;\n}\n"),
                ),
                (
                    "web/loose.js",
                    Some("export function open(path, mode) {\n  return path;\n}\n"),
                ),
            ],
        );
        let report = report(&temp);
        assert_eq!(
            broken(&report),
            vec![
                "crates/core/src/lib.rs:4 passes 2 arguments; 3 are required now".to_string(),
                "web/use.ts:3 passes 1 argument; 2 are required now".to_string(),
            ],
            "{}",
            report.to_text()
        );
    }

    #[test]
    fn a_method_is_attributed_through_its_owner_and_not_through_an_unknown_receiver() {
        let temp = changed(
            &[(
                "pkg/shapes.py",
                "class Circle:\n    def area(self, scale):\n        return scale\n\n    def double(self):\n        return self.area(2)\n\n\ndef total(shapes):\n    return sum(shape.area(1) for shape in shapes)\n",
            )],
            &[(
                "pkg/shapes.py",
                Some("class Circle:\n    def area(self, scale, unit):\n        return scale\n\n    def double(self):\n        return self.area(2)\n\n\ndef total(shapes):\n    return sum(shape.area(1) for shape in shapes)\n"),
            )],
        );
        let report = report(&temp);
        let contract = &report.contracts[0];
        assert_eq!(contract.display_name(), "Circle.area");
        assert_eq!(
            broken(&report),
            vec!["pkg/shapes.py:6 does not pass `unit`, which is required now".to_string()]
        );
        assert_eq!(contract.possible.len(), 1, "{}", report.to_text());
        assert_eq!(contract.possible[0].line, 10);
    }

    #[test]
    fn only_files_limits_the_declarations_examined_and_no_change_reports_nothing() {
        let temp = changed(
            &[
                ("a.py", "def one(x):\n    pass\n"),
                ("b.py", "def two(x):\n    pass\n"),
                (
                    "c.py",
                    "from a import one\nfrom b import two\n\none(1)\ntwo(1)\n",
                ),
            ],
            &[
                ("a.py", Some("def one(x, y):\n    pass\n")),
                ("b.py", Some("def two(x, y):\n    pass\n")),
            ],
        );
        assert_eq!(report(&temp).broken_sites, 2);
        let only = ripple(
            temp.path(),
            None,
            &RippleInput {
                base: None,
                only_files: vec![PathBuf::from("a.py")],
            },
        )
        .unwrap();
        assert_eq!(
            broken(&only),
            vec!["c.py:4 does not pass `y`, which is required now"]
        );
        git_in(temp.path(), &["checkout", "-q", "--", "."]);
        let clean = report(&temp);
        assert!(clean.contracts.is_empty());
        assert!(clean.headline().contains("no signature changed"));
    }

    #[test]
    fn shapes_fits_and_usages_read_what_each_language_writes() {
        let python = shape(
            &Language::Python,
            "def f(self, a, b=1, /, c=2, *args, d, e=3, **kw)",
            true,
            "f",
        )
        .unwrap();
        assert!(python.receiver && python.varargs && python.varkw);
        assert_eq!(
            python
                .params
                .iter()
                .map(|p| (
                    p.name.as_str(),
                    p.required,
                    p.keyword_only,
                    p.positional_only
                ))
                .collect::<Vec<_>>(),
            vec![
                ("a", true, false, true),
                ("b", false, false, true),
                ("c", false, false, false),
                ("d", true, true, false),
                ("e", false, true, false),
            ]
        );
        let rust = shape(
            &Language::Rust,
            "fn(&mut self, a: Vec<(u8, u8)>, f: impl Fn(u8, u8) -> u8) bool",
            true,
            "apply",
        )
        .unwrap();
        assert!(rust.receiver);
        assert_eq!(rust.params.len(), 2);
        let ts = shape(
            &Language::TypeScript,
            "f(a: Map<string, number>, b?: string, ...rest: any[])",
            false,
            "f",
        )
        .unwrap();
        assert_eq!(
            ts.params.iter().map(|p| p.required).collect::<Vec<_>>(),
            vec![true, false]
        );
        assert!(ts.varargs);

        let lines = [
            "x = f(a, key=lambda p, q: p, *rest)",
            "g(|a, b| a + b, 3)",
            "h(",
            "    1,  # one",
            "    'a,b',",
            ")",
        ];
        let call = call_args(&Language::Python, &lines, 1, "f").unwrap();
        assert_eq!(
            (call.positional, call.keywords.clone(), call.spread),
            (1, vec!["key".to_string()], true)
        );
        assert_eq!(
            call_args(&Language::Rust, &lines, 2, "g")
                .unwrap()
                .positional,
            2
        );
        assert_eq!(
            call_args(&Language::Python, &lines, 3, "h")
                .unwrap()
                .positional,
            2
        );

        let two = Shape {
            params: vec![positional(true), positional(true)],
            ..Shape::default()
        };
        let one = CallArgs {
            positional: 1,
            ..CallArgs::default()
        };
        assert!(matches!(
            fit(&Language::Rust, &two, &one, false),
            Fit::Breaks(_)
        ));
        assert_eq!(
            fit(
                &Language::Rust,
                &two,
                &CallArgs {
                    spread: true,
                    ..one.clone()
                },
                false
            ),
            Fit::Unknown("it unpacks an argument list")
        );

        assert!(usage("let s = \"call name(1) here\";", "name", None, false).is_none());
        assert!(usage("print(f\"{name(1)}\")", "name", None, true).is_some_and(|u| u.called));
        assert!(usage("fn name(a: u8) {}", "name", None, false).is_some_and(|u| u.declares));
        let used = usage("Self::name(1)", "name", Some("Owner"), false).unwrap();
        assert!(used.called && used.by_self && used.by_path);
        assert_eq!(
            qualifier("x = headers::Host::new()", "Host"),
            Some("headers")
        );

        let modules = module_names("crates/axum-extra/src/extract/host.rs");
        assert!(
            modules.contains("axum_extra") && modules.contains("host") && !modules.contains("src")
        );
        assert_eq!(
            import_evidence("use axum_extra::extract::Host;\n", "Host", &modules),
            Some(true)
        );
        assert_eq!(import_evidence("use {\n    same_file::Host,\n    walkdir::WalkDir,\n};\n\nmod tests {\n    use super::*;\n}\n", "Host", &modules), Some(false));
        assert_eq!(import_evidence("fn f() {}\n", "Host", &modules), None);
        assert_eq!(
            import_evidence("from axum_extra import *\n", "Host", &modules),
            Some(true)
        );
        assert!(in_python_docstring(
            "def f():\n    \"\"\"One\n    two\n    \"\"\"\n    pass\n",
            3
        ));
        assert!(!in_python_docstring(
            "def f():\n    \"\"\"One\n    two\n    \"\"\"\n    pass\n",
            5
        ));
    }
}
