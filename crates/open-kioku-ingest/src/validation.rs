use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, LineRange, Symbol, TestTarget,
};
use open_kioku_errors::Result;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_VALIDATION_ARTIFACT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_VALIDATION_FACTS: usize = 10_000;
const MAX_TEST_LINKS_PER_FILE: usize = 8;

#[derive(Debug, Clone)]
struct CoverageRecord {
    file_id: FileId,
    file_path: PathBuf,
    lines: Vec<u32>,
    source: String,
    format: &'static str,
}

#[derive(Debug, Clone)]
struct JunitRecord {
    file_id: FileId,
    test_name: String,
    status: &'static str,
    duration_s: Option<f64>,
    message: Option<String>,
    source: String,
    line: Option<u32>,
}

struct ValidationContext<'a> {
    root: &'a Path,
    files_by_path: HashMap<String, &'a File>,
    files: &'a [File],
    symbols_by_file: HashMap<FileId, Vec<&'a Symbol>>,
    tests: &'a [TestTarget],
    test_files: HashMap<FileId, &'a File>,
}

pub fn collect_validation_analysis_facts(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    tests: &[TestTarget],
) -> Result<Vec<AnalysisFact>> {
    let context = ValidationContext::new(root, files, symbols, tests);
    let mut facts = Vec::new();
    let mut seen_artifacts = HashSet::new();
    for artifact in validation_artifacts(root) {
        if !seen_artifacts.insert(normalize_path(&artifact.to_string_lossy())) {
            continue;
        }
        if facts.len() >= MAX_VALIDATION_FACTS {
            break;
        }
        let metadata = match fs::metadata(&artifact) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_file() || metadata.len() > MAX_VALIDATION_ARTIFACT_BYTES {
            continue;
        }
        let content = fs::read_to_string(&artifact)?;
        if is_lcov_artifact(&artifact) {
            for record in parse_lcov(&context, &artifact, &content) {
                facts.extend(coverage_facts(&context, &record));
            }
            continue;
        }
        if is_coverage_json_artifact(&artifact) {
            for record in parse_coverage_json(&context, &artifact, &content) {
                facts.extend(coverage_facts(&context, &record));
            }
            continue;
        }
        if !artifact
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
        {
            continue;
        }
        let Ok(document) = roxmltree::Document::parse(&content) else {
            continue;
        };
        if looks_like_junit(&artifact, &document) {
            for record in parse_junit(&context, &artifact, &document) {
                facts.extend(junit_facts(&record));
            }
        }
        if looks_like_coverage_xml(&artifact, &document) {
            for record in parse_coverage_xml(&context, &artifact, &document) {
                facts.extend(coverage_facts(&context, &record));
            }
        }
    }
    Ok(dedupe_analysis_facts(
        facts.into_iter().take(MAX_VALIDATION_FACTS).collect(),
    ))
}

impl<'a> ValidationContext<'a> {
    fn new(
        root: &'a Path,
        files: &'a [File],
        symbols: &'a [Symbol],
        tests: &'a [TestTarget],
    ) -> Self {
        let files_by_path = files
            .iter()
            .map(|file| (normalize_path(&file.path.to_string_lossy()), file))
            .collect::<HashMap<_, _>>();
        let symbols_by_file =
            symbols
                .iter()
                .fold(HashMap::<FileId, Vec<&Symbol>>::new(), |mut acc, symbol| {
                    acc.entry(symbol.file_id.clone()).or_default().push(symbol);
                    acc
                });
        let test_files = tests
            .iter()
            .filter_map(|test| {
                files
                    .iter()
                    .find(|file| file.id == test.file_id)
                    .map(|file| (test.file_id.clone(), file))
            })
            .collect();
        Self {
            root,
            files_by_path,
            files,
            symbols_by_file,
            tests,
            test_files,
        }
    }

    fn file_for_path(&self, value: &str) -> Option<&'a File> {
        let normalized = normalize_report_path(self.root, value);
        self.files_by_path.get(&normalized).copied().or_else(|| {
            self.files
                .iter()
                .find(|file| path_suffix_matches(&file.path, &normalized))
        })
    }

    fn symbol_for_line(&self, file_id: &FileId, line: u32) -> Option<&'a Symbol> {
        self.symbols_by_file
            .get(file_id)?
            .iter()
            .copied()
            .find(|symbol| {
                symbol
                    .range
                    .as_ref()
                    .is_some_and(|range| line >= range.start && line <= range.end)
            })
    }

    fn test_file_for_record(
        &self,
        file_hint: Option<&str>,
        classname: Option<&str>,
        name: &str,
    ) -> Option<&'a File> {
        if let Some(file_hint) = file_hint {
            if let Some(file) = self.file_for_path(file_hint) {
                return Some(file);
            }
        }
        let classname_path = classname.map(|value| value.replace('.', "/"));
        self.tests.iter().find_map(|test| {
            let file = self.test_files.get(&test.file_id)?;
            let path = normalize_path(&file.path.to_string_lossy()).to_ascii_lowercase();
            let class_match = classname_path
                .as_ref()
                .is_some_and(|class_path| path.contains(&class_path.to_ascii_lowercase()));
            (class_match || test.name == name || test.name.contains(name)).then_some(*file)
        })
    }

    fn linked_tests_for_coverage(&self, covered_path: &Path) -> Vec<&'a File> {
        let covered = normalize_path(&covered_path.to_string_lossy()).to_ascii_lowercase();
        let stem = covered_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut scored = self
            .tests
            .iter()
            .filter_map(|test| {
                let file = self.test_files.get(&test.file_id)?;
                let test_path = normalize_path(&file.path.to_string_lossy()).to_ascii_lowercase();
                let test_name = test.name.to_ascii_lowercase();
                let mut score = 0;
                if !stem.is_empty() && (test_path.contains(&stem) || test_name.contains(&stem)) {
                    score += 3;
                }
                if same_top_level_area(&covered, &test_path) {
                    score += 1;
                }
                if test_path.contains("test") || test_path.contains("spec") {
                    score += 1;
                }
                (score > 0).then_some((score, *file))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.path.cmp(&right.1.path))
        });
        scored
            .into_iter()
            .map(|(_, file)| file)
            .take(MAX_TEST_LINKS_PER_FILE)
            .collect()
    }
}

fn validation_artifacts(root: &Path) -> Vec<PathBuf> {
    let mut artifacts = Vec::new();
    for base in [
        root.join(".ok/analysis"),
        root.join(".ok/coverage"),
        root.join("coverage"),
        root.join("target/site"),
        root.join("build/reports"),
        root.join("reports"),
    ] {
        if !base.is_dir() {
            continue;
        }
        artifacts.extend(
            walkdir::WalkDir::new(base)
                .max_depth(5)
                .into_iter()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().to_path_buf())
                .filter(|path| {
                    is_lcov_artifact(path)
                        || is_coverage_json_artifact(path)
                        || path
                            .extension()
                            .and_then(|value| value.to_str())
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
                }),
        );
    }
    artifacts
}

fn parse_lcov(
    context: &ValidationContext<'_>,
    artifact: &Path,
    content: &str,
) -> Vec<CoverageRecord> {
    let mut records = Vec::new();
    let mut current_file: Option<&File> = None;
    let mut lines = Vec::new();
    for line in content.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            if let Some(file) = current_file.take() {
                push_coverage_record(
                    &mut records,
                    file,
                    std::mem::take(&mut lines),
                    artifact,
                    "lcov",
                );
            }
            current_file = context.file_for_path(path.trim());
        } else if let Some(data) = line.strip_prefix("DA:") {
            let mut parts = data.split(',');
            let line_number = parts.next().and_then(|value| value.parse::<u32>().ok());
            let hits = parts
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            if hits > 0 {
                if let Some(line_number) = line_number {
                    lines.push(line_number);
                }
            }
        } else if line == "end_of_record" {
            if let Some(file) = current_file.take() {
                push_coverage_record(
                    &mut records,
                    file,
                    std::mem::take(&mut lines),
                    artifact,
                    "lcov",
                );
            }
        }
    }
    if let Some(file) = current_file {
        push_coverage_record(&mut records, file, lines, artifact, "lcov");
    }
    records
}

fn parse_coverage_json(
    context: &ValidationContext<'_>,
    artifact: &Path,
    content: &str,
) -> Vec<CoverageRecord> {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return Vec::new();
    };
    let Some(files) = value.get("files").and_then(Value::as_object) else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(|(path, item)| {
            let file = context.file_for_path(path)?;
            let mut lines = item
                .get("executed_lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|line| line.as_u64().and_then(|line| u32::try_from(line).ok()))
                .collect::<Vec<_>>();
            lines.sort_unstable();
            lines.dedup();
            (!lines.is_empty()).then(|| CoverageRecord {
                file_id: file.id.clone(),
                file_path: file.path.clone(),
                lines,
                source: format!("open-kioku-validation:{}", artifact.display()),
                format: "coverage.py-json",
            })
        })
        .collect()
}

fn parse_coverage_xml(
    context: &ValidationContext<'_>,
    artifact: &Path,
    document: &roxmltree::Document<'_>,
) -> Vec<CoverageRecord> {
    let mut by_file = BTreeMap::<String, Vec<u32>>::new();
    for class in document
        .descendants()
        .filter(|node| node.has_tag_name("class"))
    {
        let Some(filename) = class.attribute("filename") else {
            continue;
        };
        for line in class.descendants().filter(|node| node.has_tag_name("line")) {
            let Some(line_number) = xml_u32(&line, &["number", "nr"]) else {
                continue;
            };
            let hits = xml_u32(&line, &["hits", "ci", "covered"]).unwrap_or(0);
            if hits > 0 {
                by_file
                    .entry(filename.to_string())
                    .or_default()
                    .push(line_number);
            }
        }
    }
    for package in document
        .descendants()
        .filter(|node| node.has_tag_name("package"))
    {
        let package_name = package
            .attribute("name")
            .unwrap_or_default()
            .replace('.', "/");
        for sourcefile in package
            .children()
            .filter(|node| node.has_tag_name("sourcefile"))
        {
            let Some(name) = sourcefile.attribute("name") else {
                continue;
            };
            let filename = if package_name.is_empty() {
                name.to_string()
            } else {
                format!("{package_name}/{name}")
            };
            for line in sourcefile
                .children()
                .filter(|node| node.has_tag_name("line"))
            {
                let Some(line_number) = xml_u32(&line, &["nr", "number"]) else {
                    continue;
                };
                let covered = xml_u32(&line, &["ci", "hits", "covered"]).unwrap_or(0);
                if covered > 0 {
                    by_file
                        .entry(filename.clone())
                        .or_default()
                        .push(line_number);
                }
            }
        }
    }
    by_file
        .into_iter()
        .filter_map(|(path, mut lines)| {
            let file = context.file_for_path(&path)?;
            lines.sort_unstable();
            lines.dedup();
            Some(CoverageRecord {
                file_id: file.id.clone(),
                file_path: file.path.clone(),
                lines,
                source: format!("open-kioku-validation:{}", artifact.display()),
                format: coverage_xml_format(artifact, document),
            })
        })
        .collect()
}

fn parse_junit(
    context: &ValidationContext<'_>,
    artifact: &Path,
    document: &roxmltree::Document<'_>,
) -> Vec<JunitRecord> {
    document
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
        .filter_map(|case| {
            let name = case.attribute("name").unwrap_or("unknown_test");
            let classname = case.attribute("classname");
            let file_hint = case.attribute("file");
            let file = context.test_file_for_record(file_hint, classname, name)?;
            let status = if case.children().any(|node| node.has_tag_name("failure")) {
                "failed"
            } else if case.children().any(|node| node.has_tag_name("error")) {
                "error"
            } else if case.children().any(|node| node.has_tag_name("skipped")) {
                "skipped"
            } else {
                "passed"
            };
            let message = case
                .children()
                .find(|node| {
                    node.has_tag_name("failure")
                        || node.has_tag_name("error")
                        || node.has_tag_name("skipped")
                })
                .and_then(|node| node.attribute("message").or_else(|| node.text()))
                .map(compact_message);
            Some(JunitRecord {
                file_id: file.id.clone(),
                test_name: name.to_string(),
                status,
                duration_s: case
                    .attribute("time")
                    .and_then(|value| value.parse::<f64>().ok()),
                message,
                source: format!("open-kioku-validation:{}", artifact.display()),
                line: case
                    .attribute("line")
                    .and_then(|value| value.parse::<u32>().ok()),
            })
        })
        .collect()
}

fn coverage_facts(context: &ValidationContext<'_>, record: &CoverageRecord) -> Vec<AnalysisFact> {
    let Some(first_line) = record.lines.first().copied() else {
        return Vec::new();
    };
    let covered_line_count = record.lines.len();
    let symbol = record
        .lines
        .iter()
        .find_map(|line| context.symbol_for_line(&record.file_id, *line));
    let linked_tests = context.linked_tests_for_coverage(&record.file_path);
    let mut facts = Vec::new();
    for test_file in linked_tests {
        facts.push(AnalysisFact {
            id: identity::stable_hash(&format!(
                "validation:{}:{}:{}:{}",
                record.format,
                record.file_path.display(),
                test_file.path.display(),
                first_line
            )),
            file_id: record.file_id.clone(),
            symbol_id: symbol.map(|symbol| symbol.id.clone()),
            target: normalize_path(&test_file.path.to_string_lossy()),
            target_kind: GraphNodeType::Test,
            edge_type: GraphEdgeType::TestCovers,
            range: Some(LineRange::single(first_line)),
            confidence: if symbol.is_some() {
                Confidence::High
            } else {
                Confidence::Medium
            },
            source: record.source.clone(),
            source_type: EvidenceSourceType::ExternalIntegration,
            message: format!(
                "{} coverage maps {} covered line(s) to validation target `{}`{}",
                record.format,
                covered_line_count,
                test_file.path.display(),
                symbol
                    .map(|symbol| format!(" for symbol `{}`", symbol.qualified_name))
                    .unwrap_or_default()
            ),
        });
    }
    facts
}

fn junit_facts(record: &JunitRecord) -> Vec<AnalysisFact> {
    let mut message = format!(
        "JUnit observed test `{}` status {}",
        record.test_name, record.status
    );
    if let Some(duration) = record.duration_s {
        message.push_str(&format!(", duration_s {:.3}", duration));
    }
    if let Some(detail) = &record.message {
        message.push_str(&format!(", message {detail}"));
    }
    vec![AnalysisFact {
        id: identity::stable_hash(&format!(
            "validation:junit:{}:{}:{}",
            record.file_id.0, record.test_name, record.status
        )),
        file_id: record.file_id.clone(),
        symbol_id: None,
        target: record.test_name.clone(),
        target_kind: GraphNodeType::Test,
        edge_type: GraphEdgeType::Validates,
        range: record.line.map(LineRange::single),
        confidence: if record.status == "passed" {
            Confidence::Medium
        } else {
            Confidence::High
        },
        source: record.source.clone(),
        source_type: EvidenceSourceType::ExternalIntegration,
        message,
    }]
}

fn push_coverage_record(
    records: &mut Vec<CoverageRecord>,
    file: &File,
    mut lines: Vec<u32>,
    artifact: &Path,
    format: &'static str,
) {
    lines.sort_unstable();
    lines.dedup();
    if lines.is_empty() {
        return;
    }
    records.push(CoverageRecord {
        file_id: file.id.clone(),
        file_path: file.path.clone(),
        lines,
        source: format!("open-kioku-validation:{}", artifact.display()),
        format,
    });
}

fn looks_like_junit(path: &Path, document: &roxmltree::Document<'_>) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    file_name.starts_with("test-")
        || file_name.contains("junit")
        || document
            .descendants()
            .any(|node| node.has_tag_name("testcase"))
}

fn looks_like_coverage_xml(path: &Path, document: &roxmltree::Document<'_>) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    file_name.contains("coverage")
        || file_name.contains("cobertura")
        || file_name.contains("jacoco")
        || document.descendants().any(|node| {
            node.has_tag_name("coverage")
                || node.has_tag_name("sourcefile")
                || node.attribute("filename").is_some()
        })
}

fn coverage_xml_format(path: &Path, document: &roxmltree::Document<'_>) -> &'static str {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if file_name.contains("jacoco")
        || document
            .descendants()
            .any(|node| node.has_tag_name("sourcefile"))
    {
        "jacoco"
    } else if file_name.contains("cobertura") {
        "cobertura"
    } else {
        "coverage.py-xml"
    }
}

fn is_lcov_artifact(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    file_name == "lcov.info" || file_name.ends_with(".lcov")
}

fn is_coverage_json_artifact(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    file_name.ends_with(".json") && file_name.contains("coverage")
}

fn xml_u32(node: &roxmltree::Node<'_, '_>, names: &[&str]) -> Option<u32> {
    names.iter().find_map(|name| {
        node.attribute(*name)
            .and_then(|value| value.parse::<u32>().ok())
    })
}

fn normalize_report_path(root: &Path, value: &str) -> String {
    let path = Path::new(value);
    let rel = if path.is_absolute() {
        path.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    normalize_path(&rel.to_string_lossy())
}

fn normalize_path(value: &str) -> String {
    value.trim_start_matches("./").replace('\\', "/")
}

fn path_suffix_matches(path: &Path, normalized: &str) -> bool {
    let candidate = normalize_path(&path.to_string_lossy()).to_ascii_lowercase();
    let normalized = normalized.to_ascii_lowercase();
    candidate == normalized
        || candidate.ends_with(&format!("/{normalized}"))
        || normalized.ends_with(&candidate)
}

fn same_top_level_area(left: &str, right: &str) -> bool {
    left.split('/').next().filter(|value| !value.is_empty())
        == right.split('/').next().filter(|value| !value.is_empty())
}

fn compact_message(message: &str) -> String {
    message.trim().chars().take(160).collect()
}

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{Language, RepositoryId, SymbolId, SymbolKind};

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 10,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(file: &File, name: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("sym:{name}")),
            name: name.into(),
            qualified_name: format!("src::{name}"),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange { start, end }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn test_target(name: &str, file: &File) -> TestTarget {
        TestTarget {
            id: format!("test:{name}"),
            name: name.into(),
            file_id: file.id.clone(),
            range: Some(LineRange::single(4)),
            command: Some("cargo test".into()),
            confidence: Confidence::High,
            reason: "test fixture".into(),
            evidence_refs: vec![format!("test:{name}")],
            score_breakdown: Vec::new(),
        }
    }

    #[test]
    fn parses_lcov_and_maps_lines_to_symbols_and_tests() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".ok/coverage")).unwrap();
        fs::write(
            root.join(".ok/coverage/lcov.info"),
            "SF:src/auth.rs\nDA:3,1\nDA:8,0\nend_of_record\n",
        )
        .unwrap();
        let source = file("source", "src/auth.rs");
        let test_file = file("test", "tests/auth_test.rs");
        let facts = collect_validation_analysis_facts(
            root,
            &[source.clone(), test_file.clone()],
            &[symbol(&source, "issue_token", 1, 5)],
            &[test_target("auth_test", &test_file)],
        )
        .unwrap();

        let fact = facts
            .iter()
            .find(|fact| fact.edge_type == GraphEdgeType::TestCovers)
            .unwrap();
        assert_eq!(fact.file_id, source.id);
        assert_eq!(
            fact.symbol_id.as_ref().map(|id| id.0.as_str()),
            Some("sym:issue_token")
        );
        assert_eq!(fact.target, "tests/auth_test.rs");
        assert!(fact.message.contains("lcov coverage"));
    }

    #[test]
    fn parses_junit_failures_as_validation_facts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".ok/analysis")).unwrap();
        fs::write(
            root.join(".ok/analysis/TEST-auth.xml"),
            r#"<testsuite><testcase classname="tests.auth_test" name="auth_rejects_expired" time="0.2"><failure message="expired token"/></testcase></testsuite>"#,
        )
        .unwrap();
        let test_file = file("test", "tests/auth_test.rs");
        let facts = collect_validation_analysis_facts(
            root,
            std::slice::from_ref(&test_file),
            &[],
            &[test_target("auth_rejects_expired", &test_file)],
        )
        .unwrap();

        let fact = facts
            .iter()
            .find(|fact| fact.edge_type == GraphEdgeType::Validates)
            .unwrap();
        assert_eq!(fact.file_id, test_file.id);
        assert!(fact.message.contains("status failed"));
        assert!(fact.message.contains("expired token"));
    }

    #[test]
    fn parses_cobertura_jacoco_and_coverage_json() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("coverage")).unwrap();
        fs::write(
            root.join("coverage/cobertura.xml"),
            r#"<coverage><packages><package><classes><class filename="src/auth.rs"><lines><line number="3" hits="1"/></lines></class></classes></package></packages></coverage>"#,
        )
        .unwrap();
        fs::write(
            root.join("coverage/jacoco.xml"),
            r#"<report><package name="src"><sourcefile name="auth.rs"><line nr="4" ci="1" mi="0"/></sourcefile></package></report>"#,
        )
        .unwrap();
        fs::write(
            root.join("coverage/coverage.json"),
            r#"{"files":{"src/auth.rs":{"executed_lines":[5]}}}"#,
        )
        .unwrap();
        let source = file("source", "src/auth.rs");
        let test_file = file("test", "tests/auth_test.rs");
        let facts = collect_validation_analysis_facts(
            root,
            &[source, test_file.clone()],
            &[],
            &[test_target("auth_test", &test_file)],
        )
        .unwrap();

        assert!(facts
            .iter()
            .any(|fact| fact.message.contains("cobertura coverage")));
        assert!(facts
            .iter()
            .any(|fact| fact.message.contains("jacoco coverage")));
        assert!(facts
            .iter()
            .any(|fact| fact.message.contains("coverage.py-json coverage")));
    }
}
