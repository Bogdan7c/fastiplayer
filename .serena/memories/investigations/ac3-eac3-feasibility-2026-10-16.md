# AC-3 / E-AC-3: разведка (2026-10-16, код не менялся)

> ИТОГ 2026-10-17: решение владельца — `oxideav-ac3` НЕ используем (сносится вместе с `crates/oxideav-ac3-patch`): в нём найдены неверные значения таблицы 44.1 кГц, 55k строк непроверенного кода. Пишем свой clean-room декодер `crates/ac3-decoder` строго по ATSC A/52:2012 / ETSI TS 102 366 (код oxideav/FFmpeg/liba52 не читать; ffmpeg — только бинарник-эталон). См. `mem:ac3-decoder/core`. Сведения ниже о крейте — историческая справка.

## Состояние кода
- MKV/MP4 уже демуксятся: patch-крейты Symphonia mkv/isomp4 (`dac3`/`dec3`) → `symphonia-demux/src/track_mapper.rs` отдаёт `A_AC3`/`A_EAC3`; `audio/src/decoder/conversion.rs` мапит их в `CODEC_ID_AC3/EAC3`, но в Symphonia 0.6 декодера нет → `UnsupportedCodec` → `PlayerErrorKind::UnsupportedAudioCodec` (recoverable, тост «Unsupported audio format»).
- Не сделано: `mpeg-ts-demux/src/psi.rs` (~185) молча отбрасывает stream type 0x81/0x87 и DVB 0x06+дескрипторы 0x6A/0x7A; нет `AudioDecodeCodecFamily` для AC-3 (`audio-core/src/decode_capability.rs`, u16 bitset), веб-кандидаты (`web-media-core/normalized.rs`, `web-media-playback-plan/candidate.rs`, `media-source-open/.../catalog_capabilities.rs`, `service-ytdlp/src/candidate/planning.rs`) AC-3 отбрасывают; фикстур AC-3 в `test-assets/audio/` нет.
- FFmpeg разрешён guardrail-ом только в `video-ffmpeg` (`scripts/check-refactor-guardrails.py` FFMPEG_FORBIDDEN_DEPENDENCIES).

## Решение владельца
- Делаем свою (pure Rust) реализацию, «если нет готового крейта». FFmpeg-путь не выбран.

## Найденный кандидат: `oxideav-ac3` 0.0.11 (crates.io, MIT, Karpelès Lab)
- AC-3 + E-AC-3 (AHT, ECPL, SPX, dependent substreams, опционально JOC), спецификация ATSC A/52:2018; FFmpeg только как black-box эталон в тестах (не порт LGPL-кода). Зависимости: `oxideav-core` 0.1 (serde_json, thiserror, bytemuck) — лицензии проходят `deny.toml`. 1 `unsafe`. Есть fuzz-харнессы.
- API: `oxideav_ac3::decoder::{make_decoder, make_eac3_decoder}(&CodecParameters)` → `Box<dyn oxideav_core::Decoder>` (send_packet/receive_frame строго по очереди). Выход — только interleaved **S16** в `AudioFrame.data[0]` (внутри f32 с clamp), число каналов во фрейме не сообщается (вычислять из длины или `bsi::parse`). Порядок каналов — WAVE (FL,FR,FC,LFE,BL/SL,BR/SR) = canonical порядок `AudioChannelLayout`.
- Собственная проверка стендом (scratchpad, против `ffmpeg` decode s16le): синтетика 5.1 AC-3/E-AC-3 — 92 дБ PSNR на всех каналах (потолок s16), порядок каналов верный; музыка: расхождение растёт при низком битрейте (96k ≈ 58–60 дБ, 640k ≈ 92 дБ), некоррелировано с сигналом; выключение dither в копии крейта улучшает на ~1.8–3.2 дБ (теоретически 3 дБ) → разница в основном от независимого bap=0 dither (§7.3.4), не баг. Скорость 90–500x realtime. Порча каждого 3-го пакета (bit-flip/truncate/garbage): 0 паник, ошибки на пакет.
- Не проверено: E-AC-3 7.1 с dependent substream (ffmpeg не умеет кодировать), 44.1/32 кГц, реальные фильмы/вещательные потоки.
- Риски: версия 0.0.x и быстро меняющийся `oxideav-core` 0.1.x (40 версий) → пинить точную версию; S16-выход (потеря headroom/точности) — кандидат на локальный patch-крейт или upstream.
