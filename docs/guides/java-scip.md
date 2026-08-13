# Java SCIP proof path

Use this path when a Maven or Gradle repository needs exact Java definition and
reference evidence in Open Kioku. It verifies a real SCIP artifact locally; it
does not treat a source-text match as an exact reference.

## Prerequisites

- A Maven root with `pom.xml`, or a Gradle root with `settings.gradle`,
  `settings.gradle.kts`, `gradlew`, `build.gradle`, or `build.gradle.kts`.
- JDK 17 or newer and a working `scip-java` command. Install it using the
  upstream [scip-java getting-started guide](https://github.com/scip-code/scip-java/blob/main/docs/getting-started.md).

`scip-java index` runs the project build integration and can clean compiler
caches. Review its output and run it in an appropriate local or CI environment.
Open Kioku does not install indexers. Any network access is governed by
`scip-java` and the project's build tooling.

## Generate and verify

From the repository root:

```sh
ok init .
ok scip doctor .
scip-java index
test -f index.scip
ok index . --with-scip required
ok status .
```

`scip-java index` writes `index.scip` at the repository root. That is a default
Open Kioku SCIP input, so the required indexing command succeeds only after it
can import a valid SCIP artifact. Its normal output includes the number of
imported indexes and exact references; `ok status .` also reports the SCIP
exact-reference count.

To have Open Kioku invoke an already-installed indexer, use:

```sh
ok index . --with-scip auto
```

This uses the same `scip-java index` command and default `index.scip` output.
Use the explicit sequence above when you want a visible installation and
artifact check before importing.
