//! Which .NET project a C# test belongs to, and the `dotnet test` command that runs it there.
//!
//! Extraction reads a C# file alone, so it filters `dotnet test` to the test but cannot say
//! which project builds it, nor see the other parts of a `partial` type. Here, with every file in
//! view:
//!
//! - A test of a `partial` type is judged across its parts, matched by project as
//!   `open_kioku_languages::csharp::partial_types` matches them. When another part declares the type
//!   `abstract` (or it is generic there), the test runs only under the types deriving from it and
//!   is filtered by method name alone, as extraction filters a test of an abstract type. An
//!   MSTest method whose type no part marks `[TestClass]` never runs, so it is a helper.
//! - Each command is scoped to the project file governing the test's file: the nearest
//!   `*.csproj` above it, or, for a file of a shared project (`*.projitems`), a sibling project
//!   importing that. Of several candidates a test project is preferred, then the first by path;
//!   any of them builds and runs the test.
//! - A test of an abstract or generic type gets no command when its project runs none of it: a
//!   project that is not a test project, or a test-path project referencing a test framework but
//!   no runner (a library of shared specifications, such as one with `xunit.core` alone) that
//!   holds no test of a concrete type and no concrete class deriving from the test's type. Such
//!   a test runs only in the projects deriving from it, which the index does not link yet, and a
//!   command scoped to its own project runs nothing. It stays a test, at low confidence, and a
//!   quality note counts such tests.
//!
//! A project is a test project when `dotnet test` can run it: its file references the test SDK
//! (`Microsoft.NET.Test.Sdk`, `MSTest.Sdk`), a framework's runner or adapter
//! (`xunit.runner.visualstudio`, `NUnit3TestAdapter`, `MSTest.TestAdapter`, `MSTest`,
//! `xunit.v3*`), sets `<IsTestProject>true`, or sits on a test path (`tests/`,
//! `Acme.Ledger.Tests/`). `<IsTestProject>false` overrides all of these. Packages a
//! `Directory.Build.props` adds are not read, which is why a test path alone still makes a test
//! project. A test matched by its attribute outside a test path is confirmed as one when its
//! project is a test project.
//!
//! `dotnet test` runs a project through VSTest unless the nearest `global.json` selects
//! Microsoft.Testing.Platform, which takes the project as `--project`. Under VSTest a filter that
//! matches nothing exits 0, so each VSTest command keeps extraction's
//! `-- RunConfiguration.TreatNoTestsAsError=true`, under which it exits 1. The platform exits 8
//! on zero tests by itself and takes no such argument. Both accept the VSTest `--filter` syntax
//! for MSTest and NUnit; xUnit v3 on the platform takes `--filter-method` instead and rejects
//! `--filter`. Which of them runs a test is read from the test's own attributes
//! ([`csharp_tests::test_framework`]), with NUnit's `[Theory]` told from xUnit's by the method's
//! data attributes, its fixture, its file's `using NUnit.Framework;` or its project's NUnit
//! reference, and never from the file's text: an MSTest.Sdk project imports MSTest through an
//! implicit global using, so its file names no framework. A project file selecting the platform
//! by itself (`TestingPlatformDotnetTestSupport`, `UseMicrosoftTestingPlatformRunner`) is not
//! read. This is a reading of project files, not of the MSBuild evaluation: the full project
//! model belongs to the project model, not here.

use open_kioku_core::{
    Confidence, File, FileId, Language, QualityNote, QualityNoteKind, ScoreComponent, Symbol,
    SymbolId, SymbolKind, TestTarget, TestTargetOrigin,
};
use open_kioku_parse::csharp_tests::{self, Framework};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const TEST_PROJECT_REASON: &str = "test attribute on a method of a .NET test project";

const INHERITED_OUTSIDE_TEST_PROJECT_REASON: &str = "test of an abstract or generic type in a project that is not a test project: it runs only under the types deriving from it, which the index does not link yet, so no command is recommended";

const INHERITED_IN_TEST_LIBRARY_REASON: &str = "test of an abstract or generic type in a project that references a test framework but no runner and runs no test of its own: it runs only under the types deriving from it, which the index does not link yet, so no command is recommended";

const NEVER_RUN_REASON: &str =
    "MSTest method of a partial class no part of which is marked [TestClass]: MSTest never runs it";

/// Prefix of the filter that selects a test by method name alone.
const INHERITED_FILTER_PREFIX: &str = "FullyQualifiedName~.";

/// Packages whose reference lets `dotnet test` run a project: the test SDK, a framework's
/// runner or adapter, or a package bundling them. A framework package alone (`xunit.core`,
/// `NUnit`, `MSTest.TestFramework`) is how a library of shared test specifications is built, and
/// `dotnet test` runs nothing in it.
const TEST_PACKAGES: [&str; 5] = [
    "microsoft.net.test.sdk",
    "xunit.runner.visualstudio",
    "nunit3testadapter",
    "mstest.testadapter",
    "mstest",
];

/// Packages declaring tests without running them: a framework alone.
const FRAMEWORK_PACKAGES: [&str; 6] = [
    "xunit",
    "xunit.core",
    "xunit.assert",
    "xunit.extensibility.core",
    "nunit",
    "mstest.testframework",
];

/// One project file, as far as running its tests needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DotnetProject {
    /// Repository-relative, with `/` separators.
    path: String,
    is_test: bool,
    xunit_v3: bool,
    /// It references NUnit, so an ambiguous `[Theory]` in it is NUnit's.
    nunit: bool,
    /// A test project only by its path, referencing a test framework but no runner: a library of
    /// shared tests, unless a `Directory.Build.props` this reading does not see adds a runner.
    framework_only: bool,
}

/// Judges C# tests of `partial` types across their parts, scopes every C# test target's command
/// to its project, confirms attributed tests of test projects, and returns notes for tests that
/// belong to no project file and tests left without a command.
///
/// `msbuild_projects` maps each C# file to the directory of the MSBuild project it compiles
/// into, as partial-type visibility reads it, so both see the same parts as one type.
pub(crate) fn scope_csharp_test_commands(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    msbuild_projects: &HashMap<FileId, PathBuf>,
    tests: &mut Vec<TestTarget>,
) -> Vec<QualityNote> {
    let csharp_paths = files
        .iter()
        .filter(|file| file.language == Language::CSharp)
        .map(|file| (&file.id, file.path.as_path()))
        .collect::<HashMap<&FileId, &Path>>();
    if csharp_paths.is_empty() {
        return Vec::new();
    }
    let mut partials = PartialTypes::new(root, &csharp_paths, symbols, msbuild_projects);
    tests.retain_mut(|test| {
        if !csharp_paths.contains_key(&test.file_id) || !test.counts_as_validation_evidence() {
            return true;
        }
        match partials.judge(test) {
            PartialJudgement::Unchanged => true,
            PartialJudgement::InheritedOnly => {
                test.command = Some(csharp_tests::test_command(
                    &csharp_tests::inherited_test_filter(&test.name),
                ));
                true
            }
            // Outside a test path a callable that is no test is no target at all.
            PartialJudgement::NeverRuns if test.origin == TestTargetOrigin::Symbol => false,
            PartialJudgement::NeverRuns => {
                demote_to_helper(test);
                true
            }
        }
    });

    let mut projects = ProjectReader::new(root);
    // What makes a test-path project run tests of its own: a test of a concrete type, and the
    // base types its concrete classes name.
    let mut concrete_tests = HashSet::new();
    for test in tests.iter() {
        let Some(path) = csharp_paths.get(&test.file_id) else {
            continue;
        };
        if test.counts_as_validation_evidence() && !is_inherited(test) {
            if let Some(project) = projects.governing(path) {
                concrete_tests.insert(project.path);
            }
        }
    }
    let mut derived_bases: HashMap<String, HashSet<String>> = HashMap::new();
    for symbol in symbols {
        if symbol.kind != SymbolKind::Class || csharp_tests::is_inherited_only(symbol) {
            continue;
        }
        let Some(path) = csharp_paths.get(&symbol.file_id) else {
            continue;
        };
        if let Some(project) = projects.governing(path) {
            derived_bases
                .entry(project.path)
                .or_default()
                .extend(csharp_tests::base_type_names(symbol));
        }
    }

    let mut unscoped = BTreeSet::new();
    let mut commandless = 0usize;
    for test in tests.iter_mut() {
        let Some(path) = csharp_paths.get(&test.file_id) else {
            continue;
        };
        let Some(project) = projects.governing(path) else {
            if test.counts_as_validation_evidence() {
                unscoped.insert(path.to_path_buf());
            }
            continue;
        };
        if test.counts_as_validation_evidence() && is_inherited(test) {
            let reason = if !project.is_test {
                Some(INHERITED_OUTSIDE_TEST_PROJECT_REASON)
            } else if project.framework_only
                && !concrete_tests.contains(&project.path)
                && !partials.type_names(test).iter().any(|name| {
                    derived_bases
                        .get(&project.path)
                        .is_some_and(|bases| bases.contains(name))
                })
            {
                Some(INHERITED_IN_TEST_LIBRARY_REASON)
            } else {
                None
            };
            if let Some(reason) = reason {
                withhold_command(test, reason);
                commandless += 1;
                continue;
            }
        }
        let platform = projects.uses_testing_platform(&project);
        // Only xUnit v3 runs on the platform among xUnit versions, and a `Directory.Build.*`
        // file may be what references it, so there the test's own attributes say xUnit.
        let runner = if platform {
            match partials.framework(test, project.nunit) {
                Some(Framework::Xunit) => Runner::XunitV3,
                Some(Framework::NUnit | Framework::MSTest) => Runner::Other,
                None if project.xunit_v3 => Runner::XunitV3,
                None => Runner::Other,
            }
        } else {
            Runner::Other
        };
        if let Some(command) = &test.command {
            test.command = Some(scoped_command(command, &project, platform, runner));
        }
        if test.origin == TestTargetOrigin::Symbol && project.is_test {
            confirm(test);
        }
    }
    let mut notes = Vec::new();
    if !unscoped.is_empty() {
        notes.push(QualityNote::new(
            QualityNoteKind::TestDiscovery,
            format!(
                "{} C# test file(s) have no `.csproj` above them, so their tests' `dotnet test` commands name no project and run from the repository root",
                unscoped.len()
            ),
        ));
    }
    if commandless > 0 {
        notes.push(QualityNote::new(
            QualityNoteKind::TestDiscovery,
            format!(
                "{commandless} C# test(s) of abstract or generic types sit in projects that run none of them (projects that are not test projects, or that reference a test framework but no runner and hold no test of their own): they run only under the types deriving from them, which the index does not link yet, so they carry no command and low confidence"
            ),
        ));
    }
    notes
}

/// The runner's rule and the project's test SDK together settle what the target is, so it
/// takes test provenance; its confidence stays what extraction gave it, since the file is on no
/// test path and may have been read through syntax errors.
fn confirm(test: &mut TestTarget) {
    test.origin = TestTargetOrigin::TestFileSymbol;
    test.reason = format!("{}; {TEST_PROJECT_REASON}", test.reason);
}

/// Whether the test is filtered by method name alone, as one of an abstract or generic type is.
fn is_inherited(test: &TestTarget) -> bool {
    test.command
        .as_deref()
        .and_then(csharp_tests::command_filter)
        .is_some_and(|filter| filter.starts_with(INHERITED_FILTER_PREFIX))
}

/// A test with no command that runs it: still a test, but the weakest one.
fn withhold_command(test: &mut TestTarget, reason: &str) {
    test.command = None;
    set_confidence(test, Confidence::Low, reason);
}

fn demote_to_helper(test: &mut TestTarget) {
    test.origin = TestTargetOrigin::TestFileHelper;
    test.command = Some(csharp_tests::PROJECT_COMMAND.to_string());
    set_confidence(test, Confidence::Low, NEVER_RUN_REASON);
}

fn set_confidence(test: &mut TestTarget, confidence: Confidence, reason: &str) {
    test.confidence = confidence;
    test.reason = reason.into();
    test.score_breakdown = vec![ScoreComponent::single(
        "indexed_test_confidence",
        confidence.score(),
        test.evidence_refs.clone(),
        reason,
    )];
}

/// What the other parts of a test's `partial` types change about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialJudgement {
    Unchanged,
    /// A part declares an enclosing type abstract or generic.
    InheritedOnly,
    /// An MSTest method whose class no part marks `[TestClass]`.
    NeverRuns,
}

/// The parts of every `partial` C# type, and the files they are read from.
struct PartialTypes<'a> {
    root: &'a Path,
    paths: &'a HashMap<&'a FileId, &'a Path>,
    by_id: HashMap<&'a SymbolId, &'a Symbol>,
    /// Methods by file, first line and name, which is how a test target names its symbol.
    methods: HashMap<(&'a FileId, u32, &'a str), &'a Symbol>,
    /// Every part of a partial type that any part compiles into one type with, itself included.
    /// Parts in two projects are two types; a part outside any project, such as a shared
    /// project's, is one type with the parts of each project it joins.
    parts: HashMap<&'a SymbolId, Vec<&'a Symbol>>,
    contents: HashMap<FileId, Option<String>>,
}

impl<'a> PartialTypes<'a> {
    fn new(
        root: &'a Path,
        paths: &'a HashMap<&'a FileId, &'a Path>,
        symbols: &'a [Symbol],
        msbuild_projects: &HashMap<FileId, PathBuf>,
    ) -> Self {
        let csharp = symbols
            .iter()
            .filter(|symbol| paths.contains_key(&symbol.file_id))
            .collect::<Vec<_>>();
        // A part outside any project is in every project's assembly, so it joins them all.
        let mut joined: HashMap<usize, BTreeSet<usize>> = HashMap::new();
        for type_parts in open_kioku_languages::csharp::partial_types(symbols, msbuild_projects) {
            for assembly in type_parts.assemblies() {
                for &index in &assembly {
                    joined.entry(index).or_default().extend(&assembly);
                }
            }
        }
        let parts = joined
            .into_iter()
            .map(|(index, others)| {
                let others = others.into_iter().map(|other| &symbols[other]).collect();
                (&symbols[index].id, others)
            })
            .collect::<HashMap<&SymbolId, Vec<&Symbol>>>();
        Self {
            root,
            paths,
            by_id: csharp.iter().map(|symbol| (&symbol.id, *symbol)).collect(),
            methods: csharp
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::Method)
                .filter_map(|symbol| {
                    let start = symbol.range.as_ref()?.start;
                    Some(((&symbol.file_id, start, symbol.name.as_str()), *symbol))
                })
                .collect(),
            parts,
            contents: HashMap::new(),
        }
    }

    /// The method symbol a test target was read from.
    fn method(&self, test: &TestTarget) -> Option<&'a Symbol> {
        let start = test.range.as_ref()?.start;
        self.methods
            .get(&(&test.file_id, start, test.name.as_str()))
            .copied()
    }

    /// The types enclosing `method`, innermost first.
    fn types(&self, method: &'a Symbol) -> Vec<&'a Symbol> {
        std::iter::successors(self.parent(method), |symbol| self.parent(symbol))
            .filter(|symbol| matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface))
            .collect()
    }

    /// The names of the types enclosing a test, innermost first.
    fn type_names(&self, test: &TestTarget) -> Vec<String> {
        self.method(test)
            .map(|method| {
                self.types(method)
                    .into_iter()
                    .map(|class| class.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The framework whose attribute makes `test` a test, read from the attributes on its
    /// method and enclosing types; `nunit_project` says its project references NUnit.
    fn framework(&mut self, test: &TestTarget, nunit_project: bool) -> Option<Framework> {
        let method = self.method(test)?;
        let method_names = self.attribute_names(method);
        let mut type_names = Vec::new();
        for class in self.types(method) {
            for part in self.parts_of(class) {
                type_names.extend(self.attribute_names(part));
            }
        }
        let nunit = nunit_project
            || self
                .content(&method.file_id)
                .is_some_and(csharp_tests::imports_nunit);
        csharp_tests::test_framework(&method_names, &type_names, nunit)
    }

    fn judge(&mut self, test: &TestTarget) -> PartialJudgement {
        if self.parts.is_empty() {
            return PartialJudgement::Unchanged;
        }
        let Some(method) = self.method(test) else {
            return PartialJudgement::Unchanged;
        };
        let types = self.types(method);
        // A type nested in an abstract one is a type of its own; one nested in a generic type
        // is open with it.
        let inherited_only = types.iter().enumerate().any(|(depth, class)| {
            self.parts_of(class).iter().any(|part| {
                if depth == 0 {
                    csharp_tests::is_inherited_only(part)
                } else {
                    csharp_tests::is_generic(part)
                }
            })
        });
        if inherited_only {
            return if is_inherited(test) {
                PartialJudgement::Unchanged
            } else {
                PartialJudgement::InheritedOnly
            };
        }
        let Some(class) = types.first().copied() else {
            return PartialJudgement::Unchanged;
        };
        if !csharp_tests::is_partial(class) {
            return PartialJudgement::Unchanged;
        }
        let method_attributes = self.attribute_names(method);
        if !csharp_tests::is_mstest_only(&method_attributes) {
            return PartialJudgement::Unchanged;
        }
        let parts = self.parts_of(class);
        let marked = parts.iter().any(|part| {
            let names = self.attribute_names(part);
            csharp_tests::marks_test_class(&names)
        });
        if marked {
            PartialJudgement::Unchanged
        } else {
            PartialJudgement::NeverRuns
        }
    }

    fn parent(&self, symbol: &Symbol) -> Option<&'a Symbol> {
        symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .copied()
    }

    /// Every part of `class`, itself included.
    fn parts_of(&self, class: &'a Symbol) -> Vec<&'a Symbol> {
        match self.parts.get(&class.id) {
            Some(parts) => parts.clone(),
            None => vec![class],
        }
    }

    fn attribute_names(&mut self, symbol: &Symbol) -> Vec<String> {
        self.content(&symbol.file_id)
            .map(|content| csharp_tests::attribute_names(content, symbol))
            .unwrap_or_default()
    }

    /// The text of a C# file, read once.
    fn content(&mut self, file_id: &FileId) -> Option<&str> {
        let root = self.root;
        let paths = self.paths;
        self.contents
            .entry(file_id.clone())
            .or_insert_with(|| {
                paths
                    .get(file_id)
                    .and_then(|path| fs::read_to_string(root.join(path)).ok())
            })
            .as_deref()
    }
}

/// Which filter syntax a test's runner reads on Microsoft.Testing.Platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    /// `--filter-method`; it rejects `--filter`.
    XunitV3,
    /// The VSTest `--filter` syntax: MSTest, NUnit and every runner under VSTest.
    Other,
}

/// `command` run in `project`: extraction's filter kept, in the form the project's runner reads.
fn scoped_command(
    command: &str,
    project: &DotnetProject,
    platform: bool,
    runner: Runner,
) -> String {
    let path = shell_word(&project.path);
    let base = if platform {
        format!("dotnet test --project {path}")
    } else {
        format!("dotnet test {path}")
    };
    let Some(filter) = csharp_tests::command_filter(command) else {
        return if command == csharp_tests::PROJECT_COMMAND {
            base
        } else {
            command.to_string()
        };
    };
    if platform {
        if runner == Runner::XunitV3 {
            if let Some(name) = filter
                .strip_prefix("FullyQualifiedName~")
                .filter(|name| !name.contains('&'))
            {
                // `--filter-method` matches the whole name, with `*` at either end.
                let method = match name.strip_prefix('.') {
                    Some(method) => format!("*.{method}"),
                    None => name.to_string(),
                };
                return format!("{base} --filter-method \"{method}\"");
            }
        }
        return format!("{base} --filter \"{filter}\"");
    }
    format!(
        "{base} --filter \"{filter}\" {}",
        csharp_tests::VSTEST_NO_TESTS_GUARD
    )
}

/// The path as one shell word: quoted when it holds anything but path characters.
fn shell_word(path: &str) -> String {
    if path
        .chars()
        .all(|character| character.is_alphanumeric() || "/._-+".contains(character))
    {
        path.to_string()
    } else {
        format!("\"{}\"", path.replace('"', "\\\""))
    }
}

/// Reads project files on demand, each directory once.
struct ProjectReader<'a> {
    root: &'a Path,
    /// The project governing each directory walked, keyed by repository-relative directory.
    governing: HashMap<PathBuf, Option<DotnetProject>>,
    /// Whether the `global.json` nearest a directory selects Microsoft.Testing.Platform.
    platform: HashMap<PathBuf, bool>,
}

impl<'a> ProjectReader<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            governing: HashMap::new(),
            platform: HashMap::new(),
        }
    }

    /// The project building `path`: the first directory above it holding a project file, or a
    /// shared project imported by one.
    fn governing(&mut self, path: &Path) -> Option<DotnetProject> {
        let mut walked = Vec::new();
        let mut directory = path.parent();
        let mut found = None;
        while let Some(current) = directory {
            if let Some(cached) = self.governing.get(current) {
                found = cached.clone();
                break;
            }
            walked.push(current.to_path_buf());
            if let Some(project) = self.project_in(current) {
                found = Some(project);
                break;
            }
            directory = current.parent();
        }
        for directory in walked {
            self.governing.insert(directory, found.clone());
        }
        found
    }

    /// The project of `directory`: one of its `*.csproj`, or else, when it holds a shared
    /// project, a project in a sibling directory importing it.
    fn project_in(&self, directory: &Path) -> Option<DotnetProject> {
        let entries = self.entries(directory);
        let projects = entries
            .iter()
            .filter(|path| has_extension(path, "csproj"))
            .filter_map(|path| self.read(path))
            .collect::<Vec<_>>();
        if !projects.is_empty() {
            return preferred(projects);
        }
        let shared = entries
            .iter()
            .filter(|path| has_extension(path, "projitems"))
            .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
            .collect::<Vec<_>>();
        if shared.is_empty() {
            return None;
        }
        let parent = directory.parent()?;
        let importers = self
            .entries(parent)
            .iter()
            .filter(|sibling| sibling.as_path() != directory)
            .flat_map(|sibling| self.entries(sibling))
            .filter(|path| has_extension(path, "csproj"))
            .filter_map(|path| {
                let content = fs::read_to_string(self.root.join(&path)).ok()?;
                shared
                    .iter()
                    .any(|name| imports_shared_project(&content, name))
                    .then(|| project(&path, &content))
            })
            .collect::<Vec<_>>();
        preferred(importers)
    }

    /// The repository-relative paths in `directory`, sorted, or none when it cannot be read.
    fn entries(&self, directory: &Path) -> Vec<PathBuf> {
        let Ok(read) = fs::read_dir(self.root.join(directory)) else {
            return Vec::new();
        };
        let mut entries = read
            .filter_map(|entry| entry.ok())
            .map(|entry| directory.join(entry.file_name()))
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn read(&self, path: &Path) -> Option<DotnetProject> {
        let content = fs::read_to_string(self.root.join(path)).ok()?;
        Some(project(path, &content))
    }

    /// Whether `dotnet test` runs `project` through Microsoft.Testing.Platform: the nearest
    /// `global.json` at or above its directory selects it.
    fn uses_testing_platform(&mut self, project: &DotnetProject) -> bool {
        let mut directory = Path::new(&project.path).parent();
        let mut walked = Vec::new();
        let mut found = false;
        while let Some(current) = directory {
            if let Some(cached) = self.platform.get(current) {
                found = *cached;
                break;
            }
            walked.push(current.to_path_buf());
            if let Ok(content) = fs::read_to_string(self.root.join(current).join("global.json")) {
                found = selects_testing_platform(&content);
                break;
            }
            directory = current.parent();
        }
        for directory in walked {
            self.platform.insert(directory, found);
        }
        found
    }
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

/// A test project if any, else the first; candidates come sorted by path.
fn preferred(projects: Vec<DotnetProject>) -> Option<DotnetProject> {
    let first_test = projects.iter().position(|project| project.is_test);
    projects.into_iter().nth(first_test.unwrap_or(0))
}

/// The project at `path`. A file that is not well-formed XML is still a project, of nothing
/// but its path: a test project only by its name.
fn project(path: &Path, content: &str) -> DotnetProject {
    let path = path.to_string_lossy().replace('\\', "/");
    let content = content.trim_start_matches('\u{feff}');
    let document = roxmltree::Document::parse(content).ok();
    let elements = || {
        document
            .iter()
            .flat_map(|document| document.descendants())
            .filter(|node| node.is_element())
    };
    // `Include` may list several packages (`Include="xunit;xunit.runner.visualstudio"`).
    let packages = elements()
        .filter(|node| node.tag_name().name() == "PackageReference")
        .filter_map(|node| node.attribute("Include"))
        .flat_map(|include| include.split(';'))
        .map(|package| package.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    // `<IsTestProject>` decides when the file sets it: `false` marks a library of tests that runs
    // only where another project inherits them, whatever its name or packages.
    let is_test_project = elements()
        .filter(|node| node.tag_name().name() == "IsTestProject")
        .filter_map(|node| node.text())
        .map(|text| text.trim().eq_ignore_ascii_case("true"))
        .next_back();
    let declared = is_test_project == Some(true)
        || elements().any(|node| {
            node.tag_name().name() == "Project"
                && node
                    .attribute("Sdk")
                    .is_some_and(|sdk| sdk.split('/').next() == Some("MSTest.Sdk"))
        });
    let runs = declared
        || packages.iter().any(|package| {
            TEST_PACKAGES.contains(&package.as_str()) || package.starts_with("xunit.v3")
        });
    let is_test =
        is_test_project != Some(false) && (runs || open_kioku_core::is_test_code_path(&path));
    DotnetProject {
        xunit_v3: packages
            .iter()
            .any(|package| package.starts_with("xunit.v3")),
        nunit: packages.iter().any(|package| package.starts_with("nunit")),
        framework_only: is_test
            && !runs
            && packages
                .iter()
                .any(|package| FRAMEWORK_PACKAGES.contains(&package.as_str())),
        is_test,
        path,
    }
}

/// Whether the project file imports the shared project file `name`
/// (`<Import Project="..\Shared\Shared.projitems" Label="Shared" />`).
fn imports_shared_project(content: &str, name: &str) -> bool {
    let Ok(document) = roxmltree::Document::parse(content.trim_start_matches('\u{feff}')) else {
        return false;
    };
    let imports = document
        .descendants()
        .filter(|node| node.tag_name().name() == "Import")
        .filter_map(|node| node.attribute("Project"))
        .map(|project| project.replace('\\', "/"))
        .any(|project| project.rsplit('/').next() == Some(name));
    imports
}

/// `"test": { "runner": "Microsoft.Testing.Platform" }` in a `global.json`.
fn selects_testing_platform(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content.trim_start_matches('\u{feff}'))
        .is_ok_and(|json| json["test"]["runner"] == "Microsoft.Testing.Platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{LineRange, RepositoryId, TestSelectionTier};
    use open_kioku_parse::{HeuristicParser, Parser};

    const GUARD: &str = csharp_tests::VSTEST_NO_TESTS_GUARD;

    fn csharp_file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::CSharp,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            generated_by: None,
            is_vendor: false,
        }
    }

    fn target(file: &File, command: &str, origin: TestTargetOrigin) -> TestTarget {
        TestTarget {
            id: file.id.0.clone(),
            name: "Posts".into(),
            file_id: file.id.clone(),
            range: Some(LineRange { start: 1, end: 2 }),
            command: Some(command.into()),
            confidence: Confidence::Medium,
            reason: "attribute".into(),
            evidence_refs: vec![file.id.0.clone()],
            score_breakdown: Vec::new(),
            selection_tier: TestSelectionTier::default(),
            tier_justification: Vec::new(),
            origin,
        }
    }

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn filtered(filter: &str) -> String {
        csharp_tests::test_command(filter)
    }

    /// Writes `sources` under `root`, parses each C# file as indexing does, and scopes the
    /// result. Returns every target as `(name, origin, confidence, command)`, and the notes.
    #[allow(clippy::type_complexity)]
    fn index(
        root: &Path,
        sources: &[(&str, &str)],
    ) -> (
        Vec<(String, TestTargetOrigin, Confidence, Option<String>)>,
        Vec<QualityNote>,
    ) {
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        let mut tests = Vec::new();
        for (index, (path, content)) in sources.iter().enumerate() {
            write(root, path, content);
            if !path.ends_with(".cs") {
                continue;
            }
            let file = csharp_file(&format!("file-{index}"), path);
            let parsed = HeuristicParser.parse(&file, content);
            symbols.extend(parsed.syntax.symbols);
            tests.extend(parsed.tests);
            files.push(file);
        }
        let mut msbuild_dirs = HashMap::new();
        let msbuild_projects = files
            .iter()
            .filter_map(|file| {
                crate::prune::nearest_msbuild_project(root, &file.path, &mut msbuild_dirs)
                    .map(|project| (file.id.clone(), project))
            })
            .collect();
        let notes =
            scope_csharp_test_commands(root, &files, &symbols, &msbuild_projects, &mut tests);
        let targets = tests
            .into_iter()
            .map(|test| (test.name, test.origin, test.confidence, test.command))
            .collect();
        (targets, notes)
    }

    fn find<'t>(
        targets: &'t [(String, TestTargetOrigin, Confidence, Option<String>)],
        name: &str,
    ) -> &'t (String, TestTargetOrigin, Confidence, Option<String>) {
        targets
            .iter()
            .find(|target| target.0 == name)
            .unwrap_or_else(|| panic!("{name} in {targets:?}"))
    }

    const XUNIT_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <PackageReference Include=\"Microsoft.NET.Test.Sdk\" Version=\"17.8.0\" />\n    <PackageReference Include=\"xunit\" Version=\"2.5.3\" />\n    <PackageReference Include=\"xunit.runner.visualstudio\" Version=\"2.5.3\" />\n  </ItemGroup>\n</Project>\n";

    const MSTEST_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <PackageReference Include=\"Microsoft.NET.Test.Sdk\" />\n    <PackageReference Include=\"MSTest.TestAdapter\" />\n    <PackageReference Include=\"MSTest.TestFramework\" />\n  </ItemGroup>\n</Project>\n";

    #[test]
    fn test_projects_are_read_from_references_declarations_and_names() {
        let referenced = project(Path::new("src/Probe/Probe.csproj"), XUNIT_PROJECT);
        assert!(referenced.is_test && !referenced.xunit_v3);
        let v3 = project(
            Path::new("src/Probe/Probe.csproj"),
            "<PackageReference\n  Include=\"xunit.v3.mtp-v2\" />",
        );
        assert!(v3.is_test && v3.xunit_v3);
        for content in [
            "<PropertyGroup><IsTestProject>true</IsTestProject></PropertyGroup>",
            "<Project Sdk=\"MSTest.Sdk/3.6.0\" />",
            "<Project xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\"><ItemGroup><PackageReference Include=\"xunit;xunit.runner.visualstudio\" /></ItemGroup></Project>",
            "<PackageReference Include=\"NUnit3TestAdapter\" Version=\"4.5.0\" />",
            "<PackageReference Include=\"MSTest\" Version=\"4.0.2\" />",
        ] {
            assert!(
                project(Path::new("src/Probe/Probe.csproj"), content).is_test,
                "{content}"
            );
        }
        assert!(
            project(
                Path::new("src/Probe.Tests/Probe.Tests.csproj"),
                "<Project />"
            )
            .is_test
        );
        // A framework package alone builds a library of tests `dotnet test` runs nothing in.
        for content in [
            "<Project><ItemGroup><PackageReference Include=\"xunit.core\" /><PackageReference Include=\"xunit.assert\" /></ItemGroup></Project>",
            "<Project><ItemGroup><PackageReference Include=\"NUnit\" /></ItemGroup></Project>",
            "<Project><ItemGroup><PackageReference Include=\"MSTest.TestFramework\" /></ItemGroup></Project>",
        ] {
            assert!(
                !project(Path::new("src/Probe/Probe.csproj"), content).is_test,
                "{content}"
            );
        }
        // `<IsTestProject>false` wins over a test-path name and over test packages.
        let declared_library = project(
            Path::new("tests/Acme.Spec.Tests/Acme.Spec.Tests.csproj"),
            "<Project><PropertyGroup><IsTestProject>false</IsTestProject></PropertyGroup><ItemGroup><PackageReference Include=\"Microsoft.NET.Test.Sdk\" /></ItemGroup></Project>",
        );
        assert!(!declared_library.is_test);
        // On a test path, a framework package alone is a test project by its path, which may
        // be a library of shared tests; a runner reference makes it one that runs.
        let path_only = project(
            Path::new("test/Acme.Spec.Tests/Acme.Spec.Tests.csproj"),
            "<Project><ItemGroup><PackageReference Include=\"xunit.core\" /></ItemGroup></Project>",
        );
        assert!(path_only.is_test && path_only.framework_only);
        let runs = project(
            Path::new("test/Acme.Spec.Tests/Acme.Spec.Tests.csproj"),
            XUNIT_PROJECT,
        );
        assert!(runs.is_test && !runs.framework_only && !runs.nunit);
        assert!(
            project(
                Path::new("test/Acme.Spec.Tests/Acme.Spec.Tests.csproj"),
                "<Project><ItemGroup><PackageReference Include=\"NUnit\" /></ItemGroup></Project>"
            )
            .nunit
        );
        // Not XML: a project by its path alone.
        assert!(
            !project(
                Path::new("src/Probe/Probe.csproj"),
                "<Project><IsTestProject>true"
            )
            .is_test
        );
    }

    #[test]
    fn commands_are_scoped_in_the_form_each_runner_reads() {
        let vstest = DotnetProject {
            path: "tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj".into(),
            is_test: true,
            xunit_v3: false,
            nunit: false,
            framework_only: false,
        };
        let nested = filtered("FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts");
        // Under VSTest a filter matching nothing exits 0, so the guard stays.
        assert_eq!(
            scoped_command(&nested, &vstest, false, Runner::Other),
            format!("dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\" {GUARD}")
        );
        // The platform exits 8 on zero tests and takes no RunConfiguration argument.
        assert_eq!(
            scoped_command(&nested, &vstest, true, Runner::Other),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\""
        );
        assert_eq!(
            scoped_command(&nested, &vstest, true, Runner::XunitV3),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"Acme.Ledger.EntryTests+Nested.Posts\""
        );
        assert_eq!(
            scoped_command(
                &filtered("FullyQualifiedName~.Inherited"),
                &vstest,
                true,
                Runner::XunitV3
            ),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"*.Inherited\""
        );
        // xUnit v3 through VSTest reads the VSTest filter.
        assert_eq!(
            scoped_command(&nested, &vstest, false, Runner::XunitV3),
            format!("dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\" {GUARD}")
        );
        // A parameterized NUnit fixture's two-part filter is kept whole.
        assert_eq!(
            scoped_command(
                &filtered("FullyQualifiedName~Acme.CurrencyTests&FullyQualifiedName~.Converts"),
                &vstest,
                false,
                Runner::Other
            ),
            format!("dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.CurrencyTests&FullyQualifiedName~.Converts\" {GUARD}")
        );
        assert_eq!(
            scoped_command("dotnet test", &vstest, false, Runner::Other),
            "dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj"
        );
        let spaced = DotnetProject {
            path: "tests/Ledger Tests/Ledger.Tests.csproj".into(),
            ..vstest
        };
        assert_eq!(
            scoped_command("dotnet test", &spaced, false, Runner::Other),
            "dotnet test \"tests/Ledger Tests/Ledger.Tests.csproj\""
        );
    }

    /// The nearest project scopes a test; a shared project's file is scoped to the test project
    /// importing it; an attributed test outside a test path is confirmed by its test project;
    /// a test with no project above it keeps an unscoped command and is noted.
    #[test]
    fn tests_are_scoped_to_the_project_building_them() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            root,
            "tests/Ledger.Tests/Ledger.Tests.csproj",
            XUNIT_PROJECT,
        );
        write(
            root,
            "tests/Ledger.Tests/Posting/EntryTests.cs",
            "public class EntryTests\n{\n    [Fact]\n    public void Posts() { }\n}\n",
        );
        write(
            root,
            "tests/Shared.Tests/Shared.Tests.projitems",
            "<Project />",
        );
        write(
            root,
            "tests/Runner.A/Runner.A.csproj",
            "<Project>\n  <PackageReference Include=\"MSTest\" />\n  <Import Project=\"..\\Shared.Tests\\Shared.Tests.projitems\" Label=\"Shared\" />\n</Project>\n",
        );
        write(
            root,
            "src/Ledger.Checks/Ledger.Checks.csproj",
            "<Project><PackageReference Include=\"NUnit3TestAdapter\" /></Project>",
        );

        let files = [
            csharp_file("nearest", "tests/Ledger.Tests/Posting/EntryTests.cs"),
            csharp_file("shared", "tests/Shared.Tests/RoundingTests.cs"),
            csharp_file("attributed", "src/Ledger.Checks/Balances.cs"),
            csharp_file("orphan", "loose/OrphanTests.cs"),
        ];
        let filter = filtered("FullyQualifiedName~Acme.EntryTests.Posts");
        let mut tests = vec![
            target(&files[0], &filter, TestTargetOrigin::TestFileSymbol),
            target(&files[1], &filter, TestTargetOrigin::TestFileSymbol),
            target(&files[2], &filter, TestTargetOrigin::Symbol),
            target(&files[3], &filter, TestTargetOrigin::TestFileSymbol),
            target(&files[0], "dotnet test", TestTargetOrigin::TestFileHelper),
        ];

        let notes = scope_csharp_test_commands(root, &files, &[], &HashMap::new(), &mut tests);

        let commands = tests
            .iter()
            .map(|test| test.command.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec![
                format!("dotnet test tests/Ledger.Tests/Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.EntryTests.Posts\" {GUARD}"),
                format!("dotnet test tests/Runner.A/Runner.A.csproj --filter \"FullyQualifiedName~Acme.EntryTests.Posts\" {GUARD}"),
                format!("dotnet test src/Ledger.Checks/Ledger.Checks.csproj --filter \"FullyQualifiedName~Acme.EntryTests.Posts\" {GUARD}"),
                filter.clone(),
                "dotnet test tests/Ledger.Tests/Ledger.Tests.csproj".to_string(),
            ]
        );
        assert_eq!(tests[2].origin, TestTargetOrigin::TestFileSymbol);
        assert_eq!(tests[2].confidence, Confidence::Medium);
        assert!(tests[2].reason.ends_with(TEST_PROJECT_REASON));
        assert_eq!(tests[4].origin, TestTargetOrigin::TestFileHelper);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0]
            .message
            .starts_with("1 C# test file(s) have no `.csproj`"));
    }

    /// An attributed method of a project that is no test project stays a symbol match.
    #[test]
    fn a_library_project_does_not_confirm_an_attributed_method() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            "src/Ledger/Ledger.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />",
        );
        let files = [csharp_file("library", "src/Ledger/Probe.cs")];
        let mut tests = vec![target(
            &files[0],
            &filtered("FullyQualifiedName~Acme.Probe.Posts"),
            TestTargetOrigin::Symbol,
        )];
        assert!(
            scope_csharp_test_commands(repo.path(), &files, &[], &HashMap::new(), &mut tests)
                .is_empty()
        );
        assert_eq!(tests[0].origin, TestTargetOrigin::Symbol);
        assert_eq!(
            tests[0].command.clone(),
            Some(format!("dotnet test src/Ledger/Ledger.csproj --filter \"FullyQualifiedName~Acme.Probe.Posts\" {GUARD}"))
        );
    }

    /// A shared specification suite: an abstract base's `[Fact]` in a library declared
    /// `<IsTestProject>false`, inherited by a test project elsewhere. Scoped to the library, its
    /// command would run nothing and exit 0; it keeps no command, at low confidence, and a note
    /// says why. The same base in a test project keeps its name-only filter there.
    #[test]
    fn an_inherited_test_outside_a_test_project_gets_no_command() {
        let repo = tempfile::tempdir().unwrap();
        let base = "using Xunit;\n\nnamespace Acme.Spec;\n\npublic abstract class LedgerContractBase\n{\n    [Fact]\n    public void Balances() { }\n}\n";
        let (targets, notes) = index(
            repo.path(),
            &[
                (
                    "tests/Acme.Spec.Tests/Acme.Spec.Tests.csproj",
                    "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><IsTestProject>false</IsTestProject></PropertyGroup><ItemGroup><PackageReference Include=\"xunit.core\" /></ItemGroup></Project>",
                ),
                ("tests/Acme.Spec.Tests/LedgerContractBase.cs", base),
                ("tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj", XUNIT_PROJECT),
                (
                    "tests/Acme.Xunit.Tests/JournalContractBase.cs",
                    "using Xunit;\n\nnamespace Acme.Xunit;\n\npublic abstract class JournalContractBase\n{\n    [Fact]\n    public void Replays() { }\n}\n",
                ),
            ],
        );
        let balances = find(&targets, "Balances");
        assert_eq!(balances.1, TestTargetOrigin::TestFileSymbol);
        assert_eq!(balances.2, Confidence::Low);
        assert_eq!(balances.3, None);
        let replays = find(&targets, "Replays");
        assert_eq!(
            replays.3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~.Replays\" {GUARD}"))
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0]
            .message
            .starts_with("1 C# test(s) of abstract or generic types"));
    }

    /// `abstract` on another part of a partial class: the test runs only under the class
    /// deriving from it, so it is filtered by method name, as a test of an abstract class is.
    #[test]
    fn a_partial_class_abstract_in_another_part_is_filtered_by_method_name() {
        let repo = tempfile::tempdir().unwrap();
        let (targets, _) = index(
            repo.path(),
            &[
                ("tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj", XUNIT_PROJECT),
                (
                    "tests/Acme.Xunit.Tests/JournalContract.cs",
                    "using Xunit;\n\nnamespace Acme.Xunit;\n\npublic class SqlJournalTests : JournalContract\n{\n}\n\npublic partial class JournalContract\n{\n    [Fact]\n    public void Replays() { }\n\n    public class WhenEmpty\n    {\n        [Fact]\n        public void Totals() { }\n    }\n}\n\npublic partial class LedgerTests\n{\n    [Fact]\n    public void Posts() { }\n}\n",
                ),
                (
                    "tests/Acme.Xunit.Tests/JournalContract.Abstract.cs",
                    "namespace Acme.Xunit;\n\npublic abstract partial class JournalContract\n{\n}\n\npublic partial class LedgerTests\n{\n}\n",
                ),
            ],
        );
        assert_eq!(
            find(&targets, "Replays").3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~.Replays\" {GUARD}"))
        );
        // A partial class no part of which is abstract keeps its exact filter.
        assert_eq!(
            find(&targets, "Posts").3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~Acme.Xunit.LedgerTests.Posts\" {GUARD}"))
        );
        // A class nested in one another part makes abstract is a class of its own.
        assert_eq!(
            find(&targets, "Totals").3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~Acme.Xunit.JournalContract+WhenEmpty.Totals\" {GUARD}"))
        );
    }

    /// Parts are matched as partial-type visibility matches them: a part in another project is
    /// another type, so its `abstract` changes nothing here, while a part under no project file
    /// (a shared project's) is compiled with the project's parts and does.
    #[test]
    fn partial_parts_are_matched_within_a_project() {
        let repo = tempfile::tempdir().unwrap();
        let (targets, _) = index(
            repo.path(),
            &[
                ("tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj", XUNIT_PROJECT),
                (
                    "tests/Acme.Xunit.Tests/Contracts.cs",
                    "using Xunit;\n\nnamespace Acme.Xunit;\n\npublic partial class JournalContract\n{\n    [Fact]\n    public void Replays() { }\n}\n\npublic partial class LedgerContract\n{\n    [Fact]\n    public void Balances() { }\n}\n",
                ),
                ("tests/Acme.Audit.Tests/Acme.Audit.Tests.csproj", XUNIT_PROJECT),
                (
                    "tests/Acme.Audit.Tests/JournalContract.cs",
                    "namespace Acme.Xunit;\n\npublic abstract partial class JournalContract\n{\n}\n",
                ),
                (
                    "shared/LedgerContract.cs",
                    "namespace Acme.Xunit;\n\npublic abstract partial class LedgerContract\n{\n}\n",
                ),
            ],
        );
        assert_eq!(
            find(&targets, "Replays").3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~Acme.Xunit.JournalContract.Replays\" {GUARD}"))
        );
        assert_eq!(
            find(&targets, "Balances").3.clone(),
            Some(format!("dotnet test tests/Acme.Xunit.Tests/Acme.Xunit.Tests.csproj --filter \"FullyQualifiedName~.Balances\" {GUARD}"))
        );
    }

    /// MSTest runs a test method of a partial class only when some part is marked
    /// `[TestClass]`: marked on another part, it is a test; marked on none, a helper.
    #[test]
    fn an_mstest_partial_class_is_a_test_class_when_any_part_is_marked() {
        let repo = tempfile::tempdir().unwrap();
        let (targets, _) = index(
            repo.path(),
            &[
                ("tests/Acme.MSTest.Tests/Acme.MSTest.Tests.csproj", MSTEST_PROJECT),
                (
                    "tests/Acme.MSTest.Tests/GuardTests.Array.cs",
                    "namespace Acme.MSTestChecks;\n\npublic partial class GuardTests\n{\n    [TestMethod]\n    public void Rejects() { }\n}\n\npublic partial class UnmarkedTests\n{\n    [TestMethod]\n    public void NeverRuns() { }\n}\n",
                ),
                (
                    "tests/Acme.MSTest.Tests/GuardTests.cs",
                    "namespace Acme.MSTestChecks;\n\n[TestClass]\npublic partial class GuardTests\n{\n}\n\npublic partial class UnmarkedTests\n{\n}\n",
                ),
            ],
        );
        let rejects = find(&targets, "Rejects");
        assert_eq!(rejects.1, TestTargetOrigin::TestFileSymbol);
        let never = find(&targets, "NeverRuns");
        assert_eq!(never.1, TestTargetOrigin::TestFileHelper);
        assert_eq!(
            never.3.as_deref(),
            Some("dotnet test tests/Acme.MSTest.Tests/Acme.MSTest.Tests.csproj")
        );
    }

    /// On Microsoft.Testing.Platform each test's command takes the form its own framework
    /// reads, whatever its file's text says: an MSTest.Sdk project imports MSTest implicitly, so
    /// its file names no framework, and NUnit's `[Theory]` is NUnit's where NUnit is imported,
    /// globally or in the file, or referenced. An xUnit test there is selected by
    /// `--filter-method`, though its project names no xUnit v3 package, which a
    /// `Directory.Build.targets` may add.
    #[test]
    fn platform_commands_follow_each_tests_framework() {
        let repo = tempfile::tempdir().unwrap();
        let (targets, _) = index(
            repo.path(),
            &[
                (
                    "global.json",
                    "{ \"test\": { \"runner\": \"Microsoft.Testing.Platform\" } }",
                ),
                (
                    "tests/Acme.Journal.Tests/Acme.Journal.Tests.csproj",
                    "<Project Sdk=\"MSTest.Sdk/3.6.0\" />",
                ),
                (
                    "tests/Acme.Journal.Tests/EntryTests.cs",
                    "// Replaces the Xunit suite.\nnamespace Acme.Journal;\n\n[TestClass]\npublic sealed class EntryTests\n{\n    [TestMethod]\n    public void RecordsFact() { }\n}\n",
                ),
                (
                    "tests/Acme.Rounding.Tests/Acme.Rounding.Tests.csproj",
                    "<Project><ItemGroup><PackageReference Include=\"Microsoft.NET.Test.Sdk\" /><PackageReference Include=\"NUnit\" /><PackageReference Include=\"NUnit3TestAdapter\" /></ItemGroup></Project>",
                ),
                (
                    "tests/Acme.Rounding.Tests/GlobalUsings.cs",
                    "global using NUnit.Framework;\n",
                ),
                (
                    "tests/Acme.Rounding.Tests/RoundingTests.cs",
                    "// Ported from Xunit: [Fact] and [Theory(...)] became NUnit's.\nnamespace Acme.Rounding;\n\npublic class RoundingTests\n{\n    [Theory]\n    public void Rounds(int amount) { }\n\n    [Test]\n    public void Truncates() { }\n}\n",
                ),
                (
                    "tests/Acme.Imported.Tests/Acme.Imported.Tests.csproj",
                    "<Project />",
                ),
                (
                    "tests/Acme.Imported.Tests/ImportedTests.cs",
                    "using NUnit.Framework;\n\nnamespace Acme.Imported;\n\npublic class ImportedTests\n{\n    [Theory]\n    public void Holds(int amount) { }\n}\n",
                ),
                ("tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj", XUNIT_PROJECT),
                (
                    "tests/Acme.Ledger.Tests/LedgerTests.cs",
                    "namespace Acme.Ledger;\n\npublic class LedgerTests\n{\n    [Fact]\n    public void Posts() { }\n\n    [Theory]\n    [InlineData(1)]\n    public void Rounds(int amount) { }\n}\n",
                ),
            ],
        );
        let command = |file: &str, name: &str| {
            targets
                .iter()
                .find(|target| {
                    target.0 == name
                        && target
                            .3
                            .as_deref()
                            .is_some_and(|command| command.contains(file))
                })
                .and_then(|target| target.3.clone())
                .unwrap_or_else(|| panic!("{file} {name} in {targets:?}"))
        };
        assert_eq!(
            command("Acme.Journal.Tests", "RecordsFact"),
            "dotnet test --project tests/Acme.Journal.Tests/Acme.Journal.Tests.csproj --filter \"FullyQualifiedName~Acme.Journal.EntryTests.RecordsFact\""
        );
        assert_eq!(
            command("Acme.Rounding.Tests", "Rounds"),
            "dotnet test --project tests/Acme.Rounding.Tests/Acme.Rounding.Tests.csproj --filter \"FullyQualifiedName~Acme.Rounding.RoundingTests.Rounds\""
        );
        assert_eq!(
            command("Acme.Rounding.Tests", "Truncates"),
            "dotnet test --project tests/Acme.Rounding.Tests/Acme.Rounding.Tests.csproj --filter \"FullyQualifiedName~Acme.Rounding.RoundingTests.Truncates\""
        );
        assert_eq!(
            command("Acme.Imported.Tests", "Holds"),
            "dotnet test --project tests/Acme.Imported.Tests/Acme.Imported.Tests.csproj --filter \"FullyQualifiedName~Acme.Imported.ImportedTests.Holds\""
        );
        assert_eq!(
            command("Acme.Ledger.Tests", "Posts"),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"Acme.Ledger.LedgerTests.Posts\""
        );
        assert_eq!(
            command("Acme.Ledger.Tests", "Rounds"),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"Acme.Ledger.LedgerTests.Rounds\""
        );
    }

    /// A test-path project referencing a test framework but no runner, holding abstract tests
    /// alone, is a library of shared specifications: `dotnet test` runs none of them there, so
    /// they get no command. The same project holding a concrete class deriving from the base, or
    /// a test of its own, is one a `Directory.Build.props` may give a runner, and keeps the
    /// method-name filter.
    #[test]
    fn a_test_library_on_a_test_path_runs_none_of_its_inherited_tests() {
        let repo = tempfile::tempdir().unwrap();
        let framework_only = "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><PackageReference Include=\"xunit.core\" /><PackageReference Include=\"xunit.assert\" /></ItemGroup></Project>";
        let (targets, notes) = index(
            repo.path(),
            &[
                (
                    "test/Acme.Ledger.Specification.Tests/Acme.Ledger.Specification.Tests.csproj",
                    framework_only,
                ),
                (
                    "test/Acme.Ledger.Specification.Tests/LedgerSpecBase.cs",
                    "using Xunit;\n\nnamespace Acme.Ledger.Specification;\n\npublic abstract class LedgerSpecBase\n{\n    [Fact]\n    public void Balances() { }\n}\n",
                ),
                (
                    "test/Acme.Journal.Tests/Acme.Journal.Tests.csproj",
                    framework_only,
                ),
                (
                    "test/Acme.Journal.Tests/JournalContract.cs",
                    "using Xunit;\n\nnamespace Acme.Journal;\n\npublic abstract class JournalContract\n{\n    [Fact]\n    public void Replays() { }\n}\n\npublic class SqlJournalTests : JournalContract\n{\n}\n",
                ),
                ("test/Acme.Audit.Tests/Acme.Audit.Tests.csproj", framework_only),
                (
                    "test/Acme.Audit.Tests/AuditTests.cs",
                    "using Xunit;\n\nnamespace Acme.Audit;\n\npublic abstract class AuditBase\n{\n    [Fact]\n    public void Audits() { }\n}\n\npublic class AuditTests\n{\n    [Fact]\n    public void Checks() { }\n}\n",
                ),
            ],
        );
        let balances = find(&targets, "Balances");
        assert_eq!(
            (balances.1, balances.2, balances.3.clone()),
            (TestTargetOrigin::TestFileSymbol, Confidence::Low, None)
        );
        assert_eq!(
            find(&targets, "Replays").3.clone(),
            Some(format!("dotnet test test/Acme.Journal.Tests/Acme.Journal.Tests.csproj --filter \"FullyQualifiedName~.Replays\" {GUARD}"))
        );
        assert_eq!(
            find(&targets, "Audits").3.clone(),
            Some(format!("dotnet test test/Acme.Audit.Tests/Acme.Audit.Tests.csproj --filter \"FullyQualifiedName~.Audits\" {GUARD}"))
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0]
            .message
            .starts_with("1 C# test(s) of abstract or generic types"));
    }
}
