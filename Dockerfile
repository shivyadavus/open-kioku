# Open Kioku MCP server — runs the published 4.0.0 Linux x86-64 binary over stdio.
# The binary is fetched from the GitHub release and verified against the sha256
# pinned in release-metadata.json for that tag; nothing is compiled here.
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
