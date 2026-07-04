# Third-Party Notices

Open Kioku is licensed under the Elastic License 2.0. Third-party dependency
licenses are tracked separately from the project license.

The canonical dependency notice inventory is `NOTICE`. It lists runtime and
build-time components, versions, licenses, and upstream URLs. CI enforces the
license policy with `cargo deny check`, and release validation requires this
file to be attached to GitHub release artifacts as `THIRD_PARTY_NOTICES.md`.

For a release candidate, verify:

```sh
cargo deny check
scripts/validate-trust-gates.py
```
