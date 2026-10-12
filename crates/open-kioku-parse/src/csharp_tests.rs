//! Which C# methods xUnit, NUnit and MSTest run as tests, and the `dotnet test` filter that
//! selects each one.
//!
//! All three runners discover a test by the attribute on the method, read here from the
//! declaration's own attribute lists (`[Fact]`, `[Theory, InlineData(1)]`):
//!
//! - xUnit: `[Fact]` and `[Theory]`, and any attribute named `*Fact` or `*Theory`, the names
//!   attributes deriving from xUnit's carry (`[SkippableFact]`, `[WpfTheory]`).
//! - NUnit: `[Test]`, `[TestCase]`, `[TestCaseSource]` and `[Theory]`, and `[Combinatorial]`,
//!   `[Sequential]` and `[Pairwise]`, which make a test of a method by themselves.
//! - MSTest: `[TestMethod]`, `[DataTestMethod]` and any `*TestMethod`, in a class marked
//!   `[TestClass]` (or `*TestClass`): MSTest skips a test method of an unmarked class. A
//!   `partial` class is judged here as marked, since the mark may be on a part in another file;
//!   ingest, which sees every part, withholds the method when no part carries it.
//!
//! Attribute lists are read across comments and `#if`/`#pragma` lines; one with a target other
//! than `method:` (`[field: ..]`, `[return: ..]`) does not apply to the method.
//!
//! A name is matched with or without its `Attribute` suffix and namespace (`[FactAttribute]`,
//! `[Xunit.Fact]`), and through a `using Check = Xunit.FactAttribute;` alias declared in the
//! same file. An alias declared in another file (a `global using` in `GlobalUsings.cs`) is not
//! read, so a test written with one reads as a helper, which plans disclose as withheld. Every
//! other method of a test file is a helper: lifecycle methods (`[SetUp]`, `[TestInitialize]`,
//! `[OneTimeTearDown]`, `[ClassInitialize]`), constructors and `Dispose`/`InitializeAsync`/
//! `DisposeAsync`, which is how xUnit sets up and tears down, and fixture and builder methods.
//!
//! A test the runner skips is disabled, not runnable: an xUnit test attribute with a `Skip`
//! argument (`[Fact(Skip = "flaky")]`), or an NUnit or MSTest `[Ignore]` on the method or its
//! class. A `Skip` that xUnit v3 applies only under a condition (`SkipUnless =`, `SkipWhen =`)
//! leaves the test runnable, and an `[Ignore]` on an outer class does not reach a nested one,
//! which the runners treat as a class of its own. NUnit's `[Explicit]` test still runs when a
//! filter names it, so it stays a test.
//!
//! The filter is `FullyQualifiedName~Namespace.Type.Method`, the property all three adapters
//! expose. A nested type joins its outer type with `+`, as the runners report it, and a
//! verbatim identifier (`@event`) is named without its `@`. `~` is a substring match: it also
//! selects `Method` cases with arguments (NUnit names `Rounds(1)`) and any method whose name
//! extends this one (`PostsTwice` for `Posts`), a superset that still runs the test.
//!
//! Three shapes name the test differently from its declaration:
//!
//! - A method of an abstract or generic type runs only under each type deriving from or closing
//!   it, whose names are not known here, so it is selected by its method name alone
//!   (`FullyQualifiedName~.Method`). A type nested in an abstract one is a type of its own, while
//!   one nested in a generic type is open with it.
//! - NUnit names a test of a parameterized fixture (`[TestFixture("usd")]`,
//!   `[TestFixtureSource]`) with the fixture's arguments, `Ns.CurrencyTests("usd").Converts`, so
//!   it is selected by type and method separately:
//!   `FullyQualifiedName~Ns.CurrencyTests&FullyQualifiedName~.Converts`.
//! - NUnit names a case `[TestCase(.., TestName = "Totals one")]` renames, or one a
//!   `TestCaseData(..).SetName(..)` of the same file renames for a `[TestCaseSource]`, by that
//!   name under its type (`Ns.LedgerTests.Totals one`), so the method's name selects nothing:
//!   such a test is selected by its type (`FullyQualifiedName~Ns.LedgerTests.`), which runs the
//!   type's other tests too.
//!
//! Under VSTest a filter matching nothing exits 0, so every command ends with
//! `-- RunConfiguration.TreatNoTestsAsError=true`, which makes such a run exit 1. Ingest drops
//! the guard where Microsoft.Testing.Platform runs the project: it exits 8 on zero tests itself.

use open_kioku_core::{Symbol, SymbolId, SymbolKind};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Bound on the characters of a declaration read for its attribute lists, so a pathological
/// attribute cannot turn one symbol into a scan of the file.
const ATTRIBUTE_SECTION_LIMIT: usize = 8 * 1024;

const FILTER_PREFIX: &str = "dotnet test --filter \"";

/// Arguments after `--` that make VSTest fail a run its filter selected no test in.
pub const VSTEST_NO_TESTS_GUARD: &str = "-- RunConfiguration.TreatNoTestsAsError=true";

/// The command for a C# test-file callable that selects no test of its own: its project's run.
pub const PROJECT_COMMAND: &str = "dotnet test";

/// The `dotnet test` command running the tests `filter` selects, before ingest scopes it to a
/// project.
pub fn test_command(filter: &str) -> String {
    format!("{FILTER_PREFIX}{filter}\" {VSTEST_NO_TESTS_GUARD}")
}

/// The filter of a command [`test_command`] built.
pub fn command_filter(command: &str) -> Option<&str> {
    command
        .strip_prefix(FILTER_PREFIX)?
        .strip_suffix(VSTEST_NO_TESTS_GUARD)?
        .trim_end()
        .strip_suffix('"')
}

/// The filter selecting a method of an abstract or generic type, by its name alone.
pub fn inherited_test_filter(method: &str) -> String {
    format!("FullyQualifiedName~.{}", identifier(method))
}

/// A name as the runners report it: without the `@` of a verbatim identifier.
fn identifier(name: &str) -> &str {
    name.strip_prefix('@').unwrap_or(name)
}

/// What a C# method is to its runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The runner discovers and runs it.
    Runs,
    /// The runner discovers it but skips it.
    Skipped,
    /// No runner discovers it.
    NotATest,
}

/// One attribute of a declaration: its name, without namespace or `Attribute` suffix and through
/// the file's aliases, and the text of its argument list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribute {
    name: String,
    arguments: String,
}

/// The C# runner rules of one file.
pub(crate) struct CSharpTests<'a> {
    lines: &'a [&'a str],
    by_id: HashMap<&'a SymbolId, &'a Symbol>,
    /// `using Alias = Namespace.NameAttribute;` aliases of the file, to the attribute name each
    /// stands for (`Name`).
    aliases: HashMap<String, String>,
}

impl<'a> CSharpTests<'a> {
    pub(crate) fn new(lines: &'a [&'a str], symbols: &'a [Symbol]) -> Self {
        Self {
            lines,
            by_id: symbols.iter().map(|symbol| (&symbol.id, symbol)).collect(),
            aliases: using_aliases(lines),
        }
    }

    /// Whether xUnit, NUnit or MSTest runs `symbol`, skips it, or does not discover it.
    pub(crate) fn verdict(&self, symbol: &Symbol) -> Verdict {
        if symbol.kind != SymbolKind::Method {
            return Verdict::NotATest;
        }
        let attributes = self.attributes(symbol);
        let tests = attributes
            .iter()
            .filter(|attribute| is_test_attribute(&attribute.name))
            .collect::<Vec<_>>();
        if tests.is_empty() {
            return Verdict::NotATest;
        }
        let mstest_only = tests
            .iter()
            .all(|attribute| is_mstest_method_attribute(&attribute.name));
        // MSTest runs a test method only in a class marked `[TestClass]`, or through a class
        // deriving from an abstract one. A `partial` class may carry the mark on a part in
        // another file, which ingest checks.
        if mstest_only
            && !self.parent(symbol).is_some_and(|class| {
                self.attributes(class)
                    .iter()
                    .any(|attribute| is_test_class_attribute(&attribute.name))
                    || is_abstract_or_generic(class)
                    || declares_modifier(class, "partial")
            })
        {
            return Verdict::NotATest;
        }
        let skipped_here = tests.iter().any(|attribute| skips(&attribute.arguments))
            || attributes
                .iter()
                .any(|attribute| attribute.name == "Ignore");
        // A nested class is a class of its own to every runner: only the declaring class's
        // `[Ignore]` reaches the method.
        let skipped_with_type = self.enclosing_types(symbol).next().is_some_and(|class| {
            self.attributes(class)
                .iter()
                .any(|attribute| attribute.name == "Ignore")
        });
        if skipped_here || skipped_with_type {
            Verdict::Skipped
        } else {
            Verdict::Runs
        }
    }

    /// The `dotnet test` command selecting the test `symbol`, before ingest scopes it to the
    /// test project.
    pub(crate) fn command(&self, symbol: &Symbol) -> String {
        test_command(&self.filter(symbol))
    }

    /// `FullyQualifiedName~Namespace.Type+Nested.Method`, or one of the two shapes the module
    /// documentation describes.
    fn filter(&self, symbol: &Symbol) -> String {
        let method = identifier(&symbol.name);
        let mut namespaces = Vec::new();
        let mut types = Vec::new();
        let mut inherited_only = false;
        let mut parameterized = false;
        let mut current = self.parent(symbol);
        while let Some(parent) = current {
            match parent.kind {
                SymbolKind::Package => namespaces.push(parent.name.as_str()),
                SymbolKind::Class | SymbolKind::Interface => {
                    // A type nested in an abstract one is a type of its own; one nested in a
                    // generic type is open with it.
                    inherited_only |= if types.is_empty() {
                        is_abstract_or_generic(parent)
                    } else {
                        is_generic(parent)
                    };
                    parameterized |= self.attributes(parent).iter().any(|attribute| {
                        attribute.name == "TestFixtureSource"
                            || (attribute.name == "TestFixture"
                                && !attribute.arguments.trim().is_empty())
                    });
                    types.push(identifier(&parent.name));
                }
                _ => {}
            }
            current = self.parent(parent);
        }
        if inherited_only {
            return inherited_test_filter(method);
        }
        if types.is_empty() {
            // The declaring type was lost to error recovery: the qualified name still holds it,
            // without telling a nested type from a namespace.
            return format!(
                "FullyQualifiedName~{}",
                symbol
                    .qualified_name
                    .split("::")
                    .map(identifier)
                    .collect::<Vec<_>>()
                    .join(".")
            );
        }
        namespaces.reverse();
        types.reverse();
        let mut type_name = namespaces.join(".");
        if !type_name.is_empty() {
            type_name.push('.');
        }
        type_name.push_str(&types.join("+"));
        if self.renames_cases(symbol) {
            return if parameterized {
                format!("FullyQualifiedName~{type_name}")
            } else {
                format!("FullyQualifiedName~{type_name}.")
            };
        }
        if parameterized {
            format!("FullyQualifiedName~{type_name}&FullyQualifiedName~.{method}")
        } else {
            format!("FullyQualifiedName~{type_name}.{method}")
        }
    }

    /// Whether NUnit names some case of the test `symbol` by a name of its own rather than the
    /// method's: a `TestName` argument of a `[TestCase]`, or, for a `[TestCaseSource]`, a
    /// `.SetName(` in this file, which builds the source's cases.
    fn renames_cases(&self, symbol: &Symbol) -> bool {
        let attributes = self.attributes(symbol);
        attributes
            .iter()
            .any(|attribute| attribute.name == "TestCase" && names_test(&attribute.arguments))
            || (attributes
                .iter()
                .any(|attribute| attribute.name == "TestCaseSource")
                && self.lines.iter().any(|line| line.contains(".SetName(")))
    }

    fn parent(&self, symbol: &Symbol) -> Option<&'a Symbol> {
        symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .copied()
    }

    /// The types enclosing `symbol`, innermost first.
    fn enclosing_types(&self, symbol: &Symbol) -> impl Iterator<Item = &'a Symbol> + '_ {
        std::iter::successors(self.parent(symbol), |parent| self.parent(parent))
            .filter(|parent| matches!(parent.kind, SymbolKind::Class | SymbolKind::Interface))
    }

    /// The attributes on the declaration. A C# symbol's range starts at its first attribute
    /// list.
    fn attributes(&self, symbol: &Symbol) -> Vec<Attribute> {
        let Some(range) = &symbol.range else {
            return Vec::new();
        };
        let start = (range.start as usize).saturating_sub(1);
        let end = (range.end as usize).max(start + 1);
        let mut section = String::new();
        for (index, line) in self.lines.iter().enumerate().take(end).skip(start) {
            if section.len() > ATTRIBUTE_SECTION_LIMIT {
                break;
            }
            let line = if index == start {
                declaration_start(line, symbol)
            } else {
                line
            };
            section.push_str(line);
            section.push('\n');
        }
        attribute_lists(&section)
            .iter()
            .flat_map(|list| split_top_level(list))
            .filter_map(|attribute| self.attribute(attribute))
            .collect()
    }

    fn attribute(&self, attribute: &str) -> Option<Attribute> {
        let (head, arguments) = match attribute.find('(') {
            Some(open) => (
                &attribute[..open],
                attribute[open + 1..]
                    .rsplit_once(')')
                    .map_or("", |(inside, _)| inside),
            ),
            None => (attribute, ""),
        };
        let path = head
            .split('<')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        let path = path.strip_prefix("global::").unwrap_or(&path);
        if path.is_empty() {
            return None;
        }
        let name = match self.aliases.get(path) {
            Some(target) => target.clone(),
            None => simple_attribute_name(path),
        };
        Some(Attribute {
            name,
            arguments: arguments.to_string(),
        })
    }
}

/// The attribute names on a C# declaration in `content`, read as [`CSharpTests`] reads them:
/// for ingest, which judges a `partial` type across the files of its parts.
pub fn attribute_names(content: &str, symbol: &Symbol) -> Vec<String> {
    let lines = content.lines().collect::<Vec<_>>();
    CSharpTests::new(&lines, &[])
        .attributes(symbol)
        .into_iter()
        .map(|attribute| attribute.name)
        .collect()
}

/// Whether a method's attributes make it a test of MSTest alone, which runs it only in a class
/// marked `[TestClass]`.
pub fn is_mstest_only(names: &[String]) -> bool {
    let mut tests = names
        .iter()
        .filter(|name| is_test_attribute(name))
        .peekable();
    tests.peek().is_some() && tests.all(|name| is_mstest_method_attribute(name))
}

/// Whether a type's attributes mark it as an MSTest test class.
pub fn marks_test_class(names: &[String]) -> bool {
    names.iter().any(|name| is_test_class_attribute(name))
}

/// Whether a type is abstract or generic, so its tests run only under the types deriving from or
/// closing it.
pub fn is_inherited_only(class: &Symbol) -> bool {
    is_abstract_or_generic(class)
}

/// Whether a type is generic, so the types nested in it are open too.
pub fn is_generic(class: &Symbol) -> bool {
    class.signature.as_ref().is_some_and(|signature| {
        let header = signature.split(':').next().unwrap_or(signature);
        header.contains(&format!("{}<", class.name))
    })
}

/// The framework whose attribute makes a C# method a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    Xunit,
    NUnit,
    MSTest,
}

/// Which framework runs a test method, read from the attribute names on the method (`method`)
/// and on the types enclosing it (`types`), and whether NUnit is in use where it is declared
/// (`nunit`: its file imports `NUnit.Framework`, or its project references NUnit). `None` when
/// no attribute makes the method a test.
///
/// Only `[Theory]` is ambiguous, NUnit having one too: it is xUnit's when the method carries
/// xUnit data (`[InlineData]`, `[MemberData]`, `[ClassData]`, any `*Data`), and otherwise
/// NUnit's when a type is an NUnit fixture or `nunit` holds.
pub fn test_framework(method: &[String], types: &[String], nunit: bool) -> Option<Framework> {
    let tests = method
        .iter()
        .filter(|name| is_test_attribute(name))
        .collect::<Vec<_>>();
    if tests.iter().any(|name| is_mstest_method_attribute(name)) {
        return Some(Framework::MSTest);
    }
    if tests.iter().any(|name| is_nunit_test_attribute(name)) {
        return Some(Framework::NUnit);
    }
    if tests
        .iter()
        .any(|name| name.ends_with("Fact") || (name.ends_with("Theory") && *name != "Theory"))
    {
        return Some(Framework::Xunit);
    }
    if tests.is_empty() {
        return None;
    }
    let xunit_data = method.iter().any(|name| name.ends_with("Data"));
    let nunit_fixture = types.iter().any(|name| name.starts_with("TestFixture"));
    if !xunit_data && (nunit_fixture || nunit) {
        Some(Framework::NUnit)
    } else {
        Some(Framework::Xunit)
    }
}

/// Whether `content`, a C# file, imports NUnit (`using NUnit.Framework;`, `global using` too).
pub fn imports_nunit(content: &str) -> bool {
    static IMPORT: OnceLock<Option<Regex>> = OnceLock::new();
    IMPORT
        .get_or_init(|| {
            Regex::new(r"(?m)^\s*(?:global\s+)?using\s+(?:static\s+)?NUnit\.Framework\b").ok()
        })
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(content))
}

/// The simple names of the types a C# type's header lists as its bases
/// (`class SqlJournalTests : JournalContract<int>, IDisposable` gives `JournalContract` and
/// `IDisposable`).
pub fn base_type_names(class: &Symbol) -> Vec<String> {
    class
        .signature
        .as_deref()
        .map(base_names)
        .unwrap_or_default()
}

fn base_names(signature: &str) -> Vec<String> {
    let Some((_, bases)) = signature.split_once(':') else {
        return Vec::new();
    };
    let bases = bases.split(" where ").next().unwrap_or(bases);
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for character in bases.chars().chain(std::iter::once(',')) {
        match character {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            ',' if depth == 0 => {
                let name = current.trim();
                let name = name.rsplit(['.', ':']).next().unwrap_or(name);
                if !name.is_empty() {
                    names.push(name.to_string());
                }
                current.clear();
            }
            _ if depth == 0 => current.push(character),
            _ => {}
        }
    }
    names
}

/// Whether the type is declared `partial`, so another part may change what its tests are.
pub fn is_partial(class: &Symbol) -> bool {
    declares_modifier(class, "partial")
}

/// Whether a test attribute's arguments always skip it: a `Skip = "reason"` named argument,
/// without the `SkipUnless =` or `SkipWhen =` that makes xUnit v3 skip it only sometimes.
fn skips(arguments: &str) -> bool {
    static SKIP: OnceLock<Option<Regex>> = OnceLock::new();
    static CONDITION: OnceLock<Option<Regex>> = OnceLock::new();
    let matches = |cell: &'static OnceLock<Option<Regex>>, pattern: &str| {
        cell.get_or_init(|| Regex::new(pattern).ok())
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(arguments))
    };
    matches(&SKIP, r"(?:^|[,(\s])Skip\s*=")
        && !matches(&CONDITION, r"(?:^|[,(\s])Skip(?:Unless|When)\s*=")
}

/// Whether a `[TestCase]`'s arguments name the case: a `TestName = ".."` named argument.
fn names_test(arguments: &str) -> bool {
    static NAME: OnceLock<Option<Regex>> = OnceLock::new();
    NAME.get_or_init(|| Regex::new(r"(?:^|[,(\s])TestName\s*=").ok())
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(arguments))
}

/// The part of a declaration's first line that is the declaration: after the last `{`, `}` or
/// `;` outside brackets before its name, so `class HoldsTests { [Test] public void Holds() {} }`
/// reads `[Test] public void Holds() {} }` for `Holds`. The name is found as a whole word, and,
/// for a method, as one followed by its parameter or type-parameter list.
fn declaration_start<'l>(line: &'l str, symbol: &Symbol) -> &'l str {
    let name = symbol.name.as_str();
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let words = line
        .match_indices(name)
        .filter(|(at, _)| {
            !line[..*at].ends_with(is_word) && !line[at + name.len()..].starts_with(is_word)
        })
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    let declared = words.iter().copied().find(|at| {
        symbol.kind == SymbolKind::Method
            && line[at + name.len()..].trim_start().starts_with(['(', '<'])
    });
    let Some(at) = declared.or_else(|| words.first().copied()) else {
        return line;
    };
    let mut depth = 0i32;
    let mut start = 0;
    for (index, character) in line[..at].char_indices() {
        match character {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            '{' | '}' | ';' if depth == 0 => start = index + 1,
            _ => {}
        }
    }
    &line[start..]
}

/// `Fact` for `Xunit.FactAttribute`, `Test` for `NUnit.Framework.Test`.
fn simple_attribute_name(path: &str) -> String {
    let last = path.rsplit(['.', ':']).next().unwrap_or(path);
    match last.strip_suffix("Attribute") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => last.to_string(),
    }
}

/// The file's `using Alias = Target;` directives, `global using` ones too, mapped to the
/// attribute name the target stands for.
fn using_aliases(lines: &[&str]) -> HashMap<String, String> {
    static ALIAS: OnceLock<Option<Regex>> = OnceLock::new();
    let Some(pattern) = ALIAS
        .get_or_init(|| {
            Regex::new(
                r"^\s*(?:global\s+)?using\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_.:]*)\s*;",
            )
            .ok()
        })
        .as_ref()
    else {
        return HashMap::new();
    };
    lines
        .iter()
        .filter_map(|line| pattern.captures(line))
        .filter_map(|captures| {
            let alias = captures.get(1)?.as_str().to_string();
            let target = captures.get(2)?.as_str();
            let target = target.strip_prefix("global::").unwrap_or(target);
            Some((alias, simple_attribute_name(target)))
        })
        .collect()
}

/// The contents of the attribute lists (`[..]`) that open `declaration`, before its modifiers.
/// Comments between them are skipped, and brackets inside string and character literals do not
/// close a list.
fn attribute_lists(declaration: &str) -> Vec<String> {
    let mut lists = Vec::new();
    let mut chars = declaration.chars().peekable();
    loop {
        // Whitespace, comments and preprocessor lines (`#if NET8_0`, `#pragma warning disable`)
        // between attribute lists. A test attribute inside an `#if` region counts.
        while let Some(&character) = chars.peek() {
            if character.is_whitespace() {
                chars.next();
                continue;
            }
            if character == '#' {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
                continue;
            }
            if character == '/' {
                let mut lookahead = chars.clone();
                lookahead.next();
                match lookahead.peek() {
                    Some('/') => {
                        for skipped in chars.by_ref() {
                            if skipped == '\n' {
                                break;
                            }
                        }
                        continue;
                    }
                    Some('*') => {
                        chars.next();
                        chars.next();
                        let mut previous = '\0';
                        for skipped in chars.by_ref() {
                            if previous == '*' && skipped == '/' {
                                break;
                            }
                            previous = skipped;
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            break;
        }
        if chars.peek() != Some(&'[') {
            return lists;
        }
        chars.next();
        let mut depth = 0usize;
        let mut list = String::new();
        let mut closed = false;
        while let Some(character) = chars.next() {
            match character {
                '"' | '\'' => {
                    list.push(character);
                    let verbatim = character == '"' && list.ends_with("@\"");
                    while let Some(inner) = chars.next() {
                        list.push(inner);
                        if inner == '\\' && !verbatim {
                            if let Some(escaped) = chars.next() {
                                list.push(escaped);
                            }
                            continue;
                        }
                        if inner == character {
                            break;
                        }
                    }
                }
                '[' | '(' | '{' => {
                    depth += 1;
                    list.push(character);
                }
                ']' if depth == 0 => {
                    closed = true;
                    break;
                }
                ')' | ']' | '}' => {
                    depth = depth.saturating_sub(1);
                    list.push(character);
                }
                _ => list.push(character),
            }
        }
        if !closed {
            return lists;
        }
        // An attribute target is not part of the name, and only `method:` applies to the method:
        // `[field: Test]` on a source generator's method is for the field it generates.
        match list.split_once(':') {
            Some((target, rest))
                if !rest.starts_with(':')
                    && !target.trim().is_empty()
                    && target
                        .trim()
                        .chars()
                        .all(|character| character.is_ascii_alphabetic()) =>
            {
                if target.trim() == "method" {
                    lists.push(rest.to_string());
                }
            }
            _ => lists.push(list),
        }
    }
}

/// `list` split at the commas outside brackets and literals.
fn split_top_level(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, character) in list.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(list[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(list[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// An attribute that makes a method an xUnit, NUnit or MSTest test.
fn is_test_attribute(name: &str) -> bool {
    name.ends_with("Fact")
        || name.ends_with("Theory")
        || is_nunit_test_attribute(name)
        || is_mstest_method_attribute(name)
}

/// An attribute that makes a method an NUnit test: `[Test]`, its case attributes, and the
/// combining strategies, which build cases from `[Values]` parameters by themselves. NUnit's
/// `[Theory]`, which xUnit shares, is not among them.
fn is_nunit_test_attribute(name: &str) -> bool {
    matches!(
        name,
        "Test" | "TestCase" | "TestCaseSource" | "Combinatorial" | "Sequential" | "Pairwise"
    )
}

/// MSTest's `[TestMethod]`, `[DataTestMethod]` or an attribute deriving from them.
fn is_mstest_method_attribute(name: &str) -> bool {
    name.ends_with("TestMethod")
}

/// MSTest's `[TestClass]` or an attribute deriving from it (`[STATestClass]`).
fn is_test_class_attribute(name: &str) -> bool {
    name.ends_with("TestClass")
}

/// Whether the type is abstract or generic: its test methods run only under the types deriving
/// from or closing it.
fn is_abstract_or_generic(class: &Symbol) -> bool {
    declares_modifier(class, "abstract") || is_generic(class)
}

/// Whether the type's header, before its base list, carries `modifier`.
fn declares_modifier(class: &Symbol, modifier: &str) -> bool {
    class.signature.as_ref().is_some_and(|signature| {
        signature
            .split(':')
            .next()
            .unwrap_or(signature)
            .split_whitespace()
            .any(|word| word == modifier)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_lists_are_read_up_to_the_modifiers() {
        let declaration = "[Theory, Trait(\"kind\", \"a]b\")] // why\n/* note */ [InlineData(1, \"[\")]\n#if NET8_0\n[method: Fact]\n#endif\n[field: Test]\npublic void Rounds(int value) { var x = new[] { 1 }; }\n";
        assert_eq!(
            attribute_lists(declaration),
            vec![
                "Theory, Trait(\"kind\", \"a]b\")".to_string(),
                "InlineData(1, \"[\")".to_string(),
                " Fact".to_string(),
            ]
        );
        assert!(attribute_lists("public void Plain() { var a = b[0]; }").is_empty());
        assert_eq!(
            split_top_level("Theory, Trait(\"a,b\", \"c\"), InlineData(new[] { 1, 2 })"),
            vec![
                "Theory",
                "Trait(\"a,b\", \"c\")",
                "InlineData(new[] { 1, 2 })"
            ]
        );
    }

    #[test]
    fn attribute_names_drop_namespace_and_suffix() {
        for (path, name) in [
            ("Fact", "Fact"),
            ("FactAttribute", "Fact"),
            ("Xunit.Fact", "Fact"),
            ("global::NUnit.Framework.TestAttribute", "Test"),
            ("Attribute", "Attribute"),
        ] {
            assert_eq!(
                simple_attribute_name(path.trim_start_matches("global::")),
                name
            );
        }
    }

    #[test]
    fn test_attributes_follow_each_runner() {
        for name in [
            "Fact",
            "Theory",
            "SkippableFact",
            "WpfTheory",
            "Test",
            "TestCase",
            "TestCaseSource",
            "Combinatorial",
            "Sequential",
            "Pairwise",
            "TestMethod",
            "DataTestMethod",
            "STATestMethod",
        ] {
            assert!(is_test_attribute(name), "{name}");
        }
        for name in [
            "InlineData",
            "MemberData",
            "DataRow",
            "SetUp",
            "TearDown",
            "OneTimeSetUp",
            "OneTimeTearDown",
            "TestInitialize",
            "TestCleanup",
            "ClassInitialize",
            "TestFixture",
            "TestClass",
            "Trait",
            "Artifact",
            "TestCategory",
        ] {
            assert!(!is_test_attribute(name), "{name}");
        }
    }

    /// The framework comes from the test's own attributes; only `[Theory]` needs its context.
    #[test]
    fn a_test_framework_is_read_from_the_tests_own_attributes() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        let none: Vec<String> = Vec::new();
        for (method, types, nunit, framework) in [
            (
                names(&["TestMethod"]),
                none.clone(),
                false,
                Framework::MSTest,
            ),
            (
                names(&["DataTestMethod", "DataRow"]),
                none.clone(),
                true,
                Framework::MSTest,
            ),
            (names(&["Test"]), none.clone(), false, Framework::NUnit),
            (
                names(&["Combinatorial"]),
                none.clone(),
                false,
                Framework::NUnit,
            ),
            (names(&["Fact"]), none.clone(), true, Framework::Xunit),
            (names(&["WpfTheory"]), none.clone(), true, Framework::Xunit),
            (names(&["Theory"]), none.clone(), false, Framework::Xunit),
            (
                names(&["Theory", "InlineData"]),
                none.clone(),
                true,
                Framework::Xunit,
            ),
            (names(&["Theory"]), none.clone(), true, Framework::NUnit),
            (
                names(&["Theory"]),
                names(&["TestFixture"]),
                false,
                Framework::NUnit,
            ),
        ] {
            assert_eq!(
                test_framework(&method, &types, nunit),
                Some(framework),
                "{method:?} {types:?} {nunit}"
            );
        }
        assert_eq!(test_framework(&names(&["SetUp"]), &none, true), None);
        assert!(imports_nunit("// Xunit\nglobal using NUnit.Framework;\n"));
        assert!(!imports_nunit(
            "// replaces the NUnit.Framework suite\nusing Xunit;\n"
        ));
    }

    #[test]
    fn base_types_are_read_from_the_header() {
        assert_eq!(
            base_names(
                "public class SqlJournalTests : Acme.JournalContract<Dictionary<int, string>>, IDisposable where T : class"
            ),
            vec!["JournalContract", "IDisposable"]
        );
        assert!(base_names("public class Plain").is_empty());
    }

    #[test]
    fn aliases_name_the_attribute_they_stand_for() {
        let lines = [
            "using Check = Xunit.FactAttribute;",
            "global using Case = global::NUnit.Framework.TestCaseAttribute;",
            "using Xunit;",
        ];
        let aliases = using_aliases(&lines);
        assert_eq!(aliases.get("Check").map(String::as_str), Some("Fact"));
        assert_eq!(aliases.get("Case").map(String::as_str), Some("TestCase"));
        assert_eq!(aliases.len(), 2);
    }
}
