# S42 app-egui production boundary splits — executor wave 5 (2026-08-27)

## Поведение и API

- Behavior-neutral relocation без новых public или crate-wide API.
- `frame_prepare/timing.rs` владеет pure timing/budget/slow-frame diagnostics; frame parent сохраняет materialization, typed texture outcomes, cache/lease lifecycle и renderer submit ownership.
- `web_media_open/runtime.rs` владеет concrete registries/capability snapshots и candidate/demux open; web parent сохраняет yt-dlp preparation, exact identity/generation/cancellation, component composition и pre-publication/strong-install boundary.

## Инварианты

- `mark_submitted_to_renderer()` остаётся только после реального Presented submit; Busy/Missing/Unsupported/Error texture outcomes не слиты.
- Cancellation остаётся до preparation, между дорогими physical opens и перед stream/catalog publication.
- Timeline/refresh ports, exact component/catalog identity и source lineage проходят прежними typed values; runtime не получил queue/current/Installed authority.

## Размеры и проверки

- Production line counts parent/child: frame `1426/352`; web `686/538`. Новые private child-модули меньше hard limit 800; baseline не менялся.
- Focused PASS: frame_prepare 33/33; web_media_open 46/46.
- Full app PASS: no-default 1002/1002; all-features 1002/1002.
- Strict app Clippy PASS для no-default и all-features/all-targets с `-D warnings`; app no-default check, full rustfmt, diff check, refactor guardrails, S41 cross-provider integration 3/3, S42 final acceptance 24/24 и Serena diagnostics PASS.
- P1 review correction: historical S41 artifact не менялся; static resolver добавил canonical physical mapping только для exact `(path, symbol)` пары `web_media_open.rs` + `fn open_candidate`. Exact formerly failing S41 assertion 1/1, full S41 3/3, S42 24/24 и повторный app web focused 46/46 PASS.
- Global `scripts/check_s42_guardrails.py` остаётся red только из-за repository-wide stale/legacy module-size baseline inventory, включая ожидаемые уменьшения обоих parent paths. Baseline намеренно не менялся.
