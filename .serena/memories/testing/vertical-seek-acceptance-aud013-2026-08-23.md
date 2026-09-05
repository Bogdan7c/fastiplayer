# AUD-013: vertical seek acceptance до WGPU submit (2026-08-23)

## Вердикт отдельной проверки

- Независимая read-only сессия подтвердила исходный coverage gap: ни один тест не соединял real compressed asset с production demux, production decoder, production materializer, `WgpuVideoRenderer::render_or_clear`, `wgpu::Queue::submit` и submitted completion release до и после ненулевого seek.
- Прежний strongest test `crates/video-ffmpeg/tests/pts_only_mpeg_ts.rs` завершался прямым decoder release и не входил в WGPU boundary.
- На generated H.264/MPEG-TS отдельный production bug не воспроизвёлся: demux/decode/generation/PTS работали. Дефектом был именно разрыв системного доказательства.

## Реализованная граница

- Test-only orchestration живёт в `crates/video-ffmpeg/tests/vertical_seek_wgpu.rs`; production crates/API не менялись.
- Один `MpegTsDemuxer` и один `FfmpegSoftwareVideoBackend`, обёрнутый `wrap_video_backend_for_wgpu_submission`, проходят start generation 1, submitted release, decoder flush, nonzero seek 2 s и generation 2.
- Production `HostPlanarWgpuFrameMaterializer` загружает AVFrame-backed Y/U/V planes.
- Production `WgpuVideoRenderer::render_or_clear` рисует в offscreen BGRA8 texture; command buffer делает texture readback и передаётся настоящему `Queue::submit`.
- PASS требует video draw (`render_or_clear == true`), non-black RGB readback, current generation, post-seek PTS >= target и Missing descriptor только после submission-aware GPU completion release.
- EOF helper обязан удерживать максимум один renderer candidate; удержание всего decoded tail заполняет bounded host pool и блокирует terminal EOF.

## Corpus и команда

Corpus synthetic/local, ничего не скачивается:

```bash
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=160x90:rate=5 \
  -t 4 -c:v libx264 -preset ultrafast -profile:v baseline \
  -bf 0 -g 5 -keyint_min 5 -sc_threshold 0 -pix_fmt yuv420p -an \
  -muxpreload 0 -muxdelay 0 -mpegts_flags +resend_headers \
  -f mpegts -y /tmp/fastiplayer-aud013-vertical-seek.ts

scripts/media-regression.sh \
  --scenario h264-ts-seek-wgpu-ffmpeg \
  --path /tmp/fastiplayer-aud013-vertical-seek.ts
```

Observed marker:

```text
AUD013_FIXED before_generation=1 before_pts_us=0 after_generation=2 after_pts_us=2000000 materializer=host-planar-wgpu renderer=wgpu-video submit=completed release=completed
```

## CI и проверки

- `.github/workflows/ci.yml` содержит blocking job `Vertical seek acceptance (FFmpeg + WGPU)`.
- Job ставит FFmpeg + Mesa lavapipe, генерирует corpus и вызывает exact ignored target через `scripts/media-regression.sh`.
- Focused verification: exact real-media scenario PASS; `video-ffmpeg` 87/87; `render-wgpu-video` 100/100; strict all-target Clippy; refactor guardrails; format-guardrails; dependency inventory; `git diff --check`.
- Dependency report сохраняет только известные non-blocking RUSTSEC-2026-0150 (`audiopus_sys`) и RUSTSEC-2026-0192 (`ttf-parser`).

## Ограничения матрицы

- Blocking full vertical сейчас доказана для MPEG-TS/H.264 Baseline × FFmpeg software × HostPlanar WGPU.
- Smooth/PIFF остаётся production demux/seek evidence без decoder/materializer/renderer composition.
- Real compressed VA-API → DMA-BUF → WGPU submit остаётся NOT RUN и требует compatible hardware runner.
- Эти строки нельзя считать PASS по unit/fake/synthetic tests.

См. также `mem:video-ffmpeg/pts-only-packet-timebase-aud003-2026-08-23`, `mem:render-video/core`, `mem:testing/media-fixtures`.