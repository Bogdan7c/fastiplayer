# AV1-в-MP4 FFmpeg software decode fix (2026-06-18, Session 25)

Исправлен дефект «AV1 .mp4 не декодируется через FFmpeg software, лог сыпет
`[libdav1d] Error parsing OBU data`, кадры не идут». Старая запись в
`mem:render-video/core` об этом как о «дефекте packetization/extradata AV1 в
video-ffmpeg» НЕВЕРНА — заменяется этим.

## Истинная причина (двухчастная)
1. extradata/packets были КОРРЕКТНЫ: symphonia отдаёт MP4-сэмплы verbatim
   (валидный OBU low-overhead stream, SEQ_HEADER+FRAME с obu_has_size_field=1),
   av1C идёт как extradata, и libdav1d сам стрипает 4-байтовый
   AV1CodecConfigurationRecord header по marker-биту 0x80 (подтверждено context7
   FFmpeg trunk: `libdav1d_parse_extradata`). FFmpeg CLI `-c:v libdav1d` декодит
   файлы без ошибок. Важно: libdav1d НЕ скармливает sequence header из extradata
   самому dav1d для декода — он нужен IN-BAND, т.е. decode обязан стартовать с
   KEY_FRAME (sync-сэмпл в MP4 несёт SEQ_HEADER in-band; inter-сэмплы — нет).
2. У AV1 НЕ было packet-level keyframe probe (codec-core возвращал
   `AdapterUnavailable` → `PacketKeyframe::Unknown`). В
   `accept_video_packet_for_decoder_bootstrap` (player-core
   `session/tick/video_decoder_io.rs`) ветка `Unknown` для кодеков НЕ из
   `active_video_codec_requires_proven_decode_start` (там только H264/H265)
   ПРИНИМАЕТ первый post-flush пакет как decode start. На auto после свапа
   VAAPI→ffmpeg decoder флашится (needs_keyframe=true), а первый дошедший пакет —
   inter-frame без SEQ_HEADER → libdav1d «Error parsing OBU data». На
   `preferred_backend=software` свапа нет, первый пакет — реальный keyframe (pts 0),
   поэтому software-путь по факту РАБОТАЛ (вопреки старому описанию задачи).

## Вторая часть бага — стартовая задержка на auto
При свапе VAAPI→ffmpeg keyframe pts=0 (vid#0), прочитанный во время deferral,
ОТБРАСЫВАЛСЯ: в `send_pending_video_packets_to_decoder`
(`video_decoder_io.rs:523`) пакет дропается если
`!video_packet_belongs_to_selected_track` — а во время deferral video track ещё
не выбран. После свапа demuxer уже за vid#0, decoder ждёт следующий keyframe →
на HDR-файле (keyframes 0 / 6.84s) ~6.8s чёрного при играющем звуке.

## Фикс (3 файла, scope = codec/demux + swap application, НЕ selector)
1. Новый `crates/codec-core/src/av1.rs`: `probe_av1_packet_keyframe(packet,
   codec_private) -> Result<bool, Av1ObuError>`. Свой OBU-walker (low-overhead,
   obu_has_size_field; leb128; extension byte) + MSB bit reader (AV1 без emulation
   prevention). keyframe = первый FRAME/FRAME_HEADER OBU с show_existing_frame==0
   и frame_type==KEY_FRAME(0). reduced_still_picture_header читается из in-band
   SEQ_HEADER или из av1C (cp[4..]) → тогда всегда KEY_FRAME. Только KEY_FRAME
   считается keyframe (не INTRA_ONLY/SWITCH), что совпадает с MP4 stss sync.
   Focused-тесты в модуле. Лейаут OBU header/uncompressed_header сверен с context7
   (FFmpeg `parse_obu_header`, cbs_av1).
2. `adapter.rs`: ветка `VideoCodec::Av1` в
   `probe_video_packet_keyframe_with_codec_private` → `Keyframe(bool)` /
   `Uncertain(ParseError)`. Requirement-probe AV1 НЕ трогался (остался
   AdapterUnavailable, тест на это сохранён). Теперь AV1 packets получают
   Keyframe/NotKeyframe, и существующие ветки bootstrap корректно дропают
   inter-frames до реального keyframe (как VP9).
3. `player-core/src/session/capability_selection.rs`:
   `retry_pending_video_backend_reselection` после `activate_video_track` +
   `note_active_video_stream_requirement(..., true)` вызывает новый
   `reseek_to_current_position_after_backend_swap()` →
   `self.seek(SeekRequest::accurate(current_position))`. Accurate seek =
   DecodePointBefore, перечитывает поток с ближайшего keyframe до текущей позиции
   (stss), не сдвигая audio gate. Срабатывает ТОЛЬКО на реальном backend-swap
   (retry pending), т.е. на auto VAAPI→ffmpeg; software/hardware-preference и
   no-swap пути не затрагиваются. select_video_pipeline_plan / pending-reselection
   selection-логика НЕ менялись.

## Проверено
- auto: SDR (`<MEDIA_DIR>/av1-4k60-sdr.mp4`) и HDR PQ
  (`<MEDIA_DIR>/av1-4k60-hdr-pq.mp4`): 0 «Error parsing OBU data», re-seek target_ms=0
  DecodePointBefore accepted, «Accepted post-flush bootstrap pts_ms=0
  first_accepted_keyframe=Keyframe», видео стартует с 0 в синхрон со звуком.
- software: без свапа, без лишнего seek, 0 OBU errors.
- Тесты: codec-core 69, player-core 368, symphonia-demux 123,
  video-ffmpeg --features ffmpeg 74, guardrails OK, fmt clean.

CPU RGB conversion / FFmpeg hwdecode НЕ добавлялись; raw FFmpeg типы остались в
video-ffmpeg; не хардкод под файл.
