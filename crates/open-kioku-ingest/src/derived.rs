//! Derived-file facts: files that are siblings of one edit.
//!
//! A generated module and the modular file its header names, a test and the module it
//! exercises, a `.d.ts` and the implementation it describes — a change to one is usually a
//! change to the other, but nothing in the import graph says so (the generated file does not
//! import its origin; the test imports far more than its subject). The pairing is emitted as a
//! `DERIVED_FROM` fact from the derived file to its origin, and retrieval treats the two as one
//! edit.
//!
//! Two provenances, deliberately not one: a header that names its origin is repository truth
//! and is emitted with high confidence and a declared-origin proof; a naming convention is a
//! guess that is right most of the time and is emitted with medium confidence and no proof, so
//! it can only ever corroborate. A convention that could mean two files emits nothing.

use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, GraphEdgeType, GraphNodeType,
    Language, LineRange,
};
use open_kioku_languages::generated_origin;
use std::collections::HashMap;
use std::fs::File as FsFile;
use std::io::Read;
use std::path::Path;

/// Source label of a declared-origin fact; the graph builder attaches the proof by it.
pub const DECLARED_ORIGIN_SOURCE: &str = open_kioku_core::DERIVED_FILE_DECLARED_ORIGIN_SOURCE;
/// Source label of a test paired with its subject by file-name convention.
pub const TEST_PAIRING_SOURCE: &str = "open-kioku-derived/test-pairing";
/// Source label of a declaration file paired with its implementation by extension.
pub const DECLARATION_PAIRING_SOURCE: &str = "open-kioku-derived/declaration-pairing";

/// Bound on emitted facts so a repository with one test per source file cannot make the
/// derived pass the largest fact family.
const MAX_DERIVED_FACTS: usize = 50_000;
/// A generation banner sits in the first lines; reading more of a large generated file is
/// wasted I/O on the index's hot path.
const HEADER_READ_BYTES: usize = 4096;

/// A test source set's segment in a Java-style layout (`src/test/java`, `src/javaRestTest/java`):
/// the package path after it is the same as the subject's under `src/main/java`.
fn is_java_test_source_set(segment: &str) -> bool {
    segment == "test" || (segment.ends_with("Test") && segment != "Test")
}

struct DerivedPair<'a> {
    derived: &'a File,
    origin: &'a File,
    source: &'static str,
    confidence: Confidence,
    source_type: EvidenceSourceType,
    range: Option<LineRange>,
    message: String,
}

struct FileIndex<'a> {
    by_path: HashMap<String, &'a File>,
    by_stem: HashMap<String, Vec<&'a File>>,
    by_name: HashMap<String, Vec<&'a File>>,
}

impl<'a> FileIndex<'a> {
    fn new(files: &'a [File]) -> Self {
        let mut by_path = HashMap::with_capacity(files.len());
        let mut by_stem: HashMap<String, Vec<&File>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<&File>> = HashMap::new();
        for file in files {
            let path = normalize(&file.path.to_string_lossy());
            let (_, name) = split_dir(&path);
            by_name.entry(name.to_string()).or_default().push(file);
            by_stem
                .entry(stem(name).to_string())
                .or_default()
                .push(file);
            by_path.insert(path, file);
        }
        Self {
            by_path,
            by_stem,
            by_name,
        }
    }

    fn get(&self, path: &str) -> Option<&'a File> {
        self.by_path.get(&normalize(path)).copied()
    }

    fn in_dir(&self, dir: &str, name: &str) -> Option<&'a File> {
        self.get(&join(dir, name))
    }
}

/// Build one `DERIVED_FROM` fact per derived file whose origin is known: from the file's own
/// generation banner when it names a path that resolves to exactly one indexed file, or from
/// a test or declaration naming convention when exactly one file fits it.
pub fn collect_derived_file_facts(root: &Path, files: &[File]) -> Vec<AnalysisFact> {
    let index = FileIndex::new(files);
    let mut facts = Vec::new();
    for file in files {
        if facts.len() >= MAX_DERIVED_FACTS {
            break;
        }
        let pair = declared_origin_pair(root, file, &index)
            .or_else(|| declaration_pair(file, &index))
            .or_else(|| test_pair(file, &index));
        if let Some(pair) = pair {
            facts.push(fact_for(pair));
        }
    }
    facts
}

fn fact_for(pair: DerivedPair<'_>) -> AnalysisFact {
    let derived_path = normalize(&pair.derived.path.to_string_lossy());
    let origin_path = normalize(&pair.origin.path.to_string_lossy());
    AnalysisFact {
        id: identity::stable_hash(&format!(
            "derived-file:{derived_path}:{origin_path}:{}",
            pair.source
        )),
        file_id: pair.derived.id.clone(),
        symbol_id: None,
        target: origin_path,
        target_kind: GraphNodeType::File,
        edge_type: GraphEdgeType::DerivedFrom,
        range: pair.range,
        confidence: pair.confidence,
        source: pair.source.into(),
        source_type: pair.source_type,
        message: pair.message.into(),
    }
}

/// A generated file whose banner names its origin. The path is tried as written from the
/// repository root, then relative to the generated file, then as a bare file name that occurs
/// once in the index; a name that occurs twice is not a fact and yields nothing.
fn declared_origin_pair<'a>(
    root: &Path,
    file: &'a File,
    index: &FileIndex<'a>,
) -> Option<DerivedPair<'a>> {
    if !file.is_generated {
        return None;
    }
    let origin = generated_origin(&read_header(&root.join(&file.path))?)?;
    let derived_path = normalize(&file.path.to_string_lossy());
    let (dir, _) = split_dir(&derived_path);
    let target = index
        .get(&origin.path)
        .or_else(|| index.get(&join(dir, &origin.path)))
        .or_else(|| {
            let name = origin.path.rsplit('/').next()?;
            match index.by_name.get(name).map(Vec::as_slice) {
                Some([only]) => Some(*only),
                _ => None,
            }
        })?;
    if target.id == file.id {
        return None;
    }
    Some(DerivedPair {
        derived: file,
        origin: target,
        source: DECLARED_ORIGIN_SOURCE,
        confidence: Confidence::High,
        source_type: EvidenceSourceType::StaticAnalysis,
        range: Some(LineRange {
            start: origin.line,
            end: origin.line,
        }),
        message: format!(
            "header declares `{}` as the file it was generated from",
            origin.path
        ),
    })
}

fn read_header(path: &Path) -> Option<String> {
    let mut handle = FsFile::open(path).ok()?;
    let mut buffer = vec![0u8; HEADER_READ_BYTES];
    let mut filled = 0usize;
    while filled < buffer.len() {
        match handle.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    buffer.truncate(filled);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// `foo.d.ts` describes the `foo.ts` (or `.js`) beside it.
fn declaration_pair<'a>(file: &'a File, index: &FileIndex<'a>) -> Option<DerivedPair<'a>> {
    let path = normalize(&file.path.to_string_lossy());
    let (dir, name) = split_dir(&path);
    let base = name.strip_suffix(".d.ts")?;
    let origin = ["ts", "tsx", "mts", "js", "mjs"]
        .iter()
        .find_map(|ext| index.in_dir(dir, &format!("{base}.{ext}")))?;
    Some(DerivedPair {
        derived: file,
        origin,
        source: DECLARATION_PAIRING_SOURCE,
        confidence: Confidence::Medium,
        source_type: EvidenceSourceType::Heuristic,
        range: None,
        message: format!(
            "declaration file paired with `{}` by extension",
            normalize(&origin.path.to_string_lossy())
        ),
    })
}

/// A test file paired with the module it is named after. Same directory wins outright; a
/// mirrored tree (`tests/pkg/test_x.py` ↔ `src/pkg/x.py`, `src/test/java/p/XTest.java` ↔
/// `src/main/java/p/X.java`) pairs when exactly one non-test file fits, preferring the one
/// under the same package or parent directory name. Two fits are no pairing.
fn test_pair<'a>(file: &'a File, index: &FileIndex<'a>) -> Option<DerivedPair<'a>> {
    let path = normalize(&file.path.to_string_lossy());
    let (dir, name) = split_dir(&path);
    let (subject_stem, extensions): (String, &[&str]) = match file.language {
        Language::Go => (name.strip_suffix("_test.go")?.to_string(), &["go"]),
        Language::TypeScript | Language::JavaScript => {
            let stem = stem(name);
            let subject = stem
                .strip_suffix("_test")
                .or_else(|| stem.strip_suffix(".test"))
                .or_else(|| stem.strip_suffix(".spec"))?;
            (
                subject.to_string(),
                &["ts", "tsx", "mts", "js", "jsx", "mjs"],
            )
        }
        Language::Python => {
            let stem = stem(name);
            let subject = stem
                .strip_prefix("test_")
                .or_else(|| stem.strip_suffix("_test"))?;
            (subject.to_string(), &["py"])
        }
        Language::Java => {
            let stem = stem(name);
            let subject = stem
                .strip_suffix("Tests")
                .or_else(|| stem.strip_suffix("Test"))
                .or_else(|| stem.strip_suffix("IT"))?;
            (subject.to_string(), &["java"])
        }
        _ => return None,
    };
    if subject_stem.is_empty() {
        return None;
    }
    let find_in = |dir: &str| {
        extensions
            .iter()
            .find_map(|ext| index.in_dir(dir, &format!("{subject_stem}.{ext}")))
    };
    // Same directory, then the parent when the test sits in its own `__tests__`/`tests` folder,
    // then a mirrored tree.
    let origin = find_in(dir)
        .or_else(|| {
            let (parent, leaf) = split_dir(dir);
            is_test_dir(leaf).then(|| find_in(parent)).flatten()
        })
        .or_else(|| unique_mirrored_subject(&path, &subject_stem, extensions, index))?;
    if origin.id == file.id || open_kioku_core::is_test_path(&origin.path.to_string_lossy()) {
        return None;
    }
    Some(DerivedPair {
        derived: file,
        origin,
        source: TEST_PAIRING_SOURCE,
        confidence: Confidence::Medium,
        source_type: EvidenceSourceType::Heuristic,
        range: None,
        message: format!(
            "test file paired with `{}` by naming convention",
            normalize(&origin.path.to_string_lossy())
        ),
    })
}

fn unique_mirrored_subject<'a>(
    test_path: &str,
    subject_stem: &str,
    extensions: &[&str],
    index: &FileIndex<'a>,
) -> Option<&'a File> {
    let candidates = index
        .by_stem
        .get(subject_stem)?
        .iter()
        .copied()
        .filter(|candidate| {
            let candidate_path = normalize(&candidate.path.to_string_lossy());
            let (_, name) = split_dir(&candidate_path);
            extensions
                .iter()
                .any(|ext| name.ends_with(&format!(".{ext}")))
                && !open_kioku_core::is_test_path(&candidate_path)
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => None,
        [only] => Some(*only),
        many => {
            let test_package = package_path(test_path);
            let (test_parent, _) = split_dir(test_path);
            let test_parent_name = test_parent.rsplit('/').next().unwrap_or(test_parent);
            let same_package = many
                .iter()
                .copied()
                .filter(|candidate| {
                    package_path(&normalize(&candidate.path.to_string_lossy())) == test_package
                })
                .collect::<Vec<_>>();
            if let [only] = same_package.as_slice() {
                return Some(*only);
            }
            let same_parent = many
                .iter()
                .copied()
                .filter(|candidate| {
                    let candidate_path = normalize(&candidate.path.to_string_lossy());
                    let (parent, _) = split_dir(&candidate_path);
                    parent.rsplit('/').next().unwrap_or(parent) == test_parent_name
                })
                .collect::<Vec<_>>();
            match same_parent.as_slice() {
                [only] => Some(*only),
                _ => None,
            }
        }
    }
}

/// The directory path after a source-set root (`src/<set>/java/`, `src/`, `tests/`), so a
/// test and its subject compare equal when they mirror each other's package layout.
fn package_path(path: &str) -> String {
    let (dir, _) = split_dir(path);
    let segments = dir.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>();
    let mut start = 0usize;
    for (i, segment) in segments.iter().enumerate() {
        if *segment == "src" {
            start = i + 1;
            if segments
                .get(i + 1)
                .is_some_and(|set| *set == "main" || is_java_test_source_set(set))
            {
                start = i + 2;
                if segments
                    .get(i + 2)
                    .is_some_and(|lang| matches!(*lang, "java" | "kotlin" | "scala" | "groovy"))
                {
                    start = i + 3;
                }
            }
            break;
        }
        if matches!(*segment, "tests" | "test") {
            start = i + 1;
            break;
        }
    }
    segments[start.min(segments.len())..].join("/")
}

/// `is_test_path` classifies whole paths; a lone directory name is classified as the parent of
/// a placeholder file.
fn is_test_dir(segment: &str) -> bool {
    !segment.is_empty() && open_kioku_core::is_test_path(&format!("{segment}/x"))
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn split_dir(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        return normalize(name);
    }
    // A header may name its origin relative to the generated file with `../`.
    let mut segments = dir.split('/').collect::<Vec<_>>();
    let name = normalize(name);
    for segment in name.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{FileId, RepositoryId};
    use std::fs;
    use std::path::PathBuf;

    fn file(path: &str, language: Language, is_generated: bool) -> File {
        File {
            id: FileId::new(path),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language,
            size_bytes: 1,
            content_hash: path.into(),
            is_generated,
            is_vendor: false,
        }
    }

    fn pairs(root: &Path, files: &[File]) -> Vec<(String, String, &'static str)> {
        let by_id = files
            .iter()
            .map(|f| (f.id.clone(), f))
            .collect::<HashMap<_, _>>();
        collect_derived_file_facts(root, files)
            .into_iter()
            .map(|fact| {
                assert_eq!(fact.edge_type, GraphEdgeType::DerivedFrom);
                assert_eq!(fact.target_kind, GraphNodeType::File);
                let source = match fact.source.as_str() {
                    DECLARED_ORIGIN_SOURCE => {
                        assert_eq!(fact.confidence, Confidence::High);
                        assert_eq!(fact.source_type, EvidenceSourceType::StaticAnalysis);
                        assert!(fact.range.is_some(), "a declaration has a line");
                        "declared"
                    }
                    TEST_PAIRING_SOURCE => {
                        assert_eq!(fact.confidence, Confidence::Medium);
                        assert_eq!(fact.source_type, EvidenceSourceType::Heuristic);
                        "test"
                    }
                    DECLARATION_PAIRING_SOURCE => "declaration",
                    other => panic!("unexpected source {other}"),
                };
                (
                    by_id[&fact.file_id].path.to_string_lossy().into_owned(),
                    fact.target,
                    source,
                )
            })
            .collect()
    }

    #[test]
    fn a_generation_banner_pairs_the_file_with_the_origin_it_names() {
        let root = tempfile::tempdir().unwrap();
        let generated = "src/models/orbit/modeling_orbit.py";
        fs::create_dir_all(root.path().join("src/models/orbit")).unwrap();
        fs::write(
            root.path().join(generated),
            "# 🚨🚨\n#  This file was automatically generated from src/models/orbit/modular_orbit.py.\n#  Do NOT edit.\nclass Orbit: ...\n",
        )
        .unwrap();
        let files = vec![
            file(generated, Language::Python, true),
            file("src/models/orbit/modular_orbit.py", Language::Python, false),
        ];
        assert_eq!(
            pairs(root.path(), &files),
            vec![(
                generated.to_string(),
                "src/models/orbit/modular_orbit.py".to_string(),
                "declared"
            )]
        );
    }

    #[test]
    fn a_banner_origin_resolves_relative_to_the_generated_file_or_by_unique_name() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("pkg/gen")).unwrap();
        fs::create_dir_all(root.path().join("pkg/schema")).unwrap();
        fs::write(
            root.path().join("pkg/gen/client.ts"),
            "// Code generated by gen-client from ../schema/client.schema.ts. DO NOT EDIT.\n",
        )
        .unwrap();
        fs::write(
            root.path().join("pkg/gen/types.ts"),
            "// Code generated by gen-types from types.source.ts; DO NOT EDIT.\n",
        )
        .unwrap();
        fs::write(
            root.path().join("pkg/gen/dup.ts"),
            "// Code generated from dup.source.ts; DO NOT EDIT.\n",
        )
        .unwrap();
        let files = vec![
            file("pkg/gen/client.ts", Language::TypeScript, true),
            file("pkg/schema/client.schema.ts", Language::TypeScript, false),
            file("pkg/gen/types.ts", Language::TypeScript, true),
            file("pkg/schema/types.source.ts", Language::TypeScript, false),
            file("pkg/gen/dup.ts", Language::TypeScript, true),
            file("pkg/a/dup.source.ts", Language::TypeScript, false),
            file("pkg/b/dup.source.ts", Language::TypeScript, false),
        ];
        let found = pairs(root.path(), &files);
        assert!(found.contains(&(
            "pkg/gen/client.ts".into(),
            "pkg/schema/client.schema.ts".into(),
            "declared"
        )));
        assert!(found.contains(&(
            "pkg/gen/types.ts".into(),
            "pkg/schema/types.source.ts".into(),
            "declared"
        )));
        // Two files carry the declared name: not a fact, so no edge.
        assert!(!found
            .iter()
            .any(|(derived, _, _)| derived == "pkg/gen/dup.ts"));
    }

    #[test]
    fn a_generated_file_without_a_banner_origin_is_not_paired_by_guesswork() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("api.pb.go"),
            "// Code generated by protoc-gen-go. DO NOT EDIT.\n",
        )
        .unwrap();
        let files = vec![
            file("api.pb.go", Language::Go, true),
            file("api.go", Language::Go, false),
        ];
        assert!(pairs(root.path(), &files).is_empty());
    }

    #[test]
    fn tests_pair_with_their_subject_by_language_convention() {
        let root = tempfile::tempdir().unwrap();
        let files = vec![
            file("pkg/router.go", Language::Go, false),
            file("pkg/router_test.go", Language::Go, false),
            file("path/posix/join.ts", Language::TypeScript, false),
            file("path/posix/join_test.ts", Language::TypeScript, false),
            file("ui/button.tsx", Language::TypeScript, false),
            file("ui/__tests__/button.test.tsx", Language::TypeScript, false),
            file("lib/parse.js", Language::JavaScript, false),
            file("lib/parse.spec.js", Language::JavaScript, false),
            file("src/orbit/models/loader.py", Language::Python, false),
            file("tests/orbit/models/test_loader.py", Language::Python, false),
            file(
                "src/main/java/com/acme/geo/GeoResolver.java",
                Language::Java,
                false,
            ),
            file(
                "src/test/java/com/acme/geo/GeoResolverTests.java",
                Language::Java,
                false,
            ),
            file(
                "src/javaRestTest/java/com/acme/geo/GeoResolverIT.java",
                Language::Java,
                false,
            ),
        ];
        let found = pairs(root.path(), &files);
        let expected = [
            ("pkg/router_test.go", "pkg/router.go"),
            ("path/posix/join_test.ts", "path/posix/join.ts"),
            ("ui/__tests__/button.test.tsx", "ui/button.tsx"),
            ("lib/parse.spec.js", "lib/parse.js"),
            (
                "tests/orbit/models/test_loader.py",
                "src/orbit/models/loader.py",
            ),
            (
                "src/test/java/com/acme/geo/GeoResolverTests.java",
                "src/main/java/com/acme/geo/GeoResolver.java",
            ),
            (
                "src/javaRestTest/java/com/acme/geo/GeoResolverIT.java",
                "src/main/java/com/acme/geo/GeoResolver.java",
            ),
        ];
        for (derived, origin) in expected {
            assert!(
                found.contains(&(derived.into(), origin.into(), "test")),
                "{derived} -> {origin} missing from {found:?}"
            );
        }
        assert_eq!(found.len(), expected.len(), "{found:?}");
    }

    #[test]
    fn an_ambiguous_convention_pairs_nothing_and_a_subject_is_never_a_test() {
        let root = tempfile::tempdir().unwrap();
        let files = vec![
            // `loader.py` exists in two packages and the test mirrors neither: no edge.
            file("src/a/loader.py", Language::Python, false),
            file("src/b/loader.py", Language::Python, false),
            file("tests/test_loader.py", Language::Python, false),
            // Same-name mirror disambiguates: `tests/b/` picks `src/b/`.
            file("tests/b/test_loader.py", Language::Python, false),
            // A test named after another test is not a pairing.
            file("tests/test_helpers.py", Language::Python, false),
            file("tests/helpers_test.py", Language::Python, false),
        ];
        assert_eq!(
            pairs(root.path(), &files),
            vec![(
                "tests/b/test_loader.py".into(),
                "src/b/loader.py".into(),
                "test"
            )]
        );
    }

    #[test]
    fn a_declaration_file_pairs_with_the_implementation_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let files = vec![
            file("lib/index.d.ts", Language::TypeScript, false),
            file("lib/index.js", Language::JavaScript, false),
            file("lib/lonely.d.ts", Language::TypeScript, false),
        ];
        assert_eq!(
            pairs(root.path(), &files),
            vec![(
                "lib/index.d.ts".into(),
                "lib/index.js".into(),
                "declaration"
            )]
        );
    }

    #[test]
    fn fact_identity_is_stable_across_runs() {
        let root = tempfile::tempdir().unwrap();
        let files = vec![
            file("pkg/router.go", Language::Go, false),
            file("pkg/router_test.go", Language::Go, false),
        ];
        let first = collect_derived_file_facts(root.path(), &files);
        let second = collect_derived_file_facts(root.path(), &files);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, second[0].id);
    }
}
