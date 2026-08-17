#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/open-kioku-tree-sitter/src/lib.rs")
text = path.read_text()

old = '''    } else if recv
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && !recv.contains('.')
    {
        ReceiverKind::Type
    } else {
        ReceiverKind::Value
    }
}
'''
new = '''    } else if recv
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && !recv.contains('.')
    {
        ReceiverKind::Type
    } else if recv.starts_with("crate::") {
        ReceiverKind::Module
    } else {
        ReceiverKind::Value
    }
}
'''
if text.count(old) != 1:
    raise SystemExit(f"receiver classifier: expected one match, found {text.count(old)}")
text = text.replace(old, new)

if "crate_qualified_rust_call_is_classified_as_module_receiver" not in text:
    text += r'''

#[cfg(test)]
mod ri3_rust_module_receiver_tests {
    use super::parse_file;
    use open_kioku_core::{File, FileId, Language, ReceiverKind, RepositoryId};
    use std::path::PathBuf;

    #[test]
    fn crate_qualified_rust_call_is_classified_as_module_receiver() {
        let file = File {
            id: FileId::new("file:src/domain/call_violation.rs"),
            repository_id: RepositoryId::new("repo:test"),
            path: PathBuf::from("src/domain/call_violation.rs"),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let facts = parse_file(
            &file,
            "pub fn write() { crate::storage::persist(); }",
        )
        .expect("Rust fixture should parse");
        let call = facts
            .calls
            .iter()
            .find(|call| call.callee_name == "persist")
            .expect("qualified persist call should be extracted");

        assert_eq!(call.receiver.as_deref(), Some("crate::storage"));
        assert_eq!(call.receiver_kind, ReceiverKind::Module);
    }
}
'''

path.write_text(text)
