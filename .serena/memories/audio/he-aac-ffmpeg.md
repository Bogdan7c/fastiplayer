# HE-AAC (SBR, Parametric Stereo) через AAC-декодер FFmpeg (16b1 моста, 2026-11-18)

## Решения владельца (`user/browser-bridge/decisions.md` 347–351)
- 347: HE-AAC декодирует встроенный `aac` закреплённого LGPL FFmpeg 8.1.3 (не свой clean-room: нормативный текст SBR/PS только в платном ISO/IEC 14496-3; бесплатные 3GPP 26.40x — кодер, 26.410/411 — исходник чужого декодера). AAC-LC остаётся на патче Symphonia.
- 348: обёртка — отдельный crate `audio-ffmpeg`; guardrail «FFmpeg-биндинги только в video-ffmpeg» расширен до `FFMPEG_BINDING_OWNER_CRATES = {video-ffmpeg, audio-ffmpeg}`; от `audio-ffmpeg` зависит только `audio` (`AUDIO_FFMPEG_ALLOWED_DEPENDENTS`).
- 349: неявный HE-AAC (ADTS в MPEG-TS, паспорт «LC» или нет) узнаётся по первому пакету: патч Symphonia на `ID_FIL` с EXT_SBR_DATA(_CRC) возвращает `Unsupported(symphonia_codec_aac::IN_BAND_SBR_UNSUPPORTED)`; маршрут отдаёт тот же пакет FFmpeg. xHE-AAC (USAC, тип 42) FFmpeg 8.1 умеет, но маршрут туда НЕ ведёт — решение по паспорту, не по отказу Symphonia.
- 351: фикстуры — `fdkaac` (Fraunhofer) со своими сигналами; иерархические паспорта = снятые с сайтов (`2b920800`, `2b098800`, `eb8a0800`).

## Слои
- `ffmpeg-vendor::build_options::ENABLED_DECODERS` = h264,hevc,vp8,vp9,libdav1d,**aac** (тест закрепляет литерал и запрет `libfdk_aac`). swresample не собран: выход FLTP перекладывается в Rust.
- `crates/audio-ffmpeg` (features `ffmpeg`, `vendored-ffmpeg` = `ffmpeg-sys-next/fastiplayer-vendored`; прямой зависимости на ffmpeg-vendor нет — `.so` в тестах находится через `rustc-link-search` → LD_LIBRARY_PATH cargo). Публично: `FfmpegAacDecoder::{is_available, open(&AacStreamConfig), decode_packet(&[u8]) -> Result<Option<DecodedAacPcm>>, flush}`, `AacStreamConfig::{AudioSpecificConfig(Vec<u8>), BareFrames{core_sample_rate, core_channels}}`, `DecodedAacPcm{output_spec: AudioOutputSpec, interleaved_samples}`, `AudioFfmpegError` (FeatureDisabled, DecoderNotFound, InvalidStreamConfig, AllocationFailed, CorruptPacket, Ffmpeg{operation,code,message}, UnsupportedSampleFormat, UnsupportedChannelLayout, MixedFramesInPacket). Без фичи тип есть, `open` → FeatureDisabled.
- `src/ffi/` — единственное место `unsafe`/`AV*` (тест `tests/raw_ffi_boundary.rs`). `AacCodecContext` владеет контекстом+пакетом+кадром, `unsafe impl Send` обоснован (`thread_count = 1`). Пакет снимается на любом исходе отправки. Порядок каналов и маска читаются числами (Rust-enum из C с неизвестным значением = UB); NATIVE маска → позиции audio-core (`channel_mapping.rs`), UNSPEC — только 1–2 канала, CUSTOM/AMBISONIC — отказ. Пустой пакет в FFmpeg не отправляется (был бы drain).
- Ошибки пути декода: FFmpeg 8.1 на мусоре возвращает внутренний код без текста, не INVALIDDATA → всё, кроме ENOMEM/EINVAL, = `CorruptPacket` (`packet_error_from_averror`). При открытии — строгая классификация.
- `crates/audio/src/decoder/aac_route{.rs,/stream_kind.rs,/he_aac.rs,/tests.rs}`: `create_audio_decoder` для codec id AAC → `aac_route::create_aac_decoder`. `classify_audio_specific_config` → `HighEfficiency{signalling: Hierarchical|BackwardCompatible, parametric_stereo}` / `LowComplexity` / `Other`; PCE (channel config 0) → LowComplexity без разбора хвоста. `AacRoutedDecoder` = `ActiveAacDecoder::{LowComplexity(SymphoniaAudioDecoder), HighEfficiency(HeAacDecoder)}`; `SymphoniaAudioDecoder::decode` переводит `IN_BAND_SBR_UNSUPPORTED` в typed `InBandSbrDetected`, маршрут downcast-ит и переключается один раз; выбор переживает `reset`. `HeAacDecoder`: битый пакет → warn + пустой буфер; формат публикуется вместе с буфером; до первого PCM `sample_rate()` = 0.
- Фичи: `audio/ffmpeg`, `audio/vendored-ffmpeg`; `app-egui` `ffmpeg`/`vendored-ffmpeg` включают их вместе с видео. `pushed-media-source` dev-dep `audio` с `vendored-ffmpeg`.

## Инварианты (закреплены тестами)
- Первый PCM HE-AAC уже на выходной частоте (44,1/48 кГц), вывод создаётся один раз (нет пересборки из-за 22,05 → 44,1). FFmpeg выдаёт моно HE-AAC как 2 одинаковых канала.
- AAC-LC через маршрут бит в бит = прямой Symphonia (`plain_low_complexity_pcm_is_bit_identical_to_direct_symphonia`).
- Без FFmpeg: явный HE-AAC → ошибка создания с `AudioFfmpegError::FeatureDisabled` в цепочке; неявный → ошибка decode с тем же + «implicitly signalled HE-AAC» (глухого ядра нет).
- Priming AAC не обрезается (общее ограничение: trim не идёт через демуксер) — до выхода доходят все кодированные кадры (47 × 2048 на 2 с фикстуры).

## Тесты
- `cargo test -p audio-ffmpeg [--features vendored-ffmpeg]` — `tests/he_aac_decode.rs` (SBR: энергия выше половины частоты ядра двумя каскадами ФВЧ — SBR даёт полосы ~344 Гц, не точный тон; PS: 440 Гц слева/1 кГц справа, разведение > 30 дБ; FAAD2 даёт ~50), фикстуры `tests/fixtures/he-aac/` (`generate.sh`, `rewrite_asc_hierarchical.py`).
- `cargo test -p audio [--features vendored-ffmpeg] --lib aac_route`.
- Патч: `cargo test --manifest-path crates/symphonia-codec-aac-patch/Cargo.toml --locked` (`in_band_sbr_is_reported_instead_of_a_muffled_core`, фикстура 4 кадров).
- Сквозные: `app-egui` `local_file_open::tests::he_aac_playback` (MP4 v1, MP4 v2, MPEG-TS неявный; `CapturedAudio.output_specs`), `pushed-media-source --test he_aac_page_audio_to_output` (fMP4 v2 кусками по 1000 байт).
- В CI идут через `--all-features` (job «Workspace tests (all features)»).

## Известное
- Журнал FFmpeg (`av_log`) никто не перехватывает: «Error decoding AAC frame header» на битых пакетах идёт в stderr (как у видео).
- Capability snapshot по семейству AAC не различает HE-AAC: в сборке без FFmpeg планировщики web считают `mp4a.40.5` играбельным, отказ будет при открытии.
- Экстеншен звуковые кодеки не трогает; живая разведка: ответ «HE-AAC не умею» сайты (Instagram, TikTok) не меняет.
