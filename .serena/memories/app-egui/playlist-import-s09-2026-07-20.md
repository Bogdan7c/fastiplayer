# Web media roadmap S09 — Import toolbar и preview UI (2026-07-20)

## UI boundary и toolbar
- `PlaylistAction` получил три typed post-render действия: `StartImport(PlaylistImportIntent)`, `ContinueImport(PlaylistImportPreviewId)` и `CancelImport(PlaylistImportPreviewId)`. Renderer только публикует intent после egui render; native dialog, parser и queue mutation в UI отсутствуют.
- Icon-only toolbar сохранил 32-point row, первые четыре left axes (`AddFiles`, `AddUrl`, `Sort`, `CurrentItem`) и независимый правый Clear anchor. Новый Import занимает пятый left slot сразу после Current Item. На widths 350/420/600 layout остаётся non-overlapping.
- Import menu имеет ровно два явных intent-а в фиксированном порядке: «Добавить к плейлисту» -> `AppendToQueue`, «Открыть как новый плейлист» -> `ReplaceQueue`. Import disabled при active import dialog либо structural guard; tooltip/AccessKit принадлежат app UI.
- Нейтральная ручная геометрия `PlaylistToolbarGlyph::Import` (стрелка в tray) принадлежит `ui-artwork-egui`; app владеет layout/hit-area/interaction/actions. Characterization отдельно доказывает, что Import визуально не совпадает с Add Files.

## Process-lifetime I/O owner
- `playlist_runtime::import_io::PlaylistImportIoOwner` владеет максимум одним native single-file picker и worker job. Dialog показывает только M3U/M3U8/XSPF filters и использует `.pick_file()`; CUE отсутствует до S14. Фильтр не является validation authority.
- Worker вызывает authoritative S05/S06/S07 `playlist_io::expand_local_playlist` с bounded default limits/cancellation, затем pure `import_io::materializer` переводит expansion tree в S08 ID-less draft. Materializer сохраняет M3U drafts, XSPF first-admissible locator policy, metadata/provenance, safe issue/truncation/sensitive accounting и формирует Compound только для допустимого ненested group range.
- Worker публикует completion через wake mailbox, а `PlaylistRuntime::drain_playlist_import_job` единолично stage-ит preview. Shutdown использует общий deadline и включает import owner в `PlaylistShutdownReport`.
- `supersede_playlist_import_flow` отменяет active picker/parser и staged transaction на URL/main-open/row-play/structural replacement/shutdown boundaries. Owner проверяет authoritative cancellation marker на serialized drain boundary, поэтому даже completion, опубликованный до supersede, но ещё не применённый, не может воскресить stale preview. Duplicate `StartImport` не отменяет уже открытый picker.

## Preview/confirmation
- `ui::playlist::import_preview` рисует immutable clean/partial/issues/source truncation/capacity truncation/sensitive/replace states и возвращает не более одного typed action. Standard egui buttons поддерживают pointer, Tab, Space и Enter.
- Existing queue/sensitive confirmation остаётся единственным authoritative confirmation host и имеет приоритет в central overlay. Import preview показывается только при отсутствии confirmation; Continue может занять тот же composed S08 confirmation slot, а queue mutation выполняется позже runtime/controller boundary.

## Verification
- Focused import/UI tests: 25 PASS, включая in-flight cancellation и published-before-drain supersede race.
- `app-egui` default full suite: 747 PASS; `ui-artwork-egui`: 28 PASS.
- Rust 1.96 locked workspace all-targets/all-features check, strict touched app Clippy (с allowance только для двух прежних unrelated `large_enum_variant` baseline lint), rustfmt, `git diff --check` и refactor guardrails PASS.
- Serena rust-analyzer сохранил stale cache diagnostics (`materializer` module/старые struct fields/private method), хотя cargo compiler успешно собрал весь workspace и соответствующие файлы; новые materializer/preview/artwork файлы отдельно диагностически чистые.

Связанные memories: `mem:app-egui/playlist-import-s08-2026-07-20`, `mem:app-egui/playlist-ui-s20`, `mem:app-egui/artwork-boundary`, `mem:app-egui/playlist-toolbar-undo-2026-07-18`.