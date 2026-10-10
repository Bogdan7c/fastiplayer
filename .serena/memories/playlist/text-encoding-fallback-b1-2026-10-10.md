# B1: UTF-8 с запасным Windows-1251 для .m3u/.cue (2026-10-10)

Решение владельца (user/ux-edge-cases-2): сначала UTF-8; невалидный UTF-8 -> Windows-1251 + короткое уведомление «Плейлист прочитан в кодировке Windows-1251».

- Владелец декодирования: `playlist-io/src/text_encoding.rs` (`decode_utf8_or_windows_1251`, `LegacyFallback::{Windows1251, Forbidden}`, публичный `PlaylistTextEncoding::{Utf8, Utf8WithBom, Windows1251}`). Библиотека `encoding_rs` (workspace dependency, лицензии в allowlist deny.toml).
- M3U: `M3uDeclaredFormat::M3u` разрешает fallback, `M3u8` — нет (RFC 8216: всегда UTF-8, остаётся `InvalidUtf8`). `GenericM3uPreview::text_encoding()`.
- CUE: `cue/encoding.rs` — BOM как раньше; без BOM UTF-8 -> Windows-1251 (`CueTextEncoding::Windows1251`); NUL в тексте по-прежнему отказ (UTF-16 без BOM).
- Рекурсивное раскрытие: `LocalPlaylistExpansionSummary::windows_1251_documents()` (не issue — предупреждения в preview не появляется).
- XSPF не тронут: XML сам объявляет кодировку (bounded-xml-reader).
- Пути из cp1251-плейлиста после декодирования — обычные Unicode-пути Linux (имена файлов, перенесённые с Windows, в UTF-8).
- App: `playlist_runtime/import_io/notice.rs` (`PlaylistImportNotice`, очередь внутри `PlaylistImportIoOwner`), `PlaylistRuntime::drain_playlist_import_notices` -> `frame_prepare` -> `AppState::show_playlist_import_notices` (`notify_info`, 8 с). Одно уведомление на импорт, даже если несколько вложенных файлов в cp1251.
- Тесты: `playlist-io/tests/legacy_encoding.rs`, `text_encoding::tests`, `app-egui/.../import_path/tests.rs` (cp1251 m3u/cue -> записи + уведомление, UTF-8 -> без уведомления).
