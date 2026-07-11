# Archived: Hover Decode Execution

> Superseded since 2026-07-03. Актуальный owner и действующие инварианты: `mem:frame-server/core`.

- OBSOLETE since 2026-07-03: app-owned hover decode execution, software hover sessions, hover source opening, dependency-span resolution, prepared-frame insertion, and hover working-set storage were removed.
- Do not restore independent hover demuxers/decoders, hover network workers, hover prepared leases, or FFmpeg/VAAPI hover reservations from this memory.
- Frame-server execution remains player-owned for SeekLanding/live scrub; future playback-rate frame serving must use neutral frame-server contracts without hover-specific terminology or lifecycle.