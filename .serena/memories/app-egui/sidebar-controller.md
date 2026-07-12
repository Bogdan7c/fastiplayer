# App sidebar controller

- `AppState` owns `SidebarController`; `SettingsRuntime` owns only settings draft/apply/preview transaction.
- Stable order: `Playlist`, `Settings`, `Url`, `Info`. Order defines content-slide direction.
- Open/close uses `animation-core::SlideTransition` + `EaseInOutCubic` and configured `ui.animations.sidebar_slide_duration_ms`. Content transition duration is exactly `D/2`; zero duration completes immediately.
- During content transition only the latest queued section is kept. Both outgoing/incoming panels move one shared sidebar width inside one clipped, resizable host and have distinct stable content IDs; transition copies are interaction-disabled.
- `ui/sidebar.rs::show` содержит ровно один site создания `egui::Panel::left(app_sidebar)`. Секции и animation phases — только content renderers внутри host и архитектурно не могут создавать Panel/width/resize state.
- Host владеет одним temp width snapshot и единым диапазоном 320–560 pt. Regression test считает sites `Panel::left` и требует ровно один.
- Content child получает фиксированный host rect, min-width 0 и clip. Info values используют wrapped layout; длинная metadata строка не может менять desired width панели.
- Titlebar controls are 36x32 pt, inset 8 pt, gap 4 pt, ordered Playlist/Settings/URL/Info. The complete reserved rect is excluded from drag/resize. Tooltips/accessibility labels are Russian.
- Painter primitives, including active background, live in `ui-artwork-egui`; app-egui owns hit-testing/actions only.
- Hiding/switching away from Settings preserves its draft/live preview. Re-entering an existing draft refreshes dynamic options without `begin_edit`. Settings X/Cancel rolls back; successful OK closes through visibility reconciliation; Apply stays open.