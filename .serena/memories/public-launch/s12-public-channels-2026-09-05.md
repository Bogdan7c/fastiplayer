# S12 public repository and community routes (2026-09-05)

- Owner approved S12 opening after the read-only gate. Repository Bogdan7c/rustiplayer is now public. Opening SHA was 046e6de1fa6da5161da0881208368c131b770d18; this does not establish release qualification.
- Discussions and GitHub Private Vulnerability Reporting are enabled and verified. SECURITY.md, SUPPORT.md, README.md, CONTRIBUTING.md and issue chooser now route to active channels; this supersedes planned-channel statements in mem:public-launch/s07-community-health-2026-09-05 and mem:core.
- Main ruleset 22317252 is active with deletion and non_fast_forward rules, no bypass actors, no required-PR rule. Ordinary maintainer pushes remain allowed.
- Milestone 1.0 is milestone/1. Roadmap issues #1–#8 follow the owner-approved order, have concrete scope and functional acceptance criteria, and do not carry good first issue. Eleven useful labels exist. Homepage is empty; repository description and sixteen topics match the launch plan.
- Source-only alpha notes are prepared at docs/releases/v0.1.0-alpha.1.md with release-tag-pinned links and historical hardware provenance. All publication edits must be committed before final exact-SHA CI qualification. No tag or release is authorized before required remote CI and release gates pass; public visibility alone is not launch completion.
- Post-opening reruns of CI 33940276535 and Toolchain policy 33940276542 (attempt 2) began executing jobs, so the previous private billing blocker did not prevent their execution. Do not transfer their results to a later documentation commit. Current workflow/coverage policy is unchanged.
- GitHub profile update via CLI lacks user scope, and no browser session is available; name/location/bio update remains unresolved. Never claim it was completed.
- No production Rust APIs, behavior, dependency versions, test locations or CI policy changed in this publication preparation.
