# Open Kioku MCP server: runs the published Linux x86-64 `ok` binary over stdio.
# The binary is fetched from the GitHub release for OK_VERSION and verified
# against OK_SHA256, the sha256 release-metadata.json records for
# ok-linux-x86_64 at that tag; nothing is compiled here.
#
# The two ARG lines below are generated, not hand-edited.
#
# OK_VERSION is written by scripts/sync-version.sh together with every other
# version manifest, so a workspace version bump lands here in the same commit
# as everywhere else. OK_SHA256 is written only by the release workflow's
# hash-pin step (.github/workflows/release.yml, job `publish`), from the
# ok-linux-x86_64 it has just built, in the same commit that pins
# release-metadata.json and Formula/open-kioku.rb; the release tag is always
# at or after that commit. The sha cannot be synced any earlier because it does
# not exist until the binary is built.
#
# Between a version bump and the hash pin the two lines are therefore out of
# step on purpose, exactly as the Homebrew formula is: a build at such a commit
# fails, at the download (that release does not exist yet) or at the sha256sum
# check, rather than run a binary the pin never covered. Directories that build
# this image do so from the release tag and never see that window.
# scripts/validate-release-metadata.py holds OK_VERSION to the workspace
# version and OK_SHA256 to the ok-linux-x86_64 entry in release-metadata.json,
# so a hand edit or a skipped pin fails CI.
FROM debian:bookworm-slim

ARG OK_VERSION=4.0.0
ARG OK_SHA256=ad18fc99a2d8ed84de194816d2b49e362faa940ffc861ff7b1a184a593adf75d

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git \
 && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL -o /usr/local/bin/ok \
      "https://github.com/shivyadavus/open-kioku/releases/download/v${OK_VERSION}/ok-linux-x86_64" \
 && echo "${OK_SHA256}  /usr/local/bin/ok" | sha256sum -c - \
 && chmod +x /usr/local/bin/ok

# The server is read-only and speaks JSON-RPC over stdio. Mount the repository to
# serve at /repo; `tools/list` answers without an index, so inspection needs no mount.
RUN useradd --create-home --uid 10001 ok && mkdir -p /repo && chown ok:ok /repo
USER ok
WORKDIR /repo

ENTRYPOINT ["ok", "mcp", "serve", "--repo", "/repo", "--read-only"]
