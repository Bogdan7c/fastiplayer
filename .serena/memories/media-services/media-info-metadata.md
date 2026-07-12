# Media metadata and Info flow

- `media-core` owns neutral `MediaContainerMetadata`, `MediaTagMetadata`, `MediaMetadata`. `Demuxer::media_metadata()` defaults to `None`; `DemuxReadEvent::MediaMetadataChanged` is topology-neutral.
- Symphonia consumes metadata revisions as upserts for title/artist/album. When a revision appears beside a packet, it emits metadata first and stores the packet in `pending_events`, preserving packet ordering.
- `DualStreamDemuxer` uses video metadata as primary and fills missing tags from audio; an event from either child rebuilds the complete merged snapshot.
- `PreparedMedia` owns typed `MediaSourceInfo`. Local size is obtained with filesystem metadata and warning on failure. Remote display locations strip query/fragment while reopen identity stays unchanged. No extra HTTP request is made.
- `PlaybackPipeline` owns the current metadata/source-info slots. Metadata events update only that slot and do not alter decoder topology, queues, generations, or packet accounting.
- `PlayerSnapshot::media_info` is the read-only UI boundary. `TrackSummarySnapshot` carries track duration and neutral `VideoTrackMetadata`; raw `codec_private` is not exposed.
- `app-egui::ui::media_info` hides unknown optional fields and shows `Медиафайл не открыт` when absent. Static source/media rows were removed from telemetry; runtime diagnostics are still built without open media.