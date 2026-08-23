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
  -f mpegts -y /tmp/rustiplayer-aud003-pts-only.ts

scripts/media-regression.sh \
  --scenario h264-ts-pts-only-ffmpeg \
  --path /tmp/rustiplayer-aud003-pts-only.ts
```

Сценарий принудительно запускает `ffmpeg-sw`, materialize-ит AVFrame-backed кадры через обычный
resource-release path, проверяет строго возрастающие PTS первых трёх кадров на старте и после
middle seek, current seek generation, landing `pts >= target` и terminal EOF drain.

Web-media/app UX is checked separately through the S42 manual runner:

```bash
scripts/progressive-web-smoke.sh \
  --case progressive-http-matroska-webm \
  --url 'https://explicit-user-selected.example/media.webm' \
  --report /tmp/rustiplayer-progressive-web-report.md
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
