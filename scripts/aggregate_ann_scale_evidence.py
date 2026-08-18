#!/usr/bin/env python3
"""Aggregate and validate Open Kioku ANN scale benchmark evidence.

This script intentionally derives only measured frontiers. It does not invent production
thresholds or profile names. Missing matrix points, duplicate measurements, mixed benchmark
schemas, or non-finite metrics fail closed so profile selection can be reviewed from complete,
reproducible evidence.
"""

from __future__ import annotations

import argparse
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

EXPECTED_BENCHMARK = "cc5-ann-scale-matrix"
EXPECTED_ORACLE = "exact-flat"
EXPECTED_BACKEND = "usearch-hnsw-f32"
DEFAULT_SIZES = (50_000, 100_000, 300_000, 1_000_000)
DEFAULT_DIMENSIONS = (384, 768)
DEFAULT_EXPANSIONS = (64, 128, 256, 512, 1_024)


@dataclass(frozen=True, order=True)
class MeasurementKey:
    vector_count: int
    dimensions: int
    expansion_search: int


def parse_positive_csv(value: str) -> tuple[int, ...]:
    parsed = tuple(int(part.strip()) for part in value.split(",") if part.strip())
    if not parsed or any(item <= 0 for item in parsed):
        raise argparse.ArgumentTypeError("expected a non-empty CSV of positive integers")
    if len(parsed) != len(set(parsed)):
        raise argparse.ArgumentTypeError("CSV values must be unique")
    return parsed


def finite_number(value: Any, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{field} must be numeric")
    numeric = float(value)
    if not math.isfinite(numeric):
        raise ValueError(f"{field} must be finite")
    return numeric


def validate_quality(quality: dict[str, Any], source: Path) -> None:
    for metric in ("recall_at_1", "recall_at_5", "recall_at_10", "recall_at_20", "mrr"):
        value = finite_number(quality.get(metric), f"{source}:{metric}")
        if not 0.0 <= value <= 1.0:
            raise ValueError(f"{source}:{metric} out of range: {value}")


def validate_latency(latency: dict[str, Any], source: Path, prefix: str) -> None:
    for metric in ("mean_us", "p50_us", "p95_us", "p99_us"):
        value = finite_number(latency.get(metric), f"{source}:{prefix}.{metric}")
        if value < 0.0:
            raise ValueError(f"{source}:{prefix}.{metric} must be non-negative")


def load_reports(paths: Iterable[Path]) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    common: dict[str, Any] = {}
    for path in sorted(paths):
        report = json.loads(path.read_text())
        if report.get("schema_version") != 1:
            raise ValueError(f"{path}: unexpected schema_version")
        if report.get("benchmark") != EXPECTED_BENCHMARK:
            raise ValueError(f"{path}: unexpected benchmark")
        if report.get("backend") != EXPECTED_BACKEND:
            raise ValueError(f"{path}: unexpected backend")
        if report.get("oracle") != EXPECTED_ORACLE:
            raise ValueError(f"{path}: exact-flat must remain the oracle")
        distribution = report.get("distribution")
        if not isinstance(distribution, str) or not distribution:
            raise ValueError(f"{path}: missing distribution identity")
        if not common:
            common = {
                "benchmark": report["benchmark"],
                "backend": report["backend"],
                "oracle": report["oracle"],
                "distribution": distribution,
            }
        elif distribution != common["distribution"]:
            raise ValueError(
                f"{path}: mixed distributions are not comparable in one aggregate "
                f"({distribution!r} != {common['distribution']!r})"
            )
        for row in report.get("measurements", []):
            validate_quality(row.get("quality", {}), path)
            validate_latency(row.get("ann_query", {}), path, "ann_query")
            validate_latency(row.get("exact_query", {}), path, "exact_query")
            for field in (
                "ann_build_ms",
                "ann_vectors_per_second",
                "ann_reload_ms",
                "ann_first_query_after_reload_us",
                "ann_index_bytes",
                "ann_metadata_bytes",
                "ann_memory_bytes",
            ):
                if finite_number(row.get(field), f"{path}:{field}") < 0.0:
                    raise ValueError(f"{path}:{field} must be non-negative")
            rows.append(row)
    if not rows:
        raise ValueError("no ANN scale measurements found")
    return rows, common


def measurement_key(row: dict[str, Any]) -> MeasurementKey:
    return MeasurementKey(
        vector_count=int(row["vector_count"]),
        dimensions=int(row["dimensions"]),
        expansion_search=int(row["parameters"]["expansion_search"]),
    )


def validate_matrix(
    rows: list[dict[str, Any]],
    sizes: tuple[int, ...],
    dimensions: tuple[int, ...],
    expansions: tuple[int, ...],
) -> dict[MeasurementKey, dict[str, Any]]:
    observed: dict[MeasurementKey, dict[str, Any]] = {}
    for row in rows:
        key = measurement_key(row)
        if key in observed:
            raise ValueError(f"duplicate ANN scale measurement: {key}")
        observed[key] = row
    expected = {
        MeasurementKey(size, dims, expansion)
        for size in sizes
        for dims in dimensions
        for expansion in expansions
    }
    missing = sorted(expected - observed.keys())
    unexpected = sorted(observed.keys() - expected)
    if missing:
        preview = ", ".join(str(key) for key in missing[:8])
        raise ValueError(f"missing {len(missing)} required ANN scale measurements: {preview}")
    if unexpected:
        preview = ", ".join(str(key) for key in unexpected[:8])
        raise ValueError(f"unexpected ANN scale measurements: {preview}")
    return observed


def dominates(left: dict[str, Any], right: dict[str, Any]) -> bool:
    left_quality = float(left["quality"]["recall_at_10"])
    right_quality = float(right["quality"]["recall_at_10"])
    left_latency = float(left["ann_query"]["p95_us"])
    right_latency = float(right["ann_query"]["p95_us"])
    left_memory = float(left["ann_memory_bytes"])
    right_memory = float(right["ann_memory_bytes"])
    no_worse = (
        left_quality >= right_quality
        and left_latency <= right_latency
        and left_memory <= right_memory
    )
    strictly_better = (
        left_quality > right_quality
        or left_latency < right_latency
        or left_memory < right_memory
    )
    return no_worse and strictly_better


def pareto_frontier(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    frontier = [
        row
        for row in rows
        if not any(dominates(other, row) for other in rows if other is not row)
    ]
    return sorted(frontier, key=lambda row: int(row["parameters"]["expansion_search"]))


def summarize_row(row: dict[str, Any]) -> dict[str, Any]:
    quality = row["quality"]
    ann_query = row["ann_query"]
    return {
        "expansion_search": int(row["parameters"]["expansion_search"]),
        "recall_at_1": float(quality["recall_at_1"]),
        "recall_at_5": float(quality["recall_at_5"]),
        "recall_at_10": float(quality["recall_at_10"]),
        "recall_at_20": float(quality["recall_at_20"]),
        "mrr": float(quality["mrr"]),
        "ann_p50_us": float(ann_query["p50_us"]),
        "ann_p95_us": float(ann_query["p95_us"]),
        "ann_p99_us": float(ann_query["p99_us"]),
        "ann_build_ms": float(row["ann_build_ms"]),
        "ann_vectors_per_second": float(row["ann_vectors_per_second"]),
        "ann_reload_ms": float(row["ann_reload_ms"]),
        "ann_index_bytes": int(row["ann_index_bytes"]),
        "ann_memory_bytes": int(row["ann_memory_bytes"]),
    }


def aggregate(
    observed: dict[MeasurementKey, dict[str, Any]],
    common: dict[str, Any],
    sizes: tuple[int, ...],
    dimensions: tuple[int, ...],
    expansions: tuple[int, ...],
) -> dict[str, Any]:
    groups = []
    for dims in dimensions:
        for size in sizes:
            rows = [observed[MeasurementKey(size, dims, expansion)] for expansion in expansions]
            frontier = pareto_frontier(rows)
            groups.append(
                {
                    "vector_count": size,
                    "dimensions": dims,
                    "measurements": [summarize_row(row) for row in rows],
                    "pareto_expansion_search": [
                        int(row["parameters"]["expansion_search"]) for row in frontier
                    ],
                }
            )
    return {
        "schema_version": 1,
        "benchmark": common["benchmark"],
        "backend": common["backend"],
        "oracle": common["oracle"],
        "distribution": common["distribution"],
        "selection_policy": (
            "measured-pareto-only: maximize Recall@10 while minimizing ANN p95 latency and "
            "ANN memory; no production profile is selected by this report"
        ),
        "requested_sizes": list(sizes),
        "requested_dimensions": list(dimensions),
        "requested_search_expansions": list(expansions),
        "groups": groups,
    }


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# ANN scale evidence aggregate",
        "",
        f"Distribution: `{report['distribution']}`  ",
        f"Oracle: `{report['oracle']}`  ",
        f"Policy: {report['selection_policy']}",
        "",
    ]
    for group in report["groups"]:
        lines.extend(
            [
                f"## {group['vector_count']:,} vectors × {group['dimensions']}d",
                "",
                "| expansion_search | Recall@10 | Recall@20 | MRR | ANN p95 µs | ANN p99 µs | build ms | memory bytes | Pareto |",
                "|---:|---:|---:|---:|---:|---:|---:|---:|:---:|",
            ]
        )
        frontier = set(group["pareto_expansion_search"])
        for row in group["measurements"]:
            lines.append(
                "| {exp} | {r10:.4f} | {r20:.4f} | {mrr:.4f} | {p95:.1f} | {p99:.1f} | {build:.1f} | {memory} | {pareto} |".format(
                    exp=row["expansion_search"],
                    r10=row["recall_at_10"],
                    r20=row["recall_at_20"],
                    mrr=row["mrr"],
                    p95=row["ann_p95_us"],
                    p99=row["ann_p99_us"],
                    build=row["ann_build_ms"],
                    memory=row["ann_memory_bytes"],
                    pareto="yes" if row["expansion_search"] in frontier else "",
                )
            )
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("--sizes", type=parse_positive_csv, default=DEFAULT_SIZES)
    parser.add_argument("--dimensions", type=parse_positive_csv, default=DEFAULT_DIMENSIONS)
    parser.add_argument("--expansions", type=parse_positive_csv, default=DEFAULT_EXPANSIONS)
    parser.add_argument("--json-output", type=Path, required=True)
    parser.add_argument("--markdown-output", type=Path, required=True)
    args = parser.parse_args()

    paths: list[Path] = []
    for candidate in args.inputs:
        if candidate.is_dir():
            paths.extend(candidate.rglob("*.json"))
        elif candidate.is_file():
            paths.append(candidate)
    rows, common = load_reports(paths)
    observed = validate_matrix(rows, args.sizes, args.dimensions, args.expansions)
    report = aggregate(observed, common, args.sizes, args.dimensions, args.expansions)

    args.json_output.parent.mkdir(parents=True, exist_ok=True)
    args.markdown_output.parent.mkdir(parents=True, exist_ok=True)
    args.json_output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    args.markdown_output.write_text(render_markdown(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
