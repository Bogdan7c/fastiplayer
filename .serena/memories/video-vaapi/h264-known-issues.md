# H.264 known issues (диагностика 2026-06-02)

Файлы 4K60 `test-assets/H264/4k60fps/LXb3EKWsInQ_2160p60_*` (3840×2160@59.94, 154–180 Mbps).
Полные разборы и Codex-промты: `user/h264_playback_perf_findings.md`, `user/h264_seek_bug_diagnosis.md`.

## Seek сломан (баг корректности, не perf, воспроизводится и в release)
- Корень: seek-flush зовёт cros `inner.flush()` (H.264 adapter `codec_adapter.rs` `flush()`), который
  отдаёт буферизованные DPB-tail B-кадры как pending events. `VaapiVideoDecoder::flush`
  (`decoder.rs`) после adapter.flush() только `release_decoder_owned_ready_frames`, но НЕ drain-ит и
  не выбрасывает свежие cros tail-события. Следующий post-seek пакет затягивает их через
  `drain_decoder_events` в `ready_queue`; `decode_queued_packet` (`decoder_thread.rs`) делает
  `frame.generation = decode_packet.generation` и штампует stale-кадр новым generation.
- Эффект: первый presented frame seek-а = stale-кадр с ранним pts; commit policy "presented-frame"
  (`seek_transaction.rs` `note_presented_frame_for_seek` / `final_seek_presented_frame_commit_position`)
  коммитит seek на pts старого кадра. Измерено: seek→30s коммитится в debug на 4.0s, в release на ~6s
  (≈ прежняя позиция). VP9 иммунен: Profile 0/2 без B-frame reorder → flush не отдаёт дисплейных tail.
- Фикс (план): на seek-flush явно discard cros tail events + очистить ready_queue (release VA surface),
  отделив seek-discard от EOF-drain (`begin_end_of_stream_drain`, который tail-кадры СОХРАНЯЕТ — см.
  `mem:video-core/decoder-stream-boundary`); плюс защитный generation/pts-гейт у player.

## Playback в release ок; debug не тянет (perf, не баг)
- В release все 5 файлов ~60fps, 0 late drops. В debug (`cargo run`) decoder отстаёт (in_flight упирается
  в packet-channel cap 32), present queue голодает → repeats/late. Причина — CPU-парсинг битстрима +
  per-frame работа; VP9 4K выживает в debug из-за ~6× меньшего битрейта.
- Perf-опт для отдельной сессии: (1) SPS/PPS инжектить только на keyframe (сейчас `BeforeAccessUnit` на
  каждый AU в `codec_adapter.rs access_unit_to_annex_b`) + scratch-буфер вместо Vec на кадр; нужно
  протянуть keyframe-флаг в `submit_packet` boundary. (2) Persistent DMA-BUF import reuse выключен
  намеренно (`imports_reused=0`): `zero_copy_surface_pool.rs allows_persistent_reuse` требует
  `explicit_external_memory_reuse_sync`, а `vaapi_vulkan_dma_buf()` ставит его false (нет
  VA-writer→Vulkan-sampler sync); общий путь VP9+H.264.

Диагностика — без правок кода (temp autoseek-хук в worker и temp test seek_diag удалены).
