ALWAYS CONSULT CONTEXT7 BEFORE EDITING OR WRITING CODE!!!!

Profanity and informal communication are very welcome.

Before starting a coding task:
- Call Serena `initial_instructions` if they have not yet been read in the current session.
- Activate the project.
- Read `mem:core` and the relevant memories.
- If memories are clearly outdated relative to the current code or AGENTS.md, update them before making edits.

After completing code changes:
- Assess whether architectural boundaries, public/internal APIs, workflows, validation commands, important invariants, known limitations, or the locations of key tests have changed.
- If so, explicitly update the relevant Serena memories through `write_memory`/`edit_memory`.
- If the changes are local and do not change project knowledge, do not update memories, but state in the final response that memory updates were not required.

WHEN SOLVING CODE PROBLEMS, FIND THE ROOT CAUSE, NOT JUST THE SYMPTOM!!

## Rules for writing tests

Always write tests in the code that verify working functionality, rather than only helpers or particular internal stages. For example, test that video from a source is not merely read, but actually played back (reaches the rendering stage).

The user does not have a programming background, but takes responsibility for important architectural decisions. When asking a question, explain the options in plain language so the user can understand them and choose an appropriate option.

This is "vibe coding taken to the max": we are not asking AI to do everything for us; we make decisions, understand the process, and learn AI-assisted development.

Use MCP Serena automatically for coding tasks:
- Before starting a coding task, call Serena `initial_instructions` if they have not yet been read in the current session.
- After activating the project, read the relevant Serena memories, starting with `mem:core`, if onboarding has already been completed.
- To explore code, first use Serena `get_symbols_overview`, `find_symbol`, `find_declaration`, `find_referencing_symbols`, and `find_implementations`, rather than reading entire files unnecessarily.
- Before changing a boundary/API, check references, implementations, and diagnostics through Serena.
- For safe, targeted changes to an entire symbol, prefer Serena symbolic editing when it is more precise than a regular patch.
- If Serena onboarding has not been completed, complete it before the first substantial project task and write the memories.

If an important decision needs to be made during the work, stop, ask the user, and discuss it.

## Architectural rules for new features

The project is no longer a prototype. Implement new features so the system can be extended, tested, and repaired in specific areas without rewriting neighboring modules.

These rules apply to every part of the project: player-core, render, decoder, worker, session/tick, media opening, diagnostics, config, and future modules. `PlaybackPipeline` is an example of this approach, not an exception.

1. A module owns its data and invariants. External code must not read or modify its internal fields directly when the operation can be expressed through a meaningful internal API.

2. Internal APIs must describe intent, not storage layout. Good examples: `can_send_video_decode_packets()`, `release_frame_to_video_decoder()`, `drain_completed_*()`. Bad example: inspecting a specific field in another module for convenience.

3. Do not couple modules through knowledge of a particular implementation. `session`, `tick`, scheduler, decoder, render, and pipeline must depend on contracts and boundary methods, not on whichever thread, queue, backend, or field happens to be inside.

4. Keep ownership and lifecycle responsibilities at the correct layer. Do not hide important decisions about ownership, release, generation, scheduler semantics, or accounting inside a "convenience" method if that changes a layer's responsibility.

5. If a new feature requires direct access to another module's field, treat that as an architectural smell first. Either add a small method to the state owner or explicitly explain why direct access is actually necessary here.

6. Boundary methods must preserve existing error and state semantics: absent resources, backpressure, fatal errors, no-ops, counters, and release paths must not be collapsed into a single generic `bool` when the caller needs to distinguish them.

7. For every new boundary/API, add focused tests for an absent resource, an active fake/stub, an error, edge-case accounting, and confirmation that the method does not change state it should not own.

8. Do not combine cosmetic refactoring with a feature. An architectural boundary change must be a separate, deliberate change with a clear reason and validation.

9. Before implementing a feature, briefly describe the architecture: which modules own the state, which methods form the boundaries, which invariants must not be broken, and which tests will enforce them.

## Module size and Rust API design rules

1. Do not bloat central modules or crates. For Fastiplayer, this is especially important for `app-egui`, `player-core`, `render-wgpu-video`, `video-frame-contract`, and `video-backend-api`: new logic must go into the module that owns the relevant state and invariants, not the largest or most convenient file.

2. If a file is already approaching 700–800 lines, put a new feature in a separate module by default. An exception is acceptable only for a small, local change; in that case, explicitly explain why a new module would reduce readability or fragment one coherent invariant.

3. New Rust boundary/internal APIs must be self-documenting at the call site. Do not add positional `bool` arguments, ambiguous `Option` values, or numbers or strings with unclear meaning when an `enum`, newtype, named method, or separate intent method can express that meaning. If an existing API forces an unclear literal, add a short comment naming the parameter at the call site.

Write clean, maintainable, production-ready code.
Propose the architecture first, then implement it.
After implementation, perform a self-review and improve the code if you find problems.
Do not sacrifice readability for brevity.
Document production code thoroughly in Russian so the owner can understand it.

Comment key production-code logic, non-obvious decisions, and important invariants in Russian so the owner can understand them. Do not comment every line or merely restate the syntax.

The following are prohibited:
- Putting everything in one function.
- Using unclear names such as `data`, `temp`, `obj`, or `arr` without context.
- Silently ignoring errors.
- Hardcoding configuration.
- Mixing input/output, business logic, and formatting.
- Writing "magic" code without an explanation.
