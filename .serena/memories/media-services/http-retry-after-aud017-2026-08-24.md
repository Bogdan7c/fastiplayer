# AUD-017 — bounded HTTP Retry-After (2026-08-24)

## Итог

- Независимая read-only production-boundary сессия подтвердила дефект: public `AdaptiveHttpContext::fetch_resource_blocking` через `fetch_with_redirects` и `HttpSourceSession::fetch_bounded_single_hop` повторял `429 Retry-After: 2` через локальные ~5.6 ms; стандартный HTTP-date также игнорировался, malformed безопасно падал на тот же local backoff.
- После исправления тот же временный loopback harness с local backoff 5 ms измерил: delta-seconds 2 → 2000.792 ms; HTTP-date → 2667.244 ms в server deadline; malformed → 5.876 ms fallback.
- AUD-017 отмечен закрытым 2026-08-24 в `user/project_health_audit_2026-08-22.md`.

## Ownership и boundary

- `source-core` остаётся единственным владельцем raw reqwest response headers. Новый secret-safe `HttpRetryAfter` хранит только `Unavailable` либо проверенную relative `Duration`; raw server header не входит в Debug/errors.
- `crates/source-core/src/http_retry_after.rs` разбирает delta-seconds и sender-mandatory стандартный IMF-fixdate относительно момента получения response. Past date становится zero delay, malformed/непредставимое значение — `Unavailable`.
- `SourceError::HttpStatus` теперь несёт поле `retry_after: HttpRetryAfter`. Bounded full/range hop заполняет его из headers до возврата ошибки. Существующие progressive/range-open paths явно ставят `Unavailable`, поэтому их retry/error semantics не изменились.
- `web-media-adaptive::AdaptiveRetryPolicy` независимо владеет local exponential `maximum_backoff` и caller-owned `maximum_retry_after`. `retry_delay_after` выбирает `max(local, capped_server)`; server cap не может быть zero или больше neutral 60-second readiness bound.
- Blocking fetch, manifest scheduler и segment scheduler используют один `retry_delay_after`. Blocking wait остаётся cancellation-aware; async owners хранят bounded `retry_not_before` deadline. Attempt budgets, redirect/secret policy, expiry observation и generation fences не изменились.
- `app-egui::web_media_adaptive_config::maximum_adaptive_retry_after` является единым app-owned production cap (60 s) для HLS, DASH, Smooth и HDS composition roots. Test/provider owners задают собственные малые caps.
- Direct dependency edge `source-core -> time.workspace` использует уже зафиксированный `time 0.3.53` с parsing feature; Cargo.lock изменился только добавлением `time` в dependency list source-core.

## Regression evidence

- source deterministic tests: exact delta-seconds, IMF-fixdate, past date, malformed header.
- policy deterministic tests: server hint сильнее 5 ms local backoff при local cap 20 ms; отдельный server cap 2 s; unavailable fallback; zero и >60 s cap errors.
- functional blocking regression `tests::retry_after::blocking_fetch_waits_for_retry_after_before_second_request`: реальный loopback `429 Retry-After: 1 -> 200`, body доходит через public bounded fetch после server delay.
- functional segment regression `cancellation_during_retry_backoff_prevents_follow_up_request`: `503 Retry-After: 1` публикует длинный retry deadline, cancellation завершает lifecycle, request count остаётся 1.

## Проверки

- `cargo +1.96.0 test -p source-core --locked`: 60/60 PASS (loopback suite запускалась вне sandbox из-за запрета bind).
- `cargo +1.96.0 test -p web-media-adaptive --locked`: 40/40 PASS.
- `cargo +1.96.0 check --workspace --all-targets --locked`: PASS.
- `cargo +1.92.0 check --workspace --locked`: PASS.
- `cargo +1.96.0 clippy -p source-core -p web-media-adaptive -p app-egui --all-targets --locked -- -D warnings`: PASS.
- strict no-deps rustdoc для source-core/web-media-adaptive, `cargo +1.96.0 fmt --all --check`, `scripts/check-refactor-guardrails.py`, `git diff --check`: PASS.

Связанные memories: `mem:core`, `mem:media-services/core`, `mem:media-services/adaptive-transport-s31-2026-07-23`, `mem:media-services/web-transport-s21t-2026-07-21`.
