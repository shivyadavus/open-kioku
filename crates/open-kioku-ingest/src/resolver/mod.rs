use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, Import, ImportResolution, Language, ResolutionStatus, Symbol, SymbolId,
};
use open_kioku_errors::{OkError, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const MAX_CONFIGS: usize = 64;
const MAX_ALIAS_ENTRIES: usize = 512;

#[derive(Debug, Clone, Default)]
pub struct ResolverReport {
    pub resolutions: Vec<ImportResolution>,
    pub analysis_facts: Vec<AnalysisFact>,
    pub quality_notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct FileIndex {
    by_path: HashMap<String, File>,
    symbols_by_file: HashMap<FileId, Vec<Symbol>>,
}

#[derive(Debug, Clone, Default)]
struct ManifestIndex {
    ts_configs: Vec<TsConfig>,
    packages: HashSet<String>,
    go_modules: Vec<String>,
    quality_notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct TsConfig {
    dir: PathBuf,
    base_url: Option<PathBuf>,
    aliases: Vec<TsAlias>,
}

#[derive(Debug, Clone)]
struct TsAlias {
    pattern: String,
    targets: Vec<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    file: File,
    strategy: String,
    caveats: Vec<String>,
}

pub fn resolve_imports(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    imports: &[Import],
) -> Result<ResolverReport> {
    let file_index = FileIndex::new(files, symbols)?;
    let manifests = ManifestIndex::discover(root)?;
    let mut report = ResolverReport {
        quality_notes: manifests.quality_notes.clone(),
        ..Default::default()
    };

    let files_by_id = files
        .iter()
        .map(|file| (file.id.clone(), file))
        .collect::<HashMap<_, _>>();
    for import in imports {
        let Some(source_file) = files_by_id.get(&import.file_id) else {
            continue;
        };
        let resolution = resolve_one(source_file, import, &file_index, &manifests);
        if !resolution.caveats.is_empty() {
            report.quality_notes.push(format!(
                "import resolver caveat in {} for `{}`: {}",
                source_file.path.display(),
                import.imported,
                resolution.caveats.join("; ")
            ));
        }
        if let Some(fact) = analysis_fact_for_resolution(source_file, &resolution, &file_index) {
            report.analysis_facts.push(fact);
        }
        report.resolutions.push(resolution);
    }

    report.quality_notes.sort();
    report.quality_notes.dedup();
    report.analysis_facts.sort_by(|a, b| a.id.cmp(&b.id));
    report.analysis_facts.dedup_by(|a, b| a.id == b.id);
    Ok(report)
}

fn resolve_one(
    source_file: &File,
    import: &Import,
    files: &FileIndex,
    manifests: &ManifestIndex,
) -> ImportResolution {
    let imported = import.imported.trim();
    if is_builtin(&source_file.language, imported) {
        return resolution(
            import,
            ResolutionStatus::Builtin,
            None,
            None,
            Confidence::High,
            "builtin",
            vec![],
        );
    }

    let mut candidates = Vec::new();
    if is_relative_import(imported) {
        candidates.extend(resolve_relative(source_file, imported, files));
    }
    if matches!(
        source_file.language,
        Language::TypeScript | Language::JavaScript
    ) {
        candidates.extend(resolve_ts_alias_or_base_url(
            source_file,
            imported,
            files,
            manifests,
        ));
    }
    if matches!(source_file.language, Language::Go) {
        candidates.extend(resolve_go_module(imported, files, manifests));
    }
    candidates.extend(resolve_language_module(source_file, imported, files));

    candidates.sort_by(|a, b| {
        a.strategy
            .cmp(&b.strategy)
            .then_with(|| a.file.path.cmp(&b.file.path))
    });
    candidates.dedup_by(|a, b| a.file.id == b.file.id);

    match candidates.len() {
        0 => {
            let package = package_name(imported);
            if manifests.packages.contains(&package) || is_bare_package_import(imported) {
                resolution(
                    import,
                    ResolutionStatus::ExternalPackage,
                    None,
                    None,
                    Confidence::Medium,
                    "manifest-package",
                    vec![],
                )
            } else {
                resolution(
                    import,
                    ResolutionStatus::Unresolved,
                    None,
                    None,
                    Confidence::Low,
                    "unresolved",
                    vec![
                        "no repo-relative file, alias target, builtin, or manifest package matched"
                            .into(),
                    ],
                )
            }
        }
        1 => {
            let candidate = candidates.remove(0);
            let symbol_id = files.best_symbol_for_import(&candidate.file.id, imported);
            resolution(
                import,
                ResolutionStatus::Resolved,
                Some(candidate.file.id),
                symbol_id,
                Confidence::High,
                &candidate.strategy,
                candidate.caveats,
            )
        }
        count => resolution(
            import,
            ResolutionStatus::Ambiguous { candidates: count },
            None,
            None,
            Confidence::Low,
            "ambiguous",
            vec![format!("{count} repo-relative candidates matched")],
        ),
    }
}

fn resolution(
    import: &Import,
    status: ResolutionStatus,
    target_file: Option<FileId>,
    target_symbol: Option<SymbolId>,
    confidence: Confidence,
    strategy: &str,
    caveats: Vec<String>,
) -> ImportResolution {
    ImportResolution {
        import: import.clone(),
        status,
        target_file,
        target_symbol,
        confidence,
        strategy: strategy.into(),
        caveats,
    }
}

fn analysis_fact_for_resolution(
    source_file: &File,
    resolution: &ImportResolution,
    files: &FileIndex,
) -> Option<AnalysisFact> {
    let (edge_type, target_kind, target, message) = match &resolution.status {
        ResolutionStatus::Resolved => {
            let target_file = files.by_id(resolution.target_file.as_ref()?)?;
            (
                if resolution.target_symbol.is_some() {
                    GraphEdgeType::References
                } else {
                    GraphEdgeType::Imports
                },
                GraphNodeType::File,
                identity::normalize_repo_path(&target_file.path).ok()?,
                format!(
                    "resolved import `{}` to {} with {} confidence",
                    resolution.import.imported,
                    target_file.path.display(),
                    confidence_label(resolution.confidence)
                ),
            )
        }
        ResolutionStatus::Builtin => (
            GraphEdgeType::DependsOn,
            GraphNodeType::Package,
            resolution.import.imported.clone(),
            format!("resolved `{}` as a language builtin", resolution.import.imported),
        ),
        ResolutionStatus::ExternalPackage => (
            GraphEdgeType::DependsOn,
            GraphNodeType::Package,
            package_name(&resolution.import.imported),
            format!(
                "resolved `{}` as an external package dependency",
                resolution.import.imported
            ),
        ),
        ResolutionStatus::Ambiguous { candidates } => (
            GraphEdgeType::Imports,
            GraphNodeType::Module,
            resolution.import.imported.clone(),
            format!(
                "ambiguous import `{}` matched {candidates} candidates; no high-confidence edge emitted",
                resolution.import.imported
            ),
        ),
        ResolutionStatus::Unresolved => (
            GraphEdgeType::Imports,
            GraphNodeType::Module,
            resolution.import.imported.clone(),
            format!("unresolved import `{}`", resolution.import.imported),
        ),
    };

    let mut message = message;
    if !resolution.caveats.is_empty() {
        message.push_str("; caveats: ");
        message.push_str(&resolution.caveats.join("; "));
    }

    Some(AnalysisFact {
        id: identity::stable_hash(&format!(
            "import-resolution:{}:{}:{:?}:{}",
            source_file.path.display(),
            resolution.import.imported,
            resolution.status,
            resolution.strategy
        )),
        file_id: source_file.id.clone(),
        symbol_id: resolution.target_symbol.clone(),
        target,
        target_kind,
        edge_type,
        range: resolution.import.range.clone(),
        confidence: resolution.confidence,
        source: format!("open-kioku-import-resolver/{}", resolution.strategy),
        source_type: EvidenceSourceType::StaticAnalysis,
        message,
    })
}

impl FileIndex {
    fn new(files: &[File], symbols: &[Symbol]) -> Result<Self> {
        let mut by_path = HashMap::new();
        for file in files {
            by_path.insert(identity::normalize_repo_path(&file.path)?, file.clone());
        }
        let mut symbols_by_file = HashMap::<FileId, Vec<Symbol>>::new();
        for symbol in symbols {
            symbols_by_file
                .entry(symbol.file_id.clone())
                .or_default()
                .push(symbol.clone());
        }
        Ok(Self {
            by_path,
            symbols_by_file,
        })
    }

    fn by_id(&self, id: &FileId) -> Option<&File> {
        self.by_path.values().find(|file| file.id == *id)
    }

    fn matching_files(&self, path: &Path) -> Vec<Candidate> {
        let mut candidates = candidate_paths(path)
            .into_iter()
            .filter_map(|candidate| {
                let normalized = identity::normalize_repo_path(&candidate).ok()?;
                self.by_path
                    .get(&normalized)
                    .cloned()
                    .map(|file| Candidate {
                        file,
                        strategy: "repo-path".into(),
                        caveats: vec![],
                    })
            })
            .collect::<Vec<_>>();
        if let Ok(normalized_dir) = identity::normalize_repo_path(path) {
            let prefix = format!("{normalized_dir}/");
            candidates.extend(
                self.by_path
                    .iter()
                    .filter(|(candidate_path, _)| {
                        candidate_path.starts_with(&prefix)
                            && !candidate_path[prefix.len()..].contains('/')
                    })
                    .map(|(_, file)| Candidate {
                        file: file.clone(),
                        strategy: "repo-path-directory".into(),
                        caveats: vec![
                            "directory import matched a file inside the directory".into(),
                        ],
                    }),
            );
        }
        candidates.sort_by(|a, b| a.file.path.cmp(&b.file.path));
        candidates.dedup_by(|a, b| a.file.id == b.file.id);
        candidates
    }

    fn best_symbol_for_import(&self, file_id: &FileId, imported: &str) -> Option<SymbolId> {
        let name = imported
            .rsplit(['/', '.', ':'])
            .next()
            .unwrap_or(imported)
            .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '$');
        let symbols = self.symbols_by_file.get(file_id)?;
        let mut matches = symbols
            .iter()
            .filter(|symbol| symbol.name == name || symbol.qualified_name.ends_with(name))
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            matches.pop().map(|symbol| symbol.id.clone())
        } else {
            None
        }
    }
}

impl ManifestIndex {
    fn discover(root: &Path) -> Result<Self> {
        let mut index = ManifestIndex::default();
        let mut config_count = 0usize;
        let mut alias_count = 0usize;
        for entry in WalkDir::new(root).into_iter().filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git" | ".ok" | "target" | "node_modules" | "vendor"
            )
        }) {
            let entry = entry.map_err(|err| OkError::Index(err.to_string()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let Some(name) = rel.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if is_manifest_name(name) {
                if config_count >= MAX_CONFIGS {
                    index.quality_notes.push(format!(
                        "import resolver config cap hit at {MAX_CONFIGS} manifest/config files"
                    ));
                    break;
                }
                config_count += 1;
                let content = fs::read_to_string(entry.path()).unwrap_or_default();
                index.read_manifest(rel, name, &content, &mut alias_count);
            }
        }
        index.ts_configs.sort_by(|a, b| {
            b.dir
                .components()
                .count()
                .cmp(&a.dir.components().count())
                .then_with(|| a.dir.cmp(&b.dir))
        });
        index.go_modules.sort();
        index.go_modules.dedup();
        Ok(index)
    }

    fn read_manifest(&mut self, rel: &Path, name: &str, content: &str, alias_count: &mut usize) {
        match name {
            "package.json" | "composer.json" | "pubspec.yaml" => {
                self.read_package_manifest(name, content);
            }
            "tsconfig.json" | "jsconfig.json" => {
                if *alias_count >= MAX_ALIAS_ENTRIES {
                    self.quality_notes.push(format!(
                        "import resolver alias cap hit at {MAX_ALIAS_ENTRIES} entries"
                    ));
                    return;
                }
                if let Some(config) = read_ts_config(rel.parent().unwrap_or(Path::new("")), content)
                {
                    *alias_count += config.aliases.len();
                    if *alias_count > MAX_ALIAS_ENTRIES {
                        self.quality_notes.push(format!(
                            "import resolver alias cap hit at {MAX_ALIAS_ENTRIES} entries"
                        ));
                    } else {
                        self.ts_configs.push(config);
                    }
                }
            }
            "go.mod" | "go.work" => {
                self.read_go_manifest(content);
            }
            "pyproject.toml"
            | "setup.cfg"
            | "Cargo.toml"
            | "Cargo.lock"
            | "pom.xml"
            | "build.gradle"
            | "settings.gradle"
            | "build.gradle.kts"
            | "settings.gradle.kts" => {
                self.read_text_manifest(content);
            }
            _ => {}
        }
    }

    fn read_package_manifest(&mut self, name: &str, content: &str) {
        if name == "package.json" || name == "composer.json" {
            if let Ok(json) = serde_json::from_str::<Value>(content) {
                for section in [
                    "dependencies",
                    "devDependencies",
                    "peerDependencies",
                    "optionalDependencies",
                    "require",
                    "require-dev",
                ] {
                    if let Some(object) = json.get(section).and_then(Value::as_object) {
                        self.packages.extend(object.keys().cloned());
                    }
                }
                if let Some(name) = json.get("name").and_then(Value::as_str) {
                    self.packages.insert(name.to_string());
                }
            }
        } else {
            self.read_text_manifest(content);
        }
    }

    fn read_go_manifest(&mut self, content: &str) {
        for line in content.lines() {
            let line = line.trim();
            if let Some(module) = line.strip_prefix("module ") {
                self.go_modules.push(module.trim().to_string());
                self.packages.insert(module.trim().to_string());
            }
            if let Some(require) = line.strip_prefix("require ") {
                let package = require.split_whitespace().next().unwrap_or_default();
                if !package.is_empty() && package != "(" {
                    self.packages.insert(package.to_string());
                }
            }
        }
    }

    fn read_text_manifest(&mut self, content: &str) {
        for token in manifest_tokens(content) {
            if looks_like_package_name(&token) {
                self.packages.insert(token);
            }
        }
    }
}

fn read_ts_config(dir: &Path, content: &str) -> Option<TsConfig> {
    let json = serde_json::from_str::<Value>(&strip_json_comments(content)).ok()?;
    let compiler = json.get("compilerOptions").unwrap_or(&Value::Null);
    let base_url = compiler
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let aliases = compiler
        .get("paths")
        .and_then(Value::as_object)
        .map(|paths| {
            paths
                .iter()
                .filter_map(|(pattern, targets)| {
                    let targets = targets
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    (!targets.is_empty()).then(|| TsAlias {
                        pattern: pattern.clone(),
                        targets,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(TsConfig {
        dir: dir.to_path_buf(),
        base_url,
        aliases,
    })
}

fn resolve_relative(source_file: &File, imported: &str, files: &FileIndex) -> Vec<Candidate> {
    let base = source_file.path.parent().unwrap_or(Path::new(""));
    files
        .matching_files(&base.join(imported))
        .into_iter()
        .map(|mut candidate| {
            candidate.strategy = "relative-path".into();
            candidate
        })
        .collect()
}

fn resolve_ts_alias_or_base_url(
    source_file: &File,
    imported: &str,
    files: &FileIndex,
    manifests: &ManifestIndex,
) -> Vec<Candidate> {
    let source_path = source_file.path.as_path();
    let Some(config) = manifests
        .ts_configs
        .iter()
        .find(|config| config.dir.as_os_str().is_empty() || source_path.starts_with(&config.dir))
    else {
        return Vec::new();
    };

    let mut aliases = config.aliases.clone();
    aliases.sort_by_key(|alias| std::cmp::Reverse(alias_prefix_len(&alias.pattern)));
    for alias in aliases {
        if let Some(capture) = alias_match(&alias.pattern, imported) {
            let mut matches = Vec::new();
            for target in alias.targets {
                let target = if target.contains('*') {
                    target.replace('*', capture)
                } else {
                    target
                };
                let base = config
                    .base_url
                    .as_ref()
                    .map(|base_url| config.dir.join(base_url))
                    .unwrap_or_else(|| config.dir.clone());
                matches.extend(files.matching_files(&base.join(target)));
            }
            for candidate in &mut matches {
                candidate.strategy = format!("tsconfig-paths:{}", alias.pattern);
            }
            return matches;
        }
    }

    if let Some(base_url) = &config.base_url {
        return files
            .matching_files(&config.dir.join(base_url).join(imported))
            .into_iter()
            .map(|mut candidate| {
                candidate.strategy = "tsconfig-baseUrl".into();
                candidate
            })
            .collect();
    }
    Vec::new()
}

fn resolve_go_module(
    imported: &str,
    files: &FileIndex,
    manifests: &ManifestIndex,
) -> Vec<Candidate> {
    for module in &manifests.go_modules {
        if let Some(suffix) = imported.strip_prefix(module) {
            let suffix = suffix.trim_start_matches('/');
            return files
                .matching_files(Path::new(suffix))
                .into_iter()
                .map(|mut candidate| {
                    candidate.strategy = format!("go-module:{module}");
                    candidate
                })
                .collect();
        }
    }
    Vec::new()
}

fn resolve_language_module(
    source_file: &File,
    imported: &str,
    files: &FileIndex,
) -> Vec<Candidate> {
    match source_file.language {
        Language::Rust => {
            let module = imported
                .strip_prefix("crate::")
                .or_else(|| imported.strip_prefix("self::"))
                .unwrap_or(imported)
                .replace("::", "/");
            if module == imported && !imported.contains("::") {
                Vec::new()
            } else {
                files
                    .matching_files(&Path::new("src").join(module))
                    .into_iter()
                    .map(|mut candidate| {
                        candidate.strategy = "rust-module".into();
                        candidate
                    })
                    .collect()
            }
        }
        Language::Python => files
            .matching_files(Path::new(&imported.replace('.', "/")))
            .into_iter()
            .map(|mut candidate| {
                candidate.strategy = "python-module".into();
                candidate
            })
            .collect(),
        Language::Java => {
            let module = imported.replace('.', "/");
            [
                PathBuf::from("src/main/java"),
                PathBuf::from("src/test/java"),
            ]
            .into_iter()
            .flat_map(|prefix| files.matching_files(&prefix.join(&module)))
            .map(|mut candidate| {
                candidate.strategy = "java-package".into();
                candidate
            })
            .collect()
        }
        _ => Vec::new(),
    }
}

fn candidate_paths(path: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    let extensions = [
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go", "java",
    ];
    if path.extension().is_none() {
        candidates.extend(extensions.iter().map(|ext| path.with_extension(ext)));
        candidates.extend(
            [
                "index.ts",
                "index.tsx",
                "index.js",
                "index.jsx",
                "__init__.py",
                "mod.rs",
            ]
            .iter()
            .map(|name| path.join(name)),
        );
    }
    candidates
}

fn alias_match<'a>(pattern: &str, imported: &'a str) -> Option<&'a str> {
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        imported
            .strip_prefix(prefix)?
            .strip_suffix(suffix)
            .or(Some(imported.strip_prefix(prefix)?))
    } else {
        (pattern == imported).then_some("")
    }
}

fn alias_prefix_len(pattern: &str) -> usize {
    pattern.split('*').next().unwrap_or(pattern).len()
}

fn is_relative_import(imported: &str) -> bool {
    imported.starts_with("./") || imported.starts_with("../")
}

fn is_bare_package_import(imported: &str) -> bool {
    !is_relative_import(imported) && !imported.starts_with('/') && !imported.contains("::")
}

fn package_name(imported: &str) -> String {
    let mut parts = imported.split('/');
    let first = parts.next().unwrap_or(imported);
    if first.starts_with('@') {
        parts
            .next()
            .map(|second| format!("{first}/{second}"))
            .unwrap_or_else(|| first.to_string())
    } else {
        first.to_string()
    }
}

fn is_builtin(language: &Language, imported: &str) -> bool {
    match language {
        Language::Rust => matches!(imported.split("::").next(), Some("std" | "core" | "alloc")),
        Language::JavaScript | Language::TypeScript => matches!(
            imported.strip_prefix("node:").unwrap_or(imported),
            "fs" | "path" | "url" | "http" | "https" | "crypto" | "os" | "stream"
        ),
        Language::Python => matches!(
            imported.split('.').next(),
            Some("os" | "sys" | "pathlib" | "json" | "typing" | "asyncio" | "datetime")
        ),
        Language::Go => !imported.contains('.') && imported.split('/').all(|part| !part.is_empty()),
        Language::Java => imported.starts_with("java.") || imported.starts_with("javax."),
        _ => false,
    }
}

fn is_manifest_name(name: &str) -> bool {
    matches!(
        name,
        "Cargo.toml"
            | "Cargo.lock"
            | "package.json"
            | "tsconfig.json"
            | "jsconfig.json"
            | "go.mod"
            | "go.work"
            | "pyproject.toml"
            | "setup.cfg"
            | "pom.xml"
            | "build.gradle"
            | "settings.gradle"
            | "build.gradle.kts"
            | "settings.gradle.kts"
            | "composer.json"
            | "pubspec.yaml"
    )
}

fn manifest_tokens(content: &str) -> Vec<String> {
    content
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '"' | '\'' | '=' | ':' | ',' | '(' | ')' | '[' | ']' | '{' | '}'
                )
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn looks_like_package_name(token: &str) -> bool {
    token.len() > 1
        && token
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '@'))
        && token.chars().any(|ch| ch.is_alphabetic())
}

fn strip_json_comments(content: &str) -> String {
    let mut stripped = String::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            stripped.push_str(line.split("//").next().unwrap_or(line));
            stripped.push('\n');
        }
    }
    stripped
}

fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
        Confidence::Exact => "exact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{LineRange, RepositoryId, SymbolKind};
    use tempfile::TempDir;

    fn file(id: &str, path: &str, language: Language) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language,
            size_bytes: 10,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn import(file_id: &str, imported: &str) -> Import {
        Import {
            file_id: FileId::new(file_id),
            imported: imported.into(),
            range: Some(LineRange::single(1)),
            confidence: Confidence::Medium,
        }
    }

    fn symbol(id: &str, file_id: &str, name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Function,
            file_id: FileId::new(file_id),
            range: Some(LineRange::single(1)),
            language: Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    #[test]
    fn resolves_ts_path_alias_with_longest_prefix() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["src/app/*"],"@app/auth/*":["src/app/auth/*"]}}}"#,
        )
        .unwrap();
        let files = vec![
            file("entry", "src/index.ts", Language::TypeScript),
            file("session", "src/app/auth/session.ts", Language::TypeScript),
        ];
        let report = resolve_imports(
            tmp.path(),
            &files,
            &[symbol("handler", "session", "session")],
            &[import("entry", "@app/auth/session")],
        )
        .unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("session"))
        );
        assert!(report.resolutions[0]
            .strategy
            .starts_with("tsconfig-paths:@app/auth/"));
        assert!(report
            .analysis_facts
            .iter()
            .any(|fact| fact.edge_type == GraphEdgeType::References));
    }

    #[test]
    fn resolves_js_base_url_import() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("jsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":"src"}}"#,
        )
        .unwrap();
        let files = vec![
            file("entry", "src/index.js", Language::JavaScript),
            file("util", "src/lib/util.js", Language::JavaScript),
        ];
        let report =
            resolve_imports(tmp.path(), &files, &[], &[import("entry", "lib/util")]).unwrap();

        assert_eq!(report.resolutions[0].target_file, Some(FileId::new("util")));
        assert_eq!(report.resolutions[0].strategy, "tsconfig-baseUrl");
    }

    #[test]
    fn ambiguous_candidates_are_low_confidence_caveats() {
        let tmp = TempDir::new().unwrap();
        let files = vec![
            file("entry", "src/index.ts", Language::TypeScript),
            file("one", "src/util.ts", Language::TypeScript),
            file("two", "src/util/index.ts", Language::TypeScript),
        ];
        let report =
            resolve_imports(tmp.path(), &files, &[], &[import("entry", "./util")]).unwrap();

        assert_eq!(
            report.resolutions[0].status,
            ResolutionStatus::Ambiguous { candidates: 2 }
        );
        assert_eq!(report.resolutions[0].confidence, Confidence::Low);
        assert!(!report.resolutions[0].caveats.is_empty());
    }

    #[test]
    fn package_and_builtin_imports_become_dependencies() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies":{"react":"latest"}}"#,
        )
        .unwrap();
        let files = vec![file("entry", "src/index.ts", Language::TypeScript)];
        let report = resolve_imports(
            tmp.path(),
            &files,
            &[],
            &[import("entry", "react"), import("entry", "node:fs")],
        )
        .unwrap();

        assert_eq!(
            report.resolutions[0].status,
            ResolutionStatus::ExternalPackage
        );
        assert_eq!(report.resolutions[1].status, ResolutionStatus::Builtin);
        assert!(report
            .analysis_facts
            .iter()
            .all(|fact| fact.edge_type == GraphEdgeType::DependsOn));
    }

    #[test]
    fn config_cap_hit_is_reported() {
        let tmp = TempDir::new().unwrap();
        for index in 0..(MAX_CONFIGS + 1) {
            let dir = tmp.path().join(format!("pkg{index}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("tsconfig.json"), "{}").unwrap();
        }
        let report = resolve_imports(tmp.path(), &[], &[], &[]).unwrap();
        assert!(report
            .quality_notes
            .iter()
            .any(|note| note.contains("config cap hit")));
    }

    #[test]
    fn resolves_rust_workspace_module_import() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\".\"]",
        )
        .unwrap();
        let files = vec![
            file("lib", "src/lib.rs", Language::Rust),
            file("utils", "src/utils.rs", Language::Rust),
        ];
        let report =
            resolve_imports(tmp.path(), &files, &[], &[import("lib", "crate::utils")]).unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("utils"))
        );
    }

    #[test]
    fn resolves_go_module_directory_import() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("go.mod"), "module example.com/app\n").unwrap();
        let files = vec![
            file("main", "cmd/server/main.go", Language::Go),
            file("orders", "internal/orders/orders.go", Language::Go),
        ];
        let report = resolve_imports(
            tmp.path(),
            &files,
            &[],
            &[import("main", "example.com/app/internal/orders")],
        )
        .unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("orders"))
        );
        assert!(report.resolutions[0].strategy.starts_with("go-module:"));
    }

    #[test]
    fn resolves_python_package_import() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("pyproject.toml"), "[project]\nname='app'\n").unwrap();
        let files = vec![
            file("entry", "app/main.py", Language::Python),
            file("service", "app/service.py", Language::Python),
        ];
        let report =
            resolve_imports(tmp.path(), &files, &[], &[import("entry", "app.service")]).unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("service"))
        );
    }

    #[test]
    fn resolves_java_maven_package_import() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("pom.xml"), "<project></project>").unwrap();
        let files = vec![
            file(
                "controller",
                "src/main/java/com/acme/OrderController.java",
                Language::Java,
            ),
            file(
                "client",
                "src/main/java/com/acme/Client.java",
                Language::Java,
            ),
        ];
        let report = resolve_imports(
            tmp.path(),
            &files,
            &[],
            &[import("controller", "com.acme.Client")],
        )
        .unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("client"))
        );
    }

    #[test]
    fn deepest_nested_ts_config_wins() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("packages/web");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            tmp.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["wrong/*"]}}}"#,
        )
        .unwrap();
        fs::write(
            nested.join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["src/*"]}}}"#,
        )
        .unwrap();
        let files = vec![
            file("entry", "packages/web/src/index.ts", Language::TypeScript),
            file(
                "target",
                "packages/web/src/auth/session.ts",
                Language::TypeScript,
            ),
        ];
        let report = resolve_imports(
            tmp.path(),
            &files,
            &[],
            &[import("entry", "@app/auth/session")],
        )
        .unwrap();

        assert_eq!(report.resolutions[0].status, ResolutionStatus::Resolved);
        assert_eq!(
            report.resolutions[0].target_file,
            Some(FileId::new("target"))
        );
    }
}
