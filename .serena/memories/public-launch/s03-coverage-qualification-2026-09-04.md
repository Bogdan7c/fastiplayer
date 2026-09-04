# S03 — coverage qualification и зелёный private HEAD (2026-09-04)

## Scope и результат

S03 квалифицировала S01/S02 module split и test-stability fallout без изменения публичных API или архитектурных boundaries. Base для file-local audit: `d61a2d87082e4e57de6c550691049bce50e0449a`. Финальный tracked baseline получен только из exact 9/9 intersection трёх независимых cohort-ов; предыдущие неуспешные или снятые со stale coverage object cohort-ы не использованы.

## Найденные причины, а не симптомы

- `playlist-state::resume::worker` disconnect test иногда не выполнял return-to-wait path: добавлен явный stale wake перед disconnect.
- Ogg/Vorbis vertical test ошибочно считал единственно допустимым трёхзапросный schedule. Production допускает bounded 3-request active-fetch и 5-request window-reset path; functional oracle теперь принимает 3..=5 и по-прежнему требует PCM. Active-fetch stale ordinary-error path закреплён отдельным deterministic owner-level test в `media-prefetch`.
- `source-core::abortable_http_task` и app playlist import polling имели schedule-dependent loop-body coordinates; polling сохранён через bounded `repeat_with/find_map|find` с deadline и явным error propagation.
- HLS live lifecycle test читал availability сразу после prepare, хотя coordinator намеренно стартует `without_dvr`; availability packet-proven. Consumer теперь проводит реальные packets до initial availability observation.
- `DemuxSeekCancellationToken::wait_cancelled()` мог быть вызван после того, как test parent уже отменил token, поэтому Condvar body случайно не исполнялся. Owner-local private observer implementation и zero-capacity rendezvous test гарантируют порядок pending predicate → cancel status → Condvar wait → notify; public `wait_cancelled()` API/semantics прежние.
- Первая попытка observer-а через `cfg(test)` field была отвергнута: она создавала разные CodeRegion maps production function в media-core unit и dependency binaries, и strict extractor fail-closed остановил cohort. Финальная форма одинаково компилируется во всех crate copies.
- Один repeat check обнаружил stale coverage object `playlist_runtime/import_io.rs`: qualification HTML содержал прежний `while`, свежий clean build — актуальный deadline-code. Все такие cohort-ы отозваны; финальные v3 artifacts дополнительно проверены по HTML/coordinate universe.

## Финальная qualification

Cohort hashes:

1. `sha256:435ae105a59985a757f9c225fd0008fbfd68fe49498fa978e550f468ab1180b3`
2. `sha256:0443fbe772dbc357f6bc75b67a1fd5070be5d5432b550b1591d10c04cb6e4124`
3. `sha256:54928c449752f63b64a3c0d3ca0fc9dedf7ce50bb5c836d5c3193950fb6b5f9b`

Logical baseline hash: `sha256:ed3320f87c1c353f89aeeb181fa2cbf20f057dbe83e95032e5080119d700773a`; raw baseline SHA-256: `4f50fbc2b48e5e4858d8a377f41af8d54d1d14b4184130c95e1bc32c8e30b2da`; source-files: 992, hash `sha256:175a8a56855dd50d5f07bde052d5ffe6dfb0e429fc381f7fd40bbcb3bcac699f`.

Exact workspace: functions 15,800/20,033; lines 164,686/212,402; regions 206,815/270,182. Exact blocking group: functions 9,917/11,760; lines 100,352/116,605; regions 125,944/149,522.

File-local audit: 76 changed Rust files. Ledger raw SHA-256: `6ad36935e47b817ad99b7bb770795e1970487081fe4df0f6b9d10f6e1c73b7c9`; ровно пять reviewed cross-universe rows, все review_by 2026-12-04: blocking functions, adaptive functions, DASH functions, Smooth functions, Smooth regions. Scheduler/HLS/condvar fixes не получили exceptions.

Fresh repeat checks:

- cohort `sha256:b7b956951e8f8b3b2b8f4007f5d90d81240af677ad4a9665821aeaa9c4dc0aa5`
- cohort `sha256:121c309ed45d3996dd8d5e3d6b9c0d2de49462da2baa981507704b7465d121fb`
- оба: check hash `sha256:7785ad71cb726dee9d314ed212385c589b09a52d442c6bcd43a071dacc99cc2e`, `regressions=[]`, `universe_changes=[]`.

## Gates

Финальный `scripts/pre-pr-checks.sh` зелёный: guardrails, rustfmt, dependency policy, workspace tests/doc-tests, strict Clippy, strict rustdoc, app no-default-features и MSRV 1.92. Explicit `cargo +1.96.0 build --workspace --all-features --release --locked` зелёный. Coverage commands и GitHub workflow topology не менялись; см. `mem:testing/coverage` и `mem:task_completion`.