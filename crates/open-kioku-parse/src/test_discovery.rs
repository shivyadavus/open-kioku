//! Which callables in a test-path file a runner executes as tests.
//!
//! A file the shared test-path rule recognises holds tests and the scaffolding around them:
//! helpers, builders, fixtures and lifecycle hooks. Only the first are validation, so each
//! language's callables are judged by the rule its runner discovers tests by, not by a name
//! heuristic and not by the file alone:
//!
//! - Rust: a test attribute in the attribute stack, directly or through `cfg_attr`: one whose
//!   last path segment is `test` (`#[tokio::test]`), `rstest`, `test_case`, `proptest`,
//!   `quickcheck`, `wasm_bindgen_test`, `pg_test` or rstest_reuse's `apply`; never a function
//!   marked `#[template]` or `#[fixture]`.
//! - Java: a JUnit or TestNG test annotation (`@Test`, `@ParameterizedTest`), a JUnit 3
//!   `public void test*()` of a class that extends another (the `TestCase` may sit behind an
//!   abstract base in another file), or a public method of a class TestNG annotates `@Test`
//!   that no lifecycle or data-provider annotation marks.
//! - Python: a `test*` function of a module pytest or `unittest` collects (`test*.py`,
//!   `*_test.py`), at module level, in a `Test*` class without `__init__`, or in a class with a
//!   base (a `TestCase`, perhaps through a base in another module), and not a fixture.
//! - Go: `TestX` and `FuzzX` in a `_test.go` file, and an `ExampleX` with an output comment;
//!   `TestMain` is the package's lifecycle hook.
//! - JavaScript and TypeScript: tests are registration calls, so a declared callable is a test
//!   only when its own declaration is one.
//! - C#: an xUnit `[Fact]`/`[Theory]`, NUnit `[Test]`/`[TestCase]`/`[TestCaseSource]` or MSTest
//!   `[TestMethod]` method, the last in a `[TestClass]` class; see [`crate::csharp_tests`].
//!
//! These are the runners' default rules. Runner configuration is not read here, so a callable
//! that matches none of them is reported as matching no default rule, never as one no runner
//! executes; ingest widens Python back to the file rule where a pytest configuration changes
//! discovery. Known misses: a Rust `harness = false` target (libtest-mimic and similar custom
//! harnesses), a JUnit 5 meta-annotation standing for `@Test`, a JS/TS test registered through
//! a wrapper function rather than `test(..)`/`it(..)`, and Kotlin and Scala, which are not
//! indexed. A language with no runner model here keeps the file rule: every callable is a test.

use crate::csharp_tests::CSharpTests;
use crate::{
    declares_registered_test, has_adjacent_annotation, has_test_name_prefix,
    is_stacked_test_annotation,
};
use open_kioku_core::{File, Language, Symbol, SymbolId, SymbolKind, Visibility};
use std::collections::HashMap;

/// Bound on the lines read from a class declaration to find its supertypes, and on the lines of
/// a Go example's body read for its output comment.
const DECLARATION_LINE_LIMIT: usize = 8;

/// The runner discovery rule of one test-path file.
pub(crate) struct TestFileDiscovery<'a> {
    language: Language,
    file_name: String,
    lines: &'a [&'a str],
    symbols: &'a [Symbol],
    by_id: HashMap<&'a SymbolId, &'a Symbol>,
    /// A Java file written for JUnit 4 or 5: it imports `org.junit.Test` or `org.junit.jupiter`,
    /// or annotates something `@Test`. Those runners ignore JUnit 3 naming, so a `test*`
    /// method there is a test only when annotated.
    annotation_junit: bool,
    /// The xUnit, NUnit and MSTest rules of a C# file.
    csharp: Option<&'a CSharpTests<'a>>,
}

impl<'a> TestFileDiscovery<'a> {
    pub(crate) fn new(
        file: &File,
        lines: &'a [&'a str],
        symbols: &'a [Symbol],
        csharp: Option<&'a CSharpTests<'a>>,
    ) -> Self {
        let path = file.path.to_string_lossy().replace('\\', "/");
        let file_name = path.rsplit('/').next().unwrap_or_default().to_string();
        Self {
            language: file.language.clone(),
            file_name,
            lines,
            symbols,
            by_id: symbols.iter().map(|symbol| (&symbol.id, symbol)).collect(),
            annotation_junit: file.language == Language::Java
                && lines.iter().map(|line| line.trim()).any(|line| {
                    line.starts_with("import org.junit.jupiter")
                        || line.starts_with("import static org.junit.jupiter")
                        || line.starts_with("import org.junit.Test")
                        || java_annotation_name(line) == Some("Test")
                }),
            csharp,
        }
    }

    /// Whether the runner executes `symbol`, a callable of this file, as a test.
    pub(crate) fn runs(&self, symbol: &Symbol) -> bool {
        match self.language {
            Language::Rust => {
                has_adjacent_annotation(self.lines, symbol, is_rust_test_attribute)
                    && !has_adjacent_annotation(self.lines, symbol, is_rust_template_attribute)
            }
            Language::Java => self.runs_java(symbol),
            Language::Python => self.runs_python(symbol),
            Language::Go => self.runs_go(symbol),
            Language::TypeScript | Language::JavaScript => {
                declares_registered_test(self.lines, symbol)
            }
            Language::CSharp => self
                .csharp
                .as_ref()
                .is_some_and(|csharp| csharp.runs(symbol)),
            Language::Yaml
            | Language::Json
            | Language::Toml
            | Language::Sql
            | Language::Markdown
            | Language::Text
            | Language::Unknown => true,
        }
    }

    fn runs_java(&self, symbol: &Symbol) -> bool {
        if has_adjacent_annotation(self.lines, symbol, is_java_test_annotation) {
            return true;
        }
        let Some(class) = self.enclosing_type(symbol) else {
            return false;
        };
        // JUnit 3 runs every public no-argument `void test*()` of a `TestCase` subclass. The
        // `TestCase` may be reached through an abstract base in another file, so any class that
        // extends something qualifies. A file written for JUnit 4 or 5 does not.
        if !self.annotation_junit
            && has_test_name_prefix(&symbol.name)
            && symbol.visibility == Visibility::Public
            && self.declares_no_argument_void(symbol)
            && self.declaration_mentions(class, "extends")
        {
            return true;
        }
        // TestNG runs every public method of a class annotated `@Test`, except its
        // configuration and data-provider methods.
        symbol.visibility == Visibility::Public
            && has_adjacent_annotation(self.lines, class, is_java_test_annotation)
            && !has_adjacent_annotation(self.lines, symbol, is_java_non_test_method_annotation)
    }

    fn runs_python(&self, symbol: &Symbol) -> bool {
        // pytest collects `test_*.py` and `*_test.py`, `unittest` `test*.py`; `conftest.py`, a
        // `helpers.py` or a `testutil/` module holds no test either runner collects.
        let stem = self
            .file_name
            .strip_suffix(".py")
            .unwrap_or(&self.file_name);
        if !(stem.starts_with("test") || stem.ends_with("_test")) {
            return false;
        }
        // Both runners collect by the `test` prefix alone, so `testable` is collected too.
        if !symbol.name.starts_with("test")
            || has_adjacent_annotation(self.lines, symbol, is_python_fixture_decorator)
        {
            return false;
        }
        match self.enclosing(symbol) {
            None => true,
            // pytest collects a `Test*` class without `__init__`; `unittest` collects a
            // `TestCase` subclass, which may inherit it through a base in another module, so a
            // class with any base but `object` qualifies.
            Some(parent) if is_type_kind(&parent.kind) => {
                (parent.name.starts_with("Test") && !self.defines_init(parent))
                    || self.has_python_base(parent)
            }
            // A function nested in another is never collected.
            Some(_) => false,
        }
    }

    /// Whether the Python class declares `__init__`, which makes pytest skip it.
    fn defines_init(&self, class: &Symbol) -> bool {
        self.symbols.iter().any(|candidate| {
            candidate.name == "__init__"
                && self
                    .enclosing(candidate)
                    .is_some_and(|parent| parent.id == class.id)
        })
    }

    /// Whether the Python class names a base other than `object` (a metaclass is not one).
    fn has_python_base(&self, class: &Symbol) -> bool {
        let Some(range) = &class.range else {
            return false;
        };
        let declaration = self
            .lines
            .iter()
            .skip((range.start as usize).saturating_sub(1))
            .take(DECLARATION_LINE_LIMIT)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        // The header ends at the first `:` outside the base list; the body's calls are not bases.
        let mut depth = 0i32;
        let mut bases = String::new();
        for character in declaration.chars() {
            match character {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ':' if depth == 0 => break,
                _ => {}
            }
            bases.push(character);
        }
        let Some(open) = bases.find('(') else {
            return false;
        };
        let close = bases.rfind(')').unwrap_or(bases.len());
        bases
            .get(open + 1..close)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .any(|base| !base.is_empty() && base != "object" && !base.contains('='))
    }

    /// Whether the Java method is declared `void name()`, the shape JUnit 3 runs.
    fn declares_no_argument_void(&self, symbol: &Symbol) -> bool {
        let Some(range) = &symbol.range else {
            return false;
        };
        let call = format!("{}(", symbol.name);
        self.lines
            .iter()
            .skip((range.start as usize).saturating_sub(1))
            .take(DECLARATION_LINE_LIMIT)
            .find(|line| line.contains(&call))
            .is_some_and(|line| {
                let after = &line[line.find(&call).unwrap_or_default() + call.len()..];
                line.contains("void ") && after.trim_start().starts_with(')')
            })
    }

    fn runs_go(&self, symbol: &Symbol) -> bool {
        if !self.file_name.ends_with("_test.go") {
            return false;
        }
        let name = symbol.name.as_str();
        // `TestMain` sets up and tears down the package's tests; it is not one.
        if name == "TestMain" {
            return false;
        }
        if go_test_function_name(name, "Test") || go_test_function_name(name, "Fuzz") {
            return true;
        }
        // An example runs only when its body ends with an output comment.
        go_test_function_name(name, "Example") && self.body_has_output_comment(symbol)
    }

    /// The symbol's parent, by id, or else the innermost type, function or method of the file
    /// whose range holds it.
    fn enclosing(&self, symbol: &Symbol) -> Option<&'a Symbol> {
        if let Some(parent) = symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
        {
            return Some(parent);
        }
        let range = symbol.range.as_ref()?;
        self.symbols
            .iter()
            .filter(|candidate| candidate.id != symbol.id)
            .filter(|candidate| {
                is_type_kind(&candidate.kind)
                    || matches!(candidate.kind, SymbolKind::Function | SymbolKind::Method)
            })
            .filter(|candidate| {
                candidate.range.as_ref().is_some_and(|outer| {
                    outer.start <= range.start
                        && range.end <= outer.end
                        && (outer.start, outer.end) != (range.start, range.end)
                })
            })
            .min_by_key(|candidate| {
                candidate
                    .range
                    .as_ref()
                    .map_or(u32::MAX, |outer| outer.end - outer.start)
            })
    }

    fn enclosing_type(&self, symbol: &Symbol) -> Option<&'a Symbol> {
        self.enclosing(symbol)
            .filter(|parent| is_type_kind(&parent.kind))
    }

    /// Whether the type's declaration, up to its opening brace or colon, names `word`: a
    /// supertype such as `TestCase`, `unittest.TestCase` or `IsolatedAsyncioTestCase`.
    fn declaration_mentions(&self, declaration: &Symbol, word: &str) -> bool {
        let Some(range) = &declaration.range else {
            return false;
        };
        let start = (range.start as usize).saturating_sub(1);
        for line in self.lines.iter().skip(start).take(DECLARATION_LINE_LIMIT) {
            if line.contains(word) {
                return true;
            }
            if line.contains('{') || line.trim_end().ends_with(':') {
                return false;
            }
        }
        false
    }

    fn body_has_output_comment(&self, symbol: &Symbol) -> bool {
        let Some(range) = &symbol.range else {
            return false;
        };
        let start = (range.start as usize).saturating_sub(1);
        let end = (range.end as usize).max(start + 1);
        self.lines.iter().take(end).skip(start).any(|line| {
            let line = line.trim_start().trim_start_matches("//").trim_start();
            line.starts_with("Output:") || line.starts_with("Unordered output:")
        })
    }
}

fn is_type_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait
    )
}

/// `Test`, `TestRounds` or `Test_rounds`, but not `Testify`: Go's rule is that the name after
/// the prefix does not start with a lowercase letter.
fn go_test_function_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.starts_with(|character: char| character.is_lowercase()))
}

/// The path of the Rust attribute on `line`: `tokio::test` for `#[tokio::test(flavor = ..)]`.
fn rust_attribute_path(line: &str) -> Option<&str> {
    let attribute = line.strip_prefix("#[")?;
    Some(
        attribute
            .split(['(', ']'])
            .next()
            .unwrap_or_default()
            .trim(),
    )
}

/// Whether an attribute path generates a test: its last segment is `test` (`#[tokio::test]`,
/// `#[googletest::test]`) or one of an explicit list of test-generating attributes of common
/// crates, qualified or not: `rstest`, `test_case`, `proptest`, `quickcheck`,
/// `wasm_bindgen_test`, pgrx's `pg_test` and rstest_reuse's `apply`. A list, not a `_test`
/// suffix rule, because `#[skip_test]` or `#[my_crate::not_a_test]` would read as tests; an
/// unlisted test macro reads as a withheld callable instead, which plans disclose.
fn is_rust_test_attribute_path(path: &str) -> bool {
    matches!(
        path.rsplit("::").next().unwrap_or(path).trim(),
        "test"
            | "rstest"
            | "test_case"
            | "proptest"
            | "quickcheck"
            | "wasm_bindgen_test"
            | "pg_test"
            | "apply"
    )
}

/// A test attribute, directly or through `cfg_attr`: `#[cfg_attr(not(miri), test)]`.
fn is_rust_test_attribute(line: &str) -> bool {
    if is_stacked_test_annotation(line) {
        return true;
    }
    let Some(path) = rust_attribute_path(line) else {
        return false;
    };
    if path != "cfg_attr" {
        // rstest_reuse applies a template by name; an argument spelling a macro call is not one.
        return is_rust_test_attribute_path(path)
            && !(path.ends_with("apply") && line.contains('!'));
    }
    let Some(arguments) = line
        .strip_prefix("#[")
        .and_then(|attribute| attribute.trim_end().strip_suffix(']'))
        .and_then(|attribute| attribute.strip_prefix("cfg_attr"))
        .and_then(|attribute| attribute.trim().strip_prefix('('))
        .and_then(|attribute| attribute.strip_suffix(')'))
    else {
        return false;
    };
    // The first argument is the predicate; every later one is an attribute it applies.
    top_level_arguments(arguments)
        .into_iter()
        .skip(1)
        .any(|attribute| {
            is_rust_test_attribute_path(attribute.split('(').next().unwrap_or_default())
        })
}

/// `arguments` split at the commas outside any brackets.
fn top_level_arguments(arguments: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (index, character) in arguments.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(arguments[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(arguments[start..].trim());
    parts
}

/// rstest_reuse's `#[template]`, a case list `#[apply(..)]` expands elsewhere, and rstest's
/// `#[fixture]`, a value tests take: neither is a test itself, though an `#[rstest]` may sit
/// beside it.
fn is_rust_template_attribute(line: &str) -> bool {
    rust_attribute_path(line).is_some_and(|path| {
        matches!(
            path.rsplit("::").next().unwrap_or(path).trim(),
            "template" | "fixture"
        )
    })
}

/// The simple name of a Java annotation on `line`: `Test` for `@Test` and
/// `@org.junit.jupiter.api.Test`, `ValueSource` for `@ValueSource(ints = {1})`.
fn java_annotation_name(line: &str) -> Option<&str> {
    let annotation = line.strip_prefix('@')?;
    let path = annotation
        .split(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '.')
        })
        .next()?;
    path.rsplit('.').next()
}

/// A JUnit 4/5, TestNG or property-testing annotation that makes a method a test. Recognised
/// in test files only: outside them `@Property` and `@Example` are production annotations.
fn is_java_test_annotation(line: &str) -> bool {
    is_stacked_test_annotation(line)
        || java_annotation_name(line).is_some_and(|name| {
            matches!(
                name,
                "Test"
                    | "ParameterizedTest"
                    | "RepeatedTest"
                    | "TestFactory"
                    | "TestTemplate"
                    | "Theory"
                    | "Property"
                    | "Example"
                    | "ArchTest"
                    | "CartesianTest"
                    | "FuzzTest"
            )
        })
}

/// A TestNG configuration or data-provider annotation: `@BeforeMethod`, `@AfterClass`,
/// `@DataProvider`, `@Factory`.
fn is_java_non_test_method_annotation(line: &str) -> bool {
    java_annotation_name(line).is_some_and(|name| {
        name.starts_with("Before")
            || name.starts_with("After")
            || matches!(name, "DataProvider" | "Factory")
    })
}

/// `@pytest.fixture`, `@pytest.fixture(scope="module")` or a bare `@fixture`.
fn is_python_fixture_decorator(line: &str) -> bool {
    ["@pytest.fixture", "@fixture", "@pytest_asyncio.fixture"]
        .iter()
        .any(|decorator| {
            line.strip_prefix(decorator)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('('))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_names_follow_the_toolchain_rule() {
        for name in ["Test", "TestRounds", "Test_rounds", "TestÜber"] {
            assert!(go_test_function_name(name, "Test"), "{name}");
        }
        for name in ["Testify", "testRounds", "Tester"] {
            assert!(!go_test_function_name(name, "Test"), "{name}");
        }
    }

    #[test]
    fn rust_test_attributes_are_read_by_path() {
        for line in [
            "#[test]",
            "#[tokio::test(flavor = \"multi_thread\")]",
            "#[sqlx::test]",
            "#[test_log::test]",
            "#[rstest]",
            "#[test_case(1, 2)]",
            "#[wasm_bindgen_test]",
            "#[wasm_bindgen_test::wasm_bindgen_test]",
            "#[rstest::rstest]",
            "#[test_case::test_case(1, 1 ; \"one\")]",
            "#[test_strategy::proptest]",
            "#[quickcheck_macros::quickcheck]",
            "#[pg_test]",
            "#[apply(amounts)]",
            "#[cfg_attr(not(miri), test)]",
            "#[cfg_attr(feature = \"async\", tokio::test(flavor = \"current_thread\"))]",
        ] {
            assert!(is_rust_test_attribute(line), "{line}");
        }
        assert!(!is_rust_test_attribute("#[cfg_attr(test, derive(Debug))]"));
        assert!(is_rust_template_attribute("#[template]"));
        assert!(is_rust_template_attribute("#[rstest_reuse::template]"));
        assert!(is_rust_template_attribute("#[rstest::fixture]"));
        for line in [
            "#[cfg(test)]",
            "#[testing]",
            "#[should_panic]",
            "#[serial]",
            "#[skip_test]",
            "#[my_crate::not_a_test]",
            "#[apply(cases!())]",
        ] {
            assert!(!is_rust_test_attribute(line), "{line}");
        }
    }

    #[test]
    fn java_annotations_are_read_by_simple_name() {
        for line in [
            "@Test",
            "@org.junit.jupiter.api.Test",
            "@ParameterizedTest(name = \"{0}\")",
            "@Property(tries = 10)",
        ] {
            assert!(is_java_test_annotation(line), "{line}");
        }
        for line in ["@BeforeEach", "@DisplayName(\"Test me\")", "@Override"] {
            assert!(!is_java_test_annotation(line), "{line}");
        }
        assert!(is_java_non_test_method_annotation("@BeforeMethod"));
        assert!(is_java_non_test_method_annotation(
            "@DataProvider(name = \"rates\")"
        ));
        assert!(!is_java_non_test_method_annotation("@Test"));
    }

    #[test]
    fn python_fixture_decorators_are_matched_whole() {
        for line in [
            "@pytest.fixture",
            "@pytest.fixture(scope=\"module\")",
            "@fixture",
        ] {
            assert!(is_python_fixture_decorator(line), "{line}");
        }
        for line in ["@pytest.mark.slow", "@fixtures_loaded", "@pytest.fixtures"] {
            assert!(!is_python_fixture_decorator(line), "{line}");
        }
    }
}
