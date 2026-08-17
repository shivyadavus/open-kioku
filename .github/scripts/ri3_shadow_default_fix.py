#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/open-kioku-config/src/lib.rs")
text = path.read_text()


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    text = text.replace(old, new)


replace_once(
    "                resolution_mode: ResolutionMode::Legacy,\n",
    "                resolution_mode: ResolutionMode::Shadow,\n",
    "OkConfig default resolution mode",
)
replace_once(
    "pub enum ResolutionMode {\n    #[default]\n    Legacy,\n    Shadow,\n    V2,\n}\n",
    "pub enum ResolutionMode {\n    Legacy,\n    #[default]\n    Shadow,\n    V2,\n}\n",
    "serde/default resolution mode",
)
replace_once(
    "    use super::ScipMode;\n",
    "    use super::{ResolutionMode, ScipMode};\n",
    "config test imports",
)
replace_once(
    "        assert_eq!(config.scip.mode, ScipMode::Consume);\n        assert_eq!(config.semantic.ann_min_rows, 10_000);\n",
    "        assert_eq!(config.scip.mode, ScipMode::Consume);\n        assert_eq!(config.index.resolution_mode, ResolutionMode::Shadow);\n        assert_eq!(config.semantic.ann_min_rows, 10_000);\n",
    "default config shadow assertion",
)
replace_once(
    "        assert_eq!(loaded.scip.mode, ScipMode::Auto);\n        assert_eq!(loaded.ranking.text_relevance, 1.0);\n",
    "        assert_eq!(loaded.scip.mode, ScipMode::Auto);\n        assert_eq!(loaded.index.resolution_mode, ResolutionMode::Shadow);\n        assert_eq!(loaded.ranking.text_relevance, 1.0);\n",
    "missing-field shadow compatibility assertion",
)

path.write_text(text)
