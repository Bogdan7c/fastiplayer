# AUD-004: release хвоста decoded-frame batch при fatal contract mismatch (2026-08-23)

## Подтверждённая корневая причина

- `session::tick::video_decoder_io::drain_decoded_video_frames` сначала извлекает bounded receive-pass decoder-а в `Vec<DecodedFrame>`.
- До исправления ownership-loop был `for frame in decoded_frames`; на contract mismatch он явно освобождал только `frame.resource_handle` и немедленно возвращал fatal error.
- Оставшийся `Vec::IntoIter` дропал `DecodedFrame`, но `DecodedFrame` не является RAII lease и не имеет decoder-specific `Drop`; opaque `FrameResourceHandle` поэтому не возвращался provider-у.
- Context7 `/rust-lang/reference` подтверждает общий drop-scope раннего return, но external Rust semantics не создаёт отсутствующий project-specific release callback.

## Стабильный ownership invariant и фикс

- Владельцем manual cleanup всех кадров, уже извлечённых из decoder channel, остаётся `drain_decoded_video_frames`.
- Receive-batch превращается в именованный `remaining_decoded_frames` iterator. При fatal contract mismatch текущий handle и весь оставшийся iterator явно проходят через существующую `release_video_texture` boundary.
- Префикс, уже переданный presentation queue, остаётся во владении queue и освобождается её обычными lifecycle paths; новый cleanup касается только не обработанного хвоста.
- Error kind/message, `drained_frame_count`, public/internal API, drop accounting и nonfatal stale/paused/overflow paths не менялись.
- RAII для `DecodedFrame` намеренно не вводился: это изменило бы ownership всех decoder/render paths и смешало бы внешний provider release с нейтральным frame value.

## Regression anchors

Файл: `crates/player-core/src/session/tick/tests.rs`.

- `first_contract_mismatch_releases_every_frame_extracted_in_the_batch_once`
- `second_contract_mismatch_releases_every_frame_extracted_in_the_batch_once`

Fake decoder одним receive-pass принимает handles `81`, `82`, `83`; mismatch расположен на первом или втором frame. После drain presentation queues очищаются и decoder handle заменяется. Sorted release-log обязан точно совпасть с accepted set, что одновременно доказывает отсутствие пропусков и double release.

Pre-fix evidence: первый mismatch давал `[81]`, второй `[81, 82]`. Post-fix оба дают `[81, 82, 83]`.

## Проверка

- `cargo test -p player-core contract_mismatch -- --nocapture` — 3/3 PASS.
- `cargo test -p player-core` — 643/643 PASS.
- `cargo clippy -p player-core --all-targets -- -D warnings` — PASS.
- `cargo fmt --all --check` — PASS.
- `scripts/check-refactor-guardrails.py` — PASS.
- `git diff --check` — PASS.

Исходный аудит обновлён: `user/project_health_audit_2026-08-22.md`, AUD-004 закрыт 2026-08-23.