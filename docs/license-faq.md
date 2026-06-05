# License FAQ

Open Kioku is licensed under the [Elastic License 2.0 (ELv2)](../LICENSE).
This page answers common questions in plain English.

> **Disclaimer:** This FAQ is informational, not legal advice. When in doubt,
> read the [full license text](../LICENSE) or consult your legal team.

---

## What You CAN Do

| Use Case | Allowed? |
|---|---|
| Use Open Kioku at your company | ✅ Yes |
| Use it in your CI/CD pipelines | ✅ Yes |
| Build internal tools on top of it | ✅ Yes |
| Modify the source code | ✅ Yes |
| Distribute copies (with license and notices intact) | ✅ Yes |
| Use it for personal projects | ✅ Yes |
| Use it for commercial projects | ✅ Yes |
| Bundle it into your development workflow | ✅ Yes |
| Run it on your own servers for your own team | ✅ Yes |

## What You CANNOT Do

| Restriction | Details |
|---|---|
| Offer Open Kioku as a hosted/managed service | ❌ You may not provide it to third parties as a hosted or managed service that exposes a substantial set of its features. |
| Remove or obscure license notices | ❌ You must keep all copyright and licensing notices intact. |
| Circumvent license key functionality | ❌ You may not move, change, disable, or circumvent license key mechanisms. |

---

## Common Questions

### Can I use Open Kioku at my company?

**Yes.** You can install it on developer machines, run it in CI, and use it as
part of your internal development workflow. There is no limit on the number of
developers or projects.

### Can I modify it?

**Yes.** You can modify the source code for your own use. If you distribute
modified copies, you must include prominent notices stating that you have
modified the software, and you must include the license and copyright notices.

### Can I build a product that uses Open Kioku internally?

**Yes.** You can use Open Kioku as a component of a larger product, as long as
you are not offering Open Kioku itself as a hosted/managed service to third
parties.

### Can I offer a SaaS product built with Open Kioku?

**It depends.** If your SaaS product _uses_ Open Kioku internally (e.g., to
power code analysis behind the scenes), that is fine. If your SaaS product
_is_ Open Kioku — meaning it provides third-party users with access to a
substantial set of Open Kioku's features as a service — that is not permitted.

### Is ELv2 an OSI-approved open-source license?

**No.** The Elastic License 2.0 is a source-available license, not an
OSI-approved open-source license. The source code is publicly available and you
have broad rights to use and modify it, but the hosted-service restriction
means it does not meet the OSI Open Source Definition.

### Can I contribute back?

**Yes, please!** Contributions are welcome under the same license. See
[CONTRIBUTING.md](../CONTRIBUTING.md) for details.

---

## Comparison to Common Licenses

| | MIT | Apache-2.0 | AGPL-3.0 | ELv2 (this project) |
|---|---|---|---|---|
| Use commercially | ✅ | ✅ | ✅ | ✅ |
| Modify | ✅ | ✅ | ✅ | ✅ |
| Distribute | ✅ | ✅ | ✅ (with source) | ✅ (with notices) |
| Offer as hosted service | ✅ | ✅ | ✅ (with source) | ❌ |
| Patent grant | ❌ | ✅ | ✅ | ✅ |
| Copyleft | ❌ | ❌ | ✅ (strong) | ❌ |
| OSI-approved | ✅ | ✅ | ✅ | ❌ |

**Key takeaway:** ELv2 is as permissive as Apache-2.0 for the vast majority of
users. The only additional restriction is that you cannot offer the software
itself as a hosted/managed service to third parties.

---

## Full License Text

The complete Elastic License 2.0 text is in the repository root:
[LICENSE](../LICENSE)
