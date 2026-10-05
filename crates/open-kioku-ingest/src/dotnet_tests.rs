//! Which .NET project a C# test belongs to, and the `dotnet test` command that runs it there.
//!
//! Extraction reads a C# file alone, so it filters `dotnet test` to the test but cannot say
//! which project builds it. Here each C# test target's command is scoped to the project file
//! governing its file: the nearest `*.csproj` above it, or, for a file of a shared project
//! (`*.projitems`), a sibling project importing that. Of several candidates a test project is
//! preferred, then the first by path; any of them builds and runs the test.
//!
//! A project is a test project when its file references the test SDK
//! (`Microsoft.NET.Test.Sdk`, `MSTest.Sdk`) or an xUnit, NUnit or MSTest package, sets
//! `<IsTestProject>true`, or sits on a test path (`tests/`, `Acme.Ledger.Tests/`). Packages a
//! `Directory.Build.props` adds are not read. A test matched by its attribute outside a test
//! path is confirmed as one when its project is a test project.
//!
//! `dotnet test` runs a project through VSTest unless the nearest `global.json` selects
//! Microsoft.Testing.Platform, which takes the project as `--project`. Both accept the VSTest
//! `--filter` syntax for MSTest and NUnit; xUnit v3 on the platform takes `--filter-method`
//! instead and rejects `--filter`. A project is read as xUnit v3 there when it references an
//! `xunit.v3` package or its test file uses xUnit attributes without importing NUnit or MSTest,
//! since a `Directory.Build.targets` often adds the package. This is a reading of project files,
//! not of the MSBuild evaluation: the full project model belongs to the project model, not here.

use open_kioku_core::{
    File, FileId, Language, QualityNote, QualityNoteKind, TestTarget, TestTargetOrigin,
};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// The command extraction gives a C# test: `dotnet test --filter "FullyQualifiedName~<name>"`.
const UNSCOPED_FILTER_PREFIX: &str = "dotnet test --filter \"FullyQualifiedName~";
const UNSCOPED_PROJECT_COMMAND: &str = "dotnet test";

const TEST_PROJECT_REASON: &str = "test attribute on a method of a .NET test project";

/// Packages whose reference makes a project a test project.
const TEST_PACKAGES: [&str; 9] = [
    "microsoft.net.test.sdk",
    "xunit",
    "xunit.core",
    "xunit.runner.visualstudio",
    "nunit",
    "nunit3testadapter",
    "mstest",
    "mstest.testframework",
    "mstest.testadapter",
];

/// One project file, as far as running its tests needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DotnetProject {
    /// Repository-relative, with `/` separators.
    path: String,
    is_test: bool,
    xunit_v3: bool,
}

/// Scopes every C# test target's command to its project, confirms attributed tests of test
/// projects, and returns a note when some test belongs to no project file found.
pub(crate) fn scope_csharp_test_commands(
    root: &Path,
    files: &[File],
    tests: &mut [TestTarget],
) -> Vec<QualityNote> {
    let csharp_paths = files
        .iter()
        .filter(|file| file.language == Language::CSharp)
        .map(|file| (&file.id, file.path.as_path()))
        .collect::<HashMap<&FileId, &Path>>();
    if csharp_paths.is_empty() {
        return Vec::new();
    }
    let mut projects = ProjectReader::new(root);
    let mut unscoped = BTreeSet::new();
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
        let platform = projects.uses_testing_platform(&project);
        // Only xUnit v3 runs on the platform among xUnit versions, and a `Directory.Build.*`
        // file may be what references it, so there the test file's own attributes say xUnit.
        let runner = if project.xunit_v3 || (platform && projects.declares_xunit_tests(path)) {
            Runner::XunitV3
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
    if unscoped.is_empty() {
        return Vec::new();
    }
    vec![QualityNote::new(
        QualityNoteKind::TestDiscovery,
        format!(
            "{} C# test file(s) have no `.csproj` above them, so their tests' `dotnet test` commands name no project and run from the repository root",
            unscoped.len()
        ),
    )]
}

/// The runner's rule and the project's test SDK together settle what the target is, so it
/// takes test provenance; its confidence stays what extraction gave it, since the file is on no
/// test path and may have been read through syntax errors.
fn confirm(test: &mut TestTarget) {
    test.origin = TestTargetOrigin::TestFileSymbol;
    test.reason = format!("{}; {TEST_PROJECT_REASON}", test.reason);
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
    let Some(name) = command
        .strip_prefix(UNSCOPED_FILTER_PREFIX)
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return if command == UNSCOPED_PROJECT_COMMAND {
            base
        } else {
            command.to_string()
        };
    };
    if platform && runner == Runner::XunitV3 {
        // `--filter-method` matches the whole name, with `*` at either end.
        let method = match name.strip_prefix('.') {
            Some(method) => format!("*.{method}"),
            None => name.to_string(),
        };
        return format!("{base} --filter-method \"{method}\"");
    }
    format!("{base} --filter \"FullyQualifiedName~{name}\"")
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
    /// Whether a test file is written for xUnit, by file.
    xunit_files: HashMap<PathBuf, bool>,
}

impl<'a> ProjectReader<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            governing: HashMap::new(),
            platform: HashMap::new(),
            xunit_files: HashMap::new(),
        }
    }

    /// Whether the C# file at `path` declares xUnit tests: it uses an xUnit `*Fact` or `*Theory`
    /// attribute or names `Xunit`, and imports neither NUnit nor MSTest, whose `[Theory]` and
    /// `[TestMethod]` it could otherwise be.
    fn declares_xunit_tests(&mut self, path: &Path) -> bool {
        if let Some(cached) = self.xunit_files.get(path) {
            return *cached;
        }
        let xunit = fs::read_to_string(self.root.join(path)).is_ok_and(|content| {
            ["Fact]", "Fact(", "Theory]", "Theory(", "Xunit"]
                .iter()
                .any(|marker| content.contains(marker))
                && ![
                    "NUnit.Framework",
                    "Microsoft.VisualStudio.TestTools.UnitTesting",
                ]
                .iter()
                .any(|marker| content.contains(marker))
        });
        self.xunit_files.insert(path.to_path_buf(), xunit);
        xunit
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
    let declared = elements().any(|node| {
        (node.tag_name().name() == "IsTestProject"
            && node
                .text()
                .is_some_and(|text| text.trim().eq_ignore_ascii_case("true")))
            || (node.tag_name().name() == "Project"
                && node
                    .attribute("Sdk")
                    .is_some_and(|sdk| sdk.split('/').next() == Some("MSTest.Sdk")))
    });
    let is_test = declared
        || packages.iter().any(|package| {
            TEST_PACKAGES.contains(&package.as_str()) || package.starts_with("xunit.v3")
        })
        || open_kioku_core::is_test_code_path(&path);
    DotnetProject {
        xunit_v3: packages
            .iter()
            .any(|package| package.starts_with("xunit.v3")),
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
    use open_kioku_core::{Confidence, LineRange, RepositoryId, TestSelectionTier};

    fn csharp_file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::CSharp,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
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

    const XUNIT_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <PackageReference Include=\"Microsoft.NET.Test.Sdk\" Version=\"17.8.0\" />\n    <PackageReference Include=\"xunit\" Version=\"2.5.3\" />\n  </ItemGroup>\n</Project>\n";

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
            "<PackageReference Include=\"NUnit\" Version=\"4.0.0\" />",
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
        let library = project(
            Path::new("src/Probe/Probe.csproj"),
            "<Project><PackageReference Include=\"xunit.assert\" /><IsTestProject>false</IsTestProject></Project>",
        );
        assert!(!library.is_test);
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
        };
        let filtered =
            "dotnet test --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\"";
        assert_eq!(
            scoped_command(filtered, &vstest, false, Runner::Other),
            "dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\""
        );
        assert_eq!(
            scoped_command(filtered, &vstest, true, Runner::Other),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\""
        );
        assert_eq!(
            scoped_command(filtered, &vstest, true, Runner::XunitV3),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"Acme.Ledger.EntryTests+Nested.Posts\""
        );
        assert_eq!(
            scoped_command(
                "dotnet test --filter \"FullyQualifiedName~.Inherited\"",
                &vstest,
                true,
                Runner::XunitV3
            ),
            "dotnet test --project tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter-method \"*.Inherited\""
        );
        // xUnit v3 through VSTest reads the VSTest filter.
        assert_eq!(
            scoped_command(filtered, &vstest, false, Runner::XunitV3),
            "dotnet test tests/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.EntryTests+Nested.Posts\""
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
            "tests/Ledger.Tests/global.json",
            "{ \"test\": { \"runner\": \"Microsoft.Testing.Platform\" } }",
        );
        // On the platform, an xUnit file is filtered by `--filter-method` though its project
        // names no xUnit v3 package: a `Directory.Build.targets` may reference it.
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
            "<Project><PackageReference Include=\"NUnit\" /></Project>",
        );

        let files = [
            csharp_file("nearest", "tests/Ledger.Tests/Posting/EntryTests.cs"),
            csharp_file("shared", "tests/Shared.Tests/RoundingTests.cs"),
            csharp_file("attributed", "src/Ledger.Checks/Balances.cs"),
            csharp_file("orphan", "loose/OrphanTests.cs"),
        ];
        let filter = "dotnet test --filter \"FullyQualifiedName~Acme.EntryTests.Posts\"";
        let mut tests = vec![
            target(&files[0], filter, TestTargetOrigin::TestFileSymbol),
            target(&files[1], filter, TestTargetOrigin::TestFileSymbol),
            target(&files[2], filter, TestTargetOrigin::Symbol),
            target(&files[3], filter, TestTargetOrigin::TestFileSymbol),
            target(&files[0], "dotnet test", TestTargetOrigin::TestFileHelper),
        ];

        let notes = scope_csharp_test_commands(root, &files, &mut tests);

        let commands = tests
            .iter()
            .map(|test| test.command.as_deref().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec![
                "dotnet test --project tests/Ledger.Tests/Ledger.Tests.csproj --filter-method \"Acme.EntryTests.Posts\"",
                "dotnet test tests/Runner.A/Runner.A.csproj --filter \"FullyQualifiedName~Acme.EntryTests.Posts\"",
                "dotnet test src/Ledger.Checks/Ledger.Checks.csproj --filter \"FullyQualifiedName~Acme.EntryTests.Posts\"",
                filter,
                "dotnet test --project tests/Ledger.Tests/Ledger.Tests.csproj",
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
            "dotnet test --filter \"FullyQualifiedName~Acme.Probe.Posts\"",
            TestTargetOrigin::Symbol,
        )];
        assert!(scope_csharp_test_commands(repo.path(), &files, &mut tests).is_empty());
        assert_eq!(tests[0].origin, TestTargetOrigin::Symbol);
        assert_eq!(
            tests[0].command.as_deref(),
            Some("dotnet test src/Ledger/Ledger.csproj --filter \"FullyQualifiedName~Acme.Probe.Posts\"")
        );
    }
}
