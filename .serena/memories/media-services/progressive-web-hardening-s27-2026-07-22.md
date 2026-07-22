# S27 — Progressive/web UX hardening gate (2026-07-22)

## Итог

- Milestone закрыт для regression URL `https://youtu.be/QTQP64pvv34`: свежий debug binary проходит реальное yt-dlp extraction, queue-owned preparation, seekable progressive open, H.264/AAC demux и playback без прежних ошибок `planner не нашёл playable candidate` и `302 Found во время range-read`.
- Доказаны обе decode ветки:
  - HW: AMD Radeon 780M, `VA-API H.264`, 640x368, NV12 DMA-BUF zero-copy, AAC через Symphonia/CPAL.
  - SW: при test-only недоступном VA driver capability probe оставляет один backend и выбирает `ffmpeg-software` + `ffmpeg-host-upload-wgpu`; тот же H.264/AAC URL открывается и играет.
- Manual runner по контракту всё равно пишет `MANUAL REVIEW REQUIRED`, потому что не превращает визуальную UX-проверку в ложный автоматический PASS. Он принимает только explicit user URLs и сохраняет только sanitized evidence.

## Три независимых root cause

1. yt-dlp 2026.07.04 добавляет `downloader_options.http_chunk_size=10485760` к реальным HTTP formats. Старый normalizer blanket-reject-ил любое `downloader_options`, поэтому все media rows исчезали.
2. После переноса chunk hint format 18 с `avc1.42001E` выявил отсутствие ordinary H.264 Baseline; из-за существующего all-or-nothing `planning_snapshot()` этот один `RuntimeRequirement` обрывал весь snapshot. Shared typed classifier теперь различает Baseline / ConstrainedBaseline / Main / High, поэтому этот реальный row становится representable. Общая all-or-nothing семантика planning adapter-а не менялась и остаётся отдельным known limitation для будущего accepted-but-unrepresentable профиля.
3. Initial `Range: bytes=0-0` получал `206`, но поздний CDN `302` возникал уже в background prefetch. Initial redirects обрабатывал `web-media-http`, а `HttpRangeSource` раньше превращал read-time 3xx в fatal `HttpStatus`.

## Архитектурные владельцы

- `service-ytdlp` остаётся extraction/normalization/request-mapping owner. Он принимает только exact bounded positive `http_chunk_size` и переносит его как neutral `HttpRangeRequestLimit`; arbitrary downloader config/private live state не исполняется и остаётся typed rejection.
- `codec-core` владеет `H264ProfileIndication` и exact profile classification для codec tags, avcC и SPS. Capability report schema v7 сериализует distinct `baseline`.
- FFmpeg SW рекламирует/принимает ordinary Baseline 8-bit 4:2:0 через host-planar contract.
- VA-API рекламирует Baseline только при exact `VAProfileH264Baseline`; cros-codecs различает ordinary/constrained Baseline по SPS constraint evidence и не подменяет их Main.
- `source-core` владеет физическими Range requests, parsed redirect target, per-logical-read hop count, cancellation и method/body mechanics.
- `web-media-http` владеет redirect authorization и scoped secret rematerialization. Automatic reqwest redirects остаются выключены.
- Каждый logical read/retry начинает redirect chain с immutable stable base material. Redirected target/headers/body локальны цепочке. Cross-origin stripping и 302→GET монотонны; последующий 307 не может воскресить body.
- `app-egui::web_media_open` остаётся единственным composition root. Queue Ready/authorize/Enqueued/Installed boundary, URL sidebar, exact durable locator и transient secrets не смешаны.

## Focused regression proof

- Source test: два logical reads каждый идут stable base POST → 302 → 307 → final 206; hop counts `0,1,0,1`; middle/final остаются GET без body; Range diagnostics считают шесть физических requests.
- HTTP provider integration: поздний cross-origin redirect проходит через настоящий prefetch к final 206; target не получает Authorization, initial Cookie или base Set-Cookie.
- Candidate tests: VP9 neighbor, `avc1.42001E` ordinary Baseline и `avc1.42E01E` ConstrainedBaseline остаются отдельными playable planning rows.
- HW/SW focused tests проверяют exact profile matching, условную VA profile рекламу, SPS→VA mapping и FFmpeg host-planar acceptance.

## Проверки

- Affected test command прошёл для `app-egui`, codec/capability/service/source/demux/HW/SW/transport/planner crates; `app-egui` — 825 tests, `video-vaapi` — 137 tests.
- Strict affected all-targets Clippy с `-D warnings`: PASS.
- Debug build `cargo +1.96.0 build --locked -p app-egui`: PASS.
- `cargo fmt --all --check`, `git diff --check`, `bash -n scripts/progressive-web-smoke.sh`, `scripts/check-refactor-guardrails.py`: PASS.
- Live sanitized reports: HW 30-second timebox and forced-SW 25-second timebox both ended only by expected runner timeout status 124, without planner/range fatal.

## Known separate limitation

- The strong staged web install currently can lose an explicit `video.preferred_backend=software` preference and choose available hardware after the app initially selected the software pipeline. S27 did not widen scope into that pre-existing preference-propagation architecture; SW fallback itself is proven by making hardware unavailable. Track this separately if exact user-forced backend selection is required.

## Связанные memories

- `mem:core`
- `mem:media-services/core`
- `mem:media-services/progressive-http-s22-2026-07-22`
- `mem:media-services/web-transport-s21t-2026-07-21`
- `mem:codec-core/h264`
- `mem:video-ffmpeg/software-design`
- `mem:video-vaapi/core`
- `mem:app-egui/media-open-coordinator-s10c`
- `mem:app-egui/sidebar-controller`
