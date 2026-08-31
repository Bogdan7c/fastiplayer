# App sidebar controller

- `AppState` owns `SidebarController` for section/animation lifecycle and `SidebarHostState` as the single owner of live sidebar geometry. `SettingsRuntime` owns only settings draft/apply/preview transactions and debounced persistence of host-originated width changes.
- Stable order: `Playlist`, `Settings`, `Url`, `Info`. Order defines content-slide direction.
- Open/close uses `animation-core::SlideTransition` + `EaseInOutCubic` and configured `ui.animations.sidebar_slide_duration_ms`. Content transition duration is exactly `D/2`; zero duration completes immediately.
- During content transition only the latest queued section is kept. Both outgoing/incoming panels move one shared sidebar width inside one clipped, resizable host and have distinct stable content IDs; transition copies are interaction-disabled.
- `ui/sidebar.rs::show` содержит ровно один site создания `egui::Panel::left(app_sidebar)`. Секции и animation phases — только content renderers внутри host и архитектурно не могут создавать Panel/width/resize state.
- `SidebarHostState` хранит единственную live width для всех секций. Default/range принадлежат config: `420`, `350..=600` pt. Перед `Panel::show` host удаляет remembered `egui::PanelState`, чтобы egui не становился вторым владельцем. Fully-open width берётся только из захваченного до content render `ui.max_rect()` самого Panel; `response.rect` запрещён, потому что translated animation children меняют его. После content render parent UI явно расширяется обратно до panel rect, чтобы egui cursor/PanelState тоже не сжимались до content minimum. Animation/content widths никогда не коммитятся.
- `SidebarOutput` возвращает rect, live width и typed `SidebarWidthChange`; внешний код не читает поля host напрямую. Текущий rect в том же кадре сдвигает video viewport.
- `SettingsRuntime` держит latest-only pending resize и коммитит `ui.sidebar.width_points` после 500 ms тишины через settings-core runtime-setting intent API. Deadline участвует в event-loop wake; ручной SetValue/Apply/OK сначала force-flush pending. Cancel соседних draft-настроек уже committed width не откатывает.
- Persistence failure сохраняет committed config/draft, явно возвращает live host к committed width и публикует Settings status/log. Pending resize force-flush-ится перед suspend и штатным shutdown.
- Content child получает фиксированный host rect, min-width 0 и clip только во время open/close/content animation. В fully-open состоянии секция рендерится прямо в Panel UI: fixed `UiBuilder::max_rect` здесь запрещён, потому что он превращает текущую ширину в content minimum и блокирует resize handle.
- Info values используют wrapped layout; длинная metadata строка не может менять desired width панели. Regression test требует `animated_content_rect(..., fully_open=true) == None`.
- Titlebar controls are 36x32 pt, inset 8 pt, gap 4 pt, ordered Playlist/Settings/URL/Info. The complete reserved rect is excluded from drag/resize. Tooltips/accessibility labels are Russian.
- `ui/sidebar/header.rs` — подмодуль geometry owner-а sidebar. Он резервирует один точный 32-point header rect для Playlist/Settings/URL/Info; heading, X и Playlist Undo вертикально центрированы, а separator/content начинаются с одинакового offset во всех секциях.
- `WindowChromeEdgeAlignment::sidebar_section_button_rect(container_rect, SidebarSection)` возвращает typed rect общей левой grid axis. Sidebar запрашивает `SidebarSection::Url` для X-центра 32x32 Playlist Undo без numeric index/hardcode; container rect может быть translated open/content-transition header, поэтому relative axis движется вместе с visual copy.
- Painter primitives, including active background, live in `ui-artwork-egui`; app-egui owns hit-testing/actions only.
- Hiding/switching away from Settings preserves its draft/live preview. Re-entering an existing draft refreshes dynamic options without `begin_edit`. Settings X/Cancel rolls back; successful OK closes through visibility reconciliation; Apply stays open.
- Session 18: Playlist content routes to `ui/playlist/` inside the same single host. Authoritative copy owns persistent viewport anchor/output; disabled outgoing/incoming animation copies render with temporary Playlist UI state and discarded output, so they cannot overwrite anchor or duplicate visible metadata demand. См. `mem:app-egui/playlist-ui-s18`.

## S24 URL sidebar stream model (2026-07-22)

- `web_media_stream_model.rs` владеет secret-safe read-only projection активной web-media конфигурации; `ui/url_sidebar.rs` только рисует её внутри существующего `SidebarSection::Url`. Второго `Panel`, URL input, browser/profile controls и queue actions нет.
- Safe inventory строится в единственном S19→S21C→S23 preparation path-е из accepted candidates минус полный S21C rejection set. UI получает только layout, resolution/fps/bitrate, container/codec enums; raw URL, headers/cookies, candidate/semantic IDs и extractor payload отсутствуют.
- `ActiveMediaSource::YtDlpUrl` несёт exact selection отдельно от boxed `WebMediaStreamConfiguration`. Конфигурация публикуется только вместе с exact Installed source и сохраняет preference через suspend/exact reopen; settings BestPlayable reselection заново берёт current global config.
- `UrlSidebarController` не дублирует active source: active model строится из `AppState::active_media_source`. Ephemeral pending/error fenced exact source+extraction generation; runtime item override различает exact `PlaylistItemId` и source lineage. S25 владеет фактической установкой pending/override и controlled same-item switch.
- Local source даёт inactive URL model; direct-media показывает service-owned safe label и VOD/seek/buffering state без fake format choices; YtDlp показывает active/pending, global-vs-item preference, group-part scope, VOD/seek/buffering/refresh-on-reopen и bounded safe failure category.
- Focused tests: local/direct/audio-only/one-many/group part/current+stale generation/item override fencing/secret safety и отсутствие второго Panel. Полный `cargo test -p app-egui`: 817 PASS.

## S36C2/C3 component-variant sidebar and actions (2026-07-24)

- The existing single URL sidebar has stable independent `Видео` and `Аудио` sections sourced from app-owned shape-typed component presentation. UI receives only resolution/fps/bitrate/sample-rate/channel count and known codec/dynamic-range enums; variant keys, exact/semantic identities, raw codec strings/language and locator material remain outside the projection.
- `Unavailable`, `VideoAndAudio`, `VideoOnly` and `AudioOnly` render honestly without a Cartesian inventory or second Panel. Only non-active Installed rows are enabled; the active row is inert, unavailable/missing axes stay disabled, and one common pending state disables both candidate and component rows.
- UI publishes only safe `UrlSidebarAction::SelectComponentVariant { parent_generation, catalog_generation, component, variant_index }`. Model-local validation resolves it to a semantic-only reopen request; exact parent/component identities remain inside the model/open boundaries.
- Candidate and component actions share one latest strong-open transaction slot. Candidate selection keeps provider-default component choice and updates item preferred-height override; component selection rematches a fresh catalog, preserves the other axis and does not mutate global/item height preference. Same-frame candidate click has deterministic priority.

## S27 evidence note (2026-07-22)
- URL sidebar remains a secret-safe projection with no second URL ingress; guardrails reject transient transport/auth types in sidebar/config/playlist persistence owners.
- Full evidence and explicit-URL manual workflow: `mem:media-services/progressive-web-hardening-s27-2026-07-22`.

## N04/N05A current URL sidebar source/catalog boundary (2026-08-31)

- Старые S24 имена `ActiveMediaSource::YtDlpUrl`, `UrlSidebarModel::YtDlp` и отдельная native/direct source projection — исторические. N04 свёл active web source к одному neutral variant-у, а N05A заменил provider-named sidebar model на `CatalogBacked` и общий read-only `WebMediaSourceReadProjection`.
- Sidebar projection получает только ingress kind, safe label, presentation и optional safe stream configuration; locator, request material, raw exact/semantic identities и catalog attachment в read-only projection отсутствуют.
- Extractor публикует neutral candidate/separate targets; direct/native HLS публикуют один видимый inert `InstalledOnly/Automatic` row без fake parent generation и без action. Catalog generation stale fence сохраняется.
- Временная reverse route table с extractor selection tokens живёт отдельно в `web_media_stream_model/catalog_routes.rs` и должна быть удалена N05B. Полный handoff: `mem:app-egui/native-web-ingress-n05a-2026-08-31`.
