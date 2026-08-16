from pathlib import Path

path = Path('crates/open-kioku-context/src/candidates/builtins.rs')
text = path.read_text()
text = text.replace(
    '''if !(1..=6).contains(&level) || !trimmed[level..].starts_with(char::is_whitespace) {''',
    '''if !(1..=6).contains(&level)
            || !trimmed[level..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_whitespace())
        {''',
)
text = text.replace(
    '''            id: open_kioku_core::ChunkId::new("doc-1"),
            file_id: FileId::new("doc"),
            range: LineRange { start: 10, end: 12 },
            text: "# Guide\\n## Setup\\ninstall things".into(),
            symbol_id: None,
            language: Language::Unknown,
''',
    '''            id: "doc-1".into(),
            file_id: FileId::new("doc"),
            range: LineRange { start: 10, end: 12 },
            language: Language::Rust,
            text: "# Guide\\n## Setup\\ninstall things".into(),
''',
)
text = text.replace(
    '''            id: open_kioku_core::ChunkId::new("doc-2"),
            file_id: FileId::new("doc"),
            range: LineRange { start: 13, end: 14 },
            text: "continue setup\\nmore details".into(),
            symbol_id: None,
            language: Language::Unknown,
''',
    '''            id: "doc-2".into(),
            file_id: FileId::new("doc"),
            range: LineRange { start: 13, end: 14 },
            language: Language::Rust,
            text: "continue setup\\nmore details".into(),
''',
)
path.write_text(text)
