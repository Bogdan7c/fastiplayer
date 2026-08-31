# N01 provider-neutral web ingress contracts (2026-08-31)

## Ownership и dependency boundary

- `web-media-core` остаётся std-only и теперь владеет чистым semantic plane для будущего native web ingress. Новые публичные contracts находятся в `ingress.rs`, `web_selection.rs`, `recovery.rs` и `extractor.rs`; normal/dev dependencies не добавлены.
- Core по-прежнему не хранит physical locator, signed endpoint, headers/cookies/keys, HTTP/process/UI/runtime handles и не зависит от app, service-ytdlp, transport client, decoder или renderer.
- Existing `ExactSelectionIdentity`, `SemanticIdentity`, `ComponentVariantCatalog`, `ComponentVariantSelection` и `ComponentVariantSemanticSelectionRequest` переиспользованы. Второй catalog либо второй identity vocabulary не создан.

## Новые semantic contracts

- `WebMediaIngressKind::{DirectResource, NativeManifest, ExtractorBacked}` фиксирует фактический архитектурный ingress без provider ID; `WebMediaPresentationKind::{Vod, Live}` сохраняет exact lifecycle kind.
- `WebMediaSelection` связывает exact parent candidate с optional-by-shape existing component selection. `with_components` отвергает cross-parent shape. `semantic_rematch_request` удаляет snapshot-local generations, а `WebMediaSemanticSelectionRequest::rematch` требует свежий exact parent и shape-named `WebMediaSelectionRematchSource`; parent disappearance, catalog-parent mismatch, shape mismatch и existing `ComponentVariantError` остаются разными typed outcomes. Outer Debug показывает только redacted parent и shape.
- Для внутренней проверки связи selection с existing catalog `ComponentVariantSelection::catalog_identity` расширен только до `pub(crate)`; публичная variant boundary не раскрыта.
- `WebMediaRecoveryStrategy` описывает только intent: stable-resource reopen, root-manifest refresh + semantic rematch, fresh extraction + semantic rematch либо terminal unreconstructible endpoint. `continuity()` доказывает сохранение исходного ingress и не позволяет трактовать direct recovery как extractor promotion.
- `ExtractorInvocationReason` различает page media, collection/topology, extractor-owned authorization material, native-profile compatibility fallback и extractor-backed recovery без свободной строки.
- `WebMediaFallbackGate` — non-Clone/non-Copy одноразовый pre-`Installed` owner. Разрешены только provider document, extractor-owned authorization material и unsupported native profile; cancellation/network/malformed/expired/backpressure/invariant/decoder/renderer остаются отдельными rejection variants. После `Installed` fallback всегда запрещён.

## Focused evidence

- Functional tests покрывают construction/fresh-generation semantic roundtrip, exact VOD/live distinction, cross-parent и fresh-shape rejection, исчезновение component semantic identity без fallback, одноразовую phase legality, forbidden-trigger non-consumption, secret-safe Debug и recovery continuity.
- PASS: `cargo test -p web-media-core --locked` (61 unit + 3 integration), strict focused Clippy, `cargo fmt --all -- --check`, `git diff --check`, `cargo check --workspace --all-targets --all-features --locked`, Serena diagnostics.
- Decoder/render/nonzero PCM не запускались: N01 намеренно является pure value-contract session без I/O/runtime composition. Production integration начинается в последующих N03–N05 sessions.

Related: `mem:core`, `mem:media-services/core`, `mem:media-services/web-playback-planner-s21c-2026-07-21`, `mem:media-services/web-transport-s21t-2026-07-21`.