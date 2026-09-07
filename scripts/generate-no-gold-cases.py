#!/usr/bin/env python3
"""Generate retrieval cases that provably have no correct answer.

The frozen corpus carries five no-gold cases. A false-positive rate over five
samples cannot distinguish 0.8 from 1.0, and calibrating an abstention threshold
on it is fitting noise. This produces hundreds instead, and validates them by
construction rather than by hand-labelling: every content word in a generated
query is checked against the fixture's entire vocabulary, so a match is
impossible by definition rather than by judgement.

Usage:
    scripts/generate-no-gold-cases.py [--count N] [--out PATH]

Then measure with the generated file plus the existing positives:
    ok retrieval-bench . --cases-file <out> --min-cases 30
"""
from __future__ import annotations

import argparse
import itertools
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "benchmarks" / "retrieval-fixture"
CASES = ROOT / "benchmarks" / "retrieval-cases.json"

# Domains deliberately distant from the fixture. Overlap is still verified
# below - this list is a starting point, not the guarantee.
SUBJECTS = ["seismograph", "glacier", "zeppelin", "harpsichord", "obsidian", "tundra",
            "xylophone", "quasar", "marzipan", "bramble", "kestrel", "monsoon",
            "alabaster", "juniper", "pelican"]
ACTIONS = ["calibrate", "defrost", "transcribe", "levitate", "germinate", "unclog",
           "varnish", "pollinate", "refract", "winnow"]
OBJECTS = ["ledger", "gyroscope", "tapestry", "kiln", "barometer", "trellis", "sundial", "anvil"]
QUALIFIERS = ["nightly", "offline", "seasonal", "handheld", "subterranean", "portable"]


def fixture_vocabulary() -> set[str]:
    """Every term of three or more characters anywhere in the indexed fixture."""
    vocab: set[str] = set()
    for path in FIXTURE.rglob("*"):
        if not path.is_file() or ".ok" in path.parts:
            continue
        try:
            text = path.read_text(errors="replace")
        except OSError:
            continue
        for word in re.findall(r"[A-Za-z]{3,}", text):
            spaced = re.sub(r"(?<!^)(?=[A-Z])", " ", word).lower()
            vocab.update(part for part in re.findall(r"[a-z]{3,}", spaced))
        vocab.update(re.findall(r"[a-z]{3,}", path.as_posix().lower()))
    return vocab


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--count", type=int, default=400)
    parser.add_argument("--out", type=pathlib.Path, default=ROOT / "no-gold-cases.json")
    args = parser.parse_args()

    vocab = fixture_vocabulary()
    base = json.loads(CASES.read_text())
    template = next(c for c in base["cases"] if c.get("no_gold_expected"))
    positives = [c for c in base["cases"] if not c.get("no_gold_expected")]

    generated, seen = [], set()
    for action, qualifier, subject, obj in itertools.product(ACTIONS, QUALIFIERS, SUBJECTS, OBJECTS):
        query = f"{action} the {qualifier} {subject} {obj}"
        words = [w for w in re.findall(r"[a-z]{3,}", query.lower()) if w != "the"]
        if any(w in vocab for w in words) or query in seen:
            continue
        seen.add(query)
        case = dict(template)
        case["id"] = f"gen-no-gold-{len(generated):04d}"
        case["query"] = query
        case["split"] = "holdout" if len(generated) % 2 else "development"
        generated.append(case)
        if len(generated) >= args.count:
            break

    # Positives are retained: the harness needs them to calibrate, and a
    # no-gold rate is only meaningful next to the recall it costs.
    out = dict(base)
    out["cases"] = positives + generated
    args.out.write_text(json.dumps(out, indent=2) + "\n")
    print(f"fixture vocabulary: {len(vocab)} distinct terms")
    print(f"wrote {len(positives)} positive + {len(generated)} generated no-gold cases to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
