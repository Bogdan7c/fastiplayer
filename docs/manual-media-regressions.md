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
