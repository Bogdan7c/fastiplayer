# Archived: Timeline Hover Intent

> Superseded since 2026-07-03. Актуальный owner и действующие инварианты: `mem:frame-server/core`.

- OBSOLETE since 2026-07-03: timeline hover intent tracking, hover preview target coalescing, hover prepare windows, and hover leave/enter state were removed from `app-egui` and `frame-server-core`.
- Ordinary pointer hover in egui controls still exists for UI affordances, but it must not be confused with deleted timeline hover preview/predecode functionality.
- Live scrub remains the only timeline drag preview behavior retained from this area.