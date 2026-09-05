# S11 — Export runtime, atomic writer и toolbar UI (2026-07-20)

## Ownership и runtime lifecycle
- `PlaylistRuntime` единолично владеет `PlaylistExportIoOwner`; renderer публикует только typed `PlaylistExportRequest { scope, format }`.
- Owner допускает максимум один picker/preflight/writer job, присваивает monotonic generation только после успешного spawn, получает terminal через app wake mailbox, подавляет stale/cancelled completion, сохраняет exact generation при panic и участвует в общем bounded shutdown report.
- Scope снимается как immutable S10 `PlaylistExportSnapshot` до dialog worker-а. Full использует canonical top-level queue; selected переводит selection в canonical `PlaylistEntryId`, и любая selected compound part включает всю group source-order запись.
- Export не меняет queue, current, selection, allocator, revisions или persistence dirty state.

## Preflight, secret policy и writer
- Explicit format/scope выбраны до `rfd::AsyncFileDialog`; format задаёт filter, suggested extension и title. Typed private overwrite intent означает replacement target-а, выбранного save dialog.
- `playlist-io` выполняет весь locator preflight и serialization до первой filesystem mutation. URL policy повторно использует app service registry; yt-dlp userinfo/password/query требует aggregated export acknowledgement. Service durable payload fail-closed с `ServiceOwnerUnavailable` до будущего S19 owner mapping.
- Sensitive result содержит redacted prepared continuation (generation, target, bytes, overwrite intent, warning), занимает generalized confirmation slot только если он свободен и не вытесняет более новый URL/import intent. Confirm запускает writer той же generation; Cancel не касается target.
- Единственная target mutation вызывает `atomic_file_store::replace_file_atomically`. Outcomes остаются различимыми: pre-rename `NotReplaced`, post-rename `ReplacedDurabilityUnconfirmed`, `Durable`; Unix temp/target user-only 0600.

## UI и artwork
- Export — шестой left toolbar slot сразу после Import; исходные четыре axes, 32-point row и независимый Clear anchor сохранены.
- Popup order: `Весь плейлист -> M3U8/XSPF`, `Выбранные (N) -> M3U8/XSPF`. Empty queue отключает control; empty selection только selected branch; active export job запрещает конфликтующий старт.
- Pointer/keyboard/accessibility принадлежат app-egui; `ui-artwork-egui` владеет neutral Export glyph. Import/Export имеют общий tray bounds и противоположное направление стрелки.

## Verification
- PASS: `cargo test -p app-egui` — 758; `cargo test -p app-egui --no-default-features` — 756.
- PASS focused: `playlist-io` 69, `atomic-file-store` 8, `ui-artwork-egui` 29, `service-ytdlp` locator 5; S11 runtime tests покрывают atomic/durability, sensitive no-touch + 0600, cancel/stale/conflict/shutdown/panic и no queue/revision mutation.
- PASS: strict app Clippy default/all-features и no-default (`-D warnings`, только documented baseline allowance `clippy::large_enum_variant`), strict artwork/service Clippy, Rust 1.96 locked workspace all-targets/all-features check, rustfmt, `git diff --check`, refactor guardrails.
- Serena diagnostics чистые для tracked touched boundaries; новые untracked `export_io/tests.rs` и `toolbar/export_menu.rs` временно дали stale unresolved/unlinked IDE signals, тогда как Cargo test/check/Clippy authoritative и полностью зелёные.


S12 deterministic lifecycle coverage: export_io/tests/pending_writer.rs holds a real writer before mutation using channel rendezvous; owner.drain must report pending and retain ownership. After release, the actual atomic writer produces durable M3U8 bytes, owner returns exactly one Written terminal and closes the job. Production export contracts unchanged. This removes scheduling dependence from StillRunning and owner pending drain paths.
