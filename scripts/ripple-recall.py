#!/usr/bin/env python3
"""How many of the sites a type checker says a signature change broke does `ok ripple` name?

At a repository's head, one function at a time is changed in one of two ways:

  add     - a required parameter is put first in its parameter list (after `self`/`cls`);
  rename  - its declaration is renamed, and nothing else is touched.

After each change the language's own type checker is run over the whole repository, tests
included, and `ok ripple` is run on the same working tree with no index. The checker's new
errors are the truth: the sites the change broke. The change is then reverted.

  recall     of the sites the checker reports, how many ripple counts as broken, and how many
             more it lists as possible;
  precision  of the sites ripple counts as broken, how many the checker reports.

No project code is run: the checkers read source. Checkers used:

  go          go/types over every package of the module, tests included (--go-check, a small
              program built from scripts/ripple-recall-gocheck.go; `go build` stops at the
              first package that fails, which hides the callers in the packages after it)
  python      pyright, basic mode, with its own configuration so a repository's cannot narrow it
  typescript  tsc --noEmit --strict over every .ts/.tsx file, with its own configuration and no
              installed dependencies (their types are `any`; calls inside the repository are
              still checked)

A change that the checker rejects at the declaration itself (an override or an interface
method whose signature no longer matches) is recorded as ill-formed and left out of the totals:
its callers are not what the checker then reports.
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
from pathlib import Path

SKIP_DIRS = re.compile(r"(^|/)(vendor|node_modules|third_party|dist|build|target|\.git|testdata|"
                       r"fixtures|__snapshots__|docs?|examples?|benchmarks?)(/|$)")
TEST_PATH = re.compile(r"(^|/)(tests?|testing|__tests__|spec)(/|$)|_test\.go$|(^|/)test_[^/]*\.py$|"
                       r"_test\.py$|\.(test|spec|test-d)\.tsx?$|(^|/)conftest\.py$")
EXT = {"go": (".go",), "python": (".py",), "typescript": (".ts", ".tsx")}

DECL = {
    "go": re.compile(r"^func (?:\([^)]*\) )?(?P<name>[A-Za-z_]\w*)(?:\[[^\]]*\])?\("),
    "python": re.compile(r"^\s*(?:async\s+)?def (?P<name>\w+)\("),
    "typescript": re.compile(
        r"^\s*(?:(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:async\s+)?function\s*\*?\s*"
        r"|(?:export\s+)?const\s+(?=[\w$]+\s*=\s*(?:async\s*)?(?:<[^>(]*>)?\()"
        r"|(?:(?:public|private|protected|static|async|override)\s+)*)"
        r"(?P<name>[A-Za-z_$][\w$]*)\s*(?:=\s*(?:async\s*)?)?(?:<[^>(]*>)?\("),
}
TS_NOT_A_NAME = {"if", "for", "while", "switch", "catch", "return", "function", "await", "typeof",
                 "super", "new", "throw", "else", "do", "with", "yield", "delete", "void", "in",
                 "of", "case", "expect", "describe", "it", "test", "async", "get", "set",
                 "static", "constructor"}
ADDED = {"go": "ripAdded int", "python": "rip_added", "typescript": "ripAdded: number"}

# Errors that say a call no longer fits the changed declaration: counted as its sites.
ARITY = {
    "go": re.compile(r"(not enough|too many) arguments in call to (\S+\.)?{name}\b"),
    "python": re.compile(r"Arguments? missing for parameters? |Expected \d+ (more )?positional"),
    "typescript": re.compile(r"TS2554|TS2555"),
}
# Errors that say a name no longer resolves: its sites when they name the changed declaration.
# A checker's follow-on errors (a type that no longer narrows, a value now `any`) do not.
UNKNOWN = {
    "go": re.compile(r"^undefined: (\S+\.)?{name}$|\.{name} undefined \("),
    "python": re.compile(r"\"{name}\" is unknown import symbol|"
                         r"Cannot access attribute \"{name}\"|"
                         r"\"{name}\" is not a known attribute of module|"
                         r"\"{name}\" is not defined"),
    "typescript": re.compile(r"TS(2305|2339|2304|2551|2724|2614|2552) .*'{name}'"),
}


# Errors that say the changed declaration no longer agrees with another declaration of it (an
# overload, the member of a base it overrides). Such a change is ill-formed in itself.
DISAGREES = {
    "go": re.compile(r"$^"),
    "python": re.compile(r"overrides class|is incompatible with|Overload"),
    "typescript": re.compile(r"TS2394|TS2416|TS2420|TS2425|TS2426|TS2415"),
}


# Paths the checker does not read: a Go directory whose name starts with `_` or `.` is not part
# of `./...`. What ripple names there the checker can neither confirm nor refute.
UNREAD = {"go": re.compile(r"(^|/)[_.][^/]+/"), "python": re.compile(r"$^"),
          "typescript": re.compile(r"$^")}


def direct(lang, name):
    """The pattern of a checker error that is a site of the declaration called `name`."""
    escaped = re.escape(name)
    return re.compile("|".join(pattern.pattern.replace("{name}", escaped)
                               for pattern in (ARITY[lang], UNKNOWN[lang])))


def run(args, cwd=None, timeout=900):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, errors="replace",
                         timeout=timeout)
    return out.returncode, out.stdout, out.stderr


def tracked(repo, lang):
    _, out, _ = run(["git", "-C", repo, "ls-files"])
    return [p for p in out.splitlines() if p.endswith(EXT[lang]) and not SKIP_DIRS.search(p)
            and not p.endswith(".d.ts")]


def candidates(repo, lang):
    """Declarations of non-test code whose name is used on some other line of the repository."""
    found = []
    for path in tracked(repo, lang):
        if TEST_PATH.search(path):
            continue
        try:
            lines = Path(repo, path).read_text(errors="replace").split("\n")
        except OSError:
            continue
        for index, line in enumerate(lines):
            match = DECL[lang].match(line)
            if not match:
                continue
            name = match.group("name")
            if lang == "typescript":
                head = line[:match.start("name")]
                if name in TS_NOT_A_NAME or (not head.strip() and not line.rstrip().endswith("{")):
                    continue  # a call at the start of a line, not a method
                if name == "constructor":
                    continue
            if lang == "python" and name.startswith("__") and name != "__init__":
                continue
            if lang == "go" and (name in ("main", "init") or name.startswith("Test")):
                continue
            if len(name) < 3:
                continue
            found.append({"path": path, "line": index + 1, "name": name,
                          "open": match.end()})
    by_name = {}
    for item in found:
        by_name.setdefault(item["name"], []).append(item)
    kept = []
    for name, items in by_name.items():
        code, out, _ = run(["git", "-C", repo, "grep", "-n", "-w", "-F", "-I", "-c", name])
        uses = sum(int(row.rsplit(":", 1)[1]) for row in out.splitlines() if ":" in row)
        if uses > len(items):
            for item in items:
                item["lines_holding_the_name"] = uses
                item["declarations_of_the_name"] = len(items)
                kept.append(item)
    return kept


def mutate(lang, text, item, kind):
    """The file's text with one declaration changed, or None when this one cannot be."""
    lines = text.split("\n")
    offset = sum(len(row) + 1 for row in lines[:item["line"] - 1])
    start = offset + item["open"]
    name = item["name"]
    if kind == "rename":
        if name in ("__init__",):
            return None
        new = {"go": name + "Ripx", "python": name + "_ripx", "typescript": name + "Ripx"}[lang]
        at = text.rfind(name, offset, start)
        return text[:at] + new + text[at + len(name):]
    rest = text[start:]
    stripped = rest.lstrip()
    gap = len(rest) - len(stripped)
    if lang == "python":
        first = re.match(r"(self|cls)\s*", stripped)
        if first:
            after = stripped[first.end():]
            at = start + gap + first.end()
            if after.startswith(","):
                return text[:at + 1] + " rip_added," + text[at + 1:]
            if after.startswith(")"):
                return text[:at] + ", rip_added" + text[at:]
            return None
    if lang == "typescript" and re.match(r"this\s*:", stripped):
        return None
    added = ADDED[lang]
    if stripped.startswith(")"):
        return text[:start] + added + text[start:]
    return text[:start] + added + ", " + text[start:]


def check_go(repo, args):
    code, out, err = run([args.go_check, repo])
    if code not in (0, 1) and not out:
        raise RuntimeError(f"go check failed: {err[-300:]}")
    rows = []
    for row in out.splitlines():
        where, _, message = row.partition("\t")
        parts = where.rsplit(":", 2)
        if len(parts) == 3 and parts[1].isdigit():
            rows.append((os.path.relpath(parts[0], repo), int(parts[1]), message))
    return rows


def check_python(repo, args):
    config = Path(args.scratch, "pyrightconfig.json")
    if not config.exists():
        # pyright takes `include` only relative to its configuration file.
        near = os.path.relpath(repo, args.scratch)
        extra = [near] + [os.path.join(near, d) for d in ("src", "lib") if Path(repo, d).is_dir()]
        config.write_text(json.dumps({
            "include": [near], "exclude": ["**/node_modules", "**/.git", "**/.venv", "**/build"],
            "extraPaths": extra, "typeCheckingMode": "basic", "reportMissingImports": False,
            "reportMissingModuleSource": False, "reportAttributeAccessIssue": "error",
            "reportCallIssue": "error", "reportOptionalMemberAccess": False,
        }))
    code, out, err = run(["pyright", "--outputjson", "-p", str(config)])
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        raise RuntimeError(f"pyright failed: {(err or out)[-300:]}")
    if not data.get("summary", {}).get("filesAnalyzed"):
        raise RuntimeError("pyright analyzed no file")
    rows = []
    for item in data.get("generalDiagnostics", []):
        if item.get("severity") != "error":
            continue
        rows.append((os.path.relpath(item["file"], repo), item["range"]["start"]["line"] + 1,
                     item["message"].split("\n")[0]))
    return rows


def check_typescript(repo, args):
    config = Path(args.scratch, "tsconfig.json")
    if not config.exists():
        config.write_text(json.dumps({
            "compilerOptions": {"noEmit": True, "strict": True, "target": "es2022",
                                "module": "nodenext", "moduleResolution": "nodenext",
                                "skipLibCheck": True, "types": [], "jsx": "preserve",
                                "allowImportingTsExtensions": True},
            "include": [repo + "/**/*.ts", repo + "/**/*.tsx"],
            "exclude": [repo + "/**/node_modules", repo + "/**/dist", repo + "/**/*.d.ts"],
        }))
    # tsc prints paths relative to the directory it is run in.
    code, out, err = run(["tsc", "--pretty", "false", "-p", str(config)], cwd=repo)
    rows = []
    for row in out.splitlines():
        match = re.match(r"(.+?)\((\d+),\d+\): error (TS\d+): (.*)", row)
        if match:
            path = os.path.relpath(os.path.join(repo, match.group(1)), repo)
            rows.append((path, int(match.group(2)), f"{match.group(3)} {match.group(4)}"))
    return rows


CHECK = {"go": check_go, "python": check_python, "typescript": check_typescript}


def ripple(repo, ok_bin, index=False):
    code, out, err = run([ok_bin, "--repo", repo, "ripple", "--json", "--sites", "100000"]
                         + ([] if index else ["--no-index"]))
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return {"contracts": [], "error": (err or out)[-300:]}


def pair(truth, sites):
    """Matches each checker error with a ripple site in the same file on the same call.

    A checker may put an error on a later line of a call that ripple anchors at its name."""
    matched, left = [], list(sites)
    for path, line in sorted(truth):
        best = None
        for site in left:
            if site[0] == path and 0 <= line - site[1] <= 12:
                if best is None or line - site[1] < line - best[1]:
                    best = site
        if best is not None:
            left.remove(best)
            matched.append(((path, line), best))
    return matched, left


def score(report, errors, item, kind, lang):
    """One change, one ripple report: what the checker reports against what ripple names."""
    at_declaration = [m for p, l, m in errors if p == item["path"]
                      and (l == item["line"] or DISAGREES[lang].search(m))]
    is_site = direct(lang, item["name"])
    sites = sorted({(p, l) for p, l, m in errors if is_site.search(m)
                    and not (p == item["path"] and l == item["line"])})
    other = sorted({(p, l) for p, l, m in errors} - set(sites) - {(item["path"], item["line"])})
    contracts = [c for c in report.get("contracts", []) if c["name"] == item["name"]
                 or (c.get("owner") and item["name"] in ("__init__",))]
    broken = [(s["path"], s["line"]) for c in contracts for s in c.get("broken", [])]
    possible = [(s["path"], s["line"]) for c in contracts for s in c.get("possible", [])]
    hit, broken_left = pair(sites, broken)
    missed = [t for t in sites if t not in [m[0] for m in hit]]
    listed, _ = pair(missed, possible)
    unlisted = [t for t in missed if t not in [m[0] for m in listed]]
    # A broken site the checker did not report as a site may still be one of its errors.
    also, _ = pair(other, broken_left)
    left = [s for s in broken_left if s not in [m[1] for m in also]]
    # A checker reports a name that no longer resolves once, at the import, and is then
    # silent on every use of it in that file; ripple lists each use.
    reported_files = {p for p, _ in sites}
    same_file = [s for s in left if s[0] in reported_files]
    unread = [s for s in left if s[0] not in reported_files and UNREAD[lang].search(s[0])]
    unexplained = [s for s in left if s[0] not in reported_files and s not in unread]
    return {
        **{k: item[k] for k in ("path", "line", "name", "lines_holding_the_name",
                                 "declarations_of_the_name")},
        "change": kind,
        "ill_formed": at_declaration[:2],
        "ripple_saw_the_change": bool(contracts),
        "effect": contracts[0]["effect"] if contracts else None,
        "checker_sites": len(sites), "checker_other_errors": len(other),
        "ripple_broken": len(broken), "ripple_possible": len(possible),
        "named": len(hit), "listed_as_possible": len(listed), "not_listed": len(unlisted),
        "broken_the_checker_reports_otherwise": len(also),
        "broken_in_a_file_the_checker_reports_the_name_in": len(same_file),
        "broken_where_the_checker_does_not_read": len(unread),
        "broken_the_checker_does_not_report": len(unexplained),
        "examples_not_listed": [f"{p}:{l}" for p, l in unlisted[:6]],
        "examples_listed_as_possible": [f"{m[0][0]}:{m[0][1]}" for m in listed[:6]],
        "examples_checker_does_not_report": [f"{p}:{l}" for p, l in unexplained[:6]],
        "checker_messages": sorted({m for _, _, m in errors})[:4],
    }


TOTALLED = ("checker_sites", "named", "listed_as_possible", "not_listed", "ripple_broken",
            "broken_the_checker_reports_otherwise",
            "broken_in_a_file_the_checker_reports_the_name_in",
            "broken_where_the_checker_does_not_read",
            "broken_the_checker_does_not_report")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--repo", required=True, help="a scratch clone at the revision to measure")
    ap.add_argument("--language", required=True, choices=sorted(EXT))
    ap.add_argument("--ok-bin", required=True, nargs="+", metavar="[LABEL=]PATH",
                    help="one binary, or several to score against the same checker runs")
    ap.add_argument("--go-check", help="the program built from scripts/ripple-recall-gocheck.go")
    ap.add_argument("--changes", type=int, default=40, help="declarations to change, per kind")
    ap.add_argument("--kinds", nargs="+", default=["add", "rename"], choices=["add", "rename"])
    ap.add_argument("--index", action="store_true",
                    help="build an index at the head first and let ripple read it")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", required=True)
    ap.add_argument("--label")
    args = ap.parse_args()
    repo = str(Path(args.repo).resolve())
    lang = args.language
    bins = {}
    for spec in args.ok_bin:
        label, _, path = spec.rpartition("=")
        bins[label or "ripple"] = str(Path(path).resolve())
    args.scratch = tempfile.mkdtemp(prefix="ripple-recall-")
    exclude = Path(repo, ".git", "info", "exclude")
    if ".ok/" not in exclude.read_text().split():
        exclude.write_text(exclude.read_text() + "\n.ok/\n")
    code, dirty, _ = run(["git", "-C", repo, "status", "--porcelain"])
    if dirty.strip():
        sys.exit(f"{repo} has uncommitted changes; use a scratch clone")
    _, head, _ = run(["git", "-C", repo, "rev-parse", "HEAD"])

    if args.index:
        code, out, err = run([next(iter(bins.values())), "--repo", repo, "index"])
        if code != 0:
            sys.exit(f"ok index failed: {err[-300:]}")
    baseline = set(CHECK[lang](repo, args))
    pool = candidates(repo, lang)
    random.Random(args.seed).shuffle(pool)
    runs = {label: {"totals": {}, "changes": []} for label in bins}

    for kind in args.kinds:
        done = 0
        for item in pool:
            if done >= args.changes:
                break
            file = Path(repo, item["path"])
            original = file.read_text(errors="replace")
            changed = mutate(lang, original, item, kind)
            if changed is None or changed == original:
                continue
            file.write_text(changed)
            try:
                reports = {label: ripple(repo, path, args.index) for label, path in bins.items()}
                errors = [row for row in CHECK[lang](repo, args) if row not in baseline]
            finally:
                file.write_text(original)
            done += 1
            line = f"{kind:6} {item['path']}:{item['line']} {item['name']}:"
            for label, report in reports.items():
                record = score(report, errors, item, kind, lang)
                totals = runs[label]["totals"]
                runs[label]["changes"].append(record)

                def add(key, amount=1):
                    totals[key] = totals.get(key, 0) + amount

                if record["ill_formed"]:
                    add(f"{kind}_ill_formed")
                    continue
                add(f"{kind}_changes")
                add(f"{kind}_changes_ripple_saw", int(record["ripple_saw_the_change"]))
                add(f"{kind}_changes_with_a_checker_site", int(bool(record["checker_sites"])))
                for key in TOTALLED:
                    add(f"{kind}_{key}", record[key])
                line += (f" [{label}] checker {record['checker_sites']}, named {record['named']}, "
                         f"possible {record['listed_as_possible']}, not listed "
                         f"{record['not_listed']}, checker silent on "
                         f"{record['broken_the_checker_does_not_report']}")
            print(line, flush=True)
    result = {"repository": args.label or Path(repo).name, "language": lang,
              "index": bool(args.index), "head": head.strip(), "seed": args.seed,
              "changes_per_kind": args.changes, "baseline_errors": len(baseline),
              "declarations_in_the_pool": len(pool), "runs": runs}
    Path(args.out).write_text(json.dumps(result, indent=1) + "\n")
    for label, run_ in runs.items():
        print(label, json.dumps(run_["totals"]))


if __name__ == "__main__":
    main()
