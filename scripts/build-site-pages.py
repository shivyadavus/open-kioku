#!/usr/bin/env python3
"""Writes the site's inner pages from one shell: docs, the four agent guides, and the replay.

    scripts/build-site-pages.py           write the pages into demo/
    scripts/build-site-pages.py --check   fail if a page in demo/ differs from what this writes

The replay page is built from the output `scripts/demo-replay.sh --write` recorded in
demo/proof/replay-black/, so the page cannot say something the tool did not print.
"""
import html
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "demo"
PROOF = OUT / "proof" / "replay-black"
REPO = "https://github.com/shivyadavus/open-kioku"
LOGO = ('<svg viewBox="0 0 64 64" aria-hidden="true"><ellipse cx="32" cy="32" rx="26" ry="15.5" fill="none" '
        'stroke="currentColor" stroke-width="3.4"/><path d="M25.5 24.5 18 32l7.5 7.5M38.5 24.5 46 32l-7.5 7.5" '
        'fill="none" stroke="currentColor" stroke-width="4.2" stroke-linecap="round" stroke-linejoin="round"/>'
        '<circle cx="32" cy="32" r="3.1" fill="currentColor"/></svg>')
PROMPT = ("Use Open Kioku before editing. Check repo_status, search_code, get_definition,\n"
          "get_references, impact_analysis, and find_tests_for_change. Build a plan with\n"
          "plan_change first, then edit, and verify after the edit with verify_change.")


def e(text):
    return html.escape(text, quote=True)


def code(text, label=None, copy=None, prose=False):
    head = f'<span class="label">{e(label)}</span>' if label else ""
    button = f'<button type="button" data-copy="{e(copy if copy is not None else text)}">Copy</button>'
    kind = "code prose" if prose else "code"
    return f'<div class="{kind}">{head}<pre>{e(text)}</pre>{button}</div>'


def shell(*, path, title, description, current, body):
    nav = [("index.html#how", "How it works", ""), ("index.html#ripple", "Ripple", ""),
           ("see-it-work.html", "See it work", "replay"), ("index.html#measured", "Measured", ""), ("docs.html", "Docs", "docs")]
    links = "\n      ".join(
        f'<a href="{href}"{" aria-current=\"page\"" if key and key == current else ""}>{label}</a>'
        for href, label, key in nav)
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="description" content="{e(description)}" />
<meta name="theme-color" content="#0b0d0c" />
<link rel="canonical" href="https://www.openkioku.com/{path}" />
<meta property="og:type" content="website" />
<meta property="og:site_name" content="Open Kioku" />
<meta property="og:title" content="{e(title)}" />
<meta property="og:description" content="{e(description)}" />
<meta property="og:url" content="https://www.openkioku.com/{path}" />
<meta property="og:image" content="https://www.openkioku.com/og-image.png" />
<meta name="twitter:card" content="summary_large_image" />
<title>{e(title)}</title>
<link rel="icon" href="favicon.svg" type="image/svg+xml" />
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400..700&family=JetBrains+Mono:wght@400;500;600&display=swap" />
<link rel="stylesheet" href="site.css" />
</head>
<body id="top" class="inner">
<a class="skip" href="#main">Skip to content</a>

<header class="top">
  <div class="wrap">
    <a href="index.html" class="brand" aria-label="Open Kioku home">
      {LOGO}
      <span>Open Kioku</span>
    </a>
    <button class="nav-toggle" type="button" aria-label="Open navigation" aria-expanded="false" aria-controls="nav"><span></span></button>
    <nav class="nav" id="nav" aria-label="Primary">
      {links}
      <a class="nav-cta" href="{REPO}">View source ↗</a>
    </nav>
  </div>
</header>

<main id="main">
{body}
</main>

<footer>
  <div class="wrap">
    <span>Open Kioku · local repository evidence for coding agents</span>
    <nav aria-label="Footer">
      <a href="index.html">Home</a>
      <a href="docs.html">Docs</a>
      <a href="{REPO}">Source</a>
      <a href="{REPO}/blob/main/CHANGELOG.md">Changelog</a>
      <a href="{REPO}/blob/main/docs/security-model.md">Security model</a>
    </nav>
  </div>
</footer>

<script src="site.js" defer></script>
</body>
</html>
"""


TOOLS = [
    ("plan_change", "An edit plan with primary context, impact candidates, validation targets and the edit boundary."),
    ("verify_change", "The finished change checked against the plan's boundary."),
    ("build_context_pack", "The files, symbols and tests for one task, with confidence and caveats."),
    ("search_code", "Ranked code search with paths, line ranges and confidence."),
    ("get_definition", "Where a symbol is defined, from the local symbol index."),
    ("get_references", "Where a symbol is used, from indexed relationship edges."),
    ("impact_analysis", "What a change to a file or symbol reaches, proven edges apart from possible ones."),
    ("find_tests_for_change", "The tests that cover what is about to change."),
    ("repo_status", "Index health, coverage by language, and what was skipped and why."),
]

AGENTS = {
    "claude-code": {
        "name": "Claude Code",
        "title": "How to Set Up Open Kioku MCP Server for Claude Code | openkioku.com",
        "description": "Connect Open Kioku to Claude Code as a local MCP server: evidence before edits, the calls a change broke right after it, and the diff verified against the plan. Nothing leaves your machine.",
        "connect": ("One command indexes the repository, writes a repository-scoped <code>.mcp.json</code> and managed guidance, "
                    "and checks that the local server answers. Run it without <code>--apply</code> to preview; nothing is written."),
        "command": "ok setup agent claude --repo . --apply",
        "manual": "ok mcp install claude --repo .",
        "manual_note": "Or print the server entry and paste it into your Claude Code MCP settings:",
        "config_label": ".mcp.json",
        "config": '{\n  "mcpServers": {\n    "open-kioku": {\n      "command": "ok",\n      "args": ["mcp", "serve", "--repo", "/absolute/path/to/repo", "--read-only"]\n    }\n  }\n}',
        "prompt_label": "CLAUDE.md",
        "prompt_note": "Add this to <code>CLAUDE.md</code>. The setup command writes managed guidance of its own; this is the short form.",
        "extra": ("<li><div class=\"body\"><h3>Turn on the hooks</h3>"
                  "<p>With the hooks, a boundary is planned from each prompt, a secret-like path is refused, and after an edit "
                  "the agent is told which calls elsewhere no longer fit what it changed. The Claude Code plugin installs them for you.</p>"
                  + code("ok gate install claude --apply") + "</div></li>"),
    },
    "cursor": {
        "name": "Cursor",
        "title": "How to Set Up Open Kioku MCP Server for Cursor | openkioku.com",
        "description": "Connect Open Kioku to Cursor as a local MCP server: evidence before edits and the diff verified against the plan. No hosted index; nothing leaves your machine.",
        "connect": ("One command indexes the repository, writes a repository-scoped <code>.cursor/mcp.json</code> and managed guidance, "
                    "and checks that the local server answers. Run it without <code>--apply</code> to preview; nothing is written."),
        "command": "ok setup agent cursor --repo . --apply",
        "manual": "ok mcp install cursor --repo .",
        "manual_note": "Or print the server entry and paste it under Settings → MCP → Edit in mcp.json:",
        "config_label": "mcp.json",
        "config": '{\n  "open-kioku": {\n    "command": "ok",\n    "args": ["mcp", "serve", "--repo", "/absolute/path/to/repo", "--read-only"]\n  }\n}',
        "prompt_label": ".cursorrules",
        "prompt_note": "Add this to your rules. The setup command writes managed guidance of its own; this is the short form.",
        "extra": "",
    },
    "codex": {
        "name": "Codex",
        "title": "How to Set Up Open Kioku MCP Server for Codex | openkioku.com",
        "description": "Connect Open Kioku to Codex as a local MCP server: evidence before edits and the diff verified against the plan. No hosted index; nothing leaves your machine.",
        "connect": ("Index the repository, then print the server entry for it. <code>ok setup agent --apply</code> is wired for "
                    "Claude Code and Cursor; for Codex the entry below is the supported path."),
        "command": "ok index .\nok mcp install codex --repo .",
        "manual": None,
        "manual_note": "Paste what it prints into <code>~/.codex/config.toml</code>, or a trusted project's <code>.codex/config.toml</code>:",
        "config_label": "config.toml",
        "config": '[mcp_servers.open-kioku]\ncommand = "ok"\nargs = ["mcp", "serve", "--repo", "/absolute/path/to/repo", "--read-only"]\nenabled = true',
        "prompt_label": "Instructions",
        "prompt_note": "Add this to your Codex instructions, or paste it at the start of a session.",
        "extra": "",
    },
    "gemini-cli": {
        "name": "Gemini CLI",
        "title": "How to Set Up Open Kioku MCP Server for Gemini CLI | openkioku.com",
        "description": "Connect Open Kioku to Gemini CLI as a local MCP server: evidence before edits and the diff verified against the plan. No hosted index; nothing leaves your machine.",
        "connect": ("Index the repository, then print the server entry for it. <code>ok setup agent --apply</code> is wired for "
                    "Claude Code and Cursor; for Gemini CLI the entry below is the supported path."),
        "command": "ok index .\nok mcp install gemini --repo .",
        "manual": None,
        "manual_note": "Paste what it prints into <code>.gemini/settings.json</code> or <code>~/.gemini/settings.json</code>:",
        "config_label": "settings.json",
        "config": '{\n  "mcpServers": {\n    "open-kioku": {\n      "command": "ok",\n      "args": ["mcp", "serve", "--repo", "/absolute/path/to/repo", "--read-only"],\n      "trust": false\n    }\n  }\n}',
        "prompt_label": "GEMINI.md",
        "prompt_note": "Add this to <code>GEMINI.md</code>, or paste it at the start of a session.",
        "extra": "",
    },
}


def agent_page(key, agent):
    name = agent["name"]
    manual = ""
    if agent["manual"]:
        manual = f'<p class="small muted">{agent["manual_note"]}</p>' + code(agent["manual"]) + code(agent["config"], agent["config_label"])
    else:
        manual = f'<p class="small muted">{agent["manual_note"]}</p>' + code(agent["config"], agent["config_label"])
    others = "".join(
        f'<a class="card" href="{other}-setup.html"><h3>{data["name"]}</h3><span class="go">Guide →</span></a>'
        for other, data in AGENTS.items() if other != key)
    tools = "".join(f"<tr><td><code>{tool}</code></td><td class=\"wrap-cell\">{e(text)}</td></tr>" for tool, text in TOOLS)
    body = f"""
<div class="page-head">
  <div class="wrap">
    <p class="crumbs"><a href="index.html">Home</a> › <a href="docs.html">Docs</a> › {name}</p>
    <span class="eyebrow">{name} + Open Kioku</span>
    <h1>Give {name} something to read before it edits.</h1>
    <p class="lede">Open Kioku runs as a local MCP server. {name} asks it what a task touches, plans inside a declared boundary, and has the finished diff checked against that plan. Nothing leaves your machine.</p>
  </div>
</div>

<section>
  <div class="wrap split">
    <div class="head">
      <span class="eyebrow">Setup</span>
      <h2>Install, connect, ask for a plan.</h2>
      <p>Indexing time depends on the size of the repository; a few thousand files take seconds. Everything is stored under the repository's own <code>.ok/</code> directory.</p>
      <ul class="ticks">
        <li><code>cargo install open-kioku-cli</code>, or <code>cargo binstall</code> for the prebuilt binary.</li>
        <li>Binaries with checksums, an SBOM and provenance on <a href="{REPO}/blob/main/docs/release-trust.md">GitHub releases ↗</a>.</li>
        <li>The server is read-only by default: source edits remain in {name}'s own workflow.</li>
      </ul>
    </div>
    <ol class="steps">
      <li><div class="body"><h3>Install Open Kioku</h3><p>The npm package pulls the native binary for macOS, Linux or Windows.</p>{code("npm install -g open-kioku")}</div></li>
      <li><div class="body"><h3>Connect {name}</h3><p>{agent["connect"]}</p>{code(agent["command"])}{manual}</div></li>
      <li><div class="body"><h3>Tell the agent to use it</h3><p>{agent["prompt_note"]}</p>{code(PROMPT.replace(chr(10), " "), agent["prompt_label"], prose=True)}</div></li>
      {agent["extra"]}
    </ol>
  </div>
</section>

<section>
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">What {name} can ask</span>
      <h2>Sixteen tools, each answering one question.</h2>
      <p>These are the ones an edit usually needs. Every tool runs against the local index and says what it could not find.</p>
    </div>
    <div class="table"><table><thead><tr><th>Tool</th><th>What it answers</th></tr></thead><tbody>{tools}</tbody></table></div>
    <p class="note">Measured every night on four real repositories: on a TypeScript repository of about nine hundred files, the right file is in the top five about four times in five. <a href="index.html#measured">The numbers ↗</a> · <a href="{REPO}/blob/main/docs/mcp-tools.md">All sixteen tools ↗</a></p>
  </div>
</section>

<section>
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">Other agents</span>
      <h2>The same server, a different client.</h2>
    </div>
    <div class="grid cols-3">{others}</div>
  </div>
</section>
"""
    return shell(path=f"{key}-setup.html", title=agent["title"], description=agent["description"], current="", body=body)


def docs_page():
    guides = "".join(
        f'<a class="card" href="{key}-setup.html"><h3>{agent["name"]}</h3><span class="go">Guide →</span></a>'
        for key, agent in AGENTS.items())
    reference = [
        ("docs/ripple.md", "Ripple", "What a change breaks outside itself, how a site is attributed, and what it does not do."),
        ("docs/gate.md", "The gate", "Hooks, the Change Dossier, <code>ok gate ci</code>, modes and settings."),
        ("docs/mcp-tools.md", "MCP tools", "The sixteen tools, their inputs, and where every retired name went."),
        ("docs/ranking.md", "Ranking", "How context is ranked and what each confidence label means."),
        ("docs/security-model.md", "Security model", "What is read, what is written, what is refused."),
        ("docs/retrieval-benchmark.md", "Retrieval benchmark", "Corpora, splits, scripts and the nightly gate."),
        ("benchmarks/ripple/README.md", "Ripple, measured", "The replay on merged history and the comparison with type checkers."),
        ("STABILITY.md", "Stability", "What is stable, what is experimental, and how that changes."),
        ("CHANGELOG.md", "Changelog", "Every release, with upgrade notes."),
    ]
    refs = "".join(
        f'<a class="card" href="{REPO}/blob/main/{path}"><h3>{title} ↗</h3><p>{text}</p></a>'
        for path, title, text in reference)
    body = f"""
<div class="page-head">
  <div class="wrap">
    <p class="crumbs"><a href="index.html">Home</a> › Docs</p>
    <span class="eyebrow">Documentation</span>
    <h1>Start with evidence. Keep uncertainty visible.</h1>
    <p class="lede">Install Open Kioku, connect your coding agent over local MCP, and use the plan, edit, verify loop on the repository you already have.</p>
  </div>
</div>

<section id="quickstart">
  <div class="wrap split">
    <div class="head">
      <span class="eyebrow">First win</span>
      <h2>Install and connect.</h2>
      <p>The setup command indexes the current repository, writes repository-scoped agent guidance and MCP configuration, and checks that the local server responds.</p>
      <p class="small muted">Use <code>claude</code> for Claude Code; <code>--apply</code> is wired for those two clients. <code>ok mcp install codex|gemini --repo .</code> prints a read-only server entry for the others. Run without <code>--apply</code> to preview; nothing is written.</p>
    </div>
    <div class="grid" style="align-content:start">
      {code("npm install -g open-kioku\nok setup agent cursor --repo . --apply")}
      <div class="grid cols-2">{guides}</div>
    </div>
  </div>
</section>

<section id="workflow">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">Core workflow</span>
      <h2>Plan before editing. Check after.</h2>
    </div>
    <div class="grid cols-4">
      <article class="card"><span class="step">01 · Index</span><h3>Build local evidence</h3><p><code>ok index .</code> stores symbols, relationships, tests and history under <code>.ok/</code>.</p></article>
      <article class="card"><span class="step">02 · Plan</span><h3>Declare the boundary</h3><p><code>ok plan "your task"</code> returns bounded context, impact, validation targets and caveats.</p></article>
      <article class="card"><span class="step">03 · Edit</span><h3>See what it broke</h3><p><code>ok ripple</code> names the calls that no longer fit what the edit changed. It needs no index.</p></article>
      <article class="card"><span class="step">04 · Verify</span><h3>Check the diff</h3><p><code>ok verify --plan plan.json --git</code> compares the changed files with the declared boundary.</p></article>
    </div>
  </div>
</section>

<section id="install">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">Install channels</span>
      <h2>The same binary, from the channel you already use.</h2>
    </div>
    <div class="table"><table>
      <thead><tr><th>Channel</th><th>Command or file</th><th>Notes</th></tr></thead>
      <tbody>
        <tr><td>npm</td><td><code>npm install -g open-kioku</code></td><td class="wrap-cell">Pulls the native binary for macOS, Linux or Windows.</td></tr>
        <tr><td>crates.io</td><td><code>cargo install open-kioku-cli</code></td><td class="wrap-cell">Or <code>cargo binstall open-kioku-cli</code> for the prebuilt binary.</td></tr>
        <tr><td>GitHub releases</td><td><code>ok-{{linux,macos,windows}}-*</code></td><td class="wrap-cell">With <code>SHA256SUMS</code>, an SBOM, <code>PROVENANCE.json</code> and build-provenance attestations. <a href="{REPO}/blob/main/docs/release-trust.md">Release trust ↗</a></td></tr>
        <tr><td>Claude Code plugin</td><td><code>.claude-plugin/</code></td><td class="wrap-cell">Plugin manifest and marketplace entry; installs the hooks.</td></tr>
        <tr><td>Cursor and Codex</td><td><code>.cursor-plugin/</code> · <code>.codex-plugin/</code></td><td class="wrap-cell">Agent, rules and skill for Cursor; plugin manifest for Codex.</td></tr>
      </tbody>
    </table></div>
  </div>
</section>

<section id="reference">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">Reference</span>
      <h2>The documents behind every claim.</h2>
      <p>They live in the repository, next to the code and the measurements they describe.</p>
    </div>
    <div class="grid cols-3">{refs}</div>
  </div>
</section>
"""
    return shell(path="docs.html", title="Open Kioku Docs — install, connect, plan, verify",
                 description="Install Open Kioku, connect your coding agent over local MCP, and use the plan, edit, verify loop on the repository you already have.",
                 current="docs", body=body)


COMMIT = "1ed66a8b3385a000508a03158ac3dc2a8a75a209"
COMMIT_URL = f"https://github.com/psf/black/commit/{COMMIT}"
PROOF_URL = f"{REPO}/blob/main/demo/proof/replay-black"


def paint(text):
    """Terminal text as HTML: the same characters, with the site's four terminal colours."""
    lines = []
    for line in text.rstrip("\n").split("\n"):
        site = re.match(r"^(\s+)(\S+/\S+:\d+)( \(in [^)]*\))?(.*)$", line)
        if line.startswith("$ "):
            lines.append(f'<span class="c">$</span> <span class="w">{e(line[2:])}</span>')
        elif line.startswith(("Ripple:", "Open Kioku:")):
            lines.append(re.sub(r"(\d+ (?:call )?site\(s\))", r'<span class="b">\1</span>', e(line)))
        elif re.match(r"\s+(- |was: )", line):
            lines.append(f'<span class="c">{e(line)}</span>')
        elif re.match(r"\s+(\+ |now: )", line):
            lines.append(f'<span class="g">{e(line)}</span>')
        elif site:
            indent, where, within, rest = site.groups()
            lines.append(f'{indent}<span class="w">{e(where)}</span>'
                         f'<span class="c">{e(within or "")}</span>{e(rest)}')
        else:
            lines.append(e(line))
    return "\n".join(lines)


def term(title, text):
    return (f'<div class="term"><div class="term-bar"><i></i><i></i><i></i><span>{e(title)}</span></div>\n'
            f'<pre tabindex="0">{paint(text)}</pre>\n</div>')


def inline(markdown):
    """One line of the dossier's Markdown as HTML: escapes, code spans and bold."""
    text = e(re.sub(r"\\(.)", r"\1", markdown))
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    return re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)


def replay_page():
    ripple = (PROOF / "ripple.txt").read_text(encoding="utf-8")
    seconds = f'{float((PROOF / "seconds.txt").read_text(encoding="utf-8")):.1f}'
    key = (PROOF / "answer-key.txt").read_text(encoding="utf-8").strip().split("\n")
    total = re.match(r"(\d+) of (\d+) ", key[-1])
    if not total or total.group(1) != total.group(2):
        raise SystemExit("the recorded answer key no longer says every named site was changed")
    named = total.group(2)
    rows = "".join(
        f'<tr><td><code>{e(site)}</code></td><td>{e(verdict)}</td></tr>'
        for site, verdict in (line.split("  ", 1) for line in key[:-1]))
    dossier = [line for line in (PROOF / "dossier-ripple.md").read_text(encoding="utf-8").split("\n")
               if line.startswith(("- ", "  - "))]
    summary = inline(dossier[0][2:])
    found = "".join(f"<li>{inline(line[4:])}</li>" for line in dossier[1:])

    session_dir = PROOF / "claude-code-session"
    session = json.loads((session_dir / "session.json").read_text(encoding="utf-8"))
    first = (session_dir / "notice-1.txt").read_text(encoding="utf-8")
    second = (session_dir / "notice-2.txt").read_text(encoding="utf-8").split("\n")[0]
    counts = [re.search(r"(\d+) call site", text).group(1) for text in (first, second)]
    final = [line[2:] for line in (session_dir / "dossier.md").read_text(encoding="utf-8").split("\n")
             if line.startswith(("- Boundary: every", "- 1 changed declaration"))]
    if len(final) != 2 or "no broken site found" not in final[0]:
        raise SystemExit("the recorded session's dossier no longer ends with nothing broken")
    edits = [step for step in session["steps"] if step["tool"] == "Edit"]
    quiet = sum(1 for step in edits if "notice" not in step)

    half = ("-def get_cache_file(mode: Mode) -> Path:\n"
            "-    return CACHE_DIR / f\"cache.{mode.get_cache_key()}.pickle\"\n"
            "+def get_cache_file(mode: Mode, cache_dir: Path) -> Path:\n"
            "+    return cache_dir / f\"cache.{mode.get_cache_key()}.pickle\"")
    by_hand = ("git clone https://github.com/psf/black && cd black\n"
               f"git checkout {COMMIT[:10]}~1\n"
               "ok index\n"
               f"git checkout {COMMIT[:10]} -- src/black/cache.py\n"
               "ok ripple")
    script = ("npm install -g open-kioku\n"
              "git clone https://github.com/shivyadavus/open-kioku\n"
              "open-kioku/scripts/demo-replay.sh")
    in_ci = ('ok index .\n'
             'ok gate ci --base origin/main \\\n'
             '  --task "$PR_TITLE" --write dossier.md')
    guides = "".join(
        f'<a class="card" href="{key}-setup.html"><h3>{agent["name"]}</h3><span class="go">Set it up →</span></a>'
        for key, agent in AGENTS.items())
    body = f"""
<div class="page-head">
  <div class="wrap">
    <p class="crumbs"><a href="index.html">Home</a> › See it work</p>
    <span class="eyebrow">See it work</span>
    <h1>A real commit, stopped halfway.</h1>
    <p class="lede">A commit to Black, the Python formatter, gave one function a new required parameter and updated every caller. We stop that commit halfway, with the function changed and the callers not yet, and ask Open Kioku what is broken. The finished commit is the answer key.</p>
  </div>
</div>

<section>
  <div class="wrap">
    <div class="stats" style="margin-top:0">
      <div><b>{named} of {named}</b><span>call sites it named are lines the real commit went on to change</span></div>
      <div><b>{seconds} s</b><span>to name them, on a repository of 504 files</span></div>
      <div><b>0</b><span>builds, test runs or type checkers involved</span></div>
      <div><b>{session["seconds"]:.0f} s</b><span>for one recorded Claude Code session to make the change and leave nothing broken</span></div>
    </div>
  </div>
</section>

<section id="change">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">1 · The change</span>
      <h2>One function gets a required parameter.</h2>
      <p>Python imports this file without complaint. Each call that still passes one argument fails only when it runs.</p>
    </div>
    {code(half, label="src/black/cache.py")}
    <p class="note"><a href="{COMMIT_URL}">psf/black {COMMIT[:10]}, “Add --cache-dir command-line option” ↗</a></p>
  </div>
</section>

<section id="ripple">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">2 · Ask what it broke</span>
      <h2>Ripple names every call the change left behind.</h2>
      <p>It reads the working tree against the last commit and the index of the repository. Nothing is built and nothing is run.</p>
    </div>
    {term(f"psf/black · ok ripple · recorded output, {seconds} s", "$ ok ripple\n" + ripple)}
    <p class="note"><em>proven</em> means the index resolved the call to this declaration. <a href="{REPO}/blob/main/docs/ripple.md">How a site is attributed ↗</a></p>
  </div>
</section>

<section id="answer-key">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">3 · The answer key</span>
      <h2>Then compare it with what the authors did.</h2>
      <p>The finished commit changed each of the {named} lines ripple named.</p>
    </div>
    <div class="table">
      <table>
        <thead><tr><th>Site ripple named</th><th>In the finished commit</th></tr></thead>
        <tbody>{rows}</tbody>
      </table>
    </div>
    <p class="note">The comparison is made by <a href="{REPO}/blob/main/scripts/demo-replay.sh">the replay script ↗</a> from <code>git diff</code> of the commit. <a href="{REPO}/blob/main/benchmarks/ripple/README.md">The same replay on 60 repositories ↗</a></p>
  </div>
</section>

<section id="pull-request">
  <div class="wrap split">
    <div>
      <div class="head">
        <span class="eyebrow">4 · In a pull request</span>
        <h2>The same finding, where the review happens.</h2>
        <p><code>ok gate ci</code> judges any pull request, whatever wrote it, and writes a Change Dossier: what the task declared, what the change touched, and what it left broken.</p>
      </div>
      {code(in_ci, label="In CI")}
      <p class="note"><a href="{PROOF_URL}/dossier.md">The whole dossier for this change ↗</a> · <a href="{REPO}/blob/main/docs/gate.md">The gate ↗</a></p>
    </div>
    <div class="card">
      <span class="step">Change Dossier · Ripple</span>
      <p>{summary}</p>
      <ul class="ticks">{found}</ul>
    </div>
  </div>
</section>

<section id="agent">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">5 · In Claude Code</span>
      <h2>With the hooks, the agent hears it right after the edit.</h2>
      <p>One recorded session on the same repository, before the commit. The prompt asks for the parameter and does not mention callers.</p>
    </div>
    <ol class="steps">
      <li><div class="body"><h3>The prompt</h3>{code(session["prompt"], prose=True)}</div></li>
      <li><div class="body"><h3>First edit: the signature</h3><p>The hook answers with the {counts[0]} calls that no longer fit: the six in the tests and one in the same file.</p>
      {term("Claude Code · after the edit · recorded", first)}</div></li>
      <li><div class="body"><h3>Second edit: the call in the same file</h3><p>{counts[1]} are left, and the hook says so again.</p></div></li>
      <li><div class="body"><h3>The tests</h3><p>The agent updates the six calls. The next {quiet} edits get no notice: nothing is left.</p></div></li>
      <li><div class="body"><h3>At the stop: the Change Dossier</h3><ul class="ticks yes">{"".join(f"<li>{inline(line)}</li>" for line in reversed(final))}</ul></div></li>
    </ol>
    <p class="note">{session["turns"]} turns, {session["seconds"]:.0f} seconds, recorded {session["recorded"]}. <a href="{PROOF_URL}/claude-code-session">Every step, both notices and the dossier ↗</a></p>
  </div>
</section>

<section id="run">
  <div class="wrap split">
    <div>
      <div class="head">
        <span class="eyebrow">Run it yourself</span>
        <h2>One script, the same output.</h2>
        <p>It clones Black into a temporary directory, prints steps one to four, and removes the clone. It needs git, python3 and <code>ok</code> 5.0.0 or later.</p>
      </div>
      {code(script)}
      <p class="note">This repository's CI runs the script on every pull request and fails if the output differs from what this page shows.</p>
    </div>
    <div>
      <div class="head">
        <span class="eyebrow">Or by hand</span>
        <h2>Five commands.</h2>
        <p>The same replay, typed out.</p>
      </div>
      {code(by_hand)}
    </div>
  </div>
</section>

<section id="start">
  <div class="wrap">
    <div class="head">
      <span class="eyebrow">Your repository</span>
      <h2>Set it up with your agent.</h2>
    </div>
    <div class="grid cols-4">{guides}</div>
  </div>
</section>
"""
    return shell(path="see-it-work.html", title="Open Kioku — see it work on a real commit",
                 description="A real commit to Black, stopped halfway: Open Kioku names the six calls the change left broken, and the finished commit confirms each one.",
                 current="replay", body=body)


def main():
    pages = {f"{key}-setup.html": agent_page(key, agent) for key, agent in AGENTS.items()}
    pages["docs.html"] = docs_page()
    pages["see-it-work.html"] = replay_page()
    if "--check" in sys.argv[1:]:
        stale = [name for name, text in pages.items()
                 if not (OUT / name).exists() or (OUT / name).read_text(encoding="utf-8") != text]
        if stale:
            sys.exit("out of date, run scripts/build-site-pages.py: " + ", ".join(sorted(stale)))
        print(f"site pages are current: {len(pages)} pages")
        return
    for name, text in pages.items():
        (OUT / name).write_text(text, encoding="utf-8")
    print("wrote", len(pages), "pages")


if __name__ == "__main__":
    main()
