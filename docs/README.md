# Engineering documentation

Start with the English [project overview and build instructions](../README.md) and [architecture](../ARCHITECTURE.md). This index preserves the existing deep engineering documents in their original language; English headings inside Russian documents do not make them English translations.

**EN** = English; **RU** = Russian (with technical English terminology); **EN/RU** = mixed English and Russian; **JSON/TOML** = machine-readable evidence or policy. Session reports describe the revision and profile they recorded. Current code/manifests and newer acceptance take precedence over historical snapshots.

## Project entry points

| Document | Language | Purpose |
| --- | --- | --- |
| [Landing page](../README.md) | EN | Status, features, build, limitations and ordered roadmap |
| [Architecture](../ARCHITECTURE.md) | EN | Owners, API boundaries, frame lifetime and settings transactions |
| [Contributor/agent rules](../AGENTS.md) | EN | Engineering conventions and owner decision boundaries |
| [AI-assisted development](ai-development.md) | EN | Optional Codex/Serena workflow; not a build requirement |
| [Contributing](../CONTRIBUTING.md) | EN | Issue selection, verified commands, architecture and separate hardware acceptance |
| [Security](../SECURITY.md) | EN | Untrusted input and private reporting policy; private reporting enabled |
| [Support](../SUPPORT.md) | EN | Bugs/proposals routing; Discussions enabled |
| [Maintainers](../MAINTAINERS.md) | EN | Sole core maintainer and deliberately deferred conduct enforcement contact |
| [Changelog](../CHANGELOG.md) | EN | Development history and alpha release scope |
| [Alpha release notes](releases/v0.1.0-alpha.1.md) | EN | Source-only prerelease scope, evidence and limitations |
| [Issue forms and chooser](../.github/ISSUE_TEMPLATE) | EN | Bug/feature forms and support/security routes (YAML; internal comment in RU) |
| [Pull request template](../.github/pull_request_template.md) | EN | Problem, scope, contributor checks and hardware evidence |
| [Code owners](../.github/CODEOWNERS) | RU comment / GitHub syntax | Global review owner Bogdan7c |

## Quality, acceptance and measurements

| Document | Language | Purpose |
| --- | --- | --- |
| [Continuous integration](continuous-integration.md) | RU | Canonical checks, dependencies, toolchain and patch policy |
| [Coverage](code-coverage.md) | RU | Stable-coordinate coverage ratchet and qualification |
| [Panic and invariant policy](panic-invariant-policy.md) | RU | Fallible boundaries and internal invariants |
| [Manual media regressions](manual-media-regressions.md) | EN/RU | Explicit fixture-based playback verification |
| [Runtime acceptance manifest](runtime-acceptance-manifest.md) | RU | Executable acceptance suites and outcome contract |
| [S42 final acceptance](web-media-s42-final-acceptance.md) | RU | Scoped earlier web-media gate and manual checklist |
| [N15 acceptance](native-web-ingress-n15-acceptance.md) | RU | Native ingress, consumer/hardware proof, performance methodology and exclusions |
| [N15 acceptance results](native-web-ingress-n15-acceptance.json) | JSON | Sanitized recorded outcomes |
| [N15 performance aggregates](native-web-ingress-n15-performance.json) | JSON | Thirty-run cohorts and comparison limits; not raw samples |
| [Benchmark policy](benchmarks/README.md) | EN | Publication rules and N15 methodology guide |
| [ThinkPad T480s](benchmarks/thinkpad-t480s.md) | EN | Measured S08 hardware/software playback, raw samples, runtime capture and comparison limits |
| [Hardware evidence context](hardware/README.md) | RU | Why capability dumps are not playback acceptance |

## Media profiles and behavior

| Document | Language | Purpose |
| --- | --- | --- |
| [Web-media compatibility matrix](web-media-compatibility-matrix.md) | RU | Accepted protocols, containers/codecs and exclusions |
| [Operational errors](web-media-operational-errors.md) | EN/RU | Typed failures and safe recovery actions |
| [Progressive/web S27](progressive-web-s27.md) | RU | Historical ownership/hardening evidence; newer native ingress is covered by N15 |
| [Classic ISO BMFF S28A1](iso-bmff-s28a1.md) | RU | MP4/M4A/MOV/3GP scope and proof |
| [Fragmented ISO BMFF S28A3](iso-bmff-fmp4-s28a3.md) | RU | Fragmented-container profile and evidence |
| [Matroska/WebM S28B](matroska-webm-s28b.md) | RU | Container ownership and accepted profile |
| [Audio containers S28C](audio-containers-s28c.md) | RU | Audio-container proof |
| [Existing demux S28G](existing-demux-s28g.md) | RU | Demux hardening and limits |
| [Static DASH MPD S34A](dash-mpd-s34a.md) | RU | Static parser/profile evidence; newer live/runtime work is covered elsewhere |

## Dependencies, design and history

| Document | Language | Purpose |
| --- | --- | --- |
| [Patch inventory](dependency-patches.toml) | TOML | Seven local upstream patches, validation and removal gates |
| [Dependency report](dependency-report-2026-07-10.md) | RU | Dated audit and subsequent policy notes |
| [XML dependency audit S04X](dependency-audit-s04x-2026-07-20.md) | RU | Safe XML parser/advisory closure |
| [AES-128 dependency audit S32A](dependency-audit-s32a-hls-aes-2026-07-23.md) | RU | HLS cryptographic dependency review |
| [FFmpeg build tooling](../scripts/tooling/README.md) | RU | Optional local LGPL build tooling; historical feature-default notes must be checked against current Cargo manifests |
| [Historical reports](history/README.md) | RU | Reading dated evidence in context |
| [Readiness report, 2026-07-12](history/readiness_report_2026-07-12.md) | RU | Historical snapshot; not current release readiness |

Public launch work must keep genuine runtime captures separate from design concepts and native-ingress experiments separate from whole-player performance comparisons. No private fixtures or external owner documents are required just to read this index.
