//! What a change breaks outside itself: the calls that no longer fit a function whose
//! parameters it changed, and the code that still names a declaration it removed.
//!
//! A diff shows what changed. It cannot show the caller in another file that still passes two
//! arguments to a function that now requires three. This module finds those callers.
//!
//! It needs no index. The files of the change are parsed as they were at the base revision and
//! as they are now, and the two sets of declarations are compared. The lines that hold each
//! changed declaration's name are found in the working tree (`git grep`), and each file with
//! one is read through its syntax tree (`open_kioku_tree_sitter::name_uses`): a call is a call
//! node with its arguments counted from argument nodes, never a line that looks like one. A
//! call is reported only when it fit the declaration as it was and does not fit it as it is; a
//! call that fits is not reported, whether or not the change touched it. An index, when there
//! is one, adds what a name cannot: the call edges it proved.

use open_kioku_core::{
    File, FileId, GitChangeKind, GraphEdgeType, Language, RelationshipAuthority, RepositoryId,
    Symbol, SymbolKind,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_git::DiffFile;
use open_kioku_languages::detect_language;
use open_kioku_storage::OkStore;
use open_kioku_tree_sitter::name_uses::{
    name_uses, CallArguments, FileNameUses, NameUse, NameUseKind, Reached,
};
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
/// Files with a hit that are parsed in one run; hits in files past it are counted as
/// `possible` and not read.
const MAX_FILES_PARSED: usize = 400;
/// A file larger than this is not parsed for sites.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Changed code files whose declarations are compared in one run.
const MAX_CHANGED_FILES: usize = 300;
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fit {
    Fits,
    Breaks(String),
    Unknown(&'static str),
}

/// Whether `call` fits `shape`. `explicit_receiver` is a call that names the owner type and so
/// may pass the receiver as its first argument.
fn fit(language: &Language, shape: &Shape, call: &CallArguments, explicit_receiver: bool) -> Fit {
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
    // A Rust path call (`Type::method(value)`) and a C# extension method called on its class
    // (`Ext.Trim(value)`) pass the receiver as the first argument.
    let receiver_passed = shape.receiver
        && explicit_receiver
        && matches!(language, Language::Rust | Language::CSharp);
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

/// Names a language provides itself: a call of one is not evidence of a use of a declaration
/// the repository once had under that name.
const BUILTIN_NAMES: &[&str] = &[
    "abs", "all", "any", "append", "bool", "bytes", "cap", "clear", "close", "copy", "delete",
    "dict", "dir", "filter", "float", "format", "hash", "id", "input", "int", "iter", "len",
    "list", "make", "map", "max", "min", "new", "next", "open", "panic", "print", "println",
    "range", "recover", "repr", "round", "set", "sorted", "str", "sum", "tuple", "type", "vars",
    "zip",
];

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
fn import_says(imports: &[String], name: &str, modules: &BTreeSet<String>) -> Option<bool> {
    let mut named: Option<bool> = None;
    let mut everything = false;
    for statement in imports {
        let names_it = !word_offsets(statement, name).is_empty();
        let glob = statement.contains("::*")
            || statement.contains("import *")
            || statement.contains(".*;");
        if !names_it && !glob {
            continue;
        }
        let relative = statement.contains("from .")
            || statement.contains("'./")
            || statement.contains("\"./")
            || statement.contains("'../")
            || statement.contains("\"../")
            || statement.contains("super::")
            || statement.contains("self::")
            || statement.contains("crate::");
        let from_module = mentions_any(statement, modules);
        if names_it {
            named = Some(named.unwrap_or(false) || relative || from_module);
        } else if from_module {
            everything = true;
        }
    }
    named.or(everything.then_some(true))
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
            // A line of 0 names no line.
            if lines.start > 0 {
                sites.insert((slashed(range.path.as_path()), lines.start));
            }
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
    /// The parameters as they were, and as they are.
    before_shape: Option<Shape>,
    shape: Option<Shape>,
    language: Language,
    index_path: PathBuf,
    /// The declaring file is gone.
    file_deleted: bool,
    /// The type that declares it names a base: a member it stops declaring may still be
    /// inherited.
    owner_has_bases: bool,
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

/// The Go import path of the package a file is in: the `module` of the nearest `go.mod` above
/// it and the directories between. `None` without a `go.mod`.
fn go_import_path(repo: &Path, file: &str) -> Option<String> {
    let mut dir = Path::new(file).parent()?;
    loop {
        if let Ok(text) = fs::read_to_string(repo.join(dir).join("go.mod")) {
            let module = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("module "))?
                .trim()
                .trim_matches('"');
            let below = Path::new(file).parent()?.strip_prefix(dir).ok()?;
            let below = slashed(below);
            return Some(if below.is_empty() {
                module.to_string()
            } else {
                format!("{module}/{below}")
            });
        }
        dir = dir.parent()?;
    }
}

/// The last name of a receiver's text, and its first: `utils` and `click` of `click.utils`,
/// `super()` of `super()`.
fn receiver_ends(receiver: &str) -> (&str, &str) {
    let is_separator = |ch: char| ch == '.' || ch == ':';
    let first = receiver.split(is_separator).next().unwrap_or(receiver);
    let last = receiver.rsplit(is_separator).next().unwrap_or(receiver);
    (first.trim(), last.trim())
}

fn is_self_receiver(name: &str) -> bool {
    matches!(name, "self" | "this" | "Self" | "cls")
}

fn is_parent_receiver(receiver: &str) -> bool {
    let (first, _) = receiver_ends(receiver);
    matches!(first, "super" | "super()" | "base") || receiver.starts_with("super(")
}

/// The facts of the files that hold a hit, read once for every name asked of them.
struct SiteFiles<'a> {
    repo: &'a Path,
    facts: BTreeMap<String, Option<FileNameUses>>,
    texts: BTreeMap<String, String>,
    /// Whether a dependency manifest names each module asked about.
    dependencies: BTreeMap<String, bool>,
    go_imports: BTreeMap<String, Option<String>>,
}

impl SiteFiles<'_> {
    /// Parses each file of `wanted` for the names asked of it, up to the limits.
    fn read(&mut self, wanted: &BTreeMap<String, BTreeSet<String>>, deadline: std::time::Instant) {
        for (path, names) in wanted {
            let allowed = self.facts.values().flatten().count() < MAX_FILES_PARSED
                && std::time::Instant::now() < deadline
                && fs::metadata(self.repo.join(path))
                    .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
            let facts = allowed
                .then(|| fs::read_to_string(self.repo.join(path)).ok())
                .flatten()
                .and_then(|text| {
                    let facts = name_uses(&detect_language(Path::new(path)), &text, names);
                    self.texts.insert(path.clone(), text);
                    facts
                });
            self.facts.insert(path.clone(), facts);
        }
    }

    fn line(&self, path: &str, line: u32) -> String {
        self.texts
            .get(path)
            .and_then(|text| text.lines().nth(line.checked_sub(1)? as usize))
            .map(clip)
            .unwrap_or_default()
    }

    /// How a site that is not through a member's owner reaches the declaring module, if it
    /// does: an import of the name from it, a qualifier an import binds to it, or a language
    /// rule that needs neither.
    fn reaches(
        &mut self,
        path: &str,
        found: &NameUse,
        name: &str,
        modules: &BTreeSet<String>,
        declaring_path: &str,
    ) -> bool {
        let Some(Some(facts)) = self.facts.get(path) else {
            return false;
        };
        let language = detect_language(Path::new(path));
        let directory = |path: &str| path.rsplit_once('/').map(|(dir, _)| dir.to_string());
        let same_directory = directory(path) == directory(declaring_path);
        if found.kind == NameUseKind::Import {
            // The import statement itself: it is a use of the declaration only when it imports
            // from the declaring module.
            return import_says(&facts.imports, name, modules) == Some(true)
                || (path == declaring_path);
        }
        match &found.reached {
            Reached::Bare => {
                // The file has a variable, a parameter or a field of the name of its own.
                if facts.bound.contains(name) {
                    return false;
                }
                if path == declaring_path {
                    return true;
                }
                match language {
                    // A namespace is opened whole; no statement names the declaration.
                    Language::CSharp => true,
                    // A package is a directory: its files see each other, and others must
                    // qualify. A file that imports a package of the name means the package.
                    Language::Go => {
                        same_directory && import_says(&facts.imports, name, modules) != Some(false)
                    }
                    _ => match import_says(&facts.imports, name, modules) {
                        Some(ours) => ours,
                        None => language == Language::Java && same_directory,
                    },
                }
            }
            Reached::Member(receiver) => {
                let (first, last) = receiver_ends(receiver);
                // The qualifier is a value of this file, or names no module of the declaration.
                if facts.bound.contains(first) || !modules.contains(last) {
                    return false;
                }
                match language {
                    Language::Go => {
                        // Only the import of the declaring package's own path binds the
                        // qualifier to it: `http` may be `net/http`.
                        let expected = self
                            .go_imports
                            .entry(declaring_path.to_string())
                            .or_insert_with(|| go_import_path(self.repo, declaring_path))
                            .clone();
                        expected.is_some_and(|expected| {
                            let quoted = format!("\"{expected}\"");
                            facts.imports.iter().any(|import| {
                                import.contains(&quoted)
                                    && import.split_whitespace().next().is_none_or(|alias| {
                                        alias.starts_with('"') || alias == first
                                    })
                            })
                        })
                    }
                    // The qualifier is the type, which its file is named after.
                    Language::Java | Language::CSharp => true,
                    // An import has to bind the qualifier.
                    _ => facts
                        .imports
                        .iter()
                        .any(|import| !word_offsets(import, first).is_empty()),
                }
            }
            Reached::Path(path_text) => {
                let (first, last) = receiver_ends(path_text);
                let scope = |segment: &str| matches!(segment, "crate" | "self" | "super");
                let last_is_ours = modules.contains(last) || scope(last);
                let first_is_known = scope(first)
                    || modules.contains(first)
                    || facts
                        .imports
                        .iter()
                        .any(|import| !word_offsets(import, first).is_empty());
                last_is_ours && first_is_known && !facts.bound.contains(first)
            }
        }
    }
}

/// Decides, for one changed declaration, which of the uses of its name are broken sites.
#[allow(clippy::too_many_arguments)]
fn classify_sites(
    files: &mut SiteFiles<'_>,
    diff: &[DiffFile],
    proven: Option<&BTreeSet<(String, u32)>>,
    contract: &mut ChangedContract,
    target: &Target,
    hits: &[Hit],
    too_common: bool,
) {
    let declared = &target.declared;
    let name = target.site_name.as_str();
    let removed = contract.change == ContractChange::Removed;
    let member = declared.member && !target.constructor;
    let callable = is_callable(&declared.kind) || target.constructor;
    let owner = if target.constructor {
        None
    } else {
        declared.owner.as_deref()
    };
    let builtin = BUILTIN_NAMES.contains(&name);
    let mut modules = module_names(&contract.path);
    if target.file_deleted {
        // A module that is gone may have given way to a dependency of the same name (a
        // vendored copy replaced by the package): an import by that name then resolves, and
        // is no evidence of a use of what was removed.
        let repo = files.repo;
        modules.retain(|module| {
            !*files.dependencies.entry(module.clone()).or_insert_with(|| {
                manifest_names(repo, module) || manifest_names(repo, &module.replace('_', "-"))
            })
        });
    }

    let mut by_file: BTreeMap<&str, Vec<&Hit>> = BTreeMap::new();
    for hit in hits {
        by_file.entry(hit.path.as_str()).or_default().push(hit);
    }
    if too_common {
        contract.possible_omitted = hits
            .iter()
            .filter(|hit| is_supported_code(&detect_language(Path::new(&hit.path))))
            .count();
        contract.note = Some(format!(
            "`{name}` appears on more than {MAX_NAME_HITS} lines; its sites were not read"
        ));
        return;
    }

    // Another declaration of the name anywhere means the name does not settle a reference to a
    // free function or a type. A member is attributed through its owner, whatever else shares
    // its name.
    let mut shared = false;
    let mut shared_in_owner = false;
    let mut unread = 0usize;
    let mut candidates: Vec<(String, NameUse)> = Vec::new();
    for (path, file_hits) in &by_file {
        if !is_supported_code(&detect_language(Path::new(path))) {
            if removed {
                contract.other_mentions += file_hits
                    .iter()
                    .filter(|hit| !is_comment_line(&hit.text))
                    .count();
            }
            continue;
        }
        if skips_path(path) {
            continue;
        }
        let Some(Some(facts)) = files.facts.get(*path) else {
            unread += file_hits.len();
            continue;
        };
        let uses = facts.uses.get(name).map(Vec::as_slice).unwrap_or_default();
        if removed {
            // A line the search found with no identifier of the name on it: a string or a
            // comment.
            contract.other_mentions += file_hits
                .iter()
                .filter(|hit| !uses.iter().any(|found| found.line == hit.line))
                .count();
        }
        for found in uses {
            match found.kind {
                NameUseKind::Declaration { binding: false } => {
                    // The changed declaration itself, where it still stands, is not another
                    // one; nor is the type whose constructor changed.
                    let is_self = *path == contract.path
                        && !removed
                        && (target.constructor || contract.line == Some(found.line));
                    if !is_self {
                        shared = true;
                        shared_in_owner |= owner.is_some() && found.within_type.as_deref() == owner;
                    }
                }
                NameUseKind::Declaration { binding: true } => {
                    // `export const run = create(...)` in another module: the name a removed
                    // function had is bound for a whole module again.
                    if removed
                        && *path != contract.path
                        && found.within_callable.is_none()
                        && found.within_type.is_none()
                    {
                        shared = true;
                    }
                }
                NameUseKind::Label => {}
                _ => candidates.push((path.to_string(), found.clone())),
            }
        }
    }
    contract.possible_omitted += unread;
    // With a file that holds the name unread, whether another declaration shares it is not
    // known. A removed method that its type declares elsewhere has moved (to another file of a
    // partial class), and a removed type or constant whose name is declared elsewhere may now
    // mean that one, whatever it was nested in.
    let moved = removed
        && if member && callable {
            shared_in_owner
        } else {
            shared
        };
    let unsettled = unread > 0 || moved;

    let mut seen = BTreeSet::new();
    let mut broken = Vec::new();
    let mut possible = Vec::new();
    for (path, found) in candidates {
        let called = matches!(found.kind, NameUseKind::Call(_));
        let imported = found.kind == NameUseKind::Import;
        if !removed && !called {
            // A changed signature leaves an import, and a function passed by name, valid.
            continue;
        }
        if removed && callable && !called && !imported {
            // The bare name of a removed function is as likely a field or a local variable of
            // that name as the function passed by value.
            continue;
        }
        if let Reached::Member(receiver) | Reached::Path(receiver) = &found.reached {
            // `super().get(id)` and `base.Run()` are the parent's member, never this one.
            if member && is_parent_receiver(receiver) {
                continue;
            }
        }
        if !seen.insert((path.clone(), found.line)) {
            continue;
        }
        let site_language = detect_language(Path::new(&path));
        let changed = diff.iter().find(|entry| {
            entry
                .new_path
                .as_ref()
                .is_some_and(|new| slashed(new) == path)
        });
        let proven_here = proven.is_some_and(|sites| {
            sites.contains(&(path.clone(), found.line))
                || sites.contains(&(path.clone(), line_before(changed, found.line)))
        });
        let attributed = if found.in_error || builtin || unsettled {
            false
        } else if member {
            let in_owner = owner.is_some() && found.within_type.as_deref() == owner;
            // A member the type no longer declares may still be inherited.
            let inherited = removed && target.owner_has_bases;
            match &found.reached {
                Reached::Member(receiver) | Reached::Path(receiver) => {
                    let (_, last) = receiver_ends(receiver);
                    if is_self_receiver(last) {
                        in_owner && !inherited
                    } else {
                        // `Owner.method`: a type is told from a variable by its capital, and
                        // Go has no call on a type.
                        owner.is_some_and(|owner| {
                            last == owner
                                && owner.starts_with(char::is_uppercase)
                                && site_language != Language::Go
                        })
                    }
                }
                // Java and C# call a member of the same type by its bare name.
                Reached::Bare => {
                    in_owner
                        && !inherited
                        && matches!(site_language, Language::Java | Language::CSharp)
                }
            }
        } else {
            !shared && files.reaches(&path, &found, name, &modules, &contract.path)
        };
        let mut evidence = if proven_here {
            SiteEvidence::Proven
        } else if attributed {
            SiteEvidence::Name
        } else {
            SiteEvidence::Possible
        };
        let problem = if removed {
            match &contract.renamed_to {
                Some(renamed) => format!("still names `{name}`, which is now `{renamed}`"),
                None => format!("still names `{name}`, which this change removed"),
            }
        } else {
            let (Some(shape), NameUseKind::Call(call)) = (target.shape.as_ref(), &found.kind)
            else {
                continue;
            };
            // A Rust path call and a call on the owner type pass the receiver themselves.
            let explicit_receiver = match &found.reached {
                Reached::Path(_) => true,
                Reached::Member(receiver) => {
                    owner.is_some_and(|owner| receiver_ends(receiver).1 == owner)
                }
                Reached::Bare => false,
            };
            match fit(&target.language, shape, call, explicit_receiver) {
                Fit::Fits => {
                    if evidence != SiteEvidence::Possible {
                        contract.sites_fitting += 1;
                    }
                    continue;
                }
                Fit::Unknown(why) => {
                    evidence = SiteEvidence::Possible;
                    format!("could not be checked: {why}")
                }
                Fit::Breaks(problem) => {
                    // A call that did not fit the declaration as it was is not one this
                    // change broke: it is not a call of this declaration, or it is read wrong.
                    let fit_before = target
                        .before_shape
                        .as_ref()
                        .map(|before| fit(&target.language, before, call, explicit_receiver));
                    if fit_before != Some(Fit::Fits) {
                        evidence = SiteEvidence::Possible;
                        format!(
                            "{problem}; it did not fit the declaration before this change either"
                        )
                    } else {
                        problem
                    }
                }
            }
        };
        let site = RippleSite {
            text: files.line(&path, found.line),
            path,
            line: found.line,
            within: found.within(),
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
    if let (Some(sites), true) = (proven, removed) {
        for (path, line) in sites {
            // The index's line may be from either side of a changed file; without the name on
            // it there is nothing to anchor the site to.
            let changed = diff.iter().any(|entry| {
                entry
                    .new_path
                    .as_ref()
                    .is_some_and(|new| &slashed(new) == path)
            });
            if seen.contains(&(path.clone(), *line)) || skips_path(path) || changed {
                continue;
            }
            let Some(source) = line.checked_sub(1).and_then(|index| {
                fs::read_to_string(files.repo.join(path))
                    .ok()?
                    .lines()
                    .nth(index as usize)
                    .map(str::to_string)
            }) else {
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
    contract.possible_omitted += possible.len().saturating_sub(MAX_SITES_LISTED);
    possible.truncate(MAX_SITES_LISTED);
    contract.broken = broken;
    contract.possible = possible;
    if moved || (shared && !member) {
        contract.note = Some(format!(
            "another declaration is also named `{name}`{}; sites the index did not prove are \
             listed as possible",
            if removed { " (it may have moved)" } else { "" }
        ));
    } else if unread > 0 {
        contract.note = Some(format!(
            "{unread} line(s) that hold `{name}` are in files that were not read (past the \
             limits of one run); they are counted as possible"
        ));
    }
}

/// Whether line `line` of `text` assigns to `name` through the instance: `self.name = ...`.
fn assigned_on(text: Option<&str>, line: u32, name: &str) -> bool {
    text.and_then(|text| text.lines().nth(line.checked_sub(1)? as usize))
        .is_some_and(|source| {
            word_offsets(source, name).into_iter().any(|offset| {
                let after = source[offset + name.len()..].trim_start();
                after.starts_with('=') && !after.starts_with("==")
            })
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
    let started = std::time::Instant::now();
    let deadline = started + SEARCH_BUDGET;
    let mut files_skipped = 0usize;
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
        // A change of thousands of files is compared up to a limit and a time: a hook waits
        // on this.
        if files_examined >= MAX_CHANGED_FILES || std::time::Instant::now() > deadline {
            files_skipped += 1;
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
        // What the file still binds under each name it declared, and which of its types name a
        // base: read from the trees, once, when a declaration is gone.
        let mut after_facts: Option<Option<FileNameUses>> = None;
        let mut before_facts: Option<Option<FileNameUses>> = None;
        let declared_names = before
            .iter()
            .map(|declared| declared.name.clone())
            .collect::<BTreeSet<_>>();
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
                // Still bound under the name: declared as another kind of thing (a function
                // that became a variable holding one, a method that became an attribute),
                // imported in its place, or exported under it. What uses the name is then not
                // known to be broken.
                (_, [])
                    if after.iter().any(|other| {
                        other.name == declared.name && other.owner == declared.owner
                    }) || after_facts
                        .get_or_insert_with(|| {
                            after_text
                                .as_deref()
                                .and_then(|text| name_uses(&language, text, &declared_names))
                        })
                        .as_ref()
                        .and_then(|facts| facts.uses.get(&declared.name))
                        .is_some_and(|uses| {
                            uses.iter().any(|found| {
                                matches!(
                                    found.kind,
                                    NameUseKind::Declaration { .. } | NameUseKind::Import
                                ) || (declared.member
                                    && matches!(&found.reached, Reached::Member(receiver)
                                        if is_self_receiver(receiver_ends(receiver).1))
                                    && assigned_on(
                                        after_text.as_deref(),
                                        found.line,
                                        &declared.name,
                                    ))
                            })
                        }) =>
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
            let owner_has_bases = declared.owner.as_ref().is_some_and(|owner| {
                before_facts
                    .get_or_insert_with(|| name_uses(&language, &before_text, &BTreeSet::new()))
                    .as_ref()
                    .is_some_and(|facts| facts.types_with_bases.contains(owner))
            });
            let (before_shape, after_shape) = match shapes {
                Some((before, after)) => (Some(before), Some(after)),
                None => (None, None),
            };
            let target = Target {
                site_name: if constructor {
                    declared.owner.clone().unwrap_or_default()
                } else {
                    declared.name.clone()
                },
                constructor,
                before_shape,
                shape: after_shape,
                language: language.clone(),
                index_path: old_path.clone(),
                file_deleted: new.is_none(),
                owner_has_bases,
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

    if files_skipped > 0 {
        caveats.push(format!(
            "{files_skipped} changed file(s) were not compared: one run compares {MAX_CHANGED_FILES} \
             and stops after {} seconds",
            SEARCH_BUDGET.as_secs()
        ));
    }

    // The lines that hold each breaking declaration's name, then one read of each file that
    // has any, for every name asked of it.
    let mut contracts_omitted = contracts_omitted;
    let mut index_unreadable = None::<String>;
    let mut searched = Vec::new();
    let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (mut contract, target) in found {
        if contract.effect != ContractEffect::Breaking {
            searched.push((contract, target, None));
            continue;
        }
        if std::time::Instant::now() > deadline {
            contracts_omitted += 1;
            continue;
        }
        if !is_identifier(&target.site_name) {
            contract.note =
                Some("the name is not an identifier; its sites were not searched".into());
            searched.push((contract, target, None));
            continue;
        }
        let hits = name_hits(repo, &target.site_name)?;
        let too_common = hits.len() > MAX_NAME_HITS;
        if !too_common {
            for hit in &hits {
                if is_supported_code(&detect_language(Path::new(&hit.path)))
                    && !skips_path(&hit.path)
                {
                    wanted
                        .entry(hit.path.clone())
                        .or_default()
                        .insert(target.site_name.clone());
                }
            }
        }
        // An index that cannot be read (built by an earlier version, locked, damaged) is the
        // same as no index: the sites are attributed by name, and the report says so.
        let proven = match store {
            Some(store) if !target.constructor => {
                match proven_sites(store, &target.index_path, &target.declared, contract.change) {
                    Ok(sites) => sites,
                    Err(error) => {
                        index_unreadable.get_or_insert_with(|| error.to_string());
                        None
                    }
                }
            }
            _ => None,
        };
        searched.push((contract, target, Some((hits, too_common, proven))));
    }
    let mut files = SiteFiles {
        repo,
        facts: BTreeMap::new(),
        texts: BTreeMap::new(),
        dependencies: BTreeMap::new(),
        go_imports: BTreeMap::new(),
    };
    files.read(&wanted, deadline);
    let mut contracts = Vec::new();
    for (mut contract, target, search) in searched {
        if let Some((hits, too_common, proven)) = search {
            classify_sites(
                &mut files,
                &diff,
                proven.as_ref(),
                &mut contract,
                &target,
                &hits,
                too_common,
            );
        }
        contracts.push(contract);
    }
    if std::time::Instant::now() > deadline {
        caveats.push(format!(
            "the search stopped after {} seconds; what it had not reached is counted and was \
             not read",
            SEARCH_BUDGET.as_secs()
        ));
    }
    if let Some(error) = &index_unreadable {
        caveats.push(format!(
            "the index could not be read ({error}); sites are attributed by name, as with no \
             index"
        ));
    }
    let broken_sites = contracts
        .iter()
        .map(|contract| contract.broken.len() + contract.broken_omitted)
        .sum();
    let index_used = store.is_some() && index_unreadable.is_none();
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
        index_used,
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
    fn a_vendored_copy_given_up_for_a_dependency_of_the_same_name_is_not_a_break() {
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

    fn possible(report: &RippleReport) -> Vec<String> {
        report
            .contracts
            .iter()
            .flat_map(|contract| contract.possible.iter())
            .map(|site| format!("{}:{}", site.path, site.line))
            .collect()
    }

    /// Calls whose text a line reader miscounts: each is read from its argument nodes.
    #[test]
    fn arguments_with_shifts_quotes_comments_and_strings_are_counted_right() {
        let temp = changed(
            &[
                ("mem/alloc.go", "package mem\n\nfunc Alloc(size int) []byte {\n\treturn make([]byte, size)\n}\n"),
                ("mem/use.go", "package mem\n\nfunc Use() []byte {\n\treturn Alloc(1<<10)\n}\n"),
                ("log.py", "def warn(msg, code):\n    print(msg, code)\n\n\ndef emit(text, indent):\n    print(text)\n\n\ndef scale(value):\n    return value\n"),
                ("app.py", "from log import warn, emit, scale\n\nwarn('bad', 3)\n"),
            ],
            &[
                ("mem/alloc.go", Some("package mem\n\nfunc Alloc(size int, align int) []byte {\n\treturn make([]byte, size+align)\n}\n")),
                (
                    "mem/use.go",
                    Some("package mem\n\nfunc Use() []byte {\n\treturn Alloc(\n\t\t1<<10,\n\t\t// align: eight, as agreed with ops\n\t\t8,\n\t)\n}\n"),
                ),
                ("log.py", Some("def warn(msg):\n    print(msg)\n\n\ndef emit(text, indent, width):\n    print(text)\n\n\ndef scale(value, factor):\n    return value * factor\n")),
                (
                    "app.py",
                    Some("from log import warn, emit, scale\n\nwarn(f'bad value {x}, expected {y}')\nwarn('The request could not be completed in time, so it will be retried shortly')\nemit('(', 1, 2)\nemit(')', 1, 2)\nprint(\"scale(3) ->\", scale(3, 2))\n"),
                ),
            ],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        assert!(possible(&report).is_empty(), "{}", report.to_text());
        let fitting = report
            .contracts
            .iter()
            .map(|contract| (contract.name.as_str(), contract.sites_fitting))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            fitting,
            [("Alloc", 1), ("emit", 2), ("scale", 1), ("warn", 2)]
                .into_iter()
                .collect()
        );
    }

    /// A call is broken only if it fit the declaration as it was: one that never fit is not a
    /// call of it (a decorator rewrote the function), or is read wrong.
    #[test]
    fn a_call_that_did_not_fit_before_the_change_is_possible_and_not_counted() {
        let temp = changed(
            &[(
                "cli.py",
                "import click\n\n\n@click.command()\ndef hello(count):\n    pass\n\n\ndef plain(count):\n    pass\n\n\nif __name__ == '__main__':\n    hello()\n    plain(1)\n",
            )],
            &[(
                "cli.py",
                Some("import click\n\n\n@click.command()\ndef hello(count, name):\n    pass\n\n\ndef plain(count, name):\n    pass\n\n\nif __name__ == '__main__':\n    hello()\n    plain(1)\n"),
            )],
        );
        let report = report(&temp);
        assert_eq!(
            broken(&report),
            vec!["cli.py:15 does not pass `name`, which is required now".to_string()],
            "{}",
            report.to_text()
        );
        assert_eq!(possible(&report), vec!["cli.py:14"]);
        let hello = &report
            .contracts
            .iter()
            .find(|c| c.name == "hello")
            .unwrap()
            .possible[0];
        assert!(hello
            .problem
            .contains("did not fit the declaration before this change either"));
    }

    /// A declaration of the same name is not a call, and a name kept alive by an import or an
    /// export list was not removed.
    #[test]
    fn member_declarations_and_rebinding_imports_are_not_broken_sites() {
        let temp = changed(
            &[
                ("api.ts", "export function fetchUser(id: string): string {\n  return id;\n}\n\nexport function fetchAll(): string[] {\n  return [];\n}\n"),
                ("deps.ts", "import { fetchUser, fetchAll } from './api';\n\ninterface Deps {\n  fetchUser(id: string): string;\n}\n\nexport const all = fetchAll();\n"),
                ("store/store.go", "package store\n\ntype Store interface {\n\tLookup(key string) string\n}\n\ntype Mem struct{}\n\nfunc (m *Mem) Lookup(key string) string {\n\treturn key\n}\n\nfunc Lookup(key string) string {\n\treturn key\n}\n"),
                ("net.py", "import time\n\n\ndef retry(times):\n    def wrap(fn):\n        return fn\n    return wrap\n\n\n@retry(3)\ndef fetch(url):\n    return url\n"),
                ("use.py", "from net import retry\n\n\n@retry(2)\ndef other():\n    pass\n"),
            ],
            &[
                ("api.ts", Some("export function fetchUser(id: string, tenant: string): string {\n  return id + tenant;\n}\n\nfunction fetchEvery(): string[] {\n  return [];\n}\n\nexport { fetchEvery as fetchAll };\n")),
                ("store/store.go", Some("package store\n\ntype Store interface {\n\tLookup(key string) string\n}\n\ntype Mem struct{}\n\nfunc (m *Mem) Lookup(key string) string {\n\treturn key\n}\n\nfunc Lookup(key string, fallback string) string {\n\treturn key\n}\n")),
                ("net.py", Some("import time\nfrom tenacity import retry\n\n\n@retry(3)\ndef fetch(url):\n    return url\n")),
            ],
        );
        let report = report(&temp);
        assert_eq!(report.broken_sites, 0, "{}", report.to_text());
        // `retry` and `fetchAll` are still bound in their files: neither is a removal.
        let names = report
            .contracts
            .iter()
            .map(|contract| contract.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["fetchUser", "Lookup"], "{}", report.to_text());
    }

    /// A qualifier is the declaring module only when an import binds it to it, and a member is
    /// this type's only through this type.
    #[test]
    fn a_receiver_is_not_attributed_by_its_spelling() {
        let temp = changed(
            &[
                ("go.mod", "module example.com/app\n\ngo 1.22\n"),
                ("internal/http/client.go", "package http\n\nfunc Get(url string) string {\n\treturn url\n}\n"),
                ("cmd/std.go", "package main\n\nimport \"net/http\"\n\nfunc std() {\n\thttp.Get(\"https://example.com\")\n}\n"),
                ("cmd/own.go", "package main\n\nimport \"example.com/app/internal/http\"\n\nfunc own() {\n\thttp.Get(\"https://example.com\")\n}\n"),
                ("store/cache.py", "def lookup(key):\n    return key\n"),
                ("store/use.py", "import redis\n\ncache = redis.Redis()\ncache.lookup('k')\n"),
                ("store/real.py", "from store import cache\n\ncache.lookup('k')\n"),
                (
                    "repo.py",
                    "class Repo(Base):\n    def get(self, id):\n        return super().get(id)\n\n    def start(self):\n        pass\n\n    def run(self):\n        self.start()\n        return self.get(1)\n",
                ),
                ("Ext.cs", "static class StringExt {\n    public static string Truncate(this string s, int n) {\n        return s;\n    }\n\n    static string Use(string s) {\n        return StringExt.Truncate(s, 5);\n    }\n}\n"),
            ],
            &[
                ("internal/http/client.go", Some("package http\n\nfunc Get(url string, timeout int) string {\n\treturn url\n}\n")),
                ("store/cache.py", Some("def lookup(key, default):\n    return key\n")),
                (
                    "repo.py",
                    Some("class Repo(Base):\n    def get(self, id, tenant):\n        return super().get(id)\n\n    def run(self):\n        self.start()\n        return self.get(1)\n"),
                ),
                ("Ext.cs", Some("static class StringExt {\n    public static string Truncate(this string s, int n, string tail) {\n        return s;\n    }\n\n    static string Use(string s) {\n        return StringExt.Truncate(s, 5, \"...\");\n    }\n}\n")),
            ],
        );
        let report = report(&temp);
        assert_eq!(
            broken(&report),
            vec![
                // The package the file imports by its own module path.
                "cmd/own.go:6 passes 1 argument; 2 are required now".to_string(),
                // `self.get(1)` inside the type that declares it.
                "repo.py:7 does not pass `tenant`, which is required now".to_string(),
                // The module the file imports.
                "store/real.py:3 does not pass `default`, which is required now".to_string(),
            ],
            "{}",
            report.to_text()
        );
        // `http` of `net/http`, a variable called `cache`, and `self.start()` in a type with a
        // base, which may inherit it: listed, never counted. `super().get(id)` is the parent's
        // and is not listed at all; the extension method called on its class fits.
        assert_eq!(
            possible(&report),
            vec!["cmd/std.go:6", "repo.py:6", "store/use.py:4"],
            "{}",
            report.to_text()
        );
    }

    /// Alarms the replay raised on complete commits: a method moved to another file of its
    /// partial class, a nested type whose name another declaration still carries, a
    /// compilation symbol on a preprocessor line, another package's type of the same name
    /// (`slog.Logger`), and a function that another module now binds as a constant.
    #[test]
    fn a_declaration_that_moved_and_a_directive_symbol_are_not_broken_sites() {
        let temp = changed(
            &[
                (
                    "Requests.cs",
                    "static partial class Requests {\n    static void CheckHost(string name) {\n    }\n}\n",
                ),
                (
                    "Requests.Headers.cs",
                    "static partial class Requests {\n    static void Add(string name) {\n        CheckHost(name);\n    }\n}\n",
                ),
                (
                    "AsyncTests.cs",
                    "class AsyncTests {\n    class Response {\n    }\n\n    void Run() {\n        var r = Execute<Response>();\n    }\n}\n",
                ),
                ("PostTests.cs", "class PostTests {\n    class Response {\n    }\n}\n"),
                ("Flags.cs", "class Flags {\n    void LEGACY() {\n    }\n}\n"),
                ("Uses.cs", "class Uses {\n#if LEGACY\n    int a;\n#endif\n}\n"),
                ("Gone.cs", "class Gone {\n    static void Drop(int a) {\n    }\n\n    void Run() {\n        Drop(1);\n    }\n}\n"),
                ("go.mod", "module example.com/app\n\ngo 1.22\n"),
                ("log/logger.go", "package log\n\ntype Logger interface {\n\tInfo(msg string)\n}\n"),
                ("log/with.go", "package log\n\nimport \"log/slog\"\n\nfunc With(l *slog.Logger) int {\n\treturn 1\n}\n"),
                ("lib/node.js", "export function runNode(file) {\n  return file;\n}\n"),
                ("index.js", "export {runNode} from './lib/node.js';\n"),
                ("index.test.ts", "import {runNode} from './index.js';\n\nrunNode('a');\n"),
            ],
            &[
                ("Requests.cs", Some("static partial class Requests {\n}\n")),
                (
                    "Requests.Headers.cs",
                    Some("static partial class Requests {\n    static void Add(string name) {\n        CheckHost(name);\n    }\n\n    static void CheckHost(string name) {\n    }\n}\n"),
                ),
                (
                    "AsyncTests.cs",
                    Some("class AsyncTests {\n    void Run() {\n        var r = Execute<Response>();\n    }\n}\n"),
                ),
                ("Flags.cs", Some("class Flags {\n}\n")),
                ("Gone.cs", Some("class Gone {\n    void Run() {\n        Drop(1);\n    }\n}\n")),
                ("log/logger.go", Some("package log\n")),
                ("lib/node.js", Some("export const other = 1;\n")),
                ("index.js", Some("import {create} from './lib/create.js';\n\nexport const runNode = create();\n")),
            ],
        );
        let report = report(&temp);
        // The one removal nothing else declares is still a broken site.
        assert_eq!(
            broken(&report),
            vec!["Gone.cs:3 still names `Drop`, which this change removed".to_string()],
            "{}",
            report.to_text()
        );
        assert_eq!(
            possible(&report),
            vec![
                "AsyncTests.cs:3",
                "Requests.Headers.cs:3",
                "index.test.ts:1",
                "index.test.ts:3",
                "log/with.go:5"
            ],
            "{}",
            report.to_text()
        );
    }

    #[test]
    fn shapes_and_fits_read_what_each_language_writes() {
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
        // The parameter list is the one after the name, not an attribute's or a bound's.
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

        let two = Shape {
            params: vec![positional(true), positional(true)],
            ..Shape::default()
        };
        let one = CallArguments {
            positional: 1,
            ..CallArguments::default()
        };
        assert!(matches!(
            fit(&Language::Rust, &two, &one, false),
            Fit::Breaks(_)
        ));
        // A path call passes the receiver itself.
        let method = Shape {
            params: vec![positional(true)],
            receiver: true,
            ..Shape::default()
        };
        let both = CallArguments {
            positional: 2,
            ..CallArguments::default()
        };
        assert_eq!(fit(&Language::Rust, &method, &both, true), Fit::Fits);
        assert_eq!(fit(&Language::CSharp, &method, &both, true), Fit::Fits);
        assert!(matches!(
            fit(&Language::Rust, &method, &both, false),
            Fit::Breaks(_)
        ));
        assert_eq!(
            fit(
                &Language::Rust,
                &two,
                &CallArguments {
                    spread: true,
                    ..one.clone()
                },
                false
            ),
            Fit::Unknown("it unpacks an argument list")
        );

        let modules = module_names("crates/axum-extra/src/extract/host.rs");
        assert!(modules.contains("axum_extra") && modules.contains("host"));
        assert!(!modules.contains("src"));
        let imports = |text: &str| vec![text.to_string()];
        assert_eq!(
            import_says(&imports("use axum_extra::extract::Host;"), "Host", &modules),
            Some(true)
        );
        assert_eq!(
            import_says(
                &imports("use {\n    same_file::Host,\n    walkdir::WalkDir,\n};"),
                "Host",
                &modules
            ),
            Some(false)
        );
        assert_eq!(import_says(&[], "Host", &modules), None);
        assert_eq!(
            import_says(&imports("from axum_extra import *"), "Host", &modules),
            Some(true)
        );
    }
}
