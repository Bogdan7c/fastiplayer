# Session 14A: destructive queue replacement confirmation (2026-07-15)

Эта memory дополняет `mem:core`, `mem:app-egui/media-open-coordinator-s10c`, `mem:app-egui/wake-runtime-s10a` и `mem:app-egui/playlist-persistence-s14`.

## Ownership и boundary
- `playlist_runtime::replacement_confirmation` владеет единственным process-lifetime pending slot-ом, monotonic opaque intent ID, original secret-bearing local/typed service URL intent и committed-queue admission policy. Renderer-bound `AppState` не хранит authoritative confirmation state.
- In-app и startup/CLI origin разделены типами `InAppQueueReplacementIntent` и `TrustedStartupQueueReplacementIntent`; forgeable `confirmed: bool` отсутствует. Trusted origin bypass-ит dialog только через отдельный API и одновременно supersede-ит старый pending slot.
- Immutable `PendingQueueReplacementConfirmation` содержит только opaque ID и bounded/redacted `SafeMediaLabel`. UI возвращает `QueueReplacementConfirmationAction { intent_id, decision: Confirm|Cancel }`.
- Matching Confirm сначала атомарно забирает exact slot, затем возвращает non-forgeable `AdmittedQueueReplacementIntent` следующему owner-у. Repeat/stale response — typed no-op. Cancel удаляет slot без queue/active/dirty mutation.

## No-I/O-before-confirm workflow
- In-app local open разделён на две независимые background-фазы: `LocalFileOpenJob::spawn_picker` только получает target из `AsyncFileDialog`; `spawn_preparation` вызывает `prepare_local_file` только после runtime admission/Confirm.
- Empty committed queue выдаёт admitted token сразу. Nonempty committed queue сохраняет prompt и не передаёт path/URL coordinator-у, player-у, discovery или preparation owner-у.
- До load decision committed queue отсутствует: explicit in-app open следует D65 через `record_startup_media_replacement`, supersede-ит только restore apply и остаётся ID-less до allocator gate.
- Production in-app URL editor ещё отсутствует. Typed direct/YouTube admission и redaction покрыты tests, но новая URL input UI/network route не добавлена.

## Lifecycle и UI
- Новый explicit open заменяет старый slot. Explicit row Play, Clear и несовместимая queue replacement имеют отдельный supersede boundary. Совместимые Remove/RemoveOthers, current Play/Pause/seek observation и selection slot не меняют.
- Shutdown отменяет confirmation до завершения media/persistence owners. Suspend/resume и sidebar hide ничего не переносят: slot остаётся в process-lifetime runtime и новая AppState читает тот же model.
- Entity рендерится внутри существующего `AppState::render_center_overlay`. `ui::queue_replacement_confirmation` не создаёт `Window`, `Area` или второй `CentralPanel`.
- Local/URL payload types имеют redacted custom Debug либо вообще не реализуют automatic formatting. Local status/logging используют generic label без native/foreign path units, URL — service-owned safe label; full path, URL userinfo/path/query/fragment не публикуются.

## Verification
- PASS: 9 focused confirmation/UI guardrail tests; 8 local-file tests; full `app-egui --no-default-features` 461 tests; strict app all-targets/all-features Clippy; fmt; Rust 1.96 locked workspace check; refactor guardrails; diff check.
- Cargo linkage green; Serena diagnostics для всех изменённых production-модулей чисты.
- Out of scope: sibling discovery, production in-app URL editor, suspend media checkpoint/reopen. Следующая session — 14B.

## Session 16 generalized D15 + D79 confirmation
- The same process-lifetime slot now stores `PendingPlaylistConfirmation`: opaque intent ID, redacted safe label, and typed `PlaylistConfirmationReasons` (`queue_replacement`, `sensitive_url_persistence`). It can own either the original replacement target or an uncommitted sensitive URL append draft; no second prompt/slot exists.
- Direct in-app URL append uses the existing service classifier/normalizer without network I/O. Exact direct persistence identity is preserved. `service-direct-media::DirectMediaUrl` computes and exposes the intent-named sensitive-persistence requirement from its already parsed components; app/controller code never reparses raw URL. Raw URL never enters model/error/debug. Matching Confirm alone can commit; Cancel, supersede, shutdown, stale/repeated response are typed no-ops.
- Sensitive direct URL admission is gated even before load decision or with an empty committed queue. A direct URL replacement of a nonempty queue produces one composed reason set. The old Session 14A `pending_queue_replacement_confirmation` compatibility accessor returns a model only for replacement-only state; sensitive/composed state returns `None`, so old UI cannot silently confirm a reason it did not render. Session 19 must render the generalized model and return `PlaylistConfirmationAction`.
- URL append allows duplicate locators with new IDs and never starts playback. Verification is part of Session 16: 491 app no-default tests, strict all-target no-default/all-features Clippy, fmt, locked workspace check, guardrails, diff check, and clean Serena diagnostics.
