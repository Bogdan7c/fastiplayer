# HLS TS VOD runtime playback fix (2026-08-04)

Источник acceptance: второй playlist item / HLS TS VOD, `https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8`.

## Root-cause chain и закреплённые инварианты

- Generic extractor hint `ContainerFamily::IsoBmff` для HLS не доказывает fMP4 segment container. `app-egui::web_media_hls_open` обязан передавать `HlsContainerEvidence::ContentProbe`; exact fMP4 допустим только для явного fragmented ISO-BMFF evidence.
- `DemuxSniffBudget::max_bytes` ограничивает копируемый probe prefix, а не размер уже bounded immutable ordered segment. Registry должен replay-нуть factory весь снятый segment, включая resources >64 KiB.
- Новый HLS TS segment может перезапустить transport continuity counters без смены media timeline. `mpeg-ts-demux` сбрасывает segment-local PSI/PES/continuity assemblers на `starts_new_segment`, но публикует `TracksChanged` только для explicit timeline discontinuity/topology/config change.
- Доказанные elementary audio codec/sample-rate/channels хранятся как `AudioTrackEvidence`, переживают same-topology index rewind и очищаются при PMT topology change.
- MPEG-TS H.264 Annex-B несёт SPS/PPS in-band и может не иметь `codec_private`. VA-API H.264 adapter принимает этот contract; length-prefixed AVCC без valid avcC по-прежнему typed-rejected.
- Capability report доказывает, что backend можно создать, но не заменяет отсутствующий decoder thread. Late video track без decoder-а создаёт `VideoBackendSelectionRequested { decodable_by_active_backend: false }`.
- Shell не переиспользует pipeline только по совпадению `VideoBackendKind`: при `decodable_by_active_backend=false` он rebuild-ит decoder даже для того же backend class.
- Backend reselection хранит intent-тип `BackendReselectionResumeStrategy`. Late track без прежнего decoder-а использует `ContinueForwardToKeyframe`, потому что ранний HLS seek index может ещё не иметь RAP anchor; замена реально работавшего decoder-а сохраняет `ReseekCurrentPosition`.
- Пока backend reselection pending и decoder отсутствует, bounded pending video packets не считаются decoder starvation и не выбрасываются; стартовый keyframe должен пережить install.
- Follow-up 2026-08-05: `send_pending_video_packets_to_decoder` также обязан сохранить packet, принадлежащий `pending_video_backend_reselection`, даже если track ещё не стал selected. Между worker request и shell backend install проходит отдельный tick; прежняя проверка «не selected track» удаляла первый IDR и вынуждала ждать следующий GOP около 4.4 секунды. Чужие track packets по-прежнему удаляются.

## Restart resume readiness follow-up (2026-08-05)

- Для static HLS VOD `app-egui::web_media_hls_open` теперь всегда ждёт первый authoritative `TracksChanged` на media-open worker-е до сборки `PreparedMedia` и `Installed`, даже когда yt-dlp заранее объявил H.264/AAC и codec proof не deferred.
- Корневая гонка: declared-codec HLS раньше обходил ожидание; `PreparedMedia` снимал `duration=None`, atomic player install публиковал `UnknownTimeline`/non-seekable, а startup немедленно отправлял сохранённую ненулевую позицию до позднего player tick с `TracksChanged`.
- HLS VOD readiness owner остаётся app composition boundary; startup/session не ждут provider events и player API не менялся.
- Follow-up 2026-08-10: любой live HLS тоже обязан иметь непустой authoritative `demuxer.tracks()` snapshot до Installed, а не только deferred codec layout. Уже применённый bootstrap snapshot достаточен и не требует replay `TracksChanged`; dynamic timeline/persistent checkpoint rules не затронуты. Полный contract: `mem:media-services/hls-live-avc3-2026-08-10`.
- Regression `web_media_hls_open::tests::hls_vod_is_seekable_with_duration_at_prepared_media_boundary` доказывает, что finite HLS до player install уже публикует tracks, duration и seekable snapshot.
- Follow-up generation race: compatibility backend replacement сразу после `Installed` раньше безусловно запускал `reseek(current_position)` и supersede-ил request-owned startup restore generation. Теперь `player-core::capability_selection` сохраняет уже идущий seek/scrub generation: для принятого demux commit новый decoder повторно получает output floor, для ожидаемого worker receipt floor применяется после receipt; отдельный recovery reseek остаётся только вне active positioning lifecycle.
- Functional regression `session::tests::installed_media_restore::backend_replacement_preserves_in_flight_position_restore_generation` сначала воспроизводил generation 1 → 2 и теперь доказывает matching terminal `Applied` после backend swap.

## Проверка

- Functional player-core test `late_hls_h264_track_reaches_presentation_after_backend_install` проходит полный synthetic route: late Annex-B track -> backend request -> отдельный worker tick до backend install -> retained IDR -> decoder send -> decoded frame -> presentation scheduler. Тест также доказывает, что packet чужого track-а не удерживается.
- Live HLS trace 2026-08-05 после фикса: track list update `10:57:35.268`, backend install `10:57:35.282`, первый IDR `pts=33 ms` принят `10:57:35.283`, первый frame представлен `10:57:35.310`; bootstrap-drop до следующего keyframe отсутствует.
- Focused tests покрывают same-kind shell rebuild/reuse, packet retention, Annex-B without avcC, AVCC negative contract, >70 KiB ordered TS replay, independent TS segment continuity and audio evidence.
- Полный прогон: `cargo +1.96.0 test -p player-core -p app-egui -p video-vaapi -p demux-api -p mpeg-ts-demux -p web-media-hls --all-targets`.
- Clippy: changed libraries `--all-targets -- -D warnings`; `app-egui --all-targets --no-deps -- -D warnings`.
- Release GUI clean run с isolated XDG config реально дошёл до WGPU surface. Два screenshots показали разные кадры и timeline 01:47 -> 01:56; telemetry `video_frames_presented` выросла 1889 -> 2668 при `Playback state: Playing`.

## Known diagnostic limitation

Runtime overlay может продолжать показывать startup label `Backend: VA-API VP9`, хотя codec adapter уже сконфигурирован как H.264. Это stale display-name/telemetry label, не фактический codec route; playback evidence выше получено из H.264 Annex-B decode и меняющихся кадров.

Связанные memories: `mem:testing/web-media-playlist-acceptance-2026-08-04`, `mem:video-vaapi/h264-known-issues`, `mem:mpeg-ts-demux/core`, `mem:video-core/decoder-stream-boundary`.