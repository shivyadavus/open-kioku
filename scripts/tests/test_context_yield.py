#!/usr/bin/env python3
"""Unit tests for the gold-yield metric in score-context-cases.py and the modified-line-range
column written by commit-derived-cases.py. Stdlib only; run with
`python3 -m unittest discover -s scripts/tests`.
"""

import importlib.util
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


score = load("score-context-cases")
derive = load("commit-derived-cases")

# (path, start, end, estimated_tokens) in pack order.
UNITS = [
    ("a/noise.py", 1, 10, 3000),
    ("a/gold.py", 40, 60, 2500),
    ("b/other.py", 5, 6, 1000),
    ("a/gold.py", 100, 120, 2000),
    ("c/gold2.py", 1, 5, 500),
]
GOLD = {"a/gold.py", "c/gold2.py"}
RANGES = {"a/gold.py": [(50, 59), (110, 129)], "c/gold2.py": [(3, 3), (200, 209)]}


class YieldAtBudget(unittest.TestCase):
    def test_budget_is_a_prefix_of_the_pack_in_order(self):
        # 3000 + 2500 = 5500 fits in 8000; the 1000-token unit would make 6500, still fits;
        # the 2000-token unit would make 8500 and stops the walk, so the later 500-token
        # unit is not counted even though it would fit on its own.
        file_yield, line_yield = score.yield_at(UNITS, GOLD, RANGES, 8000)
        self.assertAlmostEqual(file_yield, 0.5)
        # gold.py 40-60 covers 50-59 (10 lines) of 10 + 20 + 1 + 10 = 41 gold lines.
        self.assertAlmostEqual(line_yield, 10 / 41)

    def test_larger_budget_reaches_more(self):
        file_yield, line_yield = score.yield_at(UNITS, GOLD, RANGES, 16000)
        self.assertAlmostEqual(file_yield, 1.0)
        # + gold.py 100-120 covers 110-120 (11 lines), + gold2.py 1-5 covers line 3.
        self.assertAlmostEqual(line_yield, (10 + 11 + 1) / 41)

    def test_budget_below_first_unit_yields_nothing(self):
        self.assertEqual(score.yield_at(UNITS, GOLD, RANGES, 2000), (0.0, 0.0))

    def test_line_yield_is_none_without_ranges(self):
        file_yield, line_yield = score.yield_at(UNITS, GOLD, None, 16000)
        self.assertAlmostEqual(file_yield, 1.0)
        self.assertIsNone(line_yield)

    def test_tokens_to_first_gold(self):
        self.assertEqual(score.tokens_to_first_gold(UNITS, GOLD), 3000)
        self.assertIsNone(score.tokens_to_first_gold(UNITS, {"z/absent.py"}))
        self.assertEqual(score.tokens_to_first_gold(UNITS[1:], GOLD), 0)

    def test_yield_row_without_units_is_null(self):
        row = score.yield_row([], GOLD, RANGES)
        self.assertIsNone(row["gold_file_yield"])
        self.assertIsNone(row["tokens_to_first_gold"])


class RangeColumn(unittest.TestCase):
    def test_parse_ranges_aligns_with_gold_order(self):
        ranges = score.parse_ranges("26-33,36-44|1-1", ["x.ts", "y.ts"])
        self.assertEqual(ranges, {"x.ts": [(26, 33), (36, 44)], "y.ts": [(1, 1)]})

    def test_parse_ranges_tolerates_absence_and_misalignment(self):
        self.assertIsNone(score.parse_ranges(None, ["x.ts"]))
        self.assertIsNone(score.parse_ranges("1-2", ["x.ts", "y.ts"]))
        self.assertIsNone(score.parse_ranges("", ["x.ts"]))

    def test_hunk_headers_give_base_side_ranges(self):
        diff = "\n".join([
            "diff --git a/fmt/duration.ts b/fmt/duration.ts",
            "--- a/fmt/duration.ts",
            "+++ b/fmt/duration.ts",
            "@@ -26,8 +26,12 @@ function addZero(num: number, digits: number) {",
            "-interface DurationObject {",
            "@@ -52 +60 @@ const x = 1;",
            "-const y = 2;",
            "@@ -70,0 +80,3 @@ const z = 3;",
            "+inserted",
            "diff --git a/expect/mod.ts b/expect/mod.ts",
            "--- a/expect/mod.ts",
            "+++ b/expect/mod.ts",
            "@@ -0,0 +1,2 @@",
            "+top",
        ])
        ranges = derive.parse_hunk_ranges(diff)
        self.assertEqual(ranges["fmt/duration.ts"], [(26, 33), (52, 52), (70, 70)])
        self.assertEqual(ranges["expect/mod.ts"], [(1, 1)])
        self.assertEqual(derive.format_ranges([ranges["fmt/duration.ts"], ranges["expect/mod.ts"]]),
                         "26-33,52-52,70-70|1-1")




class AbstainingCasesCountAsZeroYield(unittest.TestCase):
    """A pack that selects nothing delivered no gold lines; excluding it inflates the mean."""

    def test_file_yield_averages_over_every_scored_case(self):
        sample = [
            {"rank": 1, "gold_recall": 1.0, "gold_file_yield": {"4000": 1.0, "8000": 1.0, "16000": 1.0}, "gold_line_yield": None,
             "line_yield_measurable": False, "tokens_to_first_gold": 10, "pack_tokens": 100},
            {"rank": None, "gold_recall": 0.0, "gold_file_yield": None, "gold_line_yield": None,
             "line_yield_measurable": False, "tokens_to_first_gold": None, "pack_tokens": 0},
        ]
        out = score.metrics(sample)
        self.assertAlmostEqual(out["gold_file_yield@4000"], 0.5)

    def test_line_yield_denominator_is_cases_with_ranges_not_cases_with_units(self):
        sample = [
            {"rank": 1, "gold_recall": 1.0, "gold_file_yield": {"4000": 1.0, "8000": 1.0, "16000": 1.0},
             "gold_line_yield": {"4000": 0.8, "8000": 0.8, "16000": 0.8}, "line_yield_measurable": True,
             "tokens_to_first_gold": 10, "pack_tokens": 100},
            # abstained, but its case carries line ranges, so it is measurable and scores 0
            {"rank": None, "gold_recall": 0.0, "gold_file_yield": None, "gold_line_yield": None,
             "line_yield_measurable": True, "tokens_to_first_gold": None, "pack_tokens": 0},
            # no ranges annotated: not measurable, must not enter the denominator
            {"rank": 1, "gold_recall": 1.0, "gold_file_yield": {"4000": 1.0, "8000": 1.0, "16000": 1.0},
             "gold_line_yield": None, "line_yield_measurable": False,
             "tokens_to_first_gold": 10, "pack_tokens": 100},
        ]
        out = score.metrics(sample)
        self.assertAlmostEqual(out["gold_line_yield@4000"], 0.4)


if __name__ == "__main__":
    unittest.main()
