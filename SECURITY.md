# Security policy

Rustiplayer is in active development / pre-alpha. There is no stable or LTS security-support promise. Reports affecting current `main` and the `v0.1.0-alpha.1` development release line are welcome; fixes are expected on the current development line, without a guaranteed backport schedule or response SLA.

## Report a vulnerability privately

**Primary channel:** [GitHub Private Vulnerability Reporting](https://github.com/Bogdan7c/rustiplayer/security/advisories/new). Private reporting is enabled for this repository.

Open the repository's [Security advisories](https://github.com/Bogdan7c/rustiplayer/security/advisories) and choose **Report a vulnerability**. GitHub describes the private report process in its [researcher guide](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately).

If that button is unavailable, no private intake channel is established by this document. Do not put exploit details, malicious samples, or sensitive URLs in a public issue, PR, or Discussion. If you already have an established private conversation with the maintainer, ask there how to transfer the report securely. Otherwise, wait for the channel to become available; at most request that private reporting be enabled without describing the vulnerability or identifying affected private media. No security email address is designated.

A private report should include:

- Affected revision/version and environment, including relevant native library and driver versions.
- A description of the suspected impact and affected boundary.
- Minimal reproduction steps and, when safe to share privately, a small reproducer you have permission to distribute.
- Sanitized diagnostics, expected behavior, and observed behavior.

Do not send account passwords, authentication cookies, tokens, private streaming URLs, or unrelated personal data even in a private report. Coordinate sample transfer and disclosure with the maintainer. A solo maintainer cannot guarantee an immediate response, a fix date, or a bounty.

## Untrusted input and scope

**Media files and network manifests are untrusted input.** Container metadata, playlists, compressed video/audio, subtitles or descriptors, network responses, and resolved endpoints can be malformed or hostile. Opening media is not a security sandbox. Rust ownership helps, but parsers, unsafe code, native FFmpeg/VA-API libraries, FFI, GPU imports, and drivers remain security-sensitive.

Report suspected memory-safety problems, boundary bypasses, unintended credential disclosure, unsafe persistence/logging, and resource-exhaustion behavior through the private route. Bounded readers, budgets, cancellation, dependency checks, and typed failures reduce risk; they do not establish that arbitrary media is safe.

System `yt-dlp` configuration, plugins, and cookies are a trusted external environment with effects outside the player's guarantees. The exact locator explicitly supplied by a user may be persisted as durable identity; secrets embedded in that locator can therefore reach local state. Review logs, configuration, playlists, screenshots, and crash reports before sharing them. Remove signed query strings, headers, cookies, tokens, private hostnames/paths, and personal identifiers.

See the [trust boundaries](ARCHITECTURE.md#trust-boundaries), [dependency policy](docs/continuous-integration.md), and [support routing](SUPPORT.md). Ordinary playback failures without a suspected security impact belong in the bug form.
