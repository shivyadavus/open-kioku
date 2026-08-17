pub mod go;
pub mod java;
pub mod python;
pub mod registry;
pub mod rust;
pub mod semantics;
pub mod typescript;

pub use registry::semantics_for;
pub use semantics::LanguageSemantics;

use open_kioku_core::Language;
use std::path::Path;

pub fn detect_language(path: &Path) -> Language {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if name == "Dockerfile" || name.starts_with("Dockerfile.") {
            return Language::Text;
        }
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "rs" => Language::Rust,
        "java" => Language::Java,
        "ts" | "tsx" => Language::TypeScript,
        "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
        "py" => Language::Python,
        "go" => Language::Go,
        "yaml" | "yml" => Language::Yaml,
        "json" => Language::Json,
        "toml" => Language::Toml,
        "tf" | "tfvars" | "hcl" => Language::Text,
        "sql" => Language::Sql,
        "md" | "mdx" => Language::Markdown,
        "txt" => Language::Text,
        _ => Language::Unknown,
    }
}

pub fn is_supported_code(language: &Language) -> bool {
    matches!(
        language,
        Language::Rust
            | Language::Java
            | Language::TypeScript
            | Language::JavaScript
            | Language::Python
            | Language::Go
            | Language::Yaml
            | Language::Json
            | Language::Toml
            | Language::Sql
            | Language::Markdown
            | Language::Text
    )
}

pub fn likely_test_path(path: &Path) -> bool {
    let value = path.to_string_lossy().to_ascii_lowercase();
    value.contains("/test/")
        || value.contains("/tests/")
        || value.ends_with("_test.rs")
        || value.ends_with("_test.go")
        || value.ends_with("test.java")
        || value.ends_with(".spec.ts")
        || value.ends_with(".test.ts")
        || value.ends_with("_test.py")
        || value.starts_with("tests/")
}

pub fn likely_vendor_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("node_modules/")
        || value.starts_with("target/")
        || value.starts_with("vendor/")
        || value.starts_with(".venv/")
        || value.starts_with("dist/")
        || value.starts_with("build/")
        || value.contains("node_modules/")
        || value.contains("/target/")
        || value.contains("/vendor/")
        || value.contains("/.venv/")
        || value.contains("/dist/")
        || value.contains("/build/")
}

pub fn likely_generated(content: &str) -> bool {
    let head = content
        .lines()
        .take(8)
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    head.contains("@generated")
        || head.contains("code generated")
        || head.contains("automatically generated")
        || head.contains("do not edit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{GraphEdgeType, Language, ReceiverKind, SymbolKind};

    #[test]
    fn registers_all_five_major_languages() {
        let langs = [
            Language::Java,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Rust,
        ];
        for lang in &langs {
            assert!(semantics_for(lang).is_some());
        }
    }

    #[test]
    fn classifies_receivers_correctly() {
        let java_sem = semantics_for(&Language::Java).unwrap();
        assert_eq!(java_sem.classify_receiver("this"), ReceiverKind::Self_);
        assert_eq!(java_sem.classify_receiver("this.repo"), ReceiverKind::Self_);
        assert_eq!(java_sem.classify_receiver("super.repo"), ReceiverKind::Super);
        assert_eq!(java_sem.classify_receiver("Repo"), ReceiverKind::Type);
        assert_eq!(java_sem.classify_receiver("Super"), ReceiverKind::Type);

        let ts_sem = semantics_for(&Language::TypeScript).unwrap();
        assert_eq!(ts_sem.classify_receiver("this.repo"), ReceiverKind::Self_);
        assert_eq!(ts_sem.classify_receiver("Super"), ReceiverKind::Type);

        let js_sem = semantics_for(&Language::JavaScript).unwrap();
        assert_eq!(js_sem.classify_receiver("this.repo"), ReceiverKind::Self_);
        assert_eq!(js_sem.classify_receiver("Super"), ReceiverKind::Type);

        let py_sem = semantics_for(&Language::Python).unwrap();
        assert_eq!(py_sem.classify_receiver("self"), ReceiverKind::Self_);
        assert_eq!(py_sem.classify_receiver("self.repo"), ReceiverKind::Self_);
        assert_eq!(py_sem.classify_receiver("cls"), ReceiverKind::Self_);
        assert_eq!(py_sem.classify_receiver("cls.repo"), ReceiverKind::Self_);
        assert_eq!(py_sem.classify_receiver("super().save"), ReceiverKind::Super);
    }

    #[test]
    fn enforces_relationship_compatibility() {
        let sem = semantics_for(&Language::Rust).unwrap();
        assert!(sem.compatible_relationship(
            SymbolKind::Function,
            SymbolKind::Method,
            GraphEdgeType::Calls
        ));
        assert!(!sem.compatible_relationship(
            SymbolKind::Function,
            SymbolKind::Field,
            GraphEdgeType::Calls
        ));
    }
}
