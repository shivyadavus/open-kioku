#!/usr/bin/env python3
"""Perturb the identifiers in commit-derived cases, and report a paired run per class.

Commit subjects are written by the author who just edited the file, so they spell identifiers
in the repository's own casing. A task handed to an agent does not: it says `CollectionsUtils`
for `CollectionUtils`, `num_frames` for `NumFrames`, or simply mistypes. A near-miss resolver
measured on commit subjects will always report "no change", because there are almost no near
misses in the corpus. This builds a corpus that has them.

Generate, then score both arms with scripts/score-context-cases.py, then report:

    scripts/perturb-identifiers.py generate cases.tsv perturbed.tsv classes.tsv
    scripts/score-context-cases.py --ok BASE --repo R --cases perturbed.tsv --out base.json
    scripts/score-context-cases.py --ok NEW  --repo R --cases perturbed.tsv --out new.json
    scripts/perturb-identifiers.py report classes.tsv base.json new.json

One identifier per case is rewritten and the gold is left untouched, so the case still has the
same answer. Cases with no identifier to perturb are emitted unchanged under the class `none`:
they are the control, and a retrieval change that is meant to fire only on a near miss must
leave them at exactly 0.000.
"""
import json
import random
import re
import statistics
import sys
from collections import Counter

CLASSES = ["plural", "verb", "snake", "camel", "typo"]


def parts(value):
    """CamelCase / snake_case part spans, mirroring the code_identifiers tokenizer."""
    out, start = [], None
    for i, ch in enumerate(value):
        if not ch.isalnum():
            if start is not None:
                out.append((start, i))
                start = None
            continue
        if start is None:
            start = i
            continue
        prev = value[i - 1]
        nxt = value[i + 1] if i + 1 < len(value) else ""
        if ch.isupper() and (prev.islower() or prev.isdigit() or (prev.isupper() and nxt.islower())):
            out.append((start, i))
            start = i
    if start is not None:
        out.append((start, len(value)))
    return [(a, b) for a, b in out if b - a >= 2]


def is_identifier(tok):
    if len(tok) < 3 or not tok.isascii():
        return False
    inner_case = any((a.islower() or a.isdigit()) and b.isupper() for a, b in zip(tok, tok[1:]))
    return inner_case or "_" in tok


def pluralize(p):
    return p[:-1] if p.endswith("s") and not p.endswith("ss") else p + "s"


def verbify(p):
    """Swap one verb form for another. Bolting -ing onto a noun ("Exceptioning") is not a form
    any task description uses, so a part without a verb suffix is left alone."""
    if p.endswith("ing") and len(p) > 6:
        return p[:-3] + "ed"
    if p.endswith("ed") and len(p) > 5:
        return p[:-2] + "ing"
    return None


def typo(p):
    i = len(p) // 2
    return p[:i - 1] + p[i] + p[i - 1] + p[i + 1:] if p[i - 1] != p[i] else None


def to_snake(tok):
    if "_" in tok:
        return None
    ps = [tok[a:b] for a, b in parts(tok)]
    return "_".join(p.lower() for p in ps) if len(ps) >= 2 else None


def to_camel(tok):
    if "_" not in tok:
        return None
    ps = [p for p in tok.split("_") if p]
    return "".join(p[:1].upper() + p[1:] for p in ps) if len(ps) >= 2 else None


def perturb(tok, cls):
    if cls == "snake":
        return to_snake(tok)
    if cls == "camel":
        return to_camel(tok)
    for a, b in reversed(parts(tok)):          # the last part is the most distinctive
        p = tok[a:b]
        if not p.isalpha():
            continue
        if cls == "plural" and len(p) >= 4:
            new = pluralize(p)
        elif cls == "verb" and len(p) >= 5:
            new = verbify(p)
        elif cls == "typo" and len(p) >= 6:
            new = typo(p)
        else:
            continue
        if new and new.lower() != p.lower():
            return tok[:a] + new + tok[b:]
    return None


def generate(src, out_tsv, out_cls):
    rows = [l.rstrip("\n").split("\t") for l in open(src) if l.strip()]
    cases, classes, n = [], [], 0
    for row in rows:
        if len(row) < 4:
            continue
        sha, date, query, gold = row[0], row[1], row[2], row[3]
        toks = [t for t in re.split(r"[^A-Za-z0-9_]+", query) if is_identifier(t)]
        applied, newq = "none", query
        for off in range(len(CLASSES)):        # round-robin, falling through when inapplicable
            cls = CLASSES[(n + off) % len(CLASSES)]
            hit = next(((t, p) for t in toks for p in [perturb(t, cls)] if p), None)
            if hit:
                tok, rep = hit
                newq = query.replace(tok, rep, 1)
                applied = cls
                n += 1
                break
        cases.append("\t".join([sha, date, newq, gold]))
        classes.append("\t".join([sha, applied, query, newq]))
    open(out_tsv, "w").write("\n".join(cases) + "\n")
    open(out_cls, "w").write("\n".join(classes) + "\n")
    print(f"{src}: {Counter(c.split(chr(9))[1] for c in classes).most_common()}")


def report(class_files, base_files, new_files):
    rr = lambda r: 1.0 / r if r else 0.0
    rows = []
    for cf, bf, nf in zip(class_files, base_files, new_files):
        base = {x["sha"]: x for x in json.load(open(bf))["rows"] if "err" not in x}
        new = {x["sha"]: x for x in json.load(open(nf))["rows"] if "err" not in x}
        cls = {l.split("\t")[0]: l.split("\t")[1] for l in open(cf).read().splitlines() if l}
        rows += [(cls.get(s, "?"), base[s]["rank"], new[s]["rank"]) for s in base if s in new]

    def block(name, sel):
        s = [r for r in rows if sel(r[0])]
        if not s:
            return
        def m(i):
            rk = [r[i] for r in s]
            at = lambda k: sum(1 for x in rk if x and x <= k) / len(s)
            return at(5), at(20), sum(rr(x) for x in rk) / len(s)
        b5, b20, bm = m(1)
        n5, n20, nm = m(2)
        deltas = [rr(r[2]) - rr(r[1]) for r in s]
        random.seed(7)
        boots = sorted(statistics.mean(random.choices(deltas, k=len(deltas))) for _ in range(2000))
        better = sum(1 for d in deltas if d > 0)
        worse = sum(1 for d in deltas if d < 0)
        print(f"{name:16} n={len(s):3}  R@5 {b5:.3f}->{n5:.3f} ({n5-b5:+.3f})  "
              f"R@20 {b20:.3f}->{n20:.3f} ({n20-b20:+.3f})  MRR {bm:.3f}->{nm:.3f} ({nm-bm:+.3f})  "
              f"95% CI [{boots[50]:+.3f},{boots[-50]:+.3f}]  better {better} worse {worse}")

    print(f"cases: {len(rows)}")
    for cls in CLASSES:
        block(f"  {cls}", lambda c, k=cls: c == k)
    block("  ALL PERTURBED", lambda c: c != "none")
    block("  none (control)", lambda c: c == "none")


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ("generate", "report"):
        print(__doc__)
        return 2
    if sys.argv[1] == "generate":
        generate(*sys.argv[2:5])
        return 0
    rest = sys.argv[2:]
    if len(rest) % 3:
        print("report takes triples: classes.tsv base.json new.json ...")
        return 2
    third = len(rest) // 3
    report(rest[0::3], rest[1::3], rest[2::3]) if third == 1 else report(
        rest[:third], rest[third:2 * third], rest[2 * third:])
    return 0


if __name__ == "__main__":
    sys.exit(main())
