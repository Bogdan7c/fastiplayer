# HostPlanar upload: StagingBelt, однопоточная копия, GPU-копия в submit кадра

Актуально с cpu-parity сессии 07 (2026-10-22, коммиты `8298c676`, `d80084cb`). История до 07 (полосная копия в 4 потока через `std::thread::scope`, свой `queue.submit` на кадр, сессия 2026-07-03) — в git-истории этой memory; не возвращать.

## Как устроено сейчас
`render-wgpu-video::video::host_planar_upload` (`WgpuHostPlanarUploadBackend` в `host_planar_upload/wgpu_backend.rs`):
- Плоскости кадра копируются в mapped chunk `wgpu::util::StagingBelt` (chunk 16 МиБ — весь 4K-кадр) **на текущем (главном) потоке**, строка за строкой (`copy_plane_block_into_staging`): stride источника FFmpeg и staging (выравнивание `COPY_BYTES_PER_ROW_ALIGNMENT` = 256) в общем случае разные. Потоков-помощников нет. Замер 07 (AV1 4K60, 7840HS): 4 потока давали `video_prepare` 1,07 мс против 1,12 мс одним потоком, а создание 3 потоков на кадр стоило ~8 п.п. ядра процесса (181 новый поток/с). Решение владельца 07: однопоточно; проверить хвост на T480s в сессии 10, при регрессии — постоянный пул в одной функции.
- `copy_buffer_to_texture` всех плоскостей кадра пишется в один encoder (`FrameUploadRecording { encoder, destination }`). `flush_plane_uploads` НЕ делает submit: `StagingBelt::finish_and_recall_on_submit(&encoder)` (wgpu 30.0.1, re-map чанков после submit-а именно этого command buffer-а), `encoder.finish()` кладётся в слот — `PendingHostPlanarUpload` (`host_planar_upload/pending_upload.rs`, поле `WgpuHostPlanarUploadedTextures::pending_upload: Arc<..>`).
- Кадр из views материализатора: `HostPlanarWgpuTextureViews::plane_views()` / `WgpuFrameTextureViews::host_planar_plane_views()` → `WgpuRenderableFrame::from_decoded_host_planar` (несёт `host_upload: Option<&PendingHostPlanarUpload>`). `WgpuVideoRenderer` после **успешной** записи host-planar pass-а забирает копию (`claim_host_upload`) в `WgpuVideoRenderOutcome::host_upload: Option<HostPlanarUploadCommands>`; shell (`render-wgpu-shell::shell.rs`) отправляет её ПЕРВОЙ в общем `queue.submit` кадра (upload → DMA-BUF acquire → egui callbacks → encoder кадра → DMA-BUF release). Один submit на кадр.
- Невостребованная копия (кадр загрузили, но не нарисовали; рендер упал) уходит отдельным submit-ом в начале следующей загрузки (`ensure_frame_upload_recording` → `submit_unclaimed_upload`) и, на всякий случай, в `flush_plane_uploads` перед сохранением новой. Инвариант: каждая записанная копия попадает в очередь ровно один раз и раньше любого submit-а, который семплит слот. Повторный показ слота копию не повторяет.
- `WgpuRenderableFrame::from_decoded_host_yuv` (отдельные views без слота) — `pub(crate)`, только для тестов рендерера; `from_decoded_host_yuv420` удалён. Views материализатора через него рендерить нельзя: копия не попадёт в submit.
- `uploaded_textures_are_idle` = `Arc::strong_count(slot) == 1`; ожидающая копия держит `Arc<PendingHostPlanarUpload>`, а не слот, и на idle не влияет. Пул 24 слота, `MIN_REUSE_RING_SLOTS = 4`, `Busy` при полном — без изменений.

## Почему так (не повторять отклонённое)
- `create_buffer(mapped_at_creation)` на кадр — АНТИПАТТЕРН: wgpu zero-инициализирует буфер дороже самой копии. Только belt/reuse.
- `Queue::write_texture` — та же CPU-копия в staging wgpu, без выигрыша; upload-ahead на отдельном потоке отклонён (`mem:video-ffmpeg/software-upload-ahead-REJECTED`). Копия и запись остаются на render-потоке синхронно.
- Отдельный submit загрузки стоил главному потоку ~0,12 мс/кадр (профиль samply 07), дороже submit-а всего кадра.

## Backpressure SW-загрузки
`render-wgpu-video::resource_provider::WgpuReleaseVideoDecoderThreadHandle` пробрасывает `host_upload_resource_snapshot` (с 07; до этого default `UnsupportedBackend` → в живом приложении player-core не тормозил отправку пакетов по полной ready-очереди/исчерпанным upload-слотам FFmpeg). Правило: обёртка обязана пробрасывать ВСЕ default-методы `VideoDecoderThreadHandle` (та же дыра была у `decoder_activity_snapshot`, сессия 02).

## Тесты
- `host_planar_upload/pending_upload/tests.rs` (GPU, реальный материализатор → `render_or_clear` → submit → чтение пикселя): копия в submit кадра даёт белый кадр; без неё кадр не белый (материализатор не отправляет сам); незабранная копия уходит перед следующей загрузкой; повторный показ без второй копии. Пропуск без адаптера.
- `host_planar_upload/wgpu_backend/tests.rs`: копия строк с padding-ом, вся 4K-плоскость, нулевая высота.
- `resource_provider.rs` tests: проброс host-upload снимка и `UnsupportedBackend` без придумывания.
- Внешние потребители: `video-ffmpeg/tests/support/wgpu_acceptance.rs` (AV1 vendored, `--features vendored-ffmpeg -- --include-ignored`), `app-egui/.../content_probe_tests/direct_progressive_webm.rs` отправляют `host_upload` первым в своём submit-е.

## Потоки software-декода
`video.sw_decode_threads` (0 = auto = `max(2, available_parallelism − 2)`), `video.sw_decoder_surface_pool_frames` = 8 (`mem:video-ffmpeg/software-frame-pool-setting`) — без изменений; политика потоков — сессия 08 плана `user/cpu-parity/`.
