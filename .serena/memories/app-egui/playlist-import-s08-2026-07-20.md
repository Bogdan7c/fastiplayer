# Web media roadmap S08 — source-neutral import transaction (2026-07-20)

## Ownership и staged flow
- `PlaylistRuntime` теперь владеет единственным latest-only `playlist_runtime::import_transaction` owner-ом: opaque `PlaylistImportPreviewId`, monotonic generation, expected app structural revision, ID-less accepted prefix и immutable source-neutral preview counts/issues/source+capacity truncation/sensitive durable-locator count.
- Typed intents: `AppendToQueue`, interactive `ReplaceQueue`, trusted `StartupReplace`. Parser/service/app locator mapping не получают queue или allocator authority. Preview `Continue` является explicit partial/truncation decision; после него sensitive durable-locator acknowledgement и destructive replacement публикуются одним composed deterministic reason set (`SensitiveDurableLocatorPersistence`, затем `QueueReplacement`) в прежнем authoritative `PendingPlaylistConfirmation` slot-е. Параллельного prompt/pending owner-а нет.
- New import/URL/main-open/row-play/structural replacement/shutdown boundaries взаимно supersede staged import и confirmation. Generation + structural revision revalidate-ятся и перед confirmation, и непосредственно перед commit. Cancel/stale/shutdown/materialization/domain failure дают нулевую queue mutation.

## Domain commit и capacity
- `playlist-core::PlaylistImportEntryDraft::into_queue_draft` materialize-ит neutral payload без IDs. Local/URL durable locator становится legacy operational locator; service child использует только local/URL provenance root и typed-reject-ится, если такого root нет. Полный durable payload сохраняется.
- App preview вычисляет maximal whole-entry prefix. После первого overflow весь tail rejected; compound никогда не режется. Item/Group IDs и оба allocator watermark публикуются только `PlaylistQueue::{append_entries,replace_entries}` внутри controller commit.
- `PlaylistController::commit_import_append/replace` владеет app dirty/structural publication и preflight. Append не меняет active/current/playback. Empty/zero-cap prefix — typed no-op.

## Interactive replacement-detached lifecycle
- Interactive Replace атомарно заменяет queue, очищает persisted traversal current/selection/runtime row errors, detach-ит exact old active identity и удаляет old removal tombstone/automatic continuation. Он не вызывает `Clear`, не планирует media reset и не переиспользует removal continuation.
- Отдельный `ReplacementDetachedDisposition` обходит restart-first Previous: Next выбирает первый, Previous последний source-order playable target новой queue. `playlist-core::begin_replacement_detached_navigation` сохраняет compound/shuffle accounting; target failure остаётся D55 awaiting-user и не запускает hidden scan.
- Clean Ended у detached active без tombstone даёт ровно один Stop. Matching Stop или любой successful strong install снимает replacement disposition. StartupReplace не включает special manual projection.

## XSPF locator registry
- App-owned `xspf_locator_registry` проходит ordered `XspfLocationCandidate`: reversible native `file:` либо существующий service registry; выбирает первый admissible candidate, сохраняет safe rejected-prefix issues и sensitive count. Остальные locations не открываются. Никакого media open/probe/fetch до explicit playback intent.

## Verification
- PASS: 9 focused playlist-core import tests; 12 focused app S08/XSPF tests; full `playlist-core` 125 tests; full `app-egui --no-default-features` 734 tests; Rust 1.96 workspace locked check; Rust 1.92 focused check; rustfmt; refactor guardrails; cargo-deny advisories/bans/licenses/sources; git diff check; Serena diagnostics on all new/changed boundary files.
- Clippy нового/touched кода PASS с `-D warnings` после allowance только для двух pre-existing unrelated app `large_enum_variant` baseline diagnostics in `state/strong_media_open{,/pending}.rs`; unmodified strict invocation reports only those two baseline lints.
- S09 UI ещё не реализован: preview/confirmation read models и action API готовы, но toolbar/preview renderer остаётся следующей session.