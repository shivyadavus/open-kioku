//! Which callables in a test-path file a runner executes as tests.
//!
//! A file the shared test-path rule recognises holds tests and the scaffolding around them:
//! helpers, builders, fixtures and lifecycle hooks. Only the first are validation, so each
//! language's callables are judged by the rule its runner discovers tests by, not by a name
//! heuristic and not by the file alone:
//!
//! - Rust: a test attribute in the attribute stack (`#[test]`, `#[tokio::test]`, `#[rstest]`).
//! - Java: a JUnit or TestNG test annotation (`@Test`, `@ParameterizedTest`), a JUnit 3
//!   `test*` method of a `TestCase` class, or a public method of a class TestNG annotates
//!   `@Test` that no lifecycle or data-provider annotation marks.
//! - Python: a `test*` function of a module pytest or `unittest` collects (`test*.py`,
//!   `*_test.py`), at module level or in a `Test*` or `TestCase` class, and not a fixture.
//! - Go: `TestX` and `FuzzX` in a `_test.go` file, and an `ExampleX` with an output comment;
//!   `TestMain` is the package's lifecycle hook.
//! - JavaScript and TypeScript: tests are registration calls, so a declared callable is a test
//!   only when its own declaration is one.
//!
//! A language with no runner model here keeps the file rule: every callable is a test.

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
}

impl<'a> TestFileDiscovery<'a> {
    pub(crate) fn new(file: &File, lines: &'a [&'a str], symbols: &'a [Symbol]) -> Self {
        let path = file.path.to_string_lossy().replace('\\', "/");
        let file_name = path.rsplit('/').next().unwrap_or_default().to_string();
        Self {
            language: file.language.clone(),
            file_name,
            lines,
            symbols,
            by_id: symbols.iter().map(|symbol| (&symbol.id, symbol)).collect(),
        }
    }

    /// Whether the runner executes `symbol`, a callable of this file, as a test.
    pub(crate) fn runs(&self, symbol: &Symbol) -> bool {
        match self.language {
            Language::Rust => has_adjacent_annotation(self.lines, symbol, is_rust_test_attribute),
            Language::Java => self.runs_java(symbol),
            Language::Python => self.runs_python(symbol),
            Language::Go => self.runs_go(symbol),
            Language::TypeScript | Language::JavaScript => {
                declares_registered_test(self.lines, symbol)
            }
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
        // JUnit 3 runs every `test*` method of a `TestCase`.
        if has_test_name_prefix(&symbol.name) && self.declaration_mentions(class, "TestCase") {
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
            Some(parent) if is_type_kind(&parent.kind) => {
                parent.name.starts_with("Test") || self.declaration_mentions(parent, "TestCase")
            }
            // A function nested in another is never collected.
            Some(_) => false,
        }
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

/// An attribute whose path is `test` or ends in `::test` (`#[tokio::test]`, `#[sqlx::test]`), or
/// one of the test-generating attributes of common crates.
fn is_rust_test_attribute(line: &str) -> bool {
    if is_stacked_test_annotation(line) {
        return true;
    }
    let Some(attribute) = line.strip_prefix("#[") else {
        return false;
    };
    let path = attribute
        .split(['(', ']'])
        .next()
        .unwrap_or_default()
        .trim();
    path == "test"
        || path.ends_with("::test")
        || matches!(
            path,
            "rstest" | "test_case" | "wasm_bindgen_test" | "quickcheck"
        )
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
        ] {
            assert!(is_rust_test_attribute(line), "{line}");
        }
        for line in ["#[cfg(test)]", "#[testing]", "#[should_panic]", "#[serial]"] {
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
