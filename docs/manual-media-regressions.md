# Manual media regressions

`cargo test --workspace --locked` is intentionally hermetic: it never reads local media files
or user URLs.

Real-media codec, seek, and direct-file HTTP regressions are run only through
`scripts/media-regression.sh --scenario <name> --path <file>`. The old
service-owned yt-dlp WebM scenarios were removed together with that opener and must not be
added back to this file runner.

Run `scripts/media-regression.sh --list-scenarios` to see the required properties of the
selected file. The runner neither searches `test-assets/` nor assumes any filename. It reports
the selected path, detected container, public track codecs, scenario, and a `PASSED`, `FAILED`,
or `NOT RUN: missing selection` outcome.

## PTS-only MPEG-TS через software FFmpeg

AUD-003 проверяется generated fixture-ом, который не добавляется в Git. Он должен содержать
H.264 без B-frames и не меньше трёх video PES packets с PTS и без DTS. Минимальный deterministic
asset можно создать локальным FFmpeg:

```bash
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=160x90:rate=5 \
  -t 4 -c:v libx264 -preset ultrafast -profile:v baseline \
  -bf 0 -g 5 -keyint_min 5 -sc_threshold 0 -pix_fmt yuv420p -an \
  -muxpreload 0 -muxdelay 0 -mpegts_flags +resend_headers \
  -f mpegts -y /tmp/fastiplayer-aud003-pts-only.ts

scripts/media-regression.sh \
  --scenario h264-ts-pts-only-ffmpeg \
  --path /tmp/fastiplayer-aud003-pts-only.ts
```

Сценарий принудительно запускает `ffmpeg-sw`, materialize-ит AVFrame-backed кадры через обычный
resource-release path, проверяет строго возрастающие PTS первых трёх кадров на старте и после
middle seek, current seek generation, landing `pts >= target` и terminal EOF drain.

## AV1 global motion через VA-API

Аппаратная регрессия зависания AMD VCN из-за неверного разбора AV1 global motion
(`decode_subexp` без `+ mk`). Нужен MP4 AV1 Main 8-bit с крупными global-motion
translation и длительностью не меньше 40 s, а также хост с
`vainfo --display drm` → `VAProfileAV1Profile0 : VAEntrypointVLD`. Эталонный фрагмент:
yt-dlp формат 399 ролика `JrT1PjOjOjc`, 900–1000 s (в Git не добавляется):

```bash
url="$(yt-dlp -g -f 399 'https://www.youtube.com/watch?v=JrT1PjOjOjc')"
ffmpeg -v error -ss 900 -i "$url" -t 100 -c copy -y /tmp/fastiplayer-av1-global-motion.mp4

scripts/media-regression.sh \
  --scenario av1-mp4-vaapi-global-motion \
  --path /tmp/fastiplayer-av1-global-motion.mp4
```

Сценарий открывает файл через `symphonia-demux`, декодирует его production decoder thread-ом
`video-vaapi` (NV12 DMA-BUF): первые 20 s с начала, затем seek-flush в середину и всё до EOF
drain. Проверяется, что каждый temporal unit публикует ровно один кадр текущей generation и после
seek нет кадров раньше decode point. PTS каждого кадра обязан совпадать с PTS temporal unit-а,
который его показал (AV1 `show_existing_frame`). Возврат бага global motion вешает VCN, и Mesa
обрывает процесс.

## Ограниченный пул выходных VA surfaces (H.264)

Аппаратная регрессия пула выходных VA surfaces (`video-vaapi::output_surface_pool`,
план `user/cpu-parity/`, сессия 01). Нужен MP4 H.264 8-bit длиной не меньше 10 s и хост с
VA-API H.264 VLD. Подходит синтетическая фикстура из
`docs/benchmarks/tools/t480s/make_fixtures.py` (`synthetic-h264-1080p60.mp4`):

```bash
scripts/media-regression.sh \
  --scenario h264-mp4-vaapi-surface-pool \
  --path benchmark-fixtures/synthetic-h264-1080p60.mp4
```

Сценарий декодирует файл production decoder thread-ом со скользящим окном удержанных кадров,
затем перестаёт отпускать кадры, пока пул не исчерпается, отпускает их, дожимает файл до EOF,
делает seek-flush в середину и декодирует ещё 120 кадров. По счётчикам пула
(`VideoDecodeThread::output_surface_pool_stats`) проверяется, что surfaces переиспользуются
(создано не больше наибольшей ёмкости, а не по одной на кадр), backpressure начинается ровно при
занятой ёмкости, ни одна поверхность не вернулась в пул с неизвестным состоянием драйвера
(`surfaces_destroyed_not_idle == 0`), а каждый отправленный access unit показан ровно одним
кадром (нет потери и повтора).
Возврат к `vaCreateSurfaces` на каждый кадр роняет тест на первой фазе.

## Пропускная способность VA-API декодера (H.264)

Аппаратный замер потолка нашего декодера без показа (план `user/gles2-renderer/`, сессия 09a).
Нужен MP4 H.264 8-bit и хост с VA-API H.264 VLD. Подходят фикстуры
`docs/benchmarks/tools/local/make_matrix.py --preset t410`:

```bash
scripts/media-regression.sh \
  --scenario h264-mp4-vaapi-throughput \
  --path test-assets/benchmark-t410/1080p30/1080p30_h264-high_10mbps_aac.mp4
```

Файл целиком идёт через production decoder thread конвейером (до 8 packet-ов в полёте), кадры
освобождаются сразу. Тест проверяет, что каждый access unit дал ровно один кадр со своим PTS и
пул выходных surfaces ограничен, и печатает строку `vaapi_decode_throughput frames=… fps=…`.
Порог кадров/с в тесте не задан: он зависит от машины. Сравнение — с
`ffmpeg -hwaccel vaapi -hwaccel_output_format vaapi -i FILE -f null -` на том же файле в тот же
день, в release-сборке (`cargo test --release`).

Web-media/app UX is checked separately through the S42 manual runner:

```bash
scripts/progressive-web-smoke.sh \
  --case progressive-http-matroska-webm \
  --url 'https://explicit-user-selected.example/media.webm' \
  --report /tmp/fastiplayer-progressive-web-report.md
```

Every media input must be an explicit `--case` + `--url`/`--fixture` pair to count toward S42.
The backward-compatible bare `--url` mode is still available, but maps to `legacy-url-N` and
cannot complete the matrix. The runner does not search fixtures, infer URLs, choose browser
profiles, or replace `XDG_CONFIG_HOME`, so normal user-owned system yt-dlp configuration
remains available.

Real acceptance requires exact system `yt-dlp 2026.07.04`. Raw runtime logs live only in a
temporary directory; the saved report replaces explicit/derived HTTP(S)/FTP(S) endpoints and
whole secret-bearing lines. A successful launch is reported as `MANUAL REVIEW REQUIRED`, never
as an automatic UX pass. A partial safe-case selection keeps the S42 matrix `NOT RUN`.

See [S42 final acceptance](web-media-s42-final-acceptance.md) for the complete 29-case allowlist,
privacy/provenance contract and manual checklist. [S27 evidence](progressive-web-s27.md) remains
the historical ownership and hardening basis.
