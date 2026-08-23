# AUD-015: bounded FFmpeg worker shutdown/join (2026-08-23)

## Статус и независимое подтверждение

- AUD-015 отдельно воспроизведён на production `FfmpegDecoderWorker::run` и исправлен 2026-08-23.
- Pre-fix test заполнял настоящий `FfmpegHostResourceProvider` capacity 1 реальным AVFrame-backed resource, оставлял compressed packet в bounded queue, дропал packet/control senders и удерживал resource lease.
- Termination hook стабильно не срабатывал за 250 ms и за дополнительный 8 s timeout.
- Context7 `/websites/rs_crossbeam-channel` подтвердил: disconnected receive operation считается ready, а selected `recv` возвращает `RecvError`.

## Корень проблемы

При `free_slots() == 0` worker исключал `packet_rx` из `select!`. После frontend drop `control_rx` становился permanently ready/disconnected. Старое условие выходило лишь при `packet_rx.is_empty()`; queued packet делал условие ложным, пустая error-ветка немедленно повторяла цикл, а удержанный lease не создавал release notification.

## Исправленная boundary

- `FfmpegVideoDecoderThread` остаётся единственным lifecycle owner worker-а.
- Новый внутренний owner `decoder_thread/lifecycle.rs::FfmpegWorkerLifecycle` хранит отдельный bounded shutdown sender и `JoinHandle`.
- `FfmpegVideoDecoderThread::drop` явно вызывает `shutdown_and_join()` до автоматического drop packet/control/resource fields.
- Shutdown signal имеет собственную capacity 1 и не зависит от packet/control backpressure или host-pool fullness.
- `FfmpegDecoderWorker::run` проверяет shutdown перед обычными operations и слушает его в full-pool и normal select branches.
- Disconnect единственного frontend-owned control sender-а также является terminal lifecycle signal; queued packets старого frontend-а lifecycle не продлевают.
- `FfmpegWorkerLifecycle` забирает `JoinHandle` через `Option::take`, поэтому join выполняется exactly once; собственный `Drop` служит fallback.
- Neutral `VideoDecoderThreadHandle`, runtime config, packet completion, frame release, flush/EOF state и error semantics не менялись.

## Regression anchor

`crates/video-ffmpeg/src/decoder_thread/tests.rs::disconnected_frontend_terminates_worker_with_full_pool_and_queued_packet` проходит настоящий `FfmpegVideoDecoderThread::spawn`, заполняет production host pool, отправляет два packet-а (минимум один остаётся queued), дропает frontend в наблюдаемом thread-е и требует возврата `Drop + worker join` за 1 s до освобождения удержанного resource.

## Проверки

- Focused AUD-015 regression: 1/1 PASS.
- `cargo +1.96.0 test -p video-ffmpeg --features ffmpeg --locked`: 88/88 unit PASS; external real-fixture rows explicit ignored.
- `cargo +1.96.0 test -p video-ffmpeg --no-default-features --locked`: 60/60 PASS.
- `cargo +1.96.0 test -p player-core --locked`: 646/646 PASS.
- `cargo +1.96.0 clippy -p video-ffmpeg --features ffmpeg --all-targets --locked -- -D warnings`: PASS.
- `cargo +1.96.0 fmt --all --check`, refactor guardrails и `git diff --check`: PASS.
- Serena diagnostics: только ожидаемый opposite-cfg inactive-code hint; errors/warnings отсутствуют.

## Ограничение

Regression детерминированно закрывает подтверждённое full-pool/queued-packet состояние. Синхронный join ожидает завершение текущего FFmpeg owner operation; отдельный deadline/forced cancellation для зависшего внутри внешнего FFmpeg вызова этой правкой не вводился.