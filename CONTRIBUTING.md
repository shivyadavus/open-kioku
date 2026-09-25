# Contributing to Open Kioku

Open Kioku is currently a maintainer-led, source-available project.

At this stage, we welcome:

* Bug reports
* Reproducible test cases
* Documentation feedback
* Installation issues
* MCP client compatibility reports
* Real-world workflow feedback

We are not currently accepting unsolicited code contributions or large pull requests.

This is because the project is still stabilizing its architecture, licensing, and long-term product direction. Please open an issue or discussion before starting any implementation work.

Small documentation corrections may be considered, but all code, architecture, storage, MCP schema, security, licensing, and roadmap changes are maintainer-directed for now.

Thank you for understanding.

## Repository hygiene checks

Install the git hooks once:

```sh
git config core.hooksPath .githooks
```

That enables two hooks. `pre-push` syncs manifest versions. `commit-msg` runs
`scripts/check-public-surface.py` over the commit message.

This tree is public, and a commit message cannot be corrected after it merges
without rewriting history. The check rejects promotional and channel copy,
competitor superiority claims, assistant session links (for example a
`Claude-Session:` trailer) and local `/Users/` paths. A `Co-Authored-By:`
trailer is fine. Verify a message before committing with:

```sh
scripts/check-public-surface.py --commit-msg .git/COMMIT_EDITMSG
```

The same script runs in CI over every tracked file, and additionally rejects
paths under `docs/launch*`, `docs/marketing` and `docs/gtm`. Keep working
material outside the repository.
