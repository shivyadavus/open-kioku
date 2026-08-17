from pathlib import Path

path = Path("crates/open-kioku-tree-sitter/src/lib.rs")
text = path.read_text()

duplicate = "    #[test]\n    #[test]\n    fn rust_scoped_paths_distinguish_modules_from_instance_self()"
replacement = "    #[test]\n    fn rust_scoped_paths_distinguish_modules_from_instance_self()"
assert text.count(duplicate) == 1, "expected exactly one duplicated #[test] on RI3 scoped-path regression"
text = text.replace(duplicate, replacement)

old_clone = ".map(|call| (call.receiver.as_deref(), call.receiver_kind.clone()))"
new_copy = ".map(|call| (call.receiver.as_deref(), call.receiver_kind))"
assert text.count(old_clone) == 1, "expected exactly one clone-on-Copy in RI3 scoped-path regression"
text = text.replace(old_clone, new_copy)

path.write_text(text)
