# Obsolete: Player Core Timeline Hover Prepare

- OBSOLETE since 2026-07-03: timeline hover prepare, hover predecode, prepared hover working-set promotion, hover snapshot DTOs, `PlayerTimelineHoverPrepareHandoff`, and related app/player boundaries were removed from the codebase.
- Do not use the old S25/S28 notes as implementation guidance. `PlayerSnapshot` no longer exposes timeline-hover-prepare state. `player-core` no longer owns a hover working-set handoff or hover stream decode context.
- Keep live scrub and SeekLanding routed through the remaining frame-server/state-machine boundaries. New frame-server work should target live scrub or future playback-rate frame serving without reintroducing hover-specific request kinds, settings, diagnostics, or renderer overlay paths.