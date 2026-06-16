# 01. Цель и Scope

## Цель

`rustiplayer` - Linux-first видеоплеер с native hardware video decode,
optional FFmpeg software decode, explicit decoder->renderer frame contracts и
честной диагностикой неподдержанных потоков.

Главная инженерная цель: не показывать "почти работает" там, где pipeline
нарушает контракт. Если поток нельзя воспроизвести через поддержанный hardware
или software path, он должен получить typed reject с понятной причиной.

## Поддерживается сейчас

- Локальные WebM/Matroska файлы.
- YouTube/VOD WebM через временный `yt-dlp` adapter.
- VP9 Profile 0 SDR 8-bit 4:2:0 через NV12.
- VP9 Profile 2 HDR 10-bit 4:2:0 через P010 и BT.2446-C HDR-to-SDR.
- FFmpeg software decode через HostPlanar YUV + WGPU host upload, если runtime
  probe и capability intersection успешны.
- Opus audio decode в software path.
- WGPU renderer с Vulkan-first профилем.
- egui shell, timeline/scrub, worker-owned playback runtime.
- Linux desktop integration через отдельный crate boundary, с fallback stub для неподдержанных платформ.

## Не поддерживается сейчас

- CPU RGB conversion/readback decoded video для production playback.
- FFmpeg hardware decode/hwaccel path.
- Native HDR output в swapchain/display.
- MP4/MOV/fMP4/HLS/DASH как production containers.
- AV1, H.264, H.265, VP8 как production video paths.
- VP9 12-bit, VP9 4:2:2/4:4:4, VP9 Profile 1/3.
- Durable history/bookmarks/cache metadata.
- DRM/protected content.

## Платформы

Текущий практический target - Linux. Wayland является primary desktop target,
X11 остаётся fallback. Windows/DX12 и macOS/Metal являются будущими направлениями,
но текущий workspace не содержит production backend-ов для них.

## Продуктовые правила

- Ошибки decode/render/capability не скрываются silent fallback-ом.
- UI работает через команды и snapshots, а не через прямую мутацию pipeline.
- Streaming/service код не должен знать про renderer.
- Renderer не должен знать про YouTube, demuxer или player state.
- Документация фиксирует текущие контракты, а не планы закрытых фаз.
