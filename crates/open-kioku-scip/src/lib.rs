use open_kioku_config::{ScipConfig, ScipMode};
use open_kioku_core::{
    Confidence, EvidenceSourceType, FileId, Language, LineRange, RepositoryId, Symbol, SymbolId,
    SymbolKind, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
use protobuf::{Enum, Message};
use scip::types::{symbol_information, Index, SymbolRole};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScipImport {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<SymbolOccurrence>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScipImportReport {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<SymbolOccurrence>,
    pub imported_paths: Vec<PathBuf>,
    pub skipped_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScipIndexReport {
    pub mode: ScipMode,
    pub imported_paths: Vec<PathBuf>,
    pub skipped_paths: Vec<PathBuf>,
    pub generated_paths: Vec<PathBuf>,
    pub generator_attempts: Vec<ScipGeneratorAttempt>,
    pub symbols: usize,
    pub occurrences: usize,
    pub exact_references: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScipGeneratorAttempt {
    pub language: String,
    pub command: String,
    pub output_path: PathBuf,
    pub status: ScipGeneratorStatus,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScipGeneratorStatus {
    Generated,
    Skipped,
    Failed,
    TimedOut,
}

pub fn prepare_and_import_scip(
    root: impl AsRef<Path>,
    config: &ScipConfig,
    repository_id: &RepositoryId,
) -> Result<(ScipImportReport, ScipIndexReport)> {
    let root = root.as_ref();
    let mut generated_paths = Vec::new();
    let mut generator_attempts = Vec::new();
    if matches!(config.mode, ScipMode::Auto | ScipMode::Required) {
        let generated = generate_configured_scip_files(root, config)?;
        generated_paths = generated
            .iter()
            .filter(|attempt| attempt.status == ScipGeneratorStatus::Generated)
            .map(|attempt| attempt.output_path.clone())
            .collect();
        generator_attempts = generated;
    }

    let imported = if matches!(config.mode, ScipMode::Off) {
        ScipImportReport {
            symbols: Vec::new(),
            occurrences: Vec::new(),
            imported_paths: Vec::new(),
            skipped_paths: config.paths.clone(),
        }
    } else {
        import_configured_scip_files(root, &config.paths, repository_id)?
    };

    if matches!(config.mode, ScipMode::Required) && imported.imported_paths.is_empty() {
        return Err(OkError::Index(
            "SCIP mode is required but no configured SCIP index could be imported".into(),
        ));
    }

    let exact_references = imported
        .occurrences
        .iter()
        .filter(|occurrence| !occurrence.is_definition)
        .count();
    let report = ScipIndexReport {
        mode: config.mode,
        imported_paths: imported.imported_paths.clone(),
        skipped_paths: imported.skipped_paths.clone(),
        generated_paths,
        generator_attempts,
        symbols: imported.symbols.len(),
        occurrences: imported.occurrences.len(),
        exact_references,
    };
    Ok((imported, report))
}

pub fn import_configured_scip_files(
    root: impl AsRef<Path>,
    paths: &[PathBuf],
    repository_id: &RepositoryId,
) -> Result<ScipImportReport> {
    let root = root.as_ref();
    let mut report = ScipImportReport {
        symbols: Vec::new(),
        occurrences: Vec::new(),
        imported_paths: Vec::new(),
        skipped_paths: Vec::new(),
    };

    for relative_path in paths {
        let absolute_path = validated_configured_path(root, relative_path)?;
        if !absolute_path.exists() {
            report.skipped_paths.push(relative_path.clone());
            continue;
        }
        let imported = import_scip_file(&absolute_path, repository_id)?;
        report.symbols.extend(imported.symbols);
        report.occurrences.extend(imported.occurrences);
        report.imported_paths.push(relative_path.clone());
    }
    dedup_import(&mut report.symbols, &mut report.occurrences);
    Ok(report)
}

pub fn generate_configured_scip_files(
    root: impl AsRef<Path>,
    config: &ScipConfig,
) -> Result<Vec<ScipGeneratorAttempt>> {
    let root = root.as_ref();
    fs::create_dir_all(root.join(".ok/indexes"))?;
    let mut attempts = Vec::new();

    if root.join("package.json").exists() {
        let output = PathBuf::from(".ok/indexes/typescript.scip");
        attempts.push(run_installed_indexer(
            root,
            "typescript",
            "scip-typescript",
            typescript_args(root, &output),
            output,
            config.timeout_seconds,
        )?);
    }
    if root.join("go.mod").exists() {
        let output = PathBuf::from("index.scip");
        attempts.push(run_installed_indexer(
            root,
            "go",
            "scip-go",
            Vec::new(),
            output,
            config.timeout_seconds,
        )?);
    }
    if root.join("pom.xml").exists()
        || root.join("build.gradle").exists()
        || root.join("build.gradle.kts").exists()
    {
        let output = PathBuf::from(".ok/indexes/java.scip");
        attempts.push(run_installed_indexer(
            root,
            "java",
            "scip-java",
            vec![
                "index".into(),
                "--output".into(),
                output.to_string_lossy().into_owned(),
            ],
            output,
            config.timeout_seconds,
        )?);
    }
    if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
        attempts.push(ScipGeneratorAttempt {
            language: "python".into(),
            command: "scip-python index . --project-name <repo> --project-version _".into(),
            output_path: PathBuf::from(".ok/indexes/python.scip"),
            status: ScipGeneratorStatus::Skipped,
            message:
                "Python SCIP setup is reported but not auto-run until output handling is verified"
                    .into(),
        });
    }

    Ok(attempts)
}

fn typescript_args(root: &Path, output: &Path) -> Vec<String> {
    let mut args = vec![
        "index".into(),
        "--output".into(),
        output.to_string_lossy().into_owned(),
    ];
    if !root.join("tsconfig.json").exists() && !root.join("jsconfig.json").exists() {
        args.push("--infer-tsconfig".into());
    }
    if root.join("pnpm-workspace.yaml").exists() {
        args.push("--pnpm-workspaces".into());
    } else if root.join("yarn.lock").exists() && root.join("package.json").exists() {
        args.push("--yarn-workspaces".into());
    }
    args
}

fn run_installed_indexer(
    root: &Path,
    language: &str,
    binary: &str,
    args: Vec<String>,
    output_path: PathBuf,
    timeout_seconds: u64,
) -> Result<ScipGeneratorAttempt> {
    let command_text = format!("{} {}", binary, args.join(" "));
    if find_in_path(binary).is_none() {
        return Ok(ScipGeneratorAttempt {
            language: language.into(),
            command: command_text,
            output_path,
            status: ScipGeneratorStatus::Skipped,
            message: format!("{binary} is not installed or not on PATH"),
        });
    }

    let absolute_output = root.join(&output_path);
    if let Some(parent) = absolute_output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut child = Command::new(binary)
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| OkError::Index(format!("failed to run {binary}: {err}")))?;
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| OkError::Index(format!("failed to wait for {binary}: {err}")))?
        {
            let output = child
                .wait_with_output()
                .map_err(|err| OkError::Index(format!("failed to read {binary} output: {err}")))?;
            let combined = summarize_process_output(&output.stdout, &output.stderr);
            if status.success() && absolute_output.exists() {
                return Ok(ScipGeneratorAttempt {
                    language: language.into(),
                    command: command_text,
                    output_path,
                    status: ScipGeneratorStatus::Generated,
                    message: combined.unwrap_or_else(|| "generated SCIP index".into()),
                });
            }
            return Ok(ScipGeneratorAttempt {
                language: language.into(),
                command: command_text,
                output_path,
                status: ScipGeneratorStatus::Failed,
                message: combined.unwrap_or_else(|| format!("{binary} exited with {status}")),
            });
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(ScipGeneratorAttempt {
                language: language.into(),
                command: command_text,
                output_path,
                status: ScipGeneratorStatus::TimedOut,
                message: format!("{binary} timed out after {}s", timeout.as_secs()),
            });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn summarize_process_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    let summary = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect::<String>();
    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

pub fn import_scip_file(
    path: impl AsRef<Path>,
    repository_id: &RepositoryId,
) -> Result<ScipImport> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ScipImport {
            symbols: Vec::new(),
            occurrences: Vec::new(),
        });
    }
    let bytes = fs::read(path)?;
    let index = Index::parse_from_bytes(&bytes).map_err(|err| OkError::Index(err.to_string()))?;
    Ok(convert_index(index, repository_id))
}

fn convert_index(index: Index, repository_id: &RepositoryId) -> ScipImport {
    let mut symbols = Vec::new();
    let mut occurrences = Vec::new();
    for document in index.documents {
        let file_id = FileId::new(stable_id(&document.relative_path));
        let language = language_from_scip(&document.language);
        for info in &document.symbols {
            symbols.push(Symbol {
                id: SymbolId::new(stable_id(&info.symbol)),
                name: display_name(info),
                qualified_name: info.symbol.clone(),
                kind: symbol_kind(
                    info.kind
                        .enum_value_or(symbol_information::Kind::UnspecifiedKind),
                ),
                file_id: file_id.clone(),
                range: definition_range(&document.occurrences, &info.symbol),
                language: language.clone(),
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            });
        }
        for occurrence in document.occurrences {
            if occurrence.symbol.is_empty() {
                continue;
            }
            occurrences.push(SymbolOccurrence {
                symbol_id: SymbolId::new(stable_id(&occurrence.symbol)),
                file_id: file_id.clone(),
                range: scip_range(&occurrence.range),
                is_definition: has_role(occurrence.symbol_roles, SymbolRole::Definition),
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            });
        }
    }
    dedup_import(&mut symbols, &mut occurrences);
    let _ = repository_id;
    ScipImport {
        symbols,
        occurrences,
    }
}

fn dedup_import(symbols: &mut Vec<Symbol>, occurrences: &mut Vec<SymbolOccurrence>) {
    symbols.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    symbols.dedup_by(|a, b| a.id == b.id);
    occurrences.sort_by(|a, b| {
        (
            &a.symbol_id.0,
            &a.file_id.0,
            a.range.as_ref().map(|range| range.start),
            a.is_definition,
        )
            .cmp(&(
                &b.symbol_id.0,
                &b.file_id.0,
                b.range.as_ref().map(|range| range.start),
                b.is_definition,
            ))
    });
    occurrences.dedup_by(|a, b| {
        a.symbol_id == b.symbol_id
            && a.file_id == b.file_id
            && a.range == b.range
            && a.is_definition == b.is_definition
    });
}

fn validated_configured_path(root: &Path, relative_path: &Path) -> Result<PathBuf> {
    if relative_path.is_absolute() {
        return Err(OkError::Index(format!(
            "SCIP index path must be relative to the repository: {}",
            relative_path.display()
        )));
    }
    if relative_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(OkError::Index(format!(
            "SCIP index path may not escape the repository: {}",
            relative_path.display()
        )));
    }
    Ok(root.join(relative_path))
}

fn display_name(info: &scip::types::SymbolInformation) -> String {
    if !info.display_name.is_empty() {
        return info.display_name.clone();
    }
    info.symbol
        .trim_end_matches('.')
        .rsplit(['/', '#', '.', ' '])
        .find(|part| !part.is_empty())
        .unwrap_or(&info.symbol)
        .to_string()
}

fn definition_range(occurrences: &[scip::types::Occurrence], symbol: &str) -> Option<LineRange> {
    occurrences
        .iter()
        .find(|occurrence| {
            occurrence.symbol == symbol && has_role(occurrence.symbol_roles, SymbolRole::Definition)
        })
        .and_then(|occurrence| scip_range(&occurrence.range))
}

fn scip_range(range: &[i32]) -> Option<LineRange> {
    match range {
        [start_line, _, end_line, _] => Some(LineRange {
            start: (*start_line + 1).max(1) as u32,
            end: (*end_line + 1).max(1) as u32,
        }),
        [start_line, _, _] => Some(LineRange::single((*start_line + 1).max(1) as u32)),
        _ => None,
    }
}

fn has_role(roles: i32, role: SymbolRole) -> bool {
    roles & role.value() != 0
}

fn language_from_scip(language: &str) -> Language {
    match language.to_ascii_lowercase().as_str() {
        "rust" => Language::Rust,
        "java" => Language::Java,
        "typescript" | "ts" => Language::TypeScript,
        "javascript" | "js" => Language::JavaScript,
        "python" | "py" => Language::Python,
        "go" => Language::Go,
        "json" => Language::Json,
        "yaml" | "yml" => Language::Yaml,
        "toml" => Language::Toml,
        "sql" => Language::Sql,
        _ => Language::Unknown,
    }
}

fn symbol_kind(kind: symbol_information::Kind) -> SymbolKind {
    match kind {
        symbol_information::Kind::Class
        | symbol_information::Kind::Struct
        | symbol_information::Kind::Enum
        | symbol_information::Kind::Object
        | symbol_information::Kind::Type => SymbolKind::Class,
        symbol_information::Kind::Interface => SymbolKind::Interface,
        symbol_information::Kind::Method
        | symbol_information::Kind::Constructor
        | symbol_information::Kind::StaticMethod
        | symbol_information::Kind::TraitMethod => SymbolKind::Method,
        symbol_information::Kind::Function => SymbolKind::Function,
        symbol_information::Kind::Field
        | symbol_information::Kind::StaticField
        | symbol_information::Kind::Property => SymbolKind::Field,
        symbol_information::Kind::Constant => SymbolKind::Constant,
        symbol_information::Kind::Module => SymbolKind::Module,
        symbol_information::Kind::Package => SymbolKind::Package,
        symbol_information::Kind::Variable
        | symbol_information::Kind::StaticVariable
        | symbol_information::Kind::Value => SymbolKind::Variable,
        symbol_information::Kind::Trait => SymbolKind::Trait,
        symbol_information::Kind::TypeParameter => SymbolKind::Class,
        _ => SymbolKind::Unknown,
    }
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
fn _project_path(root: &Path, relative: &str) -> PathBuf {
    root.join(relative)
}

#[cfg(test)]
mod tests {
    use super::{import_configured_scip_files, import_scip_file};
    use open_kioku_core::RepositoryId;
    use protobuf::Enum;
    use scip::types::{
        symbol_information, Document, Index, Occurrence, SymbolInformation, SymbolRole,
    };
    use std::path::PathBuf;

    #[test]
    fn imports_binary_scip_index() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.scip");
        let symbol_name = "scip rust test src/lib.rs/ retry_import().";

        let mut index = Index::new();
        let mut document = Document::new();
        document.relative_path = "src/lib.rs".into();
        document.language = "rust".into();

        let mut info = SymbolInformation::new();
        info.symbol = symbol_name.into();
        info.display_name = "retry_import".into();
        info.kind = symbol_information::Kind::Function.into();
        document.symbols.push(info);

        let mut definition = Occurrence::new();
        definition.symbol = symbol_name.into();
        definition.range = vec![8, 0, 8, 12];
        definition.symbol_roles = SymbolRole::Definition.value();
        document.occurrences.push(definition);

        let mut reference = Occurrence::new();
        reference.symbol = symbol_name.into();
        reference.range = vec![3, 8, 3, 20];
        document.occurrences.push(reference);

        index.documents.push(document);
        scip::write_message_to_file(&path, index).unwrap();

        let imported = import_scip_file(&path, &RepositoryId::new("repo")).unwrap();
        assert_eq!(imported.symbols.len(), 1);
        assert_eq!(imported.symbols[0].name, "retry_import");
        assert_eq!(imported.occurrences.len(), 2);
        assert!(imported
            .occurrences
            .iter()
            .any(|occurrence| occurrence.is_definition));
        assert!(imported
            .occurrences
            .iter()
            .any(|occurrence| !occurrence.is_definition));
    }

    #[test]
    fn configured_scip_import_skips_missing_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let report = import_configured_scip_files(
            temp.path(),
            &[PathBuf::from(".ok/indexes/rust.scip")],
            &RepositoryId::new("repo"),
        )
        .unwrap();

        assert!(report.symbols.is_empty());
        assert_eq!(
            report.skipped_paths,
            vec![PathBuf::from(".ok/indexes/rust.scip")]
        );
    }

    #[test]
    fn configured_scip_import_rejects_paths_that_escape_repo() {
        let temp = tempfile::tempdir().unwrap();
        let err = import_configured_scip_files(
            temp.path(),
            &[PathBuf::from("../outside.scip")],
            &RepositoryId::new("repo"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("may not escape"));
    }
}
