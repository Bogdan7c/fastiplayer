# Dependency report — 2026-07-10

## Результат

Первый автоматизированный аудит выполнен `cargo-deny 0.20.2` и
`cargo-machete 0.9.2` для Linux all-features graph.

- licenses: pass после удаления неразрешённого пути
  `directories 6 → dirs-sys → option-ext 0.2.0 (MPL-2.0)`; config paths теперь
  определяет permissive `etcetera 0.11.0`;
- sources: pass; graph содержит crates.io, workspace/path crates и ровно четыре
  local `[replace]` patches, Git dependencies отсутствуют;
- unused direct dependencies: pass после удаления четырёх доказанно неиспользуемых
  entries (`app-egui/video-backend-api`, `video-ffmpeg/frame-server-core`,
  `video-vaapi/frame-server-core`, `vp9-parser/tracing`);
- `video-ffmpeg/pkg-config` — документированный cargo-machete false positive:
  dependency используется в `build.rs::verify_pkg_config_inputs`;
- advisories: **blocked** двумя `quick-xml 0.39.3` vulnerabilities, описанными ниже.

## Blocking security finding

`RUSTSEC-2026-0194` и `RUSTSEC-2026-0195` требуют `quick-xml >=0.41.0`.
Точный dependency path:

```text
app-egui / render-wgpu-shell / video-vaapi
  → winit / egui-winit / gbm
  → wayland-* 0.31.x
  → wayland-scanner 0.31.10
  → quick-xml 0.39.3
```

`wayland-scanner 0.31.10` — текущий доступный release и ограничивает
`quick-xml` несовместимым range. Policy ignore не добавлен, major dependency
upgrade и local patch не импровизированы; поэтому новый Dependency policy CI job
намеренно остаётся красным до отдельного решения Wayland risk domain.

В рамках сессии устранены два других blockers без major upgrade:
`anyhow 1.0.102 → 1.0.103` (`RUSTSEC-2026-0190`) и
`memmap2 0.9.10 → 0.9.11` (`RUSTSEC-2026-0186`).

## Non-blocking tracked findings

- Audio/Opus domain: `RUSTSEC-2026-0150`, unmaintained
  `audiopus_sys 0.2.2` через `audio → opus 0.3.1`. Owner: audio subsystem.
  Removal criterion: перейти на maintained Opus binding без регрессии decode/output.
- UI/font domain: `RUSTSEC-2026-0192`, unmaintained
  `ttf-parser 0.25.1` через egui font stack. Owner: UI/render shell.
  Removal criterion: upstream egui/font stack переходит на maintained parser либо
  отдельный совместимый non-major update устраняет dependency.
- Yanked crates: на дату отчёта не обнаружены. Любое новое finding остаётся видимым
  в non-blocking advisory report и требует отдельного follow-up.

## Duplicate versions

Прямого duplicate debt после удаления unused dependencies не найдено. Текущие
duplicates образованы независимыми transitive constraints:

- Wayland/UI: `calloop 0.13/0.14`, `calloop-wayland-source 0.3/0.4`,
  `smithay-client-toolkit 0.19/0.20`, `nix 0.28/0.29`;
- native build tooling: `bindgen 0.70/0.71/0.72`, `rustix 0.38/1.x`,
  `linux-raw-sys 0.4/0.6/0.12`;
- collection/runtime internals: `hashbrown 0.15/0.16/0.17`,
  `getrandom 0.2/0.3`, `rustc-hash 1/2`, `foldhash 0.1/0.2`;
- dev/config tooling: `toml 0.9/1.x` family и `winnow 0.7/1.x`;
- upstream patch branches: `thiserror 1/2` и связанные proc-macro versions.

Они остаются warnings. Wayland, bindgen и rustix branches не унифицируются без
измеримой пользы и совместимого upstream path.

## Upgrade backlog по risk domain

1. Wayland/security: получить upstream release с `quick-xml >=0.41` либо принять
   отдельное архитектурное решение о безопасном source patch; затем убрать blocker.
2. Audio/Opus: заменить unmaintained `audiopus_sys` path и прогнать audio/media matrix.
3. UI/fonts: отслеживать миграцию с unmaintained `ttf-parser`.
4. Native build toolchain: оценивать bindgen/rustix duplicates только вместе с
   upstream refresh соответствующего backend, не с Session 05.
5. Critical local patches: аудит и обновление только в Session 06.
