#!/usr/bin/env python3

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "evaluate-fusion-calibration.py"


def case(
    case_id: str,
    *,
    task_family: str,
    query_shape: str,
    split: str = "development",
    recall: float = 1.0,
    reciprocal_rank: float = 1.0,
    file_f1: float = 1.0,
    no_gold: bool = False,
    returned_any: bool = True,
) -> dict:
    return {
        "id": case_id,
        "task_family": task_family,
        "expected_query_shape": query_shape,
        "split": split,
        "recall_at": {"10": recall},
        "reciprocal_rank": reciprocal_rank,
        "file_f1_at_10": file_f1,
        "no_gold_expected": no_gold,
        "returned_any": returned_any,
    }


def strategy(label: str, cases: list[dict], *, recall: float, mrr: float, f1: float, fp: float) -> dict:
    return {
        "strategy": label,
        "by_split": {
            "development": {
                "quality": {
                    "recall_at_10": recall,
                    "mean_reciprocal_rank": mrr,
                    "file_f1_at_10": f1,
                    "no_gold_false_positive_rate": fp,
                },
                "latency": {"p95_ms": 10.0},
            }
        },
        "cases": cases,
    }


class FusionCalibrationTests(unittest.TestCase):
    def run_script(self, baseline: dict, candidate: dict) -> tuple[subprocess.CompletedProcess[str], dict | None]:
        report = {
            "corpus_id": "synthetic",
            "cases_file": "synthetic.json",
            "stream_ablations": [baseline, candidate],
        }
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            report_path = root / "report.json"
            output_path = root / "decision.json"
            report_path.write_text(json.dumps(report))
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    str(report_path),
                    "--output",
                    str(output_path),
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            decision = json.loads(output_path.read_text()) if output_path.exists() else None
            return completed, decision

    def test_development_task_family_regression_blocks_promotion(self) -> None:
        baseline_cases = [
            case("a", task_family="issue_to_code", query_shape="conceptual", recall=1.0),
            case("b", task_family="code_to_test", query_shape="exact_identifier", recall=0.5),
        ]
        candidate_cases = [
            case("a", task_family="issue_to_code", query_shape="conceptual", recall=0.5),
            case("b", task_family="code_to_test", query_shape="exact_identifier", recall=1.0),
        ]
        completed, decision = self.run_script(
            strategy("cc2:rrf_unweighted", baseline_cases, recall=0.70, mrr=0.70, f1=0.70, fp=0.0),
            strategy("cc2:rrf_evidence_prior", candidate_cases, recall=0.72, mrr=0.70, f1=0.70, fp=0.0),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        assert decision is not None
        self.assertFalse(decision["promote_candidate_to_holdout_evaluation"])
        self.assertIn(
            "task_family:issue_to_code:recall_at_10",
            decision["development_subgroup_regressions"],
        )

    def test_holdout_regression_is_not_used_for_calibration(self) -> None:
        baseline_cases = [
            case("dev", task_family="issue_to_code", query_shape="conceptual", recall=0.5),
            case(
                "holdout",
                task_family="issue_to_code",
                query_shape="conceptual",
                split="holdout",
                recall=1.0,
            ),
        ]
        candidate_cases = [
            case("dev", task_family="issue_to_code", query_shape="conceptual", recall=0.5),
            case(
                "holdout",
                task_family="issue_to_code",
                query_shape="conceptual",
                split="holdout",
                recall=0.0,
            ),
        ]
        completed, decision = self.run_script(
            strategy("cc2:rrf_unweighted", baseline_cases, recall=0.50, mrr=0.50, f1=0.50, fp=0.0),
            strategy("cc2:rrf_evidence_prior", candidate_cases, recall=0.52, mrr=0.50, f1=0.50, fp=0.0),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        assert decision is not None
        self.assertTrue(decision["promote_candidate_to_holdout_evaluation"])
        self.assertEqual(decision["development_subgroup_regressions"], [])
        self.assertFalse(decision["holdout_used_for_promotion_decision"])

    def test_development_case_identity_mismatch_fails_closed(self) -> None:
        completed, decision = self.run_script(
            strategy(
                "cc2:rrf_unweighted",
                [case("baseline", task_family="issue_to_code", query_shape="conceptual")],
                recall=0.5,
                mrr=0.5,
                f1=0.5,
                fp=0.0,
            ),
            strategy(
                "cc2:rrf_evidence_prior",
                [case("candidate", task_family="issue_to_code", query_shape="conceptual")],
                recall=0.6,
                mrr=0.5,
                f1=0.5,
                fp=0.0,
            ),
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIsNone(decision)
        self.assertIn("development case identities differ", completed.stderr)

    def test_no_meaningful_gain_remains_rejected(self) -> None:
        cases = [case("dev", task_family="issue_to_code", query_shape="conceptual")]
        completed, decision = self.run_script(
            strategy("cc2:rrf_unweighted", cases, recall=0.5, mrr=0.5, f1=0.5, fp=0.0),
            strategy("cc2:rrf_evidence_prior", cases, recall=0.5, mrr=0.5, f1=0.5, fp=0.0),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        assert decision is not None
        self.assertFalse(decision["promote_candidate_to_holdout_evaluation"])
        self.assertIn(
            "candidate has no meaningful development-split quality gain",
            decision["reasons"],
        )


if __name__ == "__main__":
    unittest.main()
