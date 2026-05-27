#!/usr/bin/env python3
"""Run a local, repeatable usefulness proof for Open Kioku planning.

The report intentionally records metrics and paths, not source snippets, so it
can be shared without publishing private code from local validation repos.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_TASK_BANK = [
    "mcp install",
    "plan_change",
    "context pack",
    "impact analysis",
    "test selector",
    "repo status",
    "search code",
    "symbol lookup",
    "release workflow",
    "npm package",
    "security policy",
    "database storage",
    "configuration",
    "authentication",
    "auth",
    "login",
    "user",
    "dashboard",
    "api",
    "database",
    "config",
    "tests",
    "security",
    "policy",
    "audit",
    "billing",
    "payment",
    "stripe",
    "agent",
    "workflow",
    "mcp",
    "context",
    "search",
    "index",
    "impact",
    "validation",
]

SKIP_DIRS = {
    ".cache",
    ".git",
    ".next",
    ".ok",
    ".turbo",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
}


@dataclass(frozen=True)
class RepoCandidate:
    path: Path
    file_count: int


def run_json(args: list[str], cwd: Path, timeout: int) -> Any:
    completed = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"{' '.join(args)} failed with exit code {completed.returncode}:"
            f" {completed.stderr.strip()}"
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"{' '.join(args)} did not return JSON") from exc


def run_text(args: list[str], cwd: Path, timeout: int) -> str:
    completed = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"{' '.join(args)} failed with exit code {completed.returncode}:"
            f" {completed.stderr.strip()}"
        )
    return completed.stdout


def repo_file_count(repo: Path, max_files: int) -> int:
    count = 0
    for root, dirs, files in os.walk(repo):
        dirs[:] = [name for name in dirs if name not in SKIP_DIRS]
        count += len(files)
        if count > max_files:
            break
    return count


def discover_repos(repo_root: Path, max_repos: int, max_files: int) -> list[RepoCandidate]:
    candidates: list[RepoCandidate] = []
    for git_dir in sorted(repo_root.glob("*/.git")):
        repo = git_dir.parent
        count = repo_file_count(repo, max_files)
        if count <= max_files:
            candidates.append(RepoCandidate(repo, count))
    candidates.sort(key=lambda candidate: (candidate.file_count, str(candidate.path)))
    return candidates[:max_repos]


def choose_tasks(ok_bin: Path, repo: Path, tasks_per_repo: int, timeout: int) -> list[str]:
    selected: list[str] = []
    for task in DEFAULT_TASK_BANK:
        try:
            results = run_json(
                [str(ok_bin), "--repo", str(repo), "search", task, "--limit", "1"],
                cwd=repo,
                timeout=timeout,
            )
        except Exception:
            continue
        if results:
            selected.append(task)
        if len(selected) >= tasks_per_repo:
            return selected
    return selected


def is_test_path(path: str) -> bool:
    lower = path.lower()
    return "/test" in lower or "test/" in lower or lower.endswith("_test.go")


def is_doc_path(path: str) -> bool:
    lower = path.lower()
    return lower.endswith((".md", ".mdx", ".txt")) or lower.startswith("docs/")


def score_plan(repo: Path, report: dict[str, Any]) -> dict[str, Any]:
    primary = report.get("primary_context") or []
    impact = (report.get("impact") or {}).get("direct_impacts") or []
    validation = report.get("validation") or []
    tool_calls = report.get("tool_calls") or []
    risk = report.get("risk") or {}
    paths = [item.get("path", "") for item in primary if item.get("path")]
    unique_paths = list(dict.fromkeys(paths))
    existing_paths = [path for path in paths if (repo / path).exists()]
    source_paths = [
        path for path in paths if path and not is_doc_path(path) and not is_test_path(path)
    ]

    checks = {
        "primary_context": len(primary) > 0,
        "paths_exist": len(existing_paths) == len(paths) and len(paths) > 0,
        "source_context": len(source_paths) > 0,
        "impact_candidates": len(impact) > 0,
        "validation_candidates": len(validation) > 0,
        "agent_tool_calls": len(tool_calls) >= 3,
        "known_risk": risk.get("level") not in {None, "", "unknown"},
    }
    weights = {
        "primary_context": 25,
        "paths_exist": 15,
        "source_context": 15,
        "impact_candidates": 15,
        "validation_candidates": 15,
        "agent_tool_calls": 10,
        "known_risk": 5,
    }
    score = sum(weights[name] for name, passed in checks.items() if passed)

    return {
        "score": score,
        "checks": checks,
        "primary_context_count": len(primary),
        "source_context_count": len(source_paths),
        "impact_count": len(impact),
        "validation_count": len(validation),
        "tool_call_count": len(tool_calls),
        "risk_level": risk.get("level", "unknown"),
        "sample_paths": unique_paths[:5],
    }


def summarize(results: list[dict[str, Any]]) -> dict[str, Any]:
    task_results = [
        task for repo in results for task in repo.get("tasks", []) if task.get("ok")
    ]
    failed_tasks = [
        task for repo in results for task in repo.get("tasks", []) if not task.get("ok")
    ]
    scores = [task["metrics"]["score"] for task in task_results]
    repos_ok = [repo for repo in results if repo.get("ok")]
    return {
        "repos_attempted": len(results),
        "repos_indexed": len(repos_ok),
        "tasks_attempted": len(task_results) + len(failed_tasks),
        "tasks_scored": len(task_results),
        "tasks_failed": len(failed_tasks),
        "average_score": round(sum(scores) / len(scores), 1) if scores else 0.0,
        "min_score": min(scores) if scores else 0,
        "max_score": max(scores) if scores else 0,
        "pass_rate_70": round(
            100 * sum(1 for score in scores if score >= 70) / len(scores), 1
        )
        if scores
        else 0.0,
    }


def write_markdown(
    out: Path,
    repo_root: Path,
    summary: dict[str, Any],
    results: list[dict[str, Any]],
    reveal_names: bool,
    command: str,
) -> None:
    displayed_repo_root = redact_local_path(str(repo_root))
    displayed_command = redact_local_path(command)
    lines = [
        "# Usefulness Proof",
        "",
        "Generated by `scripts/prove-usefulness.py` against real local repositories.",
        "",
        "The proof measures whether `ok plan` returns grounded, actionable planning data:",
        "",
        "- Primary context exists.",
        "- Returned paths exist in the indexed repo.",
        "- At least one source file is included, not only docs or tests.",
        "- Impact candidates are found.",
        "- Validation candidates are found.",
        "- Agent tool-call recommendations are present.",
        "- Risk is classified.",
        "",
        "The report records metrics and relative paths only; source snippets are intentionally omitted.",
        "",
        "## Summary",
        "",
        f"- Repo root: `{displayed_repo_root}`",
        f"- Repositories attempted: {summary['repos_attempted']}",
        f"- Repositories indexed: {summary['repos_indexed']}",
        f"- Tasks attempted: {summary['tasks_attempted']}",
        f"- Tasks scored: {summary['tasks_scored']}",
        f"- Task failures: {summary['tasks_failed']}",
        f"- Average evidence score: {summary['average_score']}/100",
        f"- Score range: {summary['min_score']}-{summary['max_score']}/100",
        f"- Tasks scoring >= 70: {summary['pass_rate_70']}%",
        "",
        "## Results",
        "",
        "| Repo | Task | Score | Context | Source | Impact | Validation | Risk | Sample paths |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |",
    ]

    for repo_index, repo in enumerate(results, start=1):
        repo_name = repo["path"] if reveal_names else f"repo-{repo_index}"
        if not repo.get("ok"):
            lines.append(
                f"| `{repo_name}` | index | 0 | 0 | 0 | 0 | 0 | failed | `{repo.get('error', '')}` |"
            )
            continue
        for task in repo.get("tasks", []):
            if not task.get("ok"):
                lines.append(
                    f"| `{repo_name}` | `{task['task']}` | 0 | 0 | 0 | 0 | 0 | failed | `{task.get('error', '')}` |"
                )
                continue
            metrics = task["metrics"]
            sample_paths = ", ".join(f"`{path}`" for path in metrics["sample_paths"][:3])
            lines.append(
                f"| `{repo_name}` | `{task['task']}` | {metrics['score']} |"
                f" {metrics['primary_context_count']} | {metrics['source_context_count']} |"
                f" {metrics['impact_count']} | {metrics['validation_count']} |"
                f" {metrics['risk_level']} | {sample_paths} |"
            )

    lines.extend(
        [
            "",
            "## Interpretation",
            "",
            "This is an evidence harness, not a marketing benchmark. A high score means the",
            "planner found real local context, impact, validation, and agent handoff data for",
            "the task. A low score is useful too: it identifies where search terms, parser",
            "coverage, impact analysis, or test detection need improvement.",
            "",
            "## Reproduce",
            "",
            "```sh",
            "cargo build -p open-kioku-cli",
            displayed_command,
            "```",
            "",
        ]
    )
    out.write_text("\n".join(lines), encoding="utf-8")


def redact_local_path(value: str) -> str:
    home = str(Path.home())
    return value.replace(home, "~")


def report_command(args: argparse.Namespace, repo_count: int) -> str:
    if args.reveal_repo_names:
        return shlex.join([Path(sys.argv[0]).as_posix(), *sys.argv[1:]])
    repo_root = redact_local_path(str(args.repo_root)).replace("~", "$HOME", 1)
    return (
        f"{Path(sys.argv[0]).as_posix()} --repo-root {repo_root} "
        f"--max-repos {repo_count} --tasks-per-repo {args.tasks_per_repo} "
        f"--timeout {args.timeout}"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path.home() / "dev")
    parser.add_argument(
        "--repo",
        action="append",
        type=Path,
        default=[],
        help="Specific repository to include. Can be passed multiple times.",
    )
    parser.add_argument("--ok-bin", type=Path, default=Path("target/debug/ok"))
    parser.add_argument("--out", type=Path, default=Path("docs/usefulness-proof.md"))
    parser.add_argument("--json-out", type=Path, default=Path("target/usefulness-proof.json"))
    parser.add_argument("--max-repos", type=int, default=8)
    parser.add_argument("--tasks-per-repo", type=int, default=4)
    parser.add_argument("--max-files", type=int, default=15_000)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--reveal-repo-names", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = Path.cwd()
    ok_bin = args.ok_bin if args.ok_bin.is_absolute() else root / args.ok_bin

    if not ok_bin.exists():
        run_text(["cargo", "build", "-p", "open-kioku-cli"], cwd=root, timeout=600)

    if args.repo:
        repos = [
            RepoCandidate(path.resolve(), repo_file_count(path.resolve(), args.max_files))
            for path in args.repo
        ]
    else:
        repos = discover_repos(args.repo_root, args.max_repos, args.max_files)
    if not repos:
        print(f"no git repositories found under {args.repo_root}", file=sys.stderr)
        return 1

    results: list[dict[str, Any]] = []
    for candidate in repos:
        print(
            f"indexing {candidate.path} ({candidate.file_count} files before ignores)",
            flush=True,
        )
        repo_result: dict[str, Any] = {
            "path": str(candidate.path),
            "file_count": candidate.file_count,
            "ok": False,
            "tasks": [],
        }
        start = time.perf_counter()
        try:
            run_text([str(ok_bin), "index", str(candidate.path)], cwd=root, timeout=args.timeout)
            repo_result["index_seconds"] = round(time.perf_counter() - start, 2)
            repo_result["ok"] = True
            tasks = choose_tasks(ok_bin, candidate.path, args.tasks_per_repo, args.timeout)
            print(
                f"selected {len(tasks)} task(s) for {candidate.path.name}: {', '.join(tasks)}",
                flush=True,
            )
            for task in tasks:
                print(f"planning {candidate.path.name}: {task}", flush=True)
                task_start = time.perf_counter()
                try:
                    plan = run_json(
                        [
                            str(ok_bin),
                            "--repo",
                            str(candidate.path),
                            "plan",
                            task,
                            "--format",
                            "json",
                        ],
                        cwd=root,
                        timeout=args.timeout,
                    )
                    repo_result["tasks"].append(
                        {
                            "task": task,
                            "ok": True,
                            "seconds": round(time.perf_counter() - task_start, 2),
                            "metrics": score_plan(candidate.path, plan),
                        }
                    )
                except Exception as exc:
                    repo_result["tasks"].append(
                        {"task": task, "ok": False, "error": str(exc)}
                    )
        except Exception as exc:
            repo_result["error"] = str(exc)
        results.append(repo_result)

    summary = summarize(results)
    payload = {
        "summary": summary,
        "results": results,
        "task_bank": DEFAULT_TASK_BANK,
        "command": report_command(args, len(repos)),
        "scoring": {
            "primary_context": 25,
            "paths_exist": 15,
            "source_context": 15,
            "impact_candidates": 15,
            "validation_candidates": 15,
            "agent_tool_calls": 10,
            "known_risk": 5,
        },
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    write_markdown(
        args.out,
        args.repo_root,
        summary,
        results,
        args.reveal_repo_names,
        payload["command"],
    )
    print(f"wrote {args.out}")
    print(f"wrote {args.json_out}")
    print(json.dumps(summary, indent=2))
    return 0 if summary["tasks_scored"] > 0 and summary["tasks_failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
