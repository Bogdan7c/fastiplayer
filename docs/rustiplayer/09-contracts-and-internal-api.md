# 09. Контракты и Internal API

## Player boundary

Публичный внутренний API `player-core`:

- commands: `PlayerCommand`, `PlayerCommandSender`;
- state: `PlayerSnapshot`, `PlaybackState`;
- events: `PlayerEvent`, `PlayerWorkerEvent`;
- runtime: `PlayerWorker`, `PlayerWorkerConfig`;
- render lease: `PresentFrameLease`, `PlayerPresentFrame`,
  `PresentFrameResourceDescriptor`, `PresentFrameResourceLookup`;
- render errors: `PlayerRenderError`, `PlayerRenderErrorKind`;
- seek: `SeekRequest`, `SeekTarget`, `SeekMode`, `ScrubCommitPolicy`;
- backend/resource provider re-exports: `StartedVideoBackend`,
  `PresentFrameResourceProvider`, `PresentFrameResourceProviderHandle`,
  `PresentFrameResourceProviderLookup`.

Контракт:

- UI отправляет команды и читает snapshots.
- Worker владеет `PlayerSession`.
- Render thread получает frame leases, а не ссылки на pipeline.
- Render bridge получает stable present-frame identity через session boundary,
  а не через прямой доступ к `PlayerSession::pipeline`.
- Частые scrub updates схлопываются по latest-wins семантике.
- Concrete production backend создаётся вне `player-core`. `player-core`
  получает только `video-backend-api::StartedVideoBackend` и renderer-neutral
  resource provider handle.
- `video-backend-api` владеет startup/resource-provider contract. Concrete
  backend crates реализуют этот contract без зависимости на `player-core`.
- WGPU materialization находится в `app-egui`/`render-wgpu-video`, а
  surface/present lifecycle остаётся в `render-wgpu-shell`: player lease хранит
  opaque handle, descriptor, lookup/release accounting и не возвращает
  `wgpu::TextureView`.

## `PlaybackPipeline` internal boundary

`player-core::PlaybackPipeline` является владельцем runtime slots текущей
session, но больше не является широким `pub(crate)` storage boundary. Сам struct
остаётся `pub(crate)`, потому что session boundary работает внутри
`player-core`, но поля struct закрыты. `PlayerSession::pipeline` тоже private:
sibling modules вне `session` должны идти через session-owned boundary methods,
а не через чтение/запись concrete fields.

Закрытые домены:

- media source и demux: `install_opened_media()`, `has_demuxer()`,
  `source_file_path()`, `source_label()`, `tracks()`, `track_count()`,
  `demux_next_packet()`, `seek_demuxer()`, `reset_media_slots()`;
- track selection и active requirement: `selected_video_track_id()`,
  `selected_audio_track_id()`, `has_selected_video_track()`,
  `has_selected_audio_track()`, `video_packet_belongs_to_selected_track()`,
  `select_video_track()`, `select_audio_track()`, `clear_selected_tracks()`,
  `active_video_requirement()`, `set_active_video_requirement()`;
- seek generation и frame timing: `seek_generation()`,
  `begin_seek_generation()`, `packet_generation_is_current()`,
  `video_frame_duration_estimate()`, `observe_decoded_video_frame_pts()`,
  `reset_video_frame_timing_estimator()`, `clear_pending_packets_for_seek()`,
  `reset_decoder_state_for_seek()`, `reset_clocks_for_seek()`;
- audio decoder/output/clock: `install_audio_decoder()`,
  `clear_audio_decoder()`, `decode_audio_packet()`, `reset_audio_decoder()`,
  `install_audio_output()`, `clear_audio_output()`,
  `write_audio_output_samples()`, `play_audio_output()`,
  `pause_audio_output()`, `clear_audio_output_for_seek()`,
  `set_audio_output_volume()`, `audio_output_buffer_level_ms()`,
  `audio_output_clock()`, `has_audio_clock()`, `install_audio_clock()`,
  `clear_audio_clock()`, `reset_audio_clock()`, `audio_clock_now()`,
  `audio_clock_underrun_callbacks()`, `media_clock_base()`,
  `set_media_clock_base()`, `media_position_from_audio_clock()`,
  `start_monotonic_media_clock()`, `clear_monotonic_media_clock()`,
  `monotonic_media_position()`, `note_audio_clock_sample()`,
  `reset_audio_clock_sample()`, `audio_clock_stalled_for()`,
  `audio_buffer_clear_generation()`, `mark_audio_buffer_clear_ack()`;
- packet queues и presentation queues: `enqueue_pending_audio_packet()`,
  `pop_pending_audio_packet_front()`, `push_pending_audio_packet_front()`,
  `pending_audio_packet_is_empty()`, `pending_audio_packet_len()`,
  `clear_pending_audio_packets()`, `enqueue_pending_video_packet()`,
  `front_pending_video_packet()`, `pop_pending_video_packet_front()`,
  `pending_video_packet_is_empty()`, `pending_video_packet_len()`,
  `clear_pending_video_packets()`, `front_queued_video_frame()`,
  `queued_video_frames()`, `front_and_next_queued_video_frames()`,
  `pop_queued_video_frame_front()`, `enqueue_queued_video_frame()`,
  `video_present_queue_is_empty()`, `video_present_queue_len()`;
- present frame и EOF seek fallback: `present_video_frame()`,
  `present_video_frame_pts()`, `present_video_frame_covers()`,
  `present_video_frame_matches()`, `has_present_video_frame()`,
  `set_present_video_frame()`, `take_present_video_frame()`,
  `replace_present_video_frame()`, `has_seek_preroll_fallback_video_frame()`,
  `take_seek_preroll_fallback_video_frame()`,
  `replace_seek_preroll_fallback_video_frame()`,
  `clear_seek_preroll_fallback_video_frame()`, `clear_video_queues()`;
- video decoder I/O и accounting: `set_video_decoder_thread_handle()`,
  `has_active_video_decoder()`, `video_backend_name()`,
  `can_send_video_decode_packets()`, `can_receive_decoded_video_frames()`,
  `video_decoder_packet_queue_depth()`, `video_decoder_resource_snapshot()`,
  `video_decoder_control_channel_pressure()`,
  `video_decoder_resource_provider()`, `release_frame_to_video_decoder()`,
  `flush_video_decoder_thread()`, `try_recv_decoded_video_frame()`,
  `try_recv_video_decoder_diagnostic_event()`,
  `try_recv_video_decoder_error()`,
  `drain_completed_video_decode_packet_count()`, `send_video_decode_packet()`,
  `reset_video_decode_in_flight()`, `note_video_packet_sent_to_decoder()`,
  `note_video_packets_completed_by_decoder()`,
  `video_decode_in_flight_packets()`, `video_decoder_needs_keyframe()`,
  `mark_video_decoder_bootstrapped()`, `require_video_decoder_keyframe()`;
- render generation и lease accounting: `advance_render_generation()`,
  `render_generation()`, `try_register_render_lease()`,
  `release_render_lease_accounting()`, `request_video_texture_release()`,
  `active_render_lease_count()`, `deferred_render_release_count()`.

## `PlayerSession` render lease boundary

`PlayerSession` владеет связкой present frame, active decoder guard-а, render
generation и render lease accounting. Sibling modules не читают `pipeline`
напрямую.

Основные boundary methods:

- `current_present_frame_identity()` возвращает stable identity latest-slot-а
  только если есть active video decoder и текущий present frame.
- `lease_present_video_frame()` регистрирует render lease и передаёт render
  bridge-у `LeasedPresentFrame`.
- `release_render_lease_with_provider()` различает submitted/unsubmitted
  renderer ownership и сохраняет release path через original provider.
- `release_video_texture()` освобождает texture сразу, через renderer provider
  или откладывает release до drop render lease-а.

Transitional method:

- `select_video_track_preserving_active_requirement()` остаётся для legacy
  command path, где команда выбора video track ещё не приносит заново
  проверенный `VideoDecodeRequirement`. В коде рядом с методом есть TODO и
  причина удаления.

Вне этой задачи:

- helper structs `DecodedAudioPacket`, `PendingAudioPacket` и
  `PendingVideoPacket` всё ещё имеют `pub(crate)` поля как маленькие packet
  transfer records внутри `player-core`; они не являются полями
  `PlaybackPipeline` и не открывают сам pipeline как data bag.
- Test-only helpers `has_audio_decoder()`, `has_audio_output()`,
  `set_seek_generation_for_tests()`, `set_video_decoder_thread()` и
  `has_deferred_video_texture_release()` остаются под `#[cfg(test)]` и не
  являются runtime storage boundary.

## Media/demux contract

`media-core::Packet` является типом передачи packets между demuxer и player.
Payload хранится как `Bytes`, поэтому clone означает shared ownership, а не копию
payload.

`player-core::PreparedMedia` является границей открытия media. Shell или service
layer открывает concrete demuxer, снимает tracks/duration/seekability и передаёт
в worker уже готовый `Box<dyn media_core::Demuxer + Send>`. `player-core` после
refactor не должен напрямую зависеть от `webm-demux` ради локального открытия.

`webm-demux::Demuxer` returns packets and supports timeline seek through
`DemuxSeekRequest`. Demuxer seek gives a decode-safe or approximate container
position; `player-core` owns final pre-roll/drop/commit.

## Codec contract

`VideoDecodeRequirement` является единственным объектом stream requirement,
который попадает в capability selection. Он объединяет codec/profile/bit-depth/
chroma/resolution, surface format, memory contract, color pipeline requirement и
timing contract.

Codec adapters могут уточнять requirements. Они не должны напрямую открывать
backend, renderer, UI или source resources.

## Capability contract

`SystemCapabilities::select_best_video_stream()` является selection gate.

Selection должна учитывать:

- supported decode format;
- mandatory export path from `VideoMemoryContract`;
- renderer format support;
- P010 readiness and storage layout;
- strict HDR metadata;
- renderer HDR-to-SDR settings.

Ошибки должны использовать `VideoCapabilityRejection`, а не generic strings,
если причина влияет на user-facing поведение.

## Video decode contract

`video-vaapi::VideoDecodeThread` owns backend threading and queues. It accepts
`DecodePacket` and publishes `video_core::DecodedFrame`.

`video-backend-api` owns `VideoBackendFactory`, `StartedVideoBackend` and
`PresentFrameResourceProvider*`. `player-core` consumes `StartedVideoBackend`
and re-exports the playback-facing provider handle types, but does not own the
factory trait. `video-vaapi::VaapiWgpuVideoBackendFactory` implements the
contract from `video-backend-api`; concrete backend crates must not depend on
`player-core`.

Decoded frame contract:

- `format`: `Nv12` or `P010` for production paths;
- `memory_path`: `DmaBufZeroCopy`;
- `texture_handle`: opaque handle, not a CPU image;
- `color`: resolved `VideoColorMetadata`;
- diagnostics travel with the frame.

`VideoTextureViewProvider` является concrete render-side bridge для WGPU texture
views. Он остаётся в `video-vaapi`/`app-egui`/`render-wgpu-video` composition
path-е, а `player-core` видит только renderer-neutral lookup/release boundary.

## Render contract

`render_wgpu_video::WgpuRenderableFrame` validates decoded frame metadata before
render. `render-wgpu-shell` получает уже подготовленный `WgpuRenderableFrame` и
владеет только WGPU surface/egui composition/submit-present lifecycle.

Allowed constructors:

- `from_decoded_nv12`;
- `from_decoded_p010`.

Оба конструктора отвергают non-zero-copy memory paths. Metadata/plane mismatch
является render boundary error.

`RenderDiagnostics` renderer-neutral: UI может показывать его без GPU handles.

## Config contract

`AppConfig::validate()` обязателен после deserialization. Defaults принадлежат
коду и описаны в [05. Config and Runtime Data](05-config-and-storage.md).

Unknown fields являются ошибками. Silent fallback для invalid config запрещён,
если validation явно не документирует compatibility mapping.

## Service/source contract

`service-youtube` may know YouTube and `yt-dlp`. It may not know renderer,
playback queues or UI layout.

`source-core::ByteSource` exposes read/seek/position/seekability/validators.
Service-specific headers are data, not hardcoded source policy.
