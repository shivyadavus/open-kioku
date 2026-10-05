//! Which C# methods xUnit, NUnit and MSTest run as tests, and the `dotnet test` filter that
//! selects each one.
//!
//! All three runners discover a test by the attribute on the method, read here from the
//! declaration's own attribute lists (`[Fact]`, `[Theory, InlineData(1)]`):
//!
//! - xUnit: `[Fact]` and `[Theory]`, and any attribute named `*Fact` or `*Theory`, the names
//!   attributes deriving from xUnit's carry (`[SkippableFact]`, `[WpfTheory]`).
//! - NUnit: `[Test]`, `[TestCase]`, `[TestCaseSource]` and `[Theory]`. `[TestFixture]` is
//!   optional to NUnit, so it changes nothing.
//! - MSTest: `[TestMethod]`, `[DataTestMethod]` and any `*TestMethod`, in a class marked
//!   `[TestClass]` (or `*TestClass`): MSTest skips a test method of an unmarked class. A
//!   `partial` class counts as marked, since the mark may be on a part in another file.
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
//! A skipped test (`[Fact(Skip = "..")]`, `[Ignore]`) is still read as a test, as an ignored Rust
//! test or a JUnit `@Disabled` one is.
//!
//! The filter is `FullyQualifiedName~Namespace.Type.Method`, the property all three adapters
//! expose. A nested type joins its outer type with `+`, as the runners report it. `~` is a
//! substring match: it also selects `Method` cases with arguments (NUnit names
//! `Rounds(1)`) and any method whose name extends this one (`PostsTwice` for `Posts`), a
//! superset that still runs the test. A filter matching nothing exits 0 under `dotnet test`, so
//! a test method of an abstract or generic type, which runs only under each type deriving from
//! it, is selected by its method name alone (`FullyQualifiedName~.Method`): its derived types'
//! names are not known here.

use open_kioku_core::{Symbol, SymbolId, SymbolKind};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Bound on the characters of a declaration read for its attribute lists, so a pathological
/// attribute cannot turn one symbol into a scan of the file.
const ATTRIBUTE_SECTION_LIMIT: usize = 8 * 1024;

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

    /// Whether xUnit, NUnit or MSTest runs `symbol` as a test.
    pub(crate) fn runs(&self, symbol: &Symbol) -> bool {
        if symbol.kind != SymbolKind::Method {
            return false;
        }
        let attributes = self.attributes(symbol);
        let tests = attributes
            .iter()
            .filter(|name| is_test_attribute(name))
            .collect::<Vec<_>>();
        if tests.is_empty() {
            return false;
        }
        if tests.iter().any(|name| !is_mstest_method_attribute(name)) {
            return true;
        }
        // MSTest runs a test method only in a class marked `[TestClass]`, or through a class
        // deriving from an abstract one. A `partial` class may carry the mark on a part in
        // another file, which is not read here.
        self.parent(symbol).is_some_and(|class| {
            self.attributes(class)
                .iter()
                .any(|name| is_test_class_attribute(name))
                || is_abstract_or_generic(class)
                || declares_modifier(class, "partial")
        })
    }

    /// The `dotnet test` command selecting the test `symbol`, before ingest scopes it to the
    /// test project.
    pub(crate) fn command(&self, symbol: &Symbol) -> String {
        format!(
            "dotnet test --filter \"FullyQualifiedName~{}\"",
            self.filter_name(symbol)
        )
    }

    /// `Namespace.Type+Nested.Method`, or `.Method` for a method of an abstract or generic
    /// type.
    fn filter_name(&self, symbol: &Symbol) -> String {
        let mut namespaces = Vec::new();
        let mut types = Vec::new();
        let mut inherited_only = false;
        let mut current = self.parent(symbol);
        while let Some(parent) = current {
            match parent.kind {
                SymbolKind::Package => namespaces.push(parent.name.as_str()),
                SymbolKind::Class | SymbolKind::Interface => {
                    inherited_only |= is_abstract_or_generic(parent);
                    types.push(parent.name.as_str());
                }
                _ => {}
            }
            current = self.parent(parent);
        }
        if inherited_only {
            return format!(".{}", symbol.name);
        }
        if types.is_empty() {
            // The declaring type was lost to error recovery: the qualified name still holds it,
            // without telling a nested type from a namespace.
            return symbol.qualified_name.replace("::", ".");
        }
        namespaces.reverse();
        types.reverse();
        let mut name = namespaces.join(".");
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&types.join("+"));
        name.push('.');
        name.push_str(&symbol.name);
        name
    }

    fn parent(&self, symbol: &Symbol) -> Option<&'a Symbol> {
        symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .copied()
    }

    /// The attribute names on the declaration, without namespace or `Attribute` suffix and
    /// through the file's aliases. A C# symbol's range starts at its first attribute list.
    fn attributes(&self, symbol: &Symbol) -> Vec<String> {
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
                declaration_start(line, &symbol.name)
            } else {
                line
            };
            section.push_str(line);
            section.push('\n');
        }
        attribute_lists(&section)
            .iter()
            .flat_map(|list| split_top_level(list))
            .filter_map(|attribute| self.attribute_name(attribute))
            .collect()
    }

    fn attribute_name(&self, attribute: &str) -> Option<String> {
        let path = attribute
            .split(['(', '<'])
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        let path = path.strip_prefix("global::").unwrap_or(&path);
        if path.is_empty() {
            return None;
        }
        if let Some(target) = self.aliases.get(path) {
            return Some(target.clone());
        }
        Some(simple_attribute_name(path))
    }
}

/// The part of a declaration's first line that is the declaration: after the last `{`, `}` or
/// `;` outside brackets before its name, so `class WhenPosted { [Fact] public void Holds() {} }`
/// reads `[Fact] public void Holds() {} }` for `Holds`.
fn declaration_start<'l>(line: &'l str, name: &str) -> &'l str {
    let Some(at) = line.find(name) else {
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
    matches!(
        name,
        "Fact" | "Theory" | "Test" | "TestCase" | "TestCaseSource"
    ) || name.ends_with("Fact")
        || name.ends_with("Theory")
        || is_mstest_method_attribute(name)
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
    declares_modifier(class, "abstract")
        || class.signature.as_ref().is_some_and(|signature| {
            let header = signature.split(':').next().unwrap_or(signature);
            header.contains(&format!("{}<", class.name))
        })
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
