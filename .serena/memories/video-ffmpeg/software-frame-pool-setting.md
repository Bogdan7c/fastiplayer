# Software frame pool setting + 4K software playback FPS fix (Session 26, 2026-06-19)

## Проблема
Software (FFmpeg) 4K60 playback (AV1 SDR) не держал стабильные 59-60fps: FPS
проседал до 45 при СВОБОДНЫХ CPU/GPU. Декод НЕ узкое место (AV1 SDR ~126fps
декода, present queue полна, `drops_decoder_starvation=0`).

## Диагноз (по `fastiplayer::render_frame_timing` логам)
Узкое место — host→GPU upload software-кадра (`video_prepare` /
`video_texture_lookup` в `app-egui/frame_prepare.rs`), на главном render-потоке
каждый кадр. На медленных кадрах upload 15-32ms при бюджете vsync 16.7ms. CPU/GPU
«свободны», потому что это memory-bandwidth + wgpu staging stall, не compute.
Ключ: первые ~3с чисто (~3ms), потом устойчивый «разогнанный» режим 9-15ms
пачками — накопление состояния. AV1 декодит ~2x realtime и за пару секунд
забивает 24-слотовый host resource table кадрами по ~12MB (~290MB резидентно) →
давление на общую память iGPU раздувает каждый upload.

A/B доказал причину: pool 24→8 срезал «разогнанные» кадры на ~70%, p90 upload
почти вдвое.

## Решение (вариант 1, согласовано с пользователем)
Отдельная «живая» настройка `video.sw_decoder_surface_pool_frames` (default 8),
независимая от hardware `video.decoder_surface_pool_frames` (24). Hardware VA
surface — дешёвый GPU descriptor; software-кадр — полный host RAM буфер, поэтому
лимиты РАЗДЕЛЕНЫ.

Проводка (зеркало `decoder_surface_pool_frames`):
- `video-core::VideoDecoderThreadConfig.software_frame_pool_frames` (нейтральное
  поле, default const `DEFAULT_SOFTWARE_FRAME_POOL_FRAMES=8`, + normalized().max(1)).
  Документировано: применяется только software-путём; hardware его игнорирует.
- `video-ffmpeg::FfmpegVideoDecoderThread::spawn` использует
  `software_frame_pool_frames` и для frame channel, и для `FfmpegHostResourceProvider`
  resource table (раньше оба брались из `decoder_surface_pool_frames`, см. Session 24).
- `video-vaapi`: НЕ затронут; vaapi→neutral `From` заполняет поле neutral default
  (у VA-API нет host-frame pool). Round-trip adapter тест выровнен на default.
- `player-core`: `decoder_thread_config_from_app_config` маппит
  `config.video.sw_decoder_surface_pool_frames -> software_frame_pool_frames`;
  новый `PlayerRuntimeSettingId::VideoSoftwareDecoderSurfacePoolFrames`.
- `config/schema.rs`: поле + `#[setting(apply="video.apply")]`, default 8, default
  TOML comment, registry list, registry range test; `validation.rs` range (1..=64).
- `fastiplayer-settings`: добавлено в `player_decoder_thread_setting` +
  `player_runtime_setting_id` → идёт через ту же controlled-rebuild группу
  `PlayerDecoderThreadConfig`, что делает настройку ЖИВОЙ (app-egui
  `rebuild_video_pipeline_with_decoder_config` пересоздаёт backend на лету).
- Focused tests: player-core `decoder_thread_config_maps_software_surface_pool_independently`,
  fastiplayer-settings `software_surface_pool_route_updates_decoder_thread_config`,
  video-core normalized() assertion.

Миграция НЕ нужна: `VideoConfig` = `#[serde(default, deny_unknown_fields)]`, поэтому
отсутствие ключа в старом TOML грузится как default 8. Schema version не менялась.

## Замеры (release, software, 4K60 SDR, ~13с окна, trace render_frame_timing)
video_prepare p90 / >9ms / missed-vsync(>17ms):
- AV1: pool24 8.17/6.7%/1.5% → pool8 5.07/3.4%/1.5% → pool6 5.03/1.9%/0.9%
- VP9 p0 (декод быстрый): pool8 4.46/0.7%/0.5% → pool6 3.88/0.1%/0.2%
- HEVC SDR 8bit (декод ~1.08x realtime, decode-bound): pool8 7.42/4.9%/3.0% →
  pool6 8.27/8.2%/4.9% (ХУЖЕ на 6!)

ВЫВОД: **8 — sweet spot**. 6 чуть лучше для upload-bound кодеков (AV1/VP9), но
РЕГРЕССирует decode-bound HEVC (декодеру нужен буфер впереди playback). Поэтому
default=8. HEVC SDR 8bit 4K60 software остаётся частично decode-bound (это
известный лимит ~1.08x realtime из `user/ffmpeg_cpu_decode_benchmark_2026-06-18.md`),
pool tuning это не лечит — нужен только запас, а не урезание.

Настройка отображается в Settings UI (registry-driven: `surface="main-settings-window"`, секция «video», группа «decode», editor integer 1..=64), `description_ru` содержит подсказку про компромисс (меньше=плавнее AV1/VP9, больше=запас для decode-bound HEVC, default 8, применяется на лету).

Настройка живая: можно крутить во время плейбека (например 6 для AV1/VP9, 8 для
HEVC) без переоткрытия файла.

CPU RGB conversion / hwdecode НЕ добавлялись; upload-ahead (ОТКЛОНЁН, см.
`mem:video-ffmpeg/software-upload-ahead-REJECTED`) НЕ трогали; raw FFmpeg типы
остались в video-ffmpeg; не хардкод под файл.
