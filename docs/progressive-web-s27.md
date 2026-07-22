# S27 — Progressive/web UX hardening gate

## Verdict boundary

S27 не добавляет новый playback path. Gate доказывает уже реализованную цепочку
S19 → S21C → S22 → S23–S26 и запрещает вернуть старый service-owned WebM opener.

Production ownership остаётся прежним:

- `service-ytdlp` владеет extraction, candidate normalization и neutral request mapping;
- `app-egui::web_media_open` — единственный composition owner concrete transport + demux;
- `PlaylistRuntime` и media-open coordinator владеют queue revision, authorization barrier и
  exact Installed publication;
- `web-media-http`/`source-core` владеют HTTP session, Range/non-Range, redirects и ephemeral
  cookie scope;
- URL sidebar получает только secret-safe read-only projection;
- config/playlist state сохраняют acknowledged exact locator, но не transport target, headers,
  cookies или request material.

## Hermetic evidence matrix

| S27 requirement | Canonical focused evidence |
| --- | --- |
| Candidate normalization/profile exclusion | `service-ytdlp::candidate::tests::{canonical_s00_target_rows_are_normalized_without_silent_drops, unknown_and_profile_excluded_candidates_remain_visible, excluded_request_data_and_impersonation_remain_visible_rejections}` |
| Audio capabilities | `audio-core::decode_capability::tests::*`, `audio::decoder::capability::tests::*`, `web-media-playback-plan::{absent_transport_demux_video_and_audio_are_exact_typed_rejections, all_four_layout_shapes_are_playable_without_runtime_construction}` |
| Config v7 preferred height | `config::{schema_v6_without_preferred_height_migrates_to_best_playable, preferred_video_height_roundtrips_and_rejects_invalid_bounds, preferred_height_settings_accessor_preserves_best_playable_and_validated_choices}`, `app-egui::{preferred_video_height_apply_persists_global_only_and_reopens_settings, only_global_preferred_height_requests_yt_dlp_reselection}` |
| HTTP Range/non-Range | `web-media-http::{non_range_reuses_probe_response_without_duplicate_request, range_source_uses_existing_prefetch_path, cancellation_and_stale_refresh_reject_before_network_mutation}` plus `tests/progressive_containers.rs` MP4/M4A/WebM and separate A/V cases |
| Queue open/barrier | `app-egui::media_open::coordinator::tests::{ready_passes_through_without_auto_authorization_then_enqueue_wins, downstream_authorization_rejection_is_pre_enqueue_resolution, authorization_ack_without_installed_terminal_is_fatal}` and playlist controller exact-Installed suites |
| URL sidebar | `app-egui::web_media_stream_model::tests::*`, including local/direct/audio-only, one/many, group part, stale generation and secret safety |
| Switch Playing/Paused/CUE/group | `barrier_captures_fresh_playing_and_paused_controls`, `playback_window_identity_wraps_reopen_request_without_source_specific_types`, `same_lineage_rebind_preserves_exact_compound_part_current`, `detached_windowed_source_reopens_without_queue_row_or_cue_identity` |
| System auth propagation | `service-ytdlp::{transport_maps_authorized_material_with_origin_path_and_secure_scope, refresh_reextraction_replaces_serialized_authorization_state, conflicting_cookie_serializations_are_typed_incompatible}`, `web-media-http` cookie/redirect isolation suite, `source-core::http_cookie::tests::*` |
| No legacy WebM opener | `public_surface_and_manifest_have_no_legacy_webm_opener` plus S27 dependency/source guardrails and removal of old `media-regression.sh` yt-dlp scenarios |
| Restore/settings/shutdown | app settings compensation tests, playlist suspend/resume tests, startup shutdown token tests and media-open lifecycle cancellation tests |
| Exact locator vs transient secrets | `generic_url_preserves_exact_identity_without_query_normalization`, locator formatting/acknowledgement tests, S27 durable/presentation source guardrail and progressive runner redaction self-test |
| Cancellation/stale | S19 semantic rematch, S22 pre-network stale refresh, coordinator stale/cancel winner, URL sidebar generation fencing and startup/url-import shutdown suites |

Canonical focused commands:

```bash
cargo +1.96.0 test --locked \
  -p audio-core -p audio -p rustiplayer-config \
  -p web-media-core -p web-media-playback-plan \
  -p web-media-transport-api -p web-media-http \
  -p source-core -p service-ytdlp

cargo +1.96.0 test -p player-core --locked
cargo +1.96.0 test -p app-egui --locked

python3 -m unittest discover -s scripts/tests -p 'test_*.py'
scripts/tests/playback-smoke-self-test.sh
scripts/tests/progressive-web-smoke-self-test.sh
scripts/check-refactor-guardrails.py
```

Repository-wide completion остаётся за обычными командами:

```bash
scripts/ci-checks.sh tests
scripts/ci-checks.sh format-guardrails
```

## Manual runner

Runner принимает только URL, которые пользователь явно передал через `--url`. URL можно
повторить для public/authenticated и Range/non-Range cases:

```bash
scripts/progressive-web-smoke.sh \
  --url 'https://first-explicit-user-url.example/media' \
  --url 'https://second-explicit-user-url.example/watch' \
  --duration 120 \
  --report /tmp/rustiplayer-progressive-web-s27.md
```

Без `--binary` runner сначала собирает release `app-egui`. С уже собранным binary можно
передать `--binary target/release/rustiplayer`. `--dry-run` проверяет selection, но не создаёт
report и не считается acceptance.

Runner намеренно не меняет `XDG_CONFIG_HOME`, не выбирает browser/cookie profile и не добавляет
auth options: system/user yt-dlp config продолжает загружаться по обычному upstream contract.
Официальная документация yt-dlp подтверждает, что config options эквивалентны CLI options, а
`--ignore-config` отключает дальнейшую загрузку; Rustiplayer manual runner этот флаг не добавляет:
[yt-dlp configuration](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#configuration).

Raw stdout/stderr каждого запуска сначала попадает в process-owned temporary directory. Перед
записью report runner:

- заменяет exact explicit URL;
- заменяет любые другие HTTP(S) endpoints из runtime log;
- заменяет целиком строки с Cookie, Authorization или Set-Cookie;
- удаляет raw temporary directory при выходе;
- никогда не объявляет UX `PASS`: итог остаётся `MANUAL REVIEW REQUIRED` до заполнения checklist.

## Что человек должен проверить

Для выбранного corpus нужны отдельные явные URL там, где различаются transport/auth свойства.
Во время запуска:

1. Убедиться, что active URL sidebar показывает safe source, варианты и active/pending state без
   второго URL input.
2. Переключить candidate во время Playing и Paused; при ошибке до barrier старое playback должно
   продолжиться.
3. Для CUE/group part проверить сохранение текущего Item, group scope и window-relative position.
4. Изменить global preferred height в Settings и проверить Installed + restore, затем shutdown и
   повторный startup/restore.
5. Для URL, защищённого system yt-dlp cookies/config, проверить успешный open без app credential UI.
6. Запустить supersede/cancel либо быстро сменить candidate и убедиться, что stale completion не
   публикуется как active.
7. Заполнить checklist только в уже redacted report; raw URL, cookie/header values и extractor
   payload туда не добавлять.

## Known limitations

- Runner не может сам доказать, что произвольный удалённый server действительно Range или
  non-Range; это свойство выбранного пользователем corpus.
- Он не автоматизирует UI clicks и не подделывает CUE/group topology. Эти проверки остаются
  ручными, а hermetic lifecycle contracts закреплены focused tests.
- Trusted system yt-dlp config/plugins могут иметь собственные side effects. Gate доказывает
  только отсутствие дополнительных download/write/exec/postprocessor/mark-watched options со
  стороны Rustiplayer и отсутствие raw secrets в сохранённом report.
- При принудительном `SIGKILL` всей shell process group cleanup trap невозможно гарантировать;
  штатный exit, handled failure и timeout удаляют raw temporary logs.
