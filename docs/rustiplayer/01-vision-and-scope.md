# 01. Vision and Scope

## Цель проекта

`rustiplayer` - нативный аппаратно-ускоренный видеоплеер на Rust.

Главная цель - построить быстрый, контролируемый и расширяемый multimedia pipeline без браузерного движка, Electron и FFmpeg. Проект ориентирован на Linux-пользователей, которым нужен легкий плеер с современным UI, сильной диагностикой и честной поддержкой возможностей системы.

Важная стратегическая цель - не только получить плеер, но и отладить Rust-first multimedia foundation, который позже можно использовать в более сложных проектах.

## Продуктовая позиция

`rustiplayer` не пытается быть универсальным software-плеером "любой файл любой ценой". Он воспроизводит только то видео, которое может быть декодировано аппаратно на текущей системе.

Если система не поддерживает нужный codec/profile/bit depth через hardware backend, плеер должен:

- не пытаться декодировать видео софтом;
- ясно показать причину отказа;
- показать список поддерживаемых codec/profile/container/stream вариантов;
- использовать эту информацию при автоматическом выборе качества и формата.

Audio decode может быть software. Это нормальный tradeoff: аудио не является основной нагрузкой, а аппаратные audio decode paths не дают такой же практической ценности, как аппаратный decode видео.

## Целевой пользовательский сценарий

1. Пользователь открывает локальный файл или YouTube URL.
2. Плеер определяет источник, контейнер, треки и доступные варианты качества.
3. Capability layer опрашивает систему и строит матрицу поддерживаемых video decode formats.
4. Stream selection выбирает лучший поток, который реально может быть воспроизведен.
5. Player core запускает demux, audio pipeline, video hardware decode и A/V sync
   внутри `PlayerWorker`.
6. UI/render shell получает decoded frame через render lease, создаёт texture views
   на render thread и отображает воспроизведение, диагностику, настройки и ошибки.
7. После реализации desktop integration плеер публикует MPRIS-состояние для KDE/media widgets.

## Поддерживаемые направления

### Video codecs

Порядок развития после полной реализации VP9:

1. VP9
2. AV1
3. H.264
4. H.265
5. VP8

Для каждого codec нужно учитывать:

- codec id;
- profile;
- level/tier, если применимо;
- bit depth;
- chroma subsampling;
- HDR transfer/color primaries;
- максимальное разрешение;
- максимальный FPS;
- backend-specific ограничения драйвера.

### Audio codecs

Audio decode допускается software. Архитектура должна позволять расширять список кодеков без переписывания player core.

Первичные аудио-направления:

- Opus;
- AAC;
- Vorbis;
- FLAC;
- PCM;
- другие форматы по мере необходимости.

### Containers and streaming formats

Целевые контейнеры и streaming форматы:

- WebM;
- Matroska;
- MP4 / ISO BMFF;
- MOV;
- fragmented MP4;
- MPEG-TS;
- HLS;
- DASH.

FFmpeg не используется ни для decode, ни для demux/probe. Контейнеры реализуются через Rust crate'ы или собственные parser/demuxer modules.

### HDR and color

Архитектура должна покрывать:

- SDR 8-bit;
- HDR input;
- HDR-to-SDR tone mapping для SDR-мониторов;
- будущий настоящий HDR output при поддержке OS/compositor/monitor;
- BT.601, BT.709, BT.2020;
- full/limited range;
- 8/10/12-bit;
- PQ и HLG.

Phase 8.5 уже закрыл SDR-prep refactor: VP9/NV12 SDR результат сохранён, а неявное предположение `NV12 + BT.709 limited SDR` заменено на явный renderer color pipeline contract. Этот contract включает typed metadata, active color path diagnostics, SDR adjustment settings и future hooks для tone mapping.

Phase 9 закрыл VP9 completion как первый реальный producer `P010 + HDR metadata + zero-copy` контракта. Phase 10 поверх этого добавил отдельный P010/HDR BT.2446-C renderer; HDR renderer не чинит codec probing/decode и не смешивается с NV12 SDR shader path.

Для SDR output текущий `wgpu` path сохраняет поведение `Unorm` swapchain как default. Это фиксируется как `PreserveCurrentUnorm`, чтобы будущий переход на `SrgbRenderTarget` или explicit shader OETF был осознанным решением, а не побочным эффектом порядка выбора surface format.

Текущая практическая HDR-цель после Phase 10: смотреть VP9/P010 HDR-видео на SDR-мониторе через корректный shader tone mapping. Native HDR output остаётся отдельной будущей задачей.

### YouTube and services

Будущий YouTube-клиент должен поддерживать:

- публичные видео;
- account/session/cookies;
- captions;
- adaptive quality selection;
- network cache;
- resume download;
- live streams;
- историю, закладки, прогресс просмотра.

`yt-dlp` является временной MVP-зависимостью, сейчас изолированной в `service-youtube`, и должен быть заменен своим Rust service/extractor layer. Default YouTube selector остаётся SDR VP9/Opus WebM; HDR/VP9.2 YouTube проверки требуют explicit selector override до capability-aware service candidates.

## Non-goals на ближайшие этапы

- Software video decode fallback.
- FFmpeg integration.
- Полноценный DRM implementation.
- Dynamic plugin ABI.
- Windows legacy DirectX ниже DX12.
- Полный GLES renderer parity с Vulkan renderer.

## Future architecture hooks

Эти возможности не реализуются сейчас, но архитектура не должна их блокировать:

- DRM abstraction;
- несколько online services;
- live streaming;
- subtitles/captions renderer;
- SQLite library/history/cache;
- MPRIS desktop control;
- platform-specific hardware decode backends;
- OpenGL ES 2.0 legacy renderer.
