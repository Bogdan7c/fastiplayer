# Субтитры: план завершён (сессия 15, 2026-10-19)

План `user/subtitles/` (issue #1) закрыт: сессии 01–15 PASS, итог и SHA — README плана «Итог плана», отчёт `user/subtitles/results/15.md` (там же текст закрывающего комментария; issue #1 закрыт и main запушен по поручению владельца 2026-10-19).

## Где что лежит (финальное состояние)
- Публичная матрица (версия **0.2.0**, ещё не выпущена; решение владельца): `docs/subtitles/compatibility.md`. Сверху сводки «формат × источник», «возможность × вид дорожки», раздел «How support is verified» (доказательства по трём критериям issue #1 со ссылками на тесты), в конце «Known limitations (summary)». Новая фича/ограничение субтитров => обновить матрицу и указать версию.
- Пользовательская справка: `docs/subtitles/user-guide.md` (источники, CC/меню, хоткеи C/V/Z/X/B, автовыбор, память, режим правки, все ключи `[subtitles]` и `[subtitles.stream]`, config v23). При добавлении ключа конфига — обновлять таблицу.
- README «What works today»/«Good to know»/roadmap п.1 done; CHANGELOG `[Unreleased]` «Planned as 0.2.0»; ARCHITECTURE строки «Subtitle engine (renderer-neutral)» и «Subtitle sources and presentation» + Validation map.
- Доказательства: GPU read-back тесты `crates/app-egui/src/state/subtitles/tests/*_gpu.rs` и `stream_*`; эталоны libass 0.17.5 — 187 случаев / 327 PNG `crates/subtitle-raster/tests/fixtures/libass/`, тесты `subtitle-raster/src/libass_parity/`, генератор `scripts/subtitle-reference/generate.py` (нужен системный libass, сборка/CI — нет).
- Нейтральность пяти `subtitle-*` crate-ов — `scripts/check-refactor-guardrails.py` (allowlist-ы).

## Карта памяти по сессиям
Ядро/форматы `mem:subtitles/core`; растеризатор `mem:subtitles/raster`; экран `mem:subtitles/on-screen-s03`; CC/хоткеи `mem:subtitles/controls-s04`; вид/правка `mem:subtitles/appearance-edit-s05`; встроенные `mem:subtitles/embedded-tracks-s06`; аудио+память `mem:subtitles/audio-tracks-memory-s07`; ASS `mem:subtitles/ass-layer1-s08`, `mem:subtitles/ass-layer2-s09`, `mem:subtitles/ass-layer3-s10`; поток-рисовальщик `mem:subtitles/render-thread-s09`; PGS `mem:subtitles/pgs-s11`; YouTube `mem:subtitles/web-tracks-s12`; стриминг `mem:subtitles/stream-feed-s13`, `mem:subtitles/stream-tracks-s13`, `mem:subtitles/dash-text-tracks-s13`; edge cases `mem:subtitles/edge-cases-s14`.

## Найдено на финальной приёмке и исправлено (вне субтитров)
- rustdoc: публичная doc-ссылка на приватную константу в `player-core/src/session/tick/result_merge.rs` (`ci-checks.sh docs` валил) — ссылка заменена текстом.
- Нативный DASH не открывал MPD от ffmpeg: допуск покрытия static Period (`mem:media-services/native-dash-vod-n09-2026-09-01`, раздел 2026-10-19) + Topology VideoOnly/AudioOnly в `build_presentation`.
- isomp4-патч учитывает `elst` (пустая правка + сдвиг начала) — A/V синхрон ffmpeg-MP4/fMP4: `mem:dependency-patches/core`.

## Стенд приёмки (как повторить)
uinput-драйвер (одно устройство на серию клавиш: первое событие свежего устройства теряется), источник drag & drop на PySide6 (`QT_QPA_PLATFORM=xcb` для X11), spectacle, изолированный конфиг `XDG_*`, X11 = `WINIT_UNIX_BACKEND=x11` без `WAYLAND_DISPLAY`. Клик uinput-указателем по шкале времени оставляет надпись на цели (особенность драйвера) — перематывать клавишами. Медиа — только Sintel (CC-BY) и samples.ffmpeg.org; файлы держать в scratchpad, не в репозитории.
