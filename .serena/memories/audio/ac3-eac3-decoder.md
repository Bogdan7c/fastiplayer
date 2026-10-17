# AC-3 / E-AC-3 adapter в `audio` (D5, 2026-10-17; заменил пакет A)

## Решение владельца
- Сторонний декодер (crate `oxideav-ac3` + локальный патч `crates/oxideav-ac3-patch`, workspace-пины `oxideav-ac3`/`oxideav-core`) УДАЛЁН полностью 2026-10-17 («55k строк непроверенного кода», неверная таблица 44,1 кГц). Вместо него — собственный clean-room `crates/ac3-decoder` (`mem:ac3-decoder/core`). История выбора: `mem:investigations/ac3-eac3-feasibility-2026-10-16` (устарело по решению).

## Слои и владельцы
- `ac3-decoder`: `Ac3Decoder::{new(DecoderConfig), decode_access_unit(&[u8]) -> Result<DecodedAudio, Ac3DecodeError>, reset()}`; планарный f32 без обрезки, typed `ChannelPosition` каждого канала, AC-3/E-AC-3 по `bsid` каждого access unit-а; любая ошибка = разрыв потока внутри декодера.
- Guardrail: `AC3_DECODER_ALLOWED_DEPENDENTS = {"audio"}` (`scripts/check-refactor-guardrails.py`, тест `test_only_concrete_audio_adapter_may_depend_on_ac3_decoder`) — остальные crate-ы видят только `audio-core`; dev-deps не ограничены.
- `audio-core`: `AudioDecodeCodecFamily::{Ac3 = 10, Eac3 = 11}` (без изменений).
- `crates/audio/src/decoder/ac3.rs` — crate-private `Ac3FamilyDecoder` + публичный typed `audio::Ac3DecodeError::{CorruptPacket{codec_id, source: ac3_decoder::Ac3DecodeError}, UnsupportedStream{codec_id, feature}, UnconvertibleChannelLayout{decoder_channels}, InvalidChannelDescription{reason}}`; `production_decoder_config()` = CRC Verify + dither `Pseudorandom(DitherSeed::DEFAULT)` + DRC `Standard` (полный dynrng = line-out, как FFmpeg default) + TPNP `Apply` (Annex E §3.7); dialnorm/downmix не применяются.
- `ac3/canonical_layout.rs` — `NeutralLayout::{for_decoder_channels, layout, interleave}` + кэш `LayoutConverter::conversion_for` (пересчёт только при смене раскладки). `ac3/frame_format.rs` — `LastFrameFormat` (формат+длина последнего удачного кадра). `ac3/persistent_failure.rs` — `PersistentFailureNotice` (о чём уже сообщили).
- `create_audio_decoder` маршрутизирует exact `A_AC3`/`A_EAC3` (после SWF, до Symphonia; lookalike-строки не подменяются). Capability `Ac3FamilyAvailability::Available`, `required_symphonia_codecs = &[]`.

## Инварианты
- Позиции: Ls/Rs → Side (5.1 = `5.1(side)`, canonical FL,FR,FC,LFE,SL,SR), S/Cs → RearCenter, Lrs/Rrs → Rear, Lc/Rc → Front*OfCenter, Lw/Rw → Front*Wide, Ts → TopCenter, Vhl/Vhr/Vhc → TopFront*, Lts/Rts → TopSide*, LFE2 → LowFrequencyEffects2. Lsd/Rsd: Side, если свободна, иначе Rear (mixer сводит side и rear одинаково −3 дБ); фиксированные позиции раскладываются первыми (порядок каналов кадра не важен); заняты обе пары → неконвертируемо.
- Масштаб: значения декодера копируются бит-в-бит (1,0 = полная шкала, > 0 dBFS сохраняется).
- Каждый кадр публикуется в своём настоящем формате (частота/раскладка), смену обрабатывает player-core (`mem:player-core/audio-spec-change-rebuild`); `reset` формат не сбрасывает; ошибка формат не меняет; смена пишется `info!`.
- Политика ошибок (три случая не сливаются): (1) битый packet (любая ошибка декодера, кроме `Unsupported`: CRC, обрезка, синтаксис, состав AU) → `warn` + тишина длиной/форматом последнего удачного кадра, до первого кадра — пусто; (2) постоянная причина (`Ac3DecodeError::Unsupported(feature)` или неконвертируемая раскладка) → ОДНА ошибка `decode` (player-core ставит одну runtime error), дальше та же тишина без ошибок, пока причина не изменится; удачный кадр снимает состояние; reset — нет; (3) нарушение контракта (число каналов) → ошибка `decode`.
- Какие ошибки декодер считает `Unsupported`, решает `ac3-decoder` (после D4 `decode_access_unit` практически их не выдаёт; ветка остаётся для будущих возможностей).

## Тесты (`crates/audio/src/decoder/ac3/tests/`)
- `playback`: factory exact routing; stereo/5.1 AC-3 и 5.1 E-AC-3 → тон в каждой lane + RMS против ffmpeg (0,5 %) → production `ChannelMixer` 5.1→stereo; AC-3→E-AC-3 на одной дорожке одним decoder-ом.
- `differential`: бит-в-бит adapter = прямой `Ac3Decoder` для mono/2.0/2.1/3.0+LFE/2.2/3.1+LFE/5.1 (перестановки вручную по Т. 5.8; фикстуры `ac3-decoder/tests/fixtures/`) и E-AC-3 5.1; кадр 2 `5p1-48k-640k-annexd` против выдержки ffmpeg: LFE PSNR ≥ 120 дБ, остальные lanes SNR ≥ 40 дБ + проверка, что сдвиг lanes ломает совпадение.
- `error_policy`: битый/обрезанный/1 бит (CRC) packet → тишина, до первого кадра пусто и spec нет, чужой track, постоянная причина один раз → тишина → снимается удачным кадром, reset = свежий decoder, вещательная смена 2.0↔5.1, битый после смены = тишина в новом формате, смена частоты 48→44,1 кГц на настоящих кадрах.
- `layout`: 5.1/7.1/2.1 перестановки, Lsd/Rsd → Side/Rear независимо от порядка, неконвертируемые/дубли/пусто, инъективность всех 20 фиксированных позиций, кэш.
- `robustness`: детерминированная случайная порча/обрезка/мусор AC-3 и E-AC-3 (4 прохода с reset) — исход всегда по политике; все префиксы packet-а → тишина.
- Сквозные потребители (тот же adapter): `symphonia-demux/tests/ac3_eac3_playback.rs`, `mpeg-ts-demux`, `ac3-elementary-demux`, `media-source-open` (ac3 vertical, fmp4 dolby, hls-audio-versions, local_media ac3), `player-core` audio spec-change, `app-egui` local_file_open prepared_handoff AC-3.

## Проверки
`cargo test -p audio --locked`, `cargo clippy -p audio --all-targets --locked -- -D warnings`, `python3 scripts/check-refactor-guardrails.py`, `python3 -m unittest scripts/tests/test_check_refactor_guardrails.py`, `python3 scripts/check-dependency-patches.py` (8 патчей).

## Живая приёмка D6 (2026-10-17)
- Метод: release GUI, изолированные XDG_* каталоги, `PIPEWIRE_NODE=<null sink>`, `pw-record --target <sink> -P stream.capture.sink=true -f f32`, БПФ-анализ; тоны по каналу 5.1 (FL 440, FR 554, FC 659, LFE 80, SL 880, SR 1100 Гц). CPAL `default` открывает 2 канала → проверяется production `ChannelMixer` (L = FL + −3 дБ FC/SL[/Rear], LFE в стерео нет). Скрипты/медиа — только в scratchpad, не в репозитории.
- PASS: MKV AC-3 5.1, MP4 AC-3 5.1 44,1 кГц (ресемплинг в 48k), MP4 E-AC-3 5.1, сырой `.eac3` 5.1, TS вещательная смена (`mem:player-core/audio-spec-change-rebuild`), HLS AAC↔E-AC-3 (`mem:app-egui/web-audio-versions-menu`). Реальный E-AC-3 7.1 (dependent substream, кадры по 1 блоку): захват = наш PCM через формулу микшера с точностью до отсчёта (64–70 дБ на многоканальных участках, 114–146 дБ на одиночных L/C/R/Rs/Lrs/Rrs). Ни одного warn адаптера.
- Найдено (не исправлено): сырой E-AC-3 7.1 без видео — в 2 из 3 прогонов по 50 с одно опустошение ring-а (~52 мс) после tick gap ровно 250 мс (= `DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL`, warn `Audio output starvation observation ... tick_gap_ms=250`); FLAC с тем же звуком и E-AC-3 5.1 6-блочный — не воспроизводится; причина в планировщике player-core не установлена. `playlist-discovery` пробует соседние `.ac3` только Symphonia → `ERROR probe reached EOF`/`reached probe limit` от symphonia-core (лимит 1 МиБ).

## Не сделано / открыто
- Lsd/Rsd/широкие/высокие раскладки через adapter — только unit-уровень (реальных потоков нет). Многоканальный выход (не стерео) живьём не проверялся.
