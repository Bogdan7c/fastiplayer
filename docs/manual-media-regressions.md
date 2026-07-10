# Manual media regressions

`cargo test --workspace --locked` is intentionally hermetic: it never reads local media files.
Real-media codec, seek, and HTTP transport regressions are run only through
`scripts/media-regression.sh --scenario <name> --path <file>`.

Run `scripts/media-regression.sh --list-scenarios` to see the required properties of the
selected file. The runner neither searches `test-assets/` nor assumes any filename. It reports
the selected path, detected container, public track codecs, scenario, and a `PASSED`, `FAILED`,
or `NOT RUN: missing selection` outcome.
