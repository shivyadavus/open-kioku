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
