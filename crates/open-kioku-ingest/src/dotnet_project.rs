//! The .NET project model (#681): which `.csproj` compiles each C# file, which projects each one
//! references, which packages it takes from outside the repository, and which `using`
//! directives it applies to every file.
//!
//! MSBuild is a programming language; this reads the declarative part of it that settles C# name
//! lookup. A project's properties and items are read from the project file and from the
//! `Directory.Build.props` MSBuild imports for it (the nearest one above, and each one above
//! that it imports in turn), and package versions from the nearest `Directory.Packages.props`.
//! A `Condition` is not evaluated: an item it guards (on the item, its `ItemGroup` or a `When`)
//! counts and is marked conditional, and the last of conditional properties wins. Items inside a
//! `<Target>` run at build time and are not read, and a `$(Property)` the files do not set keeps
//! its spelling. Solutions (`.sln`,
//! `.slnx`) name the projects they build; a project listed there that the repository does not
//! hold is reported, and one the walk found but no solution lists is still a project.

use crate::rust_use_path::normalize_path;
use open_kioku_semantic_model::{
    DotnetModel, DotnetPackage, DotnetProject, DotnetSolution, DotnetUsing,
};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// The MSBuild and solution files the manifest walk found, repository-relative.
#[derive(Debug, Default)]
pub(crate) struct DotnetFiles {
    pub(crate) projects: Vec<PathBuf>,
    pub(crate) solutions: Vec<PathBuf>,
    pub(crate) build_props: Vec<PathBuf>,
    pub(crate) package_props: Vec<PathBuf>,
}

impl DotnetFiles {
    /// Records `path` (repository-relative) when its name is one this model reads.
    pub(crate) fn record(&mut self, path: PathBuf) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".csproj") {
            self.projects.push(path);
        } else if lower.ends_with(".sln") || lower.ends_with(".slnx") {
            self.solutions.push(path);
        } else if lower == "directory.build.props" {
            self.build_props.push(path);
        } else if lower == "directory.packages.props" {
            self.package_props.push(path);
        }
    }
}

/// Reads every project and solution the walk found under `repo_root`.
pub(crate) fn read_dotnet_model(repo_root: &Path, found: &DotnetFiles) -> DotnetModel {
    let mut projects = found.projects.clone();
    projects.sort();
    projects.dedup();
    // A `ProjectReference` is matched to a project file the walk found, whatever case the
    // reference spells it in: MSBuild on Windows, where such references are written, ignores it.
    let by_lower = projects
        .iter()
        .map(|path| (slash(path).to_ascii_lowercase(), path.clone()))
        .collect::<HashMap<_, _>>();
    let props = PropsFiles::read(repo_root, &found.build_props);
    let package_props = PropsFiles::read(repo_root, &found.package_props);
    let model_projects = projects
        .iter()
        .map(|path| read_project(repo_root, path, &props, &package_props, &by_lower))
        .collect();
    let mut solutions = found
        .solutions
        .iter()
        .filter_map(|path| read_solution(repo_root, path, &by_lower))
        .collect::<Vec<_>>();
    solutions.sort_by(|left, right| left.path.cmp(&right.path));
    DotnetModel {
        projects: model_projects,
        solutions,
    }
}

/// One MSBuild file's declarative content, in document order.
#[derive(Debug, Default)]
struct MsBuildFile {
    sdk: Option<String>,
    properties: Vec<(String, String)>,
    items: Vec<MsBuildItem>,
    imports: Vec<String>,
}

#[derive(Debug, Default)]
struct MsBuildItem {
    kind: String,
    include: Option<String>,
    remove: Option<String>,
    metadata: BTreeMap<String, String>,
    /// A `Condition` on the item or an element around it (its `ItemGroup`, a `When`).
    conditional: bool,
}

impl MsBuildItem {
    fn metadata(&self, name: &str) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Reads the elements of an MSBuild file this model uses. `None` for a file that is not XML.
fn read_msbuild(content: &str) -> Option<MsBuildFile> {
    let content = content.trim_start_matches('\u{feff}');
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let document = roxmltree::Document::parse_with_options(content, options).ok()?;
    let root = document.root_element();
    let mut file = MsBuildFile {
        sdk: root.attribute("Sdk").map(str::to_string),
        ..MsBuildFile::default()
    };
    for node in root.descendants().filter(roxmltree::Node::is_element) {
        // An item or property inside a target is set when the target runs, not when the
        // project is evaluated.
        if node
            .ancestors()
            .any(|ancestor| ancestor.tag_name().name() == "Target")
        {
            continue;
        }
        let name = node.tag_name().name();
        let parent = node.parent_element().map(|parent| parent.tag_name().name());
        match (parent, name) {
            (Some("Project"), "Sdk") => {
                if let Some(sdk) = node.attribute("Name") {
                    file.sdk.get_or_insert_with(|| sdk.to_string());
                }
            }
            (_, "Import") => {
                if let Some(sdk) = node.attribute("Sdk") {
                    file.sdk.get_or_insert_with(|| sdk.to_string());
                }
                if let Some(project) = node.attribute("Project") {
                    file.imports.push(project.to_string());
                }
            }
            (Some("PropertyGroup"), property) => {
                let value = node.text().unwrap_or_default().trim().to_string();
                file.properties.push((property.to_string(), value));
            }
            (Some("ItemGroup"), kind) => {
                let mut metadata = BTreeMap::new();
                for attribute in node.attributes() {
                    if !matches!(
                        attribute.name(),
                        "Include" | "Remove" | "Update" | "Condition"
                    ) {
                        metadata
                            .insert(attribute.name().to_string(), attribute.value().to_string());
                    }
                }
                for child in node.children().filter(roxmltree::Node::is_element) {
                    let value = child.text().unwrap_or_default().trim().to_string();
                    metadata.insert(child.tag_name().name().to_string(), value);
                }
                file.items.push(MsBuildItem {
                    kind: kind.to_string(),
                    include: node.attribute("Include").map(str::to_string),
                    remove: node.attribute("Remove").map(str::to_string),
                    metadata,
                    conditional: node
                        .ancestors()
                        .filter(roxmltree::Node::is_element)
                        .any(|element| element.attribute("Condition").is_some()),
                });
            }
            _ => {}
        }
    }
    // `Microsoft.NET.Sdk/8.0.100` names a version; `A;B` names two SDKs, the first of which
    // decides the implicit usings.
    file.sdk = file.sdk.map(|sdk| {
        let first = sdk.split(';').next().unwrap_or_default();
        first
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    file.sdk = file.sdk.filter(|sdk| !sdk.is_empty());
    Some(file)
}

/// The `Directory.Build.props` or `Directory.Packages.props` files of a repository, read.
struct PropsFiles {
    /// By the repository-relative directory holding the file.
    by_dir: HashMap<String, MsBuildFile>,
}

impl PropsFiles {
    fn read(repo_root: &Path, paths: &[PathBuf]) -> Self {
        let by_dir = paths
            .iter()
            .filter_map(|path| {
                let content = fs::read_to_string(repo_root.join(path)).ok()?;
                let file = read_msbuild(&content)?;
                let dir = slash(path.parent().unwrap_or(Path::new("")));
                Some((dir, file))
            })
            .collect();
        Self { by_dir }
    }

    /// The files MSBuild imports for a project in `dir`: the nearest one at or above it, then,
    /// while one imports the next above it, that one. Outermost first, the order MSBuild
    /// evaluates them in.
    fn chain(&self, dir: &str) -> Vec<&MsBuildFile> {
        let mut chain = Vec::new();
        let mut from = Some(dir.to_string());
        while let Some(start) = from.take() {
            let Some((found_dir, file)) = ancestors(&start).find_map(|candidate| {
                self.by_dir
                    .get(&candidate)
                    .map(|file| (candidate.clone(), file))
            }) else {
                break;
            };
            chain.push(file);
            if imports_parent_props(file) && !found_dir.is_empty() {
                from = Some(parent_dir(&found_dir).to_string());
            }
        }
        chain.reverse();
        chain
    }
}

/// Whether a props file imports the next file of its own name above it, as
/// `$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))`
/// or `../Directory.Build.props` does.
fn imports_parent_props(file: &MsBuildFile) -> bool {
    file.imports.iter().any(|import| {
        let lower = import.to_ascii_lowercase();
        (lower.contains("getpathoffileabove") || lower.contains(".."))
            && (lower.contains("directory.build.props")
                || lower.contains("directory.packages.props"))
    })
}

/// `dir`, then each directory above it, ending at the repository root `""`.
fn ancestors(dir: &str) -> impl Iterator<Item = String> + '_ {
    let mut next = Some(dir.to_string());
    std::iter::from_fn(move || {
        let current = next.take()?;
        if !current.is_empty() {
            next = Some(parent_dir(&current).to_string());
        }
        Some(current)
    })
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn read_project(
    repo_root: &Path,
    path: &Path,
    props: &PropsFiles,
    package_props: &PropsFiles,
    projects: &HashMap<String, PathBuf>,
) -> DotnetProject {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    let dir = slash(path.parent().unwrap_or(Path::new("")));
    let unreadable = || DotnetProject {
        project_file: path.to_path_buf(),
        assembly_name: stem.clone(),
        root_namespace: stem.clone(),
        unreadable: true,
        ..DotnetProject::default()
    };
    let Some(file) = fs::read_to_string(repo_root.join(path))
        .ok()
        .and_then(|content| read_msbuild(&content))
    else {
        return unreadable();
    };
    let imported = props.chain(&dir);
    let mut properties = Properties::new(&stem);
    for source in imported.iter().copied().chain([&file]) {
        for (name, value) in &source.properties {
            properties.set(name, value);
        }
    }
    let sdk = file.sdk.clone();
    let assembly_name = properties
        .get("AssemblyName")
        .filter(|name| !name.is_empty())
        .unwrap_or(&stem)
        .to_string();
    properties.set("AssemblyName", &assembly_name);
    let root_namespace = properties
        .get("RootNamespace")
        .filter(|name| !name.is_empty())
        .unwrap_or(&stem)
        .to_string();
    let is_true = |name: &str| {
        properties
            .get(name)
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    };
    let is_false = |name: &str| {
        properties
            .get(name)
            .is_some_and(|value| value.eq_ignore_ascii_case("false"))
    };
    let mut project = DotnetProject {
        project_file: path.to_path_buf(),
        implicit_usings: sdk.is_some()
            && properties.get("ImplicitUsings").is_some_and(|value| {
                value.eq_ignore_ascii_case("enable") || value.eq_ignore_ascii_case("true")
            }),
        default_compile_items: sdk.is_some()
            && !is_false("EnableDefaultCompileItems")
            && !is_false("EnableDefaultItems"),
        transitive_project_references: sdk.is_some()
            && !is_true("DisableTransitiveProjectReferences"),
        sdk,
        assembly_name,
        root_namespace,
        ..DotnetProject::default()
    };
    let central = package_props
        .chain(&dir)
        .into_iter()
        .flat_map(|file| file.items.iter())
        .filter(|item| item.kind == "PackageVersion")
        .filter_map(|item| Some((item.include.as_deref()?, item.metadata("Version")?)))
        .map(|(id, version)| (id.to_ascii_lowercase(), version.to_string()))
        .collect::<HashMap<_, _>>();
    for item in imported
        .iter()
        .flat_map(|file| file.items.iter())
        .chain(&file.items)
    {
        let includes = item
            .include
            .as_deref()
            .map(|include| split_list(&properties.expand(include)))
            .unwrap_or_default();
        let removes = item
            .remove
            .as_deref()
            .map(|remove| split_list(&properties.expand(remove)))
            .unwrap_or_default();
        match item.kind.as_str() {
            "Compile" => {
                let (include, remove) = if item.conditional {
                    (
                        &mut project.conditional_compile_include,
                        &mut project.conditional_compile_remove,
                    )
                } else {
                    (&mut project.compile_include, &mut project.compile_remove)
                };
                include.extend(
                    includes
                        .iter()
                        .filter_map(|include| join_dir(&dir, include)),
                );
                remove.extend(removes.iter().filter_map(|remove| join_dir(&dir, remove)));
            }
            "ProjectReference" => {
                for include in includes {
                    match join_dir(&dir, &include)
                        .and_then(|joined| projects.get(&joined.to_ascii_lowercase()))
                    {
                        Some(reference) => {
                            if !project.project_references.contains(reference) {
                                project.project_references.push(reference.clone());
                                if item.conditional {
                                    project
                                        .conditional_project_references
                                        .push(reference.clone());
                                }
                            } else if !item.conditional {
                                // Referenced unconditionally too.
                                project
                                    .conditional_project_references
                                    .retain(|known| known != reference);
                            }
                        }
                        None => project.unresolved_project_references.push(include),
                    }
                }
            }
            "PackageReference" => {
                for id in includes {
                    let version = item
                        .metadata("Version")
                        .or_else(|| item.metadata("VersionOverride"))
                        .map(str::to_string)
                        .or_else(|| central.get(&id.to_ascii_lowercase()).cloned());
                    project.packages.push(DotnetPackage { id, version });
                }
            }
            "Using" => {
                for target in includes {
                    project.usings.push(DotnetUsing {
                        target,
                        alias: item
                            .metadata("Alias")
                            .filter(|alias| !alias.is_empty())
                            .map(str::to_string),
                        is_static: item
                            .metadata("Static")
                            .is_some_and(|value| value.eq_ignore_ascii_case("true")),
                        conditional: item.conditional,
                    });
                }
                project.removed_usings.extend(removes);
            }
            "InternalsVisibleTo" => {
                project.internals_visible_to.extend(
                    includes
                        .iter()
                        .filter_map(|include| include.split(',').next())
                        .map(|assembly| assembly.trim().to_string())
                        .filter(|assembly| !assembly.is_empty()),
                );
            }
            _ => {}
        }
    }
    // A legacy project's packages are listed in `packages.config` beside it.
    let config = repo_root.join(&dir).join("packages.config");
    if let Some(packages) = fs::read_to_string(config)
        .ok()
        .and_then(|content| read_packages_config(&content))
    {
        project.packages.extend(packages);
    }
    project
}

/// The `<package id=".." version=".." />` entries of a `packages.config`.
fn read_packages_config(content: &str) -> Option<Vec<DotnetPackage>> {
    let content = content.trim_start_matches('\u{feff}');
    let document = roxmltree::Document::parse(content).ok()?;
    Some(
        document
            .descendants()
            .filter(|node| node.has_tag_name("package"))
            .filter_map(|node| {
                Some(DotnetPackage {
                    id: node.attribute("id")?.to_string(),
                    version: node.attribute("version").map(str::to_string),
                })
            })
            .collect(),
    )
}

/// MSBuild properties as the project's evaluation sets them, for `$(Name)` expansion.
struct Properties {
    values: HashMap<String, String>,
}

impl Properties {
    fn new(project_name: &str) -> Self {
        let mut values = HashMap::new();
        values.insert("msbuildprojectname".to_string(), project_name.to_string());
        // Paths are read relative to the file's own directory, which these name.
        for directory in ["msbuildprojectdirectory", "msbuildthisfiledirectory"] {
            values.insert(directory.to_string(), String::new());
        }
        Self { values }
    }

    fn set(&mut self, name: &str, value: &str) {
        let value = self.expand(value);
        self.values.insert(name.to_ascii_lowercase(), value);
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// `text` with each `$(Name)` the properties set replaced by its value; any other is kept.
    fn expand(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("$(") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find(')') {
                Some(end) if !after[..end].contains('(') => {
                    let name = &after[..end];
                    match self.get(name) {
                        Some(value) => out.push_str(value),
                        None => out.push_str(&rest[start..start + 2 + end + 1]),
                    }
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// An MSBuild item list: `;`-separated, each entry trimmed, empty ones dropped.
fn split_list(text: &str) -> Vec<String> {
    text.split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// `path` (as an MSBuild file writes it, `\` or `/` separated) relative to the
/// repository-relative `dir`, normalized; `None` when it leaves the repository, is absolute, or
/// still spells a property this model could not expand.
fn join_dir(dir: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.contains("$(") || path.contains("@(") || path.contains("%(") {
        return None;
    }
    let joined = if dir.is_empty() {
        path
    } else {
        format!("{dir}/{path}")
    };
    normalize_path(&joined).filter(|normal| !normal.is_empty())
}

fn read_solution(
    repo_root: &Path,
    path: &Path,
    projects: &HashMap<String, PathBuf>,
) -> Option<DotnetSolution> {
    let content = fs::read_to_string(repo_root.join(path)).ok()?;
    let dir = slash(path.parent().unwrap_or(Path::new("")));
    let listed = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("slnx"))
    {
        let content = content.trim_start_matches('\u{feff}');
        let document = roxmltree::Document::parse(content).ok()?;
        document
            .descendants()
            .filter(|node| node.has_tag_name("Project"))
            .filter_map(|node| node.attribute("Path").map(str::to_string))
            .collect::<Vec<_>>()
    } else {
        content.lines().filter_map(sln_project_path).collect()
    };
    let mut solution = DotnetSolution {
        path: path.to_path_buf(),
        ..DotnetSolution::default()
    };
    for written in listed {
        if !written.to_ascii_lowercase().ends_with(".csproj") {
            continue;
        }
        match join_dir(&dir, &written).and_then(|joined| projects.get(&joined.to_ascii_lowercase()))
        {
            Some(project) => {
                if !solution.projects.contains(project) {
                    solution.projects.push(project.clone());
                }
            }
            None => solution.missing_projects.push(written),
        }
    }
    Some(solution)
}

/// The project path of a `.sln` line `Project("{type}") = "Name", "path\Name.csproj", "{id}"`.
fn sln_project_path(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("Project(")?;
    let (_, values) = rest.split_once('=')?;
    let mut quoted = values.split('"').skip(1).step_by(2);
    let _name = quoted.next()?;
    let path = quoted.next()?;
    (!path.is_empty()).then(|| path.to_string())
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("file has a parent")).expect("dir");
        fs::write(full, content).expect("write");
    }

    fn model(root: &Path, paths: &[&str]) -> DotnetModel {
        let mut found = DotnetFiles::default();
        for path in paths {
            found.record(PathBuf::from(path));
        }
        read_dotnet_model(root, &found)
    }

    fn project<'m>(model: &'m DotnetModel, path: &str) -> &'m DotnetProject {
        model
            .projects
            .iter()
            .find(|project| project.project_file == Path::new(path))
            .unwrap_or_else(|| panic!("{path} in {model:#?}"))
    }

    #[test]
    fn an_sdk_style_project_reads_its_properties_items_and_props_above_it() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "Directory.Build.props",
            "<Project>\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>\n    <RootNamespace>Acme.$(MSBuildProjectName)</RootNamespace>\n  </PropertyGroup>\n  <ItemGroup>\n    <Using Include=\"Acme.Shared\" />\n  </ItemGroup>\n</Project>\n",
        );
        write(
            root,
            "Directory.Packages.props",
            "<Project>\n  <ItemGroup>\n    <PackageVersion Include=\"Ledger.Json\" Version=\"13.0.3\" />\n  </ItemGroup>\n</Project>\n",
        );
        write(
            root,
            "src/Web/Web.csproj",
            "\u{feff}<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <PropertyGroup>\n    <AssemblyName>Acme.Web</AssemblyName>\n  </PropertyGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"..\\Core\\Core.csproj\" />\n    <ProjectReference Include=\"..\\Gone\\Gone.csproj\" />\n    <PackageReference Include=\"Ledger.Json\" />\n    <PackageReference Include=\"Ledger.Log\" Version=\"2.1.0\" />\n    <Compile Remove=\"Legacy\\**\" />\n    <Compile Include=\"..\\Shared\\Clock.cs\" Link=\"Clock.cs\" />\n    <Using Include=\"Acme.Core.Rules\" Static=\"true\" />\n    <Using Include=\"Acme.Core\" Alias=\"Core\" />\n    <Using Remove=\"System.Net.Http\" />\n    <InternalsVisibleTo Include=\"Acme.Web.Tests\" />\n  </ItemGroup>\n  <Target Name=\"Late\">\n    <ItemGroup>\n      <Compile Include=\"Generated\\**\" />\n    </ItemGroup>\n  </Target>\n</Project>\n",
        );
        write(
            root,
            "src/Core/Core.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <ImplicitUsings>disable</ImplicitUsings>\n    <DisableTransitiveProjectReferences>true</DisableTransitiveProjectReferences>\n    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n  </PropertyGroup>\n</Project>\n",
        );
        let model = model(
            root,
            &[
                "Directory.Build.props",
                "Directory.Packages.props",
                "src/Web/Web.csproj",
                "src/Core/Core.csproj",
            ],
        );
        let web = project(&model, "src/Web/Web.csproj");
        assert_eq!(web.sdk.as_deref(), Some("Microsoft.NET.Sdk.Web"));
        assert_eq!(web.assembly_name, "Acme.Web");
        // The props file's `$(MSBuildProjectName)` is the project's name.
        assert_eq!(web.root_namespace, "Acme.Web");
        assert!(web.implicit_usings);
        assert!(web.default_compile_items);
        assert!(web.transitive_project_references);
        assert_eq!(
            web.project_references,
            vec![PathBuf::from("src/Core/Core.csproj")]
        );
        assert_eq!(
            web.unresolved_project_references,
            vec!["..\\Gone\\Gone.csproj".to_string()]
        );
        assert_eq!(
            web.packages,
            vec![
                DotnetPackage {
                    id: "Ledger.Json".into(),
                    version: Some("13.0.3".into())
                },
                DotnetPackage {
                    id: "Ledger.Log".into(),
                    version: Some("2.1.0".into())
                },
            ]
        );
        assert_eq!(web.compile_remove, vec!["src/Web/Legacy/**".to_string()]);
        // An item inside a target is not read.
        assert_eq!(web.compile_include, vec!["src/Shared/Clock.cs".to_string()]);
        assert_eq!(
            web.usings,
            vec![
                DotnetUsing {
                    target: "Acme.Shared".into(),
                    alias: None,
                    is_static: false,
                    conditional: false
                },
                DotnetUsing {
                    target: "Acme.Core.Rules".into(),
                    alias: None,
                    is_static: true,
                    conditional: false
                },
                DotnetUsing {
                    target: "Acme.Core".into(),
                    alias: Some("Core".into()),
                    is_static: false,
                    conditional: false
                },
            ]
        );
        assert_eq!(web.removed_usings, vec!["System.Net.Http".to_string()]);
        assert_eq!(web.internals_visible_to, vec!["Acme.Web.Tests".to_string()]);

        let core = project(&model, "src/Core/Core.csproj");
        assert_eq!(core.assembly_name, "Core");
        assert_eq!(core.root_namespace, "Acme.Core");
        // The project's own property overrides the props file's.
        assert!(!core.implicit_usings);
        assert!(!core.default_compile_items);
        assert!(!core.transitive_project_references);
    }

    #[test]
    fn items_a_condition_guards_are_recorded_as_conditional() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "src/Core/Core.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
        );
        write(
            root,
            "src/Old/Old.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
        );
        write(
            root,
            "src/Web/Web.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup Condition=\"'$(TargetFramework)' == 'net48'\">\n    <ProjectReference Include=\"../Old/Old.csproj\" />\n    <Compile Remove=\"Modern/**\" />\n  </ItemGroup>\n  <Choose>\n    <When Condition=\"'$(Os)' == 'win'\">\n      <ItemGroup>\n        <Using Include=\"Acme.Windows\" />\n        <ProjectReference Include=\"../Core/Core.csproj\" />\n      </ItemGroup>\n    </When>\n  </Choose>\n  <ItemGroup>\n    <ProjectReference Include=\"../Core/Core.csproj\" />\n    <Compile Include=\"../Shared/*.cs\" Condition=\"'$(Shared)' == 'true'\" />\n  </ItemGroup>\n</Project>\n",
        );
        let model = model(
            root,
            &[
                "src/Core/Core.csproj",
                "src/Old/Old.csproj",
                "src/Web/Web.csproj",
            ],
        );
        let web = project(&model, "src/Web/Web.csproj");
        assert_eq!(
            web.project_references,
            vec![
                PathBuf::from("src/Old/Old.csproj"),
                PathBuf::from("src/Core/Core.csproj")
            ]
        );
        // `Core` is also referenced without a condition.
        assert_eq!(
            web.conditional_project_references,
            vec![PathBuf::from("src/Old/Old.csproj")]
        );
        assert!(web.compile_remove.is_empty());
        assert_eq!(
            web.conditional_compile_remove,
            vec!["src/Web/Modern/**".to_string()]
        );
        assert_eq!(
            web.conditional_compile_include,
            vec!["src/Shared/*.cs".to_string()]
        );
        assert_eq!(web.usings.len(), 1);
        assert!(web.usings[0].conditional);
    }

    #[test]
    fn a_legacy_project_lists_its_files_and_packages_config() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "Old/Old.csproj",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Project ToolsVersion=\"15.0\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\n  <PropertyGroup>\n    <RootNamespace>Acme.Old</RootNamespace>\n    <AssemblyName>Acme.Old</AssemblyName>\n  </PropertyGroup>\n  <ItemGroup>\n    <Compile Include=\"Ledger\\Entry.cs\" />\n    <Compile Include=\"Properties\\AssemblyInfo.cs\" />\n  </ItemGroup>\n  <Import Project=\"$(MSBuildToolsPath)\\Microsoft.CSharp.targets\" />\n</Project>\n",
        );
        write(
            root,
            "Old/packages.config",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<packages>\n  <package id=\"Ledger.Json\" version=\"6.0.8\" targetFramework=\"net45\" />\n</packages>\n",
        );
        let model = model(root, &["Old/Old.csproj"]);
        let old = project(&model, "Old/Old.csproj");
        assert_eq!(old.sdk, None);
        assert!(!old.default_compile_items);
        assert!(!old.implicit_usings);
        assert!(!old.transitive_project_references);
        assert_eq!(
            old.compile_include,
            vec![
                "Old/Ledger/Entry.cs".to_string(),
                "Old/Properties/AssemblyInfo.cs".to_string()
            ]
        );
        assert_eq!(
            old.packages,
            vec![DotnetPackage {
                id: "Ledger.Json".into(),
                version: Some("6.0.8".into())
            }]
        );
    }

    #[test]
    fn a_props_file_that_imports_the_one_above_it_is_read_after_that_one() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "Directory.Build.props",
            "<Project>\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>\n    <AssemblyName>Outer</AssemblyName>\n  </PropertyGroup>\n</Project>\n",
        );
        write(
            root,
            "src/Directory.Build.props",
            "<Project>\n  <Import Project=\"$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))\" />\n  <PropertyGroup>\n    <AssemblyName>Inner</AssemblyName>\n  </PropertyGroup>\n</Project>\n",
        );
        write(
            root,
            "tools/Directory.Build.props",
            "<Project>\n  <PropertyGroup>\n    <AssemblyName>Tool</AssemblyName>\n  </PropertyGroup>\n</Project>\n",
        );
        write(
            root,
            "src/A/A.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
        );
        write(
            root,
            "tools/B/B.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
        );
        let model = model(
            root,
            &[
                "Directory.Build.props",
                "src/Directory.Build.props",
                "tools/Directory.Build.props",
                "src/A/A.csproj",
                "tools/B/B.csproj",
            ],
        );
        let a = project(&model, "src/A/A.csproj");
        assert_eq!(a.assembly_name, "Inner");
        assert!(a.implicit_usings);
        // Without the import, only the nearest props file applies.
        let b = project(&model, "tools/B/B.csproj");
        assert_eq!(b.assembly_name, "Tool");
        assert!(!b.implicit_usings);
    }

    #[test]
    fn solutions_list_their_projects_and_report_missing_ones() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "src/A/A.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
        );
        write(
            root,
            "Acme.sln",
            "Microsoft Visual Studio Solution File, Format Version 12.00\nProject(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"A\", \"src\\A\\A.csproj\", \"{1}\"\nEndProject\nProject(\"{2150E333-8FDC-42A3-9474-1A3956D46DE8}\") = \"docs\", \"docs\", \"{2}\"\nEndProject\nProject(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"B\", \"src\\B\\B.csproj\", \"{3}\"\nEndProject\n",
        );
        write(
            root,
            "Acme.slnx",
            "<Solution>\n  <Folder Name=\"/src/\">\n    <Project Path=\"src/a/a.csproj\" />\n  </Folder>\n  <Project Path=\"tools/Gen/Gen.fsproj\" />\n</Solution>\n",
        );
        let model = model(root, &["src/A/A.csproj", "Acme.sln", "Acme.slnx"]);
        assert_eq!(
            model.solutions,
            vec![
                DotnetSolution {
                    path: "Acme.sln".into(),
                    projects: vec!["src/A/A.csproj".into()],
                    missing_projects: vec!["src\\B\\B.csproj".into()],
                },
                DotnetSolution {
                    path: "Acme.slnx".into(),
                    projects: vec!["src/A/A.csproj".into()],
                    missing_projects: vec![],
                },
            ]
        );
    }

    #[test]
    fn a_project_file_that_is_not_xml_is_unreadable_and_named_after_its_file() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        write(
            root,
            "Broken/Broken.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
        );
        let model = model(root, &["Broken/Broken.csproj"]);
        let broken = project(&model, "Broken/Broken.csproj");
        assert!(broken.unreadable);
        assert_eq!(broken.assembly_name, "Broken");
        assert!(!broken.default_compile_items);
    }
}
