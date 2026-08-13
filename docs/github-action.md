# GitHub Action evidence

[`shivyadavus/open-kioku-action@v1`](https://github.com/shivyadavus/open-kioku-action)
runs Open Kioku against the checked-out revision and uploads a concise evidence
artifact for a pull request or another CI workflow.

It requires an `open-kioku` npm release that includes `ok preflight`. The action
checks that compatibility before it indexes a repository and exits with a clear
error if the selected package version is too old.

```yaml
name: Open Kioku evidence

on:
  pull_request:

permissions:
  contents: read

jobs:
  evidence:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: shivyadavus/open-kioku-action@v1
        with:
          task: "change token expiration"
          verify: true
```

## Privacy and output

The action installs the CLI and indexes the checked-out revision within the
runner. Its default artifact contains:

- CLI version and workflow commit SHA;
- SHA-256 hashes of the local index and preflight report;
- redacted changed-file shapes, such as `**/*.rs`;
- preflight verdict, confidence, selected validation names and exit statuses;
- risks and caveats.

It does not include source snippets, raw command output, or repository-local
paths by default. Set `reveal-paths: true` only when repository-relative paths
are safe to retain in workflow artifacts. Artifact retention defaults to 14 days
and is configurable with `retention-days`.

`verify` defaults to `false`. Setting it to `true` runs only commands selected
by the local preflight and records their names and exit statuses, not their
output. Enable it only in workflows that are appropriate for the checked-out
code and runner permissions.

## Optional pull-request comment

Comments are disabled by default. To add or update one concise marked comment,
grant only the extra permission required and pass the token explicitly:

```yaml
permissions:
  contents: read
  pull-requests: write

- uses: shivyadavus/open-kioku-action@v1
  with:
    task: "change token expiration"
    comment: true
    github-token: ${{ secrets.GITHUB_TOKEN }}
```

Avoid `pull_request_target` for untrusted contributions. The action is intended
to create a review artifact, not to publish content to social platforms.
