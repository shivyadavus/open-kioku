#!/usr/bin/env python3
"""Targeted mutation test for retrieval baseline dimension comparison."""

from pathlib import Path
import subprocess


SOURCE = Path("crates/open-kioku-cli/src/bench/retrieval.rs")
TEST_COMMAND = [
    "cargo",
    "test",
    "-p",
    "open-kioku-cli",
    "retrieval_bench_tests",
]

LANGUAGE_CALL = """        append_retrieval_group_deltas(
            &mut deltas,
            caveats,
            &current.strategy,
            "language",
            "language",
            &current.by_language,
            &previous.by_language,
        );
"""

TASK_FAMILY_CALL = """        append_retrieval_group_deltas(
            &mut deltas,
            caveats,
            &current.strategy,
            "task_family",
            "task-family",
            &current.by_task_family,
            &previous.by_task_family,
        );
"""

MUTATIONS = [
    (
        "disable dimension-key mismatch detection",
        "if current.keys().ne(previous.keys()) {",
        "if false {",
    ),
    (
        "invert dimension-key mismatch detection",
        "if current.keys().ne(previous.keys()) {",
        "if current.keys().eq(previous.keys()) {",
    ),
    (
        "continue after a dimension-key mismatch",
        """        ));
        return;
    }

    for (scope, current_summary) in current {""",
        """        ));
    }

    for (scope, current_summary) in current {""",
    ),
    ("omit language comparison", LANGUAGE_CALL, ""),
    ("omit task-family comparison", TASK_FAMILY_CALL, ""),
]


def main() -> None:
    original = SOURCE.read_text()
    try:
        for name, old, new in MUTATIONS:
            occurrences = original.count(old)
            if occurrences != 1:
                raise SystemExit(
                    f"mutation {name!r} expected one target, found {occurrences}"
                )
            SOURCE.write_text(original.replace(old, new, 1))
            result = subprocess.run(TEST_COMMAND, check=False)
            if result.returncode == 0:
                raise SystemExit(f"SURVIVED: {name}")
            print(f"KILLED: {name}", flush=True)
            SOURCE.write_text(original)
    finally:
        SOURCE.write_text(original)

    subprocess.run(TEST_COMMAND, check=True)
    print(f"All {len(MUTATIONS)} targeted mutants were killed.")


if __name__ == "__main__":
    main()
