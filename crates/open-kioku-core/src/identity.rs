//! Stable graph identity helpers.
//!
//! New graph facts should derive node and edge IDs here instead of formatting
//! IDs inline. File paths are repository-relative, use `/` separators, preserve
//! case, and reject absolute or escaping paths. User-controlled payloads are
//! escaped before entering reserved namespaces (`file:`, `symbol:`, `route:`,
//! `config:`, `test:`, `runtime:`, and `architecture:`).
//!
//! Qualified names keep language entrypoint conventions stable: Python
//! `__init__.py`, JavaScript/TypeScript `index.*`, and Rust `mod.rs` resolve to
//! their parent module; Java prefers declared packages; Go prefers declared
//! package names. Nested/default/anonymous symbols should pass their represented
//! symbol name through `qualified_name` so the same module prefixing rules apply.

use crate::{EdgeId, GraphEdgeType, GraphNodeType, Language, NodeId, Symbol, TestTarget};
use open_kioku_errors::{OkError, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

const RESERVED_NAMESPACES: &[&str] = &[
    "file:",
    "symbol:",
    "route:",
    "config:",
    "test:",
    "runtime:",
    "architecture:",
];

pub fn file_node_id(path: &Path) -> NodeId {
    try_file_node_id(path).expect("file node identity requires a relative repository path")
}

pub fn try_file_node_id(path: &Path) -> Result<NodeId> {
    Ok(NodeId::new(format!("file:{}", normalize_repo_path(path)?)))
}

pub fn symbol_node_id(symbol: &Symbol) -> NodeId {
    NodeId::new(format!(
        "symbol:{}",
        escape_identity_component(&symbol.id.0)
    ))
}

pub fn route_node_id(protocol: &str, method: Option<&str>, path: &str) -> NodeId {
    let protocol = protocol.trim().to_ascii_lowercase();
    let method = method
        .map(|value| value.trim().to_ascii_uppercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "*".into());
    let path = normalize_route_path(path);
    NodeId::new(format!(
        "route:{}:{}:{}",
        escape_identity_component(&protocol),
        escape_identity_component(&method),
        escape_identity_component(&path)
    ))
}

pub fn config_node_id(key_path: &str) -> NodeId {
    NodeId::new(format!(
        "config:{}",
        escape_identity_component(key_path.trim())
    ))
}

pub fn test_node_id(test: &TestTarget) -> NodeId {
    NodeId::new(format!("test:{}", escape_identity_component(&test.id)))
}

pub fn edge_id(kind: GraphEdgeType, from: &NodeId, to: &NodeId, salt: Option<&str>) -> EdgeId {
    let mut value = format!("{kind:?}:{}:{}", from.0, to.0);
    if let Some(salt) = salt {
        value.push(':');
        value.push_str(salt);
    }
    EdgeId::new(format!("edge:{}", stable_hash(&value)))
}

pub fn normalize_repo_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(OkError::Index(format!(
            "repository path must be relative: {}",
            path.display()
        )));
    }
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(OkError::Index(format!(
                    "repository path escapes the repository: {}",
                    path.display()
                )));
            }
            value => parts.push(value.to_string()),
        }
    }
    if parts.is_empty() {
        return Err(OkError::Index("repository path cannot be empty".into()));
    }
    Ok(parts.join("/"))
}

pub fn legacy_analysis_node_id(node_type: GraphNodeType, label: &str) -> NodeId {
    NodeId::new(format!(
        "analysis:{node_type:?}:{}",
        stable_hash(&label.to_ascii_lowercase())
    ))
}

pub fn runtime_node_id(label: &str) -> NodeId {
    NodeId::new(format!(
        "runtime:{}",
        escape_identity_component(label.trim())
    ))
}

pub fn architecture_node_id(label: &str) -> NodeId {
    NodeId::new(format!(
        "architecture:{}",
        escape_identity_component(label.trim())
    ))
}

pub fn qualified_name(
    path: &Path,
    language: &Language,
    content: Option<&str>,
    name: &str,
) -> Result<String> {
    let mut parts = match language {
        Language::Java => java_qualified_parts(path, content)?,
        Language::Go => go_qualified_parts(path, content)?,
        _ => path_qualified_parts(path, language)?,
    };
    if parts.last().map(|part| part.as_str()) != Some(name) {
        parts.push(name.to_string());
    }
    Ok(parts.join("::"))
}

pub fn stable_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn path_qualified_parts(path: &Path, language: &Language) -> Result<Vec<String>> {
    let normalized = normalize_repo_path(path)?;
    let mut parts = normalized
        .split('/')
        .map(str::to_string)
        .collect::<Vec<String>>();
    if let Some(last) = parts.last_mut() {
        let stem = Path::new(last)
            .file_stem()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| last.clone());
        let extension = Path::new(last)
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase());
        *last = stem;
        let elide_file_stem = matches!(
            (language, extension.as_deref(), last.as_str()),
            (Language::Python, _, "__init__")
                | (Language::Rust, Some("rs"), "mod")
                | (
                    Language::TypeScript | Language::JavaScript,
                    Some("ts" | "tsx" | "js" | "jsx"),
                    "index",
                )
        );
        if elide_file_stem {
            parts.pop();
        }
    }
    parts.retain(|part| !part.is_empty());
    Ok(parts)
}

fn java_qualified_parts(path: &Path, content: Option<&str>) -> Result<Vec<String>> {
    if let Some(package) = java_package(content) {
        let mut parts = package
            .split('.')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if let Some(stem) = file_stem(path) {
            parts.push(stem);
        }
        return Ok(parts);
    }

    let mut parts = path_qualified_parts(path, &Language::Java)?;
    if parts.len() >= 4
        && parts[0] == "src"
        && (parts[1] == "main" || parts[1] == "test")
        && parts[2] == "java"
    {
        parts.drain(0..3);
    }
    Ok(parts)
}

fn go_qualified_parts(path: &Path, content: Option<&str>) -> Result<Vec<String>> {
    if let Some(package) = go_package(content) {
        return Ok(vec![package]);
    }
    path_qualified_parts(path, &Language::Go)
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
}

fn java_package(content: Option<&str>) -> Option<String> {
    content?.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("package ")
            .and_then(|rest| rest.strip_suffix(';').or(Some(rest)))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn go_package(content: Option<&str>) -> Option<String> {
    content?.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("package ")
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "_")
            .map(str::to_string)
    })
}

fn normalize_route_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "/".into()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn escape_identity_component(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.as_bytes() {
        match byte {
            b':' => escaped.push_str("%3A"),
            b'/' => escaped.push_str("%2F"),
            b'\\' => escaped.push_str("%5C"),
            b'%' => escaped.push_str("%25"),
            0x00..=0x1f | 0x7f => escaped.push_str(&format!("%{byte:02X}")),
            _ => escaped.push(*byte as char),
        }
    }
    if RESERVED_NAMESPACES
        .iter()
        .any(|namespace| escaped.starts_with(namespace))
    {
        format!("_{}", escaped)
    } else {
        escaped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Confidence, EvidenceSourceType, FileId, LineRange, SymbolId, SymbolKind, TestTarget,
    };

    fn symbol(id: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: "handler".into(),
            qualified_name: "handler".into(),
            kind: SymbolKind::Function,
            file_id: FileId::new("file"),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    #[test]
    fn normalizes_repo_paths_and_rejects_escaping_paths() {
        assert_eq!(
            normalize_repo_path(Path::new("./src\\not-special/lib.rs")).unwrap(),
            "src/not-special/lib.rs"
        );
        assert_eq!(
            normalize_repo_path(Path::new("src/bin/main.rs")).unwrap(),
            "src/bin/main.rs"
        );
        assert!(normalize_repo_path(Path::new("../src/lib.rs")).is_err());
        assert!(normalize_repo_path(Path::new("/tmp/src/lib.rs")).is_err());
    }

    #[test]
    fn reserved_namespace_payloads_are_escaped() {
        assert_eq!(
            symbol_node_id(&symbol("file:src/lib.rs")).0,
            "symbol:file%3Asrc%2Flib.rs"
        );
        assert_eq!(config_node_id("runtime:PORT").0, "config:runtime%3APORT");
    }

    #[test]
    fn route_config_test_and_edge_ids_are_deterministic() {
        let route = route_node_id("HTTP", Some("get"), "orders/{id}");
        assert_eq!(route.0, "route:http:GET:%2Forders%2F{id}");
        assert_eq!(config_node_id("db.url").0, "config:db.url");

        let test = TestTarget {
            id: "tests/order_test.rs::loads_order".into(),
            name: "loads_order".into(),
            file_id: FileId::new("file"),
            range: None,
            command: None,
            confidence: Confidence::High,
            reason: "unit test".into(),
            evidence_refs: vec![],
            score_breakdown: vec![],
        };
        assert_eq!(
            test_node_id(&test).0,
            "test:tests%2Forder_test.rs%3A%3Aloads_order"
        );

        let from = file_node_id(Path::new("src/lib.rs"));
        let to = symbol_node_id(&symbol("abc123"));
        assert_eq!(
            edge_id(GraphEdgeType::Defines, &from, &to, None),
            edge_id(GraphEdgeType::Defines, &from, &to, None)
        );
        assert_ne!(
            edge_id(GraphEdgeType::Defines, &from, &to, None),
            edge_id(GraphEdgeType::Defines, &from, &to, Some("second"))
        );
    }

    #[test]
    fn qualified_names_cover_language_entrypoint_conventions() {
        assert_eq!(
            qualified_name(
                Path::new("pkg/__init__.py"),
                &Language::Python,
                None,
                "Factory"
            )
            .unwrap(),
            "pkg::Factory"
        );
        assert_eq!(
            qualified_name(
                Path::new("src/index.ts"),
                &Language::TypeScript,
                None,
                "handler"
            )
            .unwrap(),
            "src::handler"
        );
        assert_eq!(
            qualified_name(Path::new("src/api/mod.rs"), &Language::Rust, None, "run").unwrap(),
            "src::api::run"
        );
        assert_eq!(
            qualified_name(
                Path::new("src/main/java/com/acme/OrderController.java"),
                &Language::Java,
                Some("package com.acme;\nclass OrderController {}"),
                "getOrder"
            )
            .unwrap(),
            "com::acme::OrderController::getOrder"
        );
        assert_eq!(
            qualified_name(
                Path::new("internal/orders/handler.go"),
                &Language::Go,
                Some("package orders\nfunc Load() {}"),
                "Load"
            )
            .unwrap(),
            "orders::Load"
        );
    }

    #[test]
    fn legacy_analysis_ids_are_stable_for_backwards_compatibility() {
        assert_eq!(
            legacy_analysis_node_id(GraphNodeType::Module, "std::io"),
            legacy_analysis_node_id(GraphNodeType::Module, "STD::IO")
        );
    }
}
