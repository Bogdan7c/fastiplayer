# Session 18B — process-lifetime desktop/MPRIS transport (2026-07-16)

## Ownership и lifecycle
- `DesktopIntegration` больше не принадлежит renderer-bound `AppState`. Process-lifetime owner находится в `PlaylistRuntime::desktop_transport`, а `AppShell` запускает его только внутри constructor-а, который уже получил successful D10e `AppInstanceLease`.
- Suspend снимает player binding и публикует detached snapshot с false player-dependent capabilities, но сохраняет process-owned queue modes, effective volume, metadata/track key и intrinsic exported `CanControl=true`. Resume применяет effective volume к новому player binding до resume/install work.
- Terminal shutdown закрывает desktop command admission/backend раньше player owner-а; lease остаётся последним полем `AppShell`. MPRIS spawn/name failure отключает только desktop backend, без fallback имени, late retry или влияния на playback/UI.

## Desktop boundary
- `desktop-integration` не зависит от `player-core`. Его public neutral vocabulary: `DesktopCommand`, `DesktopTransportAction`, `DesktopTrackKey`, `DesktopTimelineSeekOutcome`, `EffectiveVolume`, revisioned `DesktopSnapshotView`.
- Linux запрашивает только `org.mpris.MediaPlayer2.rustiplayer` через zbus 5.15 с explicit `.allow_name_replacements(false).replace_existing_names(false)`; default DoNotQueue сохраняется. `zbus::Error::NameTaken` маппится в typed non-fatal `MprisBusNameUnavailable`, остальные connection/spawn errors остаются отдельными backend outcomes.
- Neutral commands идут в bounded sync mailbox и будят общий D38/D76 `AppWakeOwner::PlaylistRuntime`; UI thread drain-ит их. Full/Disconnected возвращаются только после разрешённой capability реальной попытки enqueue. Detached commands не становятся hidden playback queue.
- MPRIS false-capability policy: Next/Previous/Play/Pause/Seek/SetPosition — no-effect; PlayPause использует error при `CanPause=false`; Rate=0 следует Pause capability. Stale/invalid SetPosition отсекается до app enqueue и остаётся spec no-op.
- Fixed rate contract: Rate/MinimumRate/MaximumRate = 1.0; 1 и другой finite nonzero — no-op/best-fit, 0 — Pause, non-finite — InvalidArgs.
- Full dynamic `PropertiesChanged` включает PlaybackStatus, Metadata, LoopStatus, Shuffle, Volume и пять dynamic `Can*`; Position, fixed rate trio и intrinsic CanControl не сигналятся. Position меняется через correlated `Seeked` only.

## Controller/app routing
- Play/Pause/PlayPause/Stop/Next/Previous используют существующие controller D16/D17/D50–D53/D58 boundaries с origin `Mpris`. MPRIS Stopped хранится как controller `AppTransportDisposition::Stopped`; navigation может internally install paused target, но exported Stopped сохраняется до Play.
- LoopStatus/Shuffle идут через startup/runtime mode boundary: до D81 gate latest values coalesce в draft; Ready/reservation используют controller desired modes; writable lineage dirty-ится только при mutation, protected generation остаётся runtime-only.
- `EffectiveVolume` принадлежит process owner: finite значения normalise в 0..=1, non-finite invalid; no-op не сигналится; UI/player snapshot и MPRIS origins сходятся в одном latest value.

## Track identity
- App owner фиксирует key только при successful active lineage. Playlist item key = lineage + stable Item ID; external key = lineage only. Same-lineage reinstall/suspend/rebind и tombstone сохраняют exact key/path; pending/failure не меняют его; новая lineage переключает.
- Только `desktop-integration::platform::linux::track_identity` кодирует checked non-reserved `/com/rustiplayer/Track/...` object paths. Playlist controller/domain и player-core не содержат zbus/D-Bus types. `HasTrackList=false` сохранён.

## Timeline seek
- App отдельно разрешает signed relative Seek и SetPosition. Underflow clamp-ится к zero; strict known beyond-end идёт в общий MPRIS Next; unknown arithmetic overflow — typed no-op. Stale track/invalid range не enqueue-ятся в player.
- `player-core` владеет neutral `TimelineSeekRequestId`, `ExactTimelineSeekRequest/Receipt/Outcome` и exact MediaInstanceId lifecycle. Outcomes: Applied, InvalidRange, BeyondEnd, StaleInstance, NotSeekable, Failed. Ordinary seek supersede-ит pending exact receipt; overlapping exact seek terminal-resolve-ит старый; only matching committed seek yields Applied.
- App publishes `Seeked` only for matching Applied. BeyondEnd routes exactly one MPRIS Next without false Seeked; all other outcomes are typed and signal-free.

## Layout и tests
- Linux adapter decomposed: `platform/linux.rs` transport/interface, `platform/linux/snapshot_properties.rs`, `track_identity.rs`, `tests.rs`.
- Focused tests include private dbus-daemon occupied nonqueued claim/no late acquisition, full property set/exclusions, capabilities/backpressure, fixed rate, stale/invalid SetPosition pre-enqueue no-op, identity stability, pre-gate/Ready mode semantics, relative arithmetic and player exact seek correlation.
- Session verification: desktop-integration 23, player-core 534, app-egui no-default 548; strict touched-crate Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails and diff check PASS.

## S01Q queue read-boundary migration (2026-07-20)
- Desktop/MPRIS owner и neutral transport API не менялись: publication продолжает читать process-owned revisioned view/track lineage, не получает queue storage или mutation authority. App startup/view/controller callsites под transport мигрированы на intent-based queue reads; legacy `PlaylistQueue::items()`/`len()` удалены.
- Полный `app-egui` suite 719/719 подтвердил MPRIS capabilities, commands, track identity, timeline seek и detached/suspend regression без изменения queue order, IDs или revisions.
