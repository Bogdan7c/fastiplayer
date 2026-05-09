# rustiplayer architecture docs

Этот каталог описывает целевую архитектуру проекта `rustiplayer`.

Документы фиксируют не только ближайший рефакторинг MVP, но и направление для полноценного аппаратно-ускоренного плеера. Исторические планы по VA-API остаются в `docs/superpowers/`, а этот каталог считается новой навигационной точкой для дальнейшей разработки.

## Содержание

- [01. Vision and Scope](01-vision-and-scope.md) - продуктовая цель, ограничения и функциональный scope.
- [02. Target Architecture](02-target-architecture.md) - слои системы и поток данных.
- [03. Project Map](03-project-map.md) - целевая карта crate'ов и ответственность каждого модуля.
- [04. Codecs and Capabilities](04-codecs-and-capabilities.md) - аппаратные декодеры, профили, HDR, матрица возможностей.
- [05. Config and Storage](05-config-and-storage.md) - TOML-настройки и SQLite-хранилище.
- [06. Rendering, UI and Platform](06-rendering-ui-platform.md) - wgpu/Vulkan, GLES fallback, egui, MPRIS, мультиплатформа.
- [07. Services and Network](07-services-network.md) - YouTube-клиент, будущие сервисы, cache, streaming.
- [08. Development Roadmap](08-development-roadmap.md) - поэтапный план разработки в порядке приоритета.
- [09. Phase 8.5 SDR Color Pipeline Prep](09-phase-8-5-sdr-color-pipeline-prep.md) - подготовка SDR color pipeline перед HDR.

## Ключевые решения

| Область | Решение |
| --- | --- |
| Название проекта | `rustiplayer` |
| Основная платформа | Linux-first |
| Оконная система | Wayland primary, X11 fallback |
| Видео decode | Только аппаратное ускорение, software fallback для видео отсутствует |
| Audio decode | Software decode допустим |
| Linux video backend | VA-API primary, с поддержкой i965 и iHD |
| Bitstream probing | Только через проверенные parser'ы/адаптеры, без новых ad-hoc bit parser'ов в `player-core` |
| Renderer primary | `wgpu`/Vulkan |
| Renderer legacy | Отдельный будущий OpenGL ES 2.0 crate для SDR 8-bit NV12 |
| Color pipeline | Phase 8.5 вводит явный SDR color pipeline contract перед HDR |
| Swapchain transfer | По умолчанию сохраняем текущий `Unorm` path через `PreserveCurrentUnorm`; `SrgbRenderTarget` и explicit shader OETF остаются future modes |
| Color metadata | Используем layered metadata с origin/confidence: manifest/container/bitstream/decoder/fallback |
| SDR adjustments | В contract закладываются brightness/contrast/saturation/exposure и RGB gain/offset с identity defaults |
| BT.2020 SDR | Сейчас показываем как fallback в SDR BT.709 diagnostics, позже добавляем настоящий gamut mapping |
| Windows | Second target, через DX12 |
| macOS | Later target |
| FFmpeg | Полностью вне проекта |
| Config | TOML через `serde` |
| Storage | SQLite через `rusqlite`, кроме пользовательских настроек |
| Services | Модульные crate'ы, компилируются в один бинарь |
| YouTube | Будущий полноценный клиент с account/session/cookies |
| DRM | Дальняя архитектурная возможность, не текущий scope |
