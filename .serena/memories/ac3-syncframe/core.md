# ac3-syncframe — заголовки AC-3/E-AC-3 и сборка access unit-ов (2026-10-16)

## Роль и границы
- `crates/ac3-syncframe` — std-only (без зависимостей, даже без thiserror) разбор syncframe-ов A/52:2018 (§5.3 AC-3, Annex E E-AC-3). Не декодирует и не знает контейнеров. Guardrail: `AC3_SYNCFRAME_ALLOWED_DEPENDENCIES = frozenset()` в `scripts/check-refactor-guardrails.py`; crate в `REQUIRED_ROLE_CRATES`; `mpeg-ts-demux` разрешён как потребитель.
- Почему не `codec-core`: тот тянет `video-frame-contract`/serde/vp9-parser и описывает требования аппаратного видео-декода; аудио-декодер и raw-demuxer не должны получать видео-зависимости.
- Потребители: `mpeg-ts-demux` (`src/ac3_elementary.rs`), raw `.ac3/.eac3` demuxer `ac3-elementary-demux` (`mem:ac3-elementary-demux/core`).

## Публичный API
- `parse_syncframe_header(&[u8]) -> Result<SyncframeHeader, SyncframeHeaderError>` — буфер начинается с `0x0B77`; нужно ≤12 байт заголовка, не весь кадр.
- `SyncframeHeader`: `codec()` (`SyncframeCodec::{Ac3, EnhancedAc3}` по bsid 0–8 / 11–16), `layout()` (`SyncframeLayout::{Ac3, EnhancedAc3(Eac3Substream)}`), `eac3_substream()`, `bitstream_id()`, `frame_length_bytes()`, `sample_rate_hz()` (вкл. fscod2 24/22.05/16 кГц), `samples_per_frame()` (1536 / numblks×256), `audio_coding_mode()` (`AudioCodingMode`, `acmod()`, `full_bandwidth_channel_count()`), `low_frequency_effects()`, `channel_count()` (substream: nfchans+lfe), `channel_locations()` (`ChannelLocations` в нотации chanmap), `starts_access_unit()`.
- `Eac3Substream`: `stream_type()` (`Eac3StreamType::{Independent, Dependent, Ac3Convertible}`), `substream_id()` (`SubstreamId`, `PRIMARY`), `channel_map()` (`DependentChannelMap::{FromCodingMode, Explicit(ChannelLocations)}`), `is_independent()`.
- `locate_syncframe(&[u8]) -> SyncframeSearch::{Found{offset, header}, NeedMoreData{discardable_prefix_bytes}}` — stateless resync, пропускает ложные `0x0B77`, не выбрасывает возможную половину синхрослова/неполный заголовок.
- `scan_access_unit(&[u8], StreamContinuation::{MoreBytesMayFollow, EndOfStream}) -> Result<AccessUnitScan, _>`; `AccessUnitScan::{Complete(AccessUnit), NeedMoreData, OrphanSyncframe{header, syncframe_length_bytes}}`. `AccessUnit`: `primary_header()`, `codec()` (E-AC-3, если в основной программе есть E-AC-3 кадр — AC-3 ядро + E-AC-3 dependent), `total_length_bytes()`, `primary_program_length_bytes()`, `syncframe_count()`, `samples_per_frame()`, `presentation_channel_count()`/`presentation_channel_locations()` (5.1 ∪ dependent Lrs/Rrs = 8), `has_additional_programs()`. `MAX_SYNCFRAMES_PER_ACCESS_UNIT = 64`.
- `SyncframeHeaderError::{Truncated{available_bytes, required_bytes}, MissingSyncWord, UnsupportedBitstreamId{bitstream_id}, ReservedSampleRateCode, ReservedFrameSizeCode{frame_size_code}, ReservedStreamType, FrameShorterThanHeader{..}, AccessUnitTooLong{..}}`, `is_truncation()`; `Display` английский.

## Инварианты
- Access unit = AC-3 кадр или независимый substream 0 + всё до следующего такого кадра (Matroska/MP4 форма). Основная программа — непрерывный префикс (порядок substream-ов по Annex E); программы 1–7 идут после и в `primary_program_length_bytes` не входят (иначе декодер перезаписал бы PCM основной программы).
- E-AC-3 пакет закрывается только заголовком следующего AU или `EndOfStream` (lookahead на один кадр).
- Повреждение/обрыв *следующего* кадра не портит собранный AU: возвращается `Complete`, ошибку даёт следующий вызов. При `MoreBytesMayFollow` обрыв — `NeedMoreData`, не ошибка.
- bsid 9–10 (с 2026-10-17, пакет I) = AC-3 пониженной частоты: `codec() == Ac3`, длина кадра по таблице базовой `fscod`, `sample_rate_hz() = fs >> (bsid − 8)` (24/22.05/16 и 12/11.025/8 кГц), 1536 сэмплов → длительность кадра вдвое/вчетверо больше (тест `reduced_rate_ac3_bitstream_ids_shift_sample_rate_but_keep_frame_bytes`; raw demuxer `half_rate_bsid_9_ac3_has_halved_sample_rate_and_doubled_frame_duration`). Будущий собственный decoder обязан давать те же частоты. bsid 17–31 явно отвергаются (`UnsupportedBitstreamId`); fscod 3 у AC-3 / fscod2 3, frmsizecod ≥38, strmtyp 3 — reserved-ошибки.
- Длина AC-3 кадра: bitrate×96000/fs слов, при остатке (44.1 кГц) нечётный frmsizecod +1 слово.

## Тесты (`crates/ac3-syncframe/tests/syncframe/`, один бинарь)
- `header_tables.rs`: вся таблица 5.18 (38 frmsizecod × 3 fscod, переписана из спеки), reserved, bsid-диапазоны, acmod×lfeon (пропуск cmix/surmix/dsurmod), E-AC-3 fscod×numblkscod и fscod2, strmtyp×substreamid, chanmap (вкл. dual mono), точный `required_bytes` на каждом префиксе.
- `access_units.rs`: AC-3 AU, dependent → 7.1, доп. программы, orphan, каждая точка разреза → NeedMoreData, порча следующего кадра, EOS-обрыв, предел 64, numblks 1, AC-3 ядро + E-AC-3 dependent → `EnhancedAc3`.
- `search.rs`, `real_frames.rs`: реальные кадры ffmpeg (`tests/fixtures/`, `generate.sh`, README со значениями ffprobe; 5 файлов ≤7 КБ), разбиение файла и потоковое чтение кусками по 100 байт с мусором впереди дают одинаковые AU.
- Проверка: `cargo test -p ac3-syncframe`, `cargo clippy -p ac3-syncframe --all-targets -- -D warnings`.

## Не проверено
- Реальные E-AC-3 с dependent substream-ами/доп. программами (ffmpeg не кодирует) — только ручные байты; reduced-rate E-AC-3 — только ручные байты.
