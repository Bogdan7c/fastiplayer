# Conventions

- Mandatory local workflow from AGENTS.md: before code edits or writing code, check Context7; before project work, set `code_index` project path to repo root and build deep index; solve root causes, not symptoms.
- Stop and ask before important architecture/API/behavior decisions, especially if Sonar or a refactor suggests changing public API, module ownership, or playback semantics.
- Before implementing a feature, briefly state architecture: owner modules/state, boundary methods, invariants that must not change, and focused tests.
- Keep code production-ready and readable; avoid one giant function, vague names (`data`, `temp`, `obj`, `arr` without context), silent error swallowing, hardcoded config, IO/business/formatting mixing, and unexplained magic.
- User explicitly asks for Russian documentation/comments. Add useful Russian comments for non-obvious code; avoid useless line-by-line noise that only restates syntax unless user insists in the active turn.
- Module ownership rule: external code should not read/change another module's storage fields when an intent boundary method can express the operation.
- Boundary APIs describe intent and preserve state/error distinctions: absent resource, backpressure, fatal error, no-op, counters, and release paths must not collapse into ambiguous `bool` if caller semantics differ.
- Ownership/lifecycle stays at the layer that owns it. Do not hide release, generation, scheduler semantics, or accounting decisions inside convenience methods that shift responsibility.
- New direct access to foreign fields is an architecture smell; add a small owner method or document why direct access is necessary.
- New boundary/API requires focused tests for absent resource, active fake/stub, error path, edge accounting, and ensuring it does not mutate state it does not own.
- Do not bundle cosmetic refactors with feature work or architecture boundary changes.
- Refactoring must preserve behavior parity: playback/render/seek/scrub semantics, HDR/P010/NV12 output, zero-copy path, queue limits, error policy, diagnostics, and config defaults stay stable unless separately decided.