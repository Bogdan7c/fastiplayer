# AUD-006 — development-only проверка system yt-dlp (2026-08-23)

## Решение владельца

- Production integration намеренно не изменена: system/user config, cookies, format policy и Python plugins продолжают наследоваться обычным запуском `yt-dlp`.
- Runtime version allowlist, version gate и production compatibility preflight не добавлены. Номер версии — diagnostic provenance, а compatibility profile `2026.07.04` остаётся checked-in upstream corpus, не запретом более новой версии.
- Совместимость новой системной версии проверяется вручную development runner-ом `scripts/ytdlp-compatibility.sh`.

## Проверяемая граница

- Runner находит exact `yt-dlp` через `PATH`, получает `--version` только для отчёта и создаёт временный PATH shim.
- Shim добавляет `--ignore-config --no-plugin-dirs` только development check-у, затем exec-ит найденный system executable с неизменёнными production arguments. Это изолирует upstream binary compatibility от user config/plugin failures; production argv не меняется.
- Ignored integration test `crates/service-ytdlp/tests/system_ytdlp_compatibility.rs` поднимает bounded loopback HTTP HTML+MP4-prefix fixture без внешней сети.
- Тест вызывает публичные production APIs `resolve_yt_dlp_candidate_snapshot_with_config` и `extract_yt_dlp_topology_with_config`. PASS требует хотя бы один accepted playback candidate и `YtDlpTopology::Video`, то есть настоящий executable output прошёл process, JSON parser и normalization boundaries.
- Test server имеет bounded request-header budget, read timeout и explicit join/shutdown; default workspace suite не зависит от наличия system `yt-dlp`, потому что тест `#[ignore]`.

## Автоматические проверки runner-а

- `scripts/tests/ytdlp-compatibility-self-test.sh` hermetic fake-ами закрепляет help/unknown argument, arbitrary future version без allowlist, exact Cargo orchestration, failed Cargo result без ложного PASS и failed `yt-dlp --version`.
- Self-test и bash syntax новых scripts включены в `scripts/ci-checks.sh format-guardrails`; реальный system executable автоматически в CI не запускается.

## Проверка закрывающей сессии

- `scripts/ytdlp-compatibility.sh`: PASS для `/usr/bin/yt-dlp 2026.08.19`; candidate + topology 1/1.
- `cargo +1.96.0 test -p service-ytdlp --locked`: PASS, 140 unit + все integration/profile suites; system test ожидаемо ignored.
- `cargo +1.96.0 clippy -p service-ytdlp --all-targets --locked -- -D warnings`: PASS.
- `scripts/tests/ytdlp-compatibility-self-test.sh`, bash syntax, rustfmt и `git diff --check`: PASS.
- `scripts/ci-checks.sh format-guardrails` дошёл до нового self-test (PASS), затем остановился на unrelated pre-existing global S42 module-size baseline drift в 40 production modules; новые files среди нарушений отсутствуют.

## Документация

- Contract описан в `README.md`, `docs/runtime-acceptance-manifest.md` и `crates/service-ytdlp/compatibility/2026.07.04/REPORT.md`.
- AUD-006 закрыт выбранным development-control решением в `user/project_health_audit_2026-08-22.md`; там явно сохранено ограничение: это не production circuit breaker и не typed pre-extraction runtime diagnostic.

Related: `mem:core`, `mem:media-services/ytdlp-process-owner-2026-08-05`, `mem:testing/media-fixtures`.