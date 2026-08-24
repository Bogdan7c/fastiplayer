# AUD-019 — bounded next-item source/demux preload (2026-08-24)

## Outcome

Independent read-only verification confirmed that existing `media-prefetch` only read ahead the current source and every clean EOF started a new cold strong-open. AUD-019 is closed with an option-A implementation: the next exact queue item may prepare source + demux before EOF, while decoder/backend/player install remain authoritative only after EOF.

## Ownership and boundaries

- `PlaylistController` owns an optional exact automatic traversal plan. `next_item_preload_target()` fixes the same opaque plan later consumed by clean EOF, so shuffle cannot preload one item and install another.
- `PreparedNextOwner` lives inside process-lifetime `PlaylistRuntime` and owns speculative policy, one `SpeculativeMediaPreparation`, and `Idle | Preparing | Ready | Failed` state.
- Exact correlation key is active identity (item/lineage/media instance/player binding generation) + full queue revision snapshot + target item.
- `SpeculativeMediaPreparation` is policy-neutral and reuses production `prepare_source`; it has a separate single-worker `PreparationExecutor`. A non-cooperative stale speculative open can delay its replacement but can never overlap a second speculative preparation.
- A ready `PreparedMediaOpen` enters the existing coordinator through caller-prepared ingress only at clean EOF, then follows unchanged queue staging, video-candidate, authorization, Installed and controller commit boundaries.
- Before EOF there is no decoder/video backend, auth reservation, packet publication, active identity mutation or queue-current commit.
- Missing/preparing/failed/stale/mismatched/expired prepared state returns `None`; the existing locator-based cold-open path remains the correctness fallback.

## Scheduling, cancellation and resources

- Default enabled: `playlist.next_item_preload_enabled = true`.
- Only `PlaybackState::Playing` with known duration enters the default 30,000 ms lead window. Live/unknown-duration and paused media do not start speculative work.
- Aggregate default budget is `playlist.next_item_preload_budget_mb = 64`; it caps both RAM cache and read-ahead. Direct gets the full budget; possible separate yt-dlp A/V components each get half. Existing smaller limits are never inflated.
- At most one target and one physical speculative worker exist. Ready envelope default maximum hold is 120,000 ms.
- Queue revision/identity/settings change, disable, suspend, authoritative open and shutdown remove speculative authority. Ready URL/source envelopes expire by hold window.
- Config schema is v9; v8 and older supported documents migrate through defaults. Budget is validated 16..512 MiB, lead 1..300 s, hold 10..600 s, and hold must be >= lead.

## Tests and verification

Functional/transition coverage includes:

- enabled default, explicit disable, pre-lead/lead/paused/live scheduling;
- fixed target equals clean-EOF install while current identity/current queue item stay unchanged;
- real local source reaches demux probing in speculative preparation;
- exact-key and hold-expiry rejection preserve cold fallback;
- aggregate RAM/read-ahead projection preserves playback-window/source identity;
- single-worker replacement cannot overlap non-cooperative stale work;
- existing caller-prepared ingress remains non-authorizing until the normal strong protocol.

Final checks:

- `cargo test -p rustiplayer-config --locked`: 93 passed.
- `cargo test -p app-egui --no-default-features --locked`: 970 passed.
- strict affected all-target Clippy with `-D warnings`, rustfmt and `git diff --check`: PASS.

Audit source: `user/project_health_audit_2026-08-22.md`.
