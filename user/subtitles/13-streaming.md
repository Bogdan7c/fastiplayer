# Сессия 13. Стриминг: HLS WebVTT и DASH TTML/IMSC

## Абзац для отправки вместе с этим файлом

> Выполни сессию 13 из приложенного Markdown для Fastiplayer (план `user/subtitles/`). Сначала прочитай `user/subtitles/README.md` и `results/04.md`. Разрешён только объём, указанный в файле. Соблюдай AGENTS.md. Сначала исследование и описание архитектуры, затем остановка и согласование со мной простым языком; реализация — после моего решения. Если объём не помещается — предложи разбиение (13a HLS / 13b DASH) на стопе. Результат запиши в `user/subtitles/results/13.md`. Работай в main без веток; коммит — только после зелёных проверок и моей ручной приёмки, push не делай.

## Границы

**Зависит от:** 04 (PASS).

**Memories:** `web-media-hls`, `hls-playlist-core`, `dash-mpd-core`, `media-source-open`, live/VOD memories.

**Разрешено:** HLS: загрузка subtitle media playlist по `InstalledHlsSubtitleRendition`, сегменты WebVTT, `X-TIMESTAMP-MAP` (сопоставление MPEG-TS/PTS времени), live-обновление плейлиста субтитров; DASH: перестать пропускать text AdaptationSet, модель text-representation, sidecar TTML (`application/ttml+xml`) и сегментированный `stpp` в fMP4 (MP4-патч — additive), парсер TTML/IMSC (стили, регионы — подмножество IMSC Text Profile) в `subtitle-formats`; WebVTT в fMP4 (`wvtt`) — если встречается.

**Запрещено:** изменение выбора видео/аудио-вариантов; субтитры Smooth/HDS.

## Факты (снимок)
- HLS: `hls-playlist-core/src/master.rs:89,159` — разбор `EXT-X-MEDIA TYPE=SUBTITLES`; `web-media-hls/src/selection.rs:82` `HlsSubtitleRenditionDescriptor`; `media-source-open/src/web_media_hls_subtitles.rs:7`; комментарий «S32B не загружает и не декодирует subtitles».
- DASH: `dash-mpd-core/src/parser/text_adaptation.rs` `consume_non_playback_text_adaptation_set` (вызов `parser.rs:229`).

## Задание

### Часть A
1. Отдельный конвейер загрузки субтитров рядом с видео/аудио, с собственными лимитами и без влияния на буферизацию видео; что при отставании или ошибке загрузки сегмента субтитров (пропуск, видео не ждёт).
2. Live: окно субтитров синхронно с окном видео; seek назад в DVR.

### Стоп: согласование с владельцем
Разбиение на 13a/13b при необходимости; поведение при ошибке сегмента.

### Часть B. Реализация
По согласованному.

## Тесты
- локальный HLS-стенд (как в тестах `web-media-hls`) с WebVTT-субтитрами → **функциональный**: текст **в кадре** в нужный момент, с `X-TIMESTAMP-MAP` со смещением;
- сегмент субтитров 404 → реплики этого сегмента отсутствуют, видео играет, остальное показывается;
- DASH с TTML → текст в кадре; IMSC-регион → позиция;
- live HLS → новые сегменты субтитров подтягиваются.

## Ручная приёмка
Локальные стенды HLS/DASH (ffmpeg/Shaka-пакеты), публичный тестовый поток с субтитрами, если доступен.

## Выходной барьер
`results/13.md`; матрица — HLS/DASH; memories web-субтитров.
