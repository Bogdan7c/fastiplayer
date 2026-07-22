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

Progressive yt-dlp/app UX is checked separately through the S27 runner:

```bash
scripts/progressive-web-smoke.sh \
  --url 'https://explicit-user-selected.example/media' \
  --report /tmp/rustiplayer-progressive-web-report.md
```

Every media input must be an explicit repeated `--url` argument. The runner does not search
fixtures, infer URLs, choose browser profiles, or replace `XDG_CONFIG_HOME`, so normal
user-owned system yt-dlp configuration remains available. Raw runtime logs live only in a
temporary directory; the saved report replaces explicit/derived HTTP(S) endpoints and whole
Cookie/Authorization/Set-Cookie lines. A successful launch is reported as
`MANUAL REVIEW REQUIRED`, never as an automatic UX pass.

See [S27 evidence and checklist](progressive-web-s27.md) for the hermetic matrix and the manual
actions that still require a person.
