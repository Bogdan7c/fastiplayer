# Archived: Timeline Hover Leave Grace

> Superseded since 2026-07-03. Актуальный owner и действующие инварианты: `mem:frame-server/core`.

- OBSOLETE since 2026-07-03: timeline hover leave grace timers, cleanup reasons, and session-end release paths were removed with hover preview/predecode.
- Do not add new hover leave grace config or controller state. Removed legacy config keys may only be stripped while loading old TOML and must not be written or exposed in settings.