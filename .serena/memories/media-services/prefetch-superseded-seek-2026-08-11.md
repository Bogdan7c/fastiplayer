# Progressive HTTP MP4: superseded seek и bounded candidate fallback (2026-08-11)

## Симптом и доказанные причины

Исходная acceptance row 08 (`https://archive.org/details/BigBuckBunny_328`)
через yt-dlp корректно выбирала playable MP4 inventory candidate (H.264
Baseline + AAC), но transport не доходил до demux.

1. ISO-BMFF metadata probe делает быстрый seek к EOF и сразу обратно к 0. Старый `media-prefetch` немедленно очищал RAM window при первом out-of-window seek. Возврат к 0 терял уже загруженные первые 64 KiB и создавал duplicate Range с offset 0.
2. Archive.org metadata указывала единственный storage-host
   `dn801203.us.archive.org`. Он не отвечал на независимые curl probes и через
   primary, и через альтернативные Archive routes. Увеличение transport
   deadline лишь превращало быстрый честный failure в многоминутное ожидание;
   source-core не должен лечить недоступный внешний origin скрытыми retries.
3. Отдельный архитектурный дефект был в BestPlayable orchestration: первая
   planner identity с typed `NetworkUnavailable` завершала весь open, хотя
   immutable extraction snapshot содержал другой ranked candidate того же
   intent-а. При этом timeout нельзя последовательно умножать на размер
   inventory.

XSPF import, queue-owned URL open, yt-dlp normalization/planner, codec
capability и Symphonia container routing не были причиной.

## Инварианты media-prefetch

- Foreground out-of-window seek только публикует `seek_request` и отменяет active fetch; фактическим `buffer.reset_to` владеет worker при consume запроса.
- Пока worker не потребил pending seek, исходное RAM window остаётся authoritative.
- Быстрый возврат в это окно снимает pending seek и переиспользует bytes без refetch.
- Foreground read не читает физически сохранённое старое окно, пока настоящий pending seek остаётся активным.
- Cancellation token active fetch-а является publish proof: отменённый read/seek result нельзя публиковать, даже если pending seek уже superseded.
- Shutdown-cancellation не учитывается как foreground cancelled fetch.
- `refetches` увеличивается только когда worker действительно применил новое окно.

Реализация: `crates/media-prefetch/src/{seek,shared,source,worker}.rs`.
Регрессия: `source::tests::active_fetch::superseded_out_of_window_seek_returns_to_buffer_without_duplicate_refetch`.

## Инварианты candidate fallback

- Fallback разрешён только для BestPlayable; Exact и Composed остаются одной
  точной попыткой.
- Typed runtime `ContentProbeRejection` может перейти к следующей planner
  identity, как и раньше.
- Typed registry-level
  `TransportOpenError::Transport(TransportFailure::NetworkUnavailable)` может
  обойти ровно один physical candidate. Вторая такая ошибка terminal, поэтому
  большой inventory не создаёт неограниченную цепочку запросов.
- `Timeout`, authentication, cancellation, parser и прочие provider errors
  terminal с первой попытки. Особенно важно, что timeout не умножается на
  число candidates.
- Это candidate fallback внутри одного immutable extraction snapshot-а, а не
  повтор того же HTTP request-а и не повторный запуск yt-dlp.
- Экспериментальные three-attempt/60-second HTTP recovery изменения полностью
  удалены; `source-core` и `web-media-http` оставлены на исходной bounded policy.

Реализация:
`crates/app-egui/src/web_media_open/content_probe_fallback.rs`.
Регрессии проверяют успешный переход после одного `NetworkUnavailable`, остановку
на втором и terminal timeout без обращения к alternate candidate.

## Проверка

- Prefetch-регрессия падала на старом коде (`refetches=2` вместо 0), после
  исправления проходит.
- Целевой набор после удаления HTTP recovery: media-prefetch 34/34,
  source-core 56/56, web-media-http 14 unit + 8 integration;
  content-probe fallback 11/11.
- Hermetic production-shaped regression
  `network_unavailable_http_open_uses_second_real_candidate` поднимает два
  loopback origins: первый возвращает 404/typed `NetworkUnavailable`, второй
  проходит реальный HTTP Range, Ogg demux и production Opus decode до
  ненулевого PCM.
- Полный workspace со всеми targets проходит сериализованно; strict Clippy
  `-D warnings`, rustfmt, diff check и refactor guardrails также проходят.
- Release runtime на W3C media-events page доказал новую network ветку:
  недоступный duplicate WebM candidate был обойдён, следующий WebM/Vorbis
  candidate открылся примерно за 1.5 s и запустил audio/video.
- Acceptance row 08 заменена на
  `https://www.w3schools.com/html/tryit.asp?filename=tryhtml5_video`. Release
  runtime отклонил первый Ogg/Theora candidate content proof-ом, открыл
  788,493-byte ISO-BMFF MP4 примерно за 0.8 s, обнаружил H.264 + AAC 48 kHz
  stereo, настроил VA-API, запустил audio и опубликовал DMA-BUF video frame.
- Archive fixture не объявляется исправленной: её единственный внешний
  storage node остаётся недоступен. Она удалена из acceptance как невалидный
  внешний oracle, а не замаскирована длинным timeout-ом.

См. также `mem:media-services/progressive-http-s22-2026-07-22` и `mem:media-services/content-probed-runtime-fallback-2026-08-05`.
