from pathlib import Path

path = Path('crates/open-kioku-cli/src/reports/proof.rs')
text = path.read_text()
old = '        out.push_str("\\n");\n'
new = "        out.push('\\n');\n"
if text.count(old) != 1:
    raise SystemExit(f'expected exactly one single-character push_str, found {text.count(old)}')
path.write_text(text.replace(old, new, 1))
