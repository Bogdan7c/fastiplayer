use super::*;
use crate::codec_adapter::{VaapiAdapterDecodeError, VaapiPacketDecodeHints};
use anyhow::Result;
use std::time::Duration;
use tracing::{debug, trace, warn};

/// Максимум повторных submit попыток после adapter-level `CheckEvents`.
///
/// `cros-codecs` использует `CheckEvents` как backpressure-сигнал:
/// вызывающий код должен обработать pending events и повторить тот же bitstream.
/// Лимит защищает decoder thread от бесконечного цикла при поломанном backend state.
pub(super) const MAX_CHECK_EVENTS_RETRIES: usize = 4;
/// Начальное разрешение для создания пула кадров.
///
/// VA-API декодер требует выходные буферы до первого decode call.
/// Используем 1920x1080 как разумный default — при смене разрешения
/// пул будет пересоздан через `FormatChanged` event.
pub(super) const INITIAL_WIDTH: u32 = 1920;
pub(super) const INITIAL_HEIGHT: u32 = 1080;

/// Итог обработки pending decoder events.
///
/// Отдельный report нужен, чтобы retry-loop мог видеть, был ли `FormatChanged`,
/// и писать диагностический лог без знания деталей `FrameReady` import path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecoderDrainReport {
    /// Количество событий, прочитанных через `next_event()`.
    pub(super) events_count: usize,

    /// Был ли среди событий `FormatChanged`.
    pub(super) format_changed: bool,
}

/// Политика обработки `FrameReady` events во время drain-а decoder-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecoderEventDrainPolicy {
    /// Обычный playback/EOF путь: кадры экспортируются и получают seek generation.
    Publish {
        /// Seek generation, к которому принадлежат все кадры этого drain-а.
        generation: u64,
    },

    /// Seek flush/reconfigure cleanup: кадры из backend tail не публикуются.
    Discard {
        /// Короткая причина для diagnostics/logging.
        reason: &'static str,
    },
}

/// Сводка одного вызова decode state machine.
///
/// `decode()` использует её только для логов и решения, был ли packet пропущен
/// как recoverable parse error. Все готовые кадры по-прежнему лежат в `ready_queue`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecodeLoopReport {
    /// Сколько раз packet был отправлен в active codec adapter.
    pub(super) attempts: usize,

    /// Количество обработанных decoder events за весь вызов.
    pub(super) events_count: usize,

    /// Был ли обработан `FormatChanged`.
    pub(super) format_changed: bool,

    /// Сколько байт backend сообщил как обработанные.
    pub(super) processed_bytes: usize,

    /// Был ли packet пропущен из-за recoverable parse error.
    pub(super) skipped_packet: bool,

    /// Был ли packet оставлен на retry из-за временной нехватки output buffers.
    pub(super) output_backpressured: bool,

    /// Суммарное время внутри submit attempts.
    pub(super) submit_elapsed: Duration,

    /// Суммарное время внутри drain events.
    pub(super) drain_elapsed: Duration,
}

impl DecodeLoopReport {
    /// Добавляет результат одного drain прохода к общей сводке.
    pub(super) fn record_drain(
        &mut self,
        drain_report: DecoderDrainReport,
        drain_elapsed: Duration,
    ) {
        self.events_count += drain_report.events_count;
        self.format_changed |= drain_report.format_changed;
        self.drain_elapsed += drain_elapsed;
    }
}

/// Минимальный интерфейс, который нужен retry state machine.
///
/// Production implementation живёт на `VaapiVideoDecoder`, а unit test подставляет
/// fake driver без VA-API, чтобы проверить контракт `CheckEvents -> retry same packet`.
pub(super) trait DecoderRetryDriver {
    /// Короткое имя codec-а для diagnostics retry-loop-а.
    fn codec_label(&self) -> &'static str;

    /// Отправляет один packet в backend decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError>;

    /// Обрабатывает все pending decoder events.
    fn drain_events(&mut self, policy: DecoderEventDrainPolicy) -> Result<DecoderDrainReport>;
}

/// Выполняет submit packet с bounded retry после `CheckEvents`.
///
/// Важно: `CheckEvents` не означает, что packet consumed. По контракту `cros-codecs`
/// вызывающий код обязан обработать события и повторить `decode()` с теми же данными.
pub(super) fn run_decode_with_event_retry<D>(
    driver: &mut D,
    timestamp_us: u64,
    packet_data: &[u8],
    keyframe: bool,
    generation: u64,
) -> Result<DecodeLoopReport>
where
    D: DecoderRetryDriver + ?Sized,
{
    let pts_ms = timestamp_us / 1000;
    let mut report = DecodeLoopReport::default();
    let codec_label = driver.codec_label();
    let decode_hints = VaapiPacketDecodeHints {
        inject_parameter_sets: keyframe,
    };

    loop {
        report.attempts += 1;
        let attempt = report.attempts;
        let submit_start = std::time::Instant::now();
        let submit_result = driver.submit_packet(timestamp_us, packet_data, decode_hints);
        report.submit_elapsed += submit_start.elapsed();

        match submit_result {
            Ok(processed_bytes) => {
                report.processed_bytes = processed_bytes;
                trace!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    processed_bytes = processed_bytes,
                    "decode() accepted bitstream"
                );
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                report.record_drain(drain_report, drain_start.elapsed());
                return Ok(report);
            }
            Err(VaapiAdapterDecodeError::CheckEvents) => {
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                let format_changed = drain_report.format_changed;
                report.record_drain(drain_report, drain_start.elapsed());

                if attempt > MAX_CHECK_EVENTS_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Decoder repeatedly requested event drain after {attempt} attempts"
                    ));
                }

                debug!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    format_changed = format_changed,
                    codec = codec_label,
                    "retrying same packet after decoder event drain"
                );
            }
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(needed)) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    needed = needed,
                    "Decoder out of output buffers"
                );
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                report.record_drain(drain_report, drain_start.elapsed());
                report.output_backpressured = true;
                return Ok(report);
            }
            Err(VaapiAdapterDecodeError::ParseFrameError(message)) => {
                report.skipped_packet = true;
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    codec = codec_label,
                    %message,
                    "parse error, skipping packet"
                );
                return Ok(report);
            }
            Err(error) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    error = ?error,
                    codec = codec_label,
                    "Decode error"
                );
                return Err(anyhow::Error::from(error));
            }
        }
    }
}

impl super::VaapiVideoDecoder {
    /// Обрабатывает готовый кадр от decoder: sync, DMA-BUF export и resource registration.
    ///
    /// # Аргументы
    /// * `handle` — handle декодированного кадра от cros-codecs.
    /// * `generation` — seek generation, которому принадлежит event drain.
    ///
    /// # Ошибки
    /// Возвращает ошибку если sync, DMA-BUF export или registration не удался.
    pub(super) fn process_ready_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        generation: u64,
    ) -> Result<()> {
        // Получаем разрешения кадра ДО sync (sync может потребовать mutable borrow).
        let resolution = handle.coded_resolution();
        let display_resolution = handle.display_resolution();
        let timestamp = handle.timestamp();

        debug!(
            pts_ms = timestamp / 1000,
            coded_width = resolution.width,
            coded_height = resolution.height,
            display_width = display_resolution.width,
            display_height = display_resolution.height,
            "FrameReady: processing decoded frame"
        );

        // Шаг 1: Синхронизируемся с завершением GPU-декодирования.
        // `sync()` блокируется до тех пор, пока VA-API не закончит decode job.
        let sync_start = std::time::Instant::now();
        if let Err(e) = handle.sync() {
            warn!(error = %e, "GPU decode sync failed — dropping frame");
            return Err(anyhow::anyhow!("GPU decode sync failed: {}", e));
        }
        let hardware_sync_latency = sync_start.elapsed();
        let decoded_contract = self.current_decoded_contract(&handle)?;

        // Шаг 2: Экспортируем VA surface как DMA-BUF. Отсутствие export-а — fatal contract error.
        let export_start = std::time::Instant::now();
        let preferred_export_layout =
            dma_buf_export_layout_from_frame_contract(self.expected_frame_contract)?;
        let dma_buf_image = match handle.dma_buf_image_with_layout(preferred_export_layout) {
            Ok(Some(dma_buf_image)) => dma_buf_image,
            Ok(None) => {
                return Err(zero_copy_contract_violation(format!(
                    "{} decoded handle does not expose DMA-BUF export",
                    decoded_contract.format
                )));
            }
            Err(export_error) => {
                let export_error_chain = format!("{:#}", export_error);
                warn!(
                    error = %export_error_chain,
                    format = %decoded_contract.format,
                    "VA surface DMA-BUF export failed; CPU fallback is disabled"
                );
                return Err(zero_copy_contract_violation(format!(
                    "{} VA surface DMA-BUF export failed: {}",
                    decoded_contract.format, export_error_chain
                )));
            }
        };
        let frame_contract = frame_contract_for_dma_buf_export(
            decoded_contract.format,
            dma_buf_image.export_layout,
        )?;
        if frame_contract != self.expected_frame_contract {
            return Err(zero_copy_contract_violation(format!(
                "VA surface DMA-BUF export produced {}, but selected stream contract requires {}",
                frame_contract.diagnostic_label(),
                self.expected_frame_contract.diagnostic_label()
            )));
        }
        let dma_buf_export_latency = export_start.elapsed();

        // Шаг 3: Регистрируем descriptor; renderer сам выполнит graphics import.
        let (resource_handle, resource_pool_diagnostics) = {
            let mut resource_pool =
                lock_resource_pool(&self.resource_pool, "DMA-BUF registration")?;
            let resource_handle = match resource_pool.register_dma_buf_image(dma_buf_image) {
                Ok(resource_handle) => resource_handle,
                Err(registration_error) => {
                    let resource_stats = resource_pool.stats();
                    let registration_error_chain = format!("{:#}", registration_error);
                    warn!(
                        error = %registration_error_chain,
                        format = %decoded_contract.format,
                            zero_copy_capacity = resource_stats.capacity,
                            zero_copy_slots = resource_stats.slots,
                            zero_copy_in_use = resource_stats.in_use,
                            zero_copy_free_surfaces = resource_stats.free_surfaces,
                            zero_copy_waiting_gpu_completion =
                                resource_stats.waiting_gpu_completion,
                            zero_copy_waiting_decoder_reuse =
                                resource_stats.waiting_decoder_reuse,
                            zero_copy_import_failures = resource_stats.import_failures,
                            zero_copy_imports_created = resource_stats.imports_created,
                            zero_copy_imports_reused = resource_stats.imports_reused,
                            zero_copy_imports_replaced = resource_stats.imports_replaced,
                        "DMA-BUF zero-copy resource registration failed; CPU fallback is disabled"
                    );
                    return Err(zero_copy_contract_violation(format!(
                        "{} DMA-BUF zero-copy resource registration failed: {}",
                        decoded_contract.format, registration_error_chain
                    )));
                }
            };
            let resource_stats = resource_pool.stats();
            (
                resource_handle,
                VideoResourcePoolDiagnostics {
                    capacity: resource_stats.capacity,
                    slots: resource_stats.slots,
                    in_use: resource_stats.in_use,
                    free_surfaces: resource_stats.free_surfaces,
                    waiting_gpu_completion: resource_stats.waiting_gpu_completion,
                    waiting_decoder_reuse: resource_stats.waiting_decoder_reuse,
                    import_failures: resource_stats.import_failures,
                    imports_created: resource_stats.imports_created,
                    imports_reused: resource_stats.imports_reused,
                    imports_replaced: resource_stats.imports_replaced,
                },
            )
        };

        if !self.zero_copy_success_logged {
            self.zero_copy_success_logged = true;
            info!(
                handle_id = resource_handle.0,
                format = %decoded_contract.format,
                sync_ms = hardware_sync_latency.as_millis(),
                "Zero-copy DMA-BUF resource registered"
            );
        }
        if decoded_contract.format == DecodedPixelFormat::P010
            && !self.p010_boundary_verified_logged
        {
            self.p010_boundary_verified_logged = true;
            info!(
                handle_id = resource_handle.0,
                width = resolution.width,
                height = resolution.height,
                bit_depth = %decoded_contract.bit_depth,
                chroma = %decoded_contract.chroma,
                "P010 zero-copy boundary verified"
            );
        }

        // Шаг 4: Удерживаем VA handle, пока renderer не подтвердит release после GPU work.
        self.zero_copy_guards.insert(resource_handle.0, handle);

        // Шаг 5: Публикуем только zero-copy frame metadata.
        self.push_ready_frame(DecodedFrame {
            generation,
            pts: Duration::from_micros(timestamp),
            frame_contract,
            width: resolution.width,
            height: resolution.height,
            render_width: display_resolution.width,
            render_height: display_resolution.height,
            display_orientation: self.display_orientation,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: VideoFrameDiagnostics {
                timings: VideoFrameTimingDiagnostics {
                    hardware_sync_latency: Some(hardware_sync_latency),
                    dma_buf_export_latency: Some(dma_buf_export_latency),
                    ..VideoFrameTimingDiagnostics::default()
                },
                decoder_ready_queue_depth: None,
                resource_pool: Some(resource_pool_diagnostics),
            },
        })?;

        Ok(())
    }

    /// Выбрасывает готовый backend frame без DMA-BUF export-а и publish-а.
    ///
    /// Seek flush использует этот путь для H.264 DPB tail, который `cros-codecs`
    /// делает видимым через `FrameReady` после `flush()`. Это редкий lifecycle
    /// path: handle синхронизируется и возвращает backing frame сразу, не через
    /// обычную suppressed reclaim queue.
    fn discard_ready_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        reason: &'static str,
    ) -> Result<()> {
        let pts_ms = handle.timestamp() / 1000;
        debug!(
            pts_ms,
            reason, "Discarding decoder-ready frame without DMA-BUF export"
        );
        sync_discard_ready_frame(&mut self.frame_pool, handle, reason)?;
        Ok(())
    }

    /// Обрабатывает `FormatChanged` одинаково для publish и discard drain-а.
    ///
    /// Этот event меняет lifecycle decoder-owned resources, поэтому его нельзя
    /// игнорировать даже при seek flush cleanup.
    fn handle_format_changed_event(&mut self) -> Result<()> {
        info!("Format changed, invalidating resource pool and frame pool");

        // Retained candidate мог относиться к старому surface/format contract.
        self.force_drain_preroll_candidate_and_suppressed_surfaces("format_changed")?;

        // Сначала освобождаем decoder-owned кадры из `ready_queue`.
        // Иначе `invalidate_all()` удалит mappings, а VA handles,
        // удерживаемые этими кадрами, останутся без release path.
        self.release_decoder_owned_ready_frames("format_changed")?;
        self.invalidate_idle_resource_pool_after_format_change()?;

        // Пересоздаём frame pool под новое разрешение/формат.
        // `stream_info()` уже обновлён внутри cros-codecs перед event-ом.
        if let Some(stream_info) = self.adapter.stream_info() {
            let res = stream_info.coded_resolution;
            let rt_format = match rt_format_for_decoded_format(stream_info.format) {
                Ok(rt_format) => rt_format,
                Err(error) => {
                    warn!(
                        error = %error,
                        decoded_format = ?stream_info.format,
                        "Cannot map decoded format to VA RT format"
                    );
                    return Ok(());
                }
            };
            if let Err(error) = self.frame_pool.resize_with_rt_format(
                res.width,
                res.height,
                self.runtime_config.surface_pool_frames,
                rt_format,
            ) {
                warn!(
                    error = %error,
                    width = res.width,
                    height = res.height,
                    rt_format,
                    "Failed to resize frame pool after format change"
                );
            } else {
                info!(
                    width = res.width,
                    height = res.height,
                    decoded_format = ?stream_info.format,
                    rt_format,
                    "Frame pool resized for new format"
                );
            }
        } else {
            warn!("FormatChanged event without stream_info — cannot resize frame pool");
        }

        Ok(())
    }

    /// Обрабатывает все pending events из `cros-codecs`.
    ///
    /// `FrameReady` либо превращается в `DecodedFrame`, либо отбрасывается
    /// по явно выбранной policy.
    /// `FormatChanged` инвалидирует старые resource descriptors и пересоздаёт frame pool
    /// под новое coded resolution/decoded format.
    pub(super) fn drain_decoder_events(
        &mut self,
        policy: DecoderEventDrainPolicy,
    ) -> Result<DecoderDrainReport> {
        let mut report = DecoderDrainReport::default();
        self.reclaim_ready_suppressed_surfaces()?;

        while let Some(event) = self.adapter.next_event() {
            report.events_count += 1;
            match event {
                VaapiDecoderEvent::FrameReady(handle) => {
                    let pts_ms = handle.timestamp() / 1000;
                    trace!(pts_ms = pts_ms, "DecoderEvent::FrameReady");
                    match policy {
                        DecoderEventDrainPolicy::Publish { generation } => {
                            let pts = Duration::from_micros(handle.timestamp());
                            if let Some(floor) = self.should_suppress_ready_frame(pts, generation) {
                                self.suppress_ready_frame(handle, generation, floor)?;
                                continue;
                            }

                            if self.should_drop_preroll_candidate_before_publish(pts, generation) {
                                self.drop_preroll_fallback_candidate(
                                    "target_or_after_ready_frame",
                                )?;
                            }
                            if let Err(error) = self.process_ready_frame(handle, generation) {
                                if is_fatal_decoder_error(&error) {
                                    return Err(error);
                                }
                                warn!(error = %error, "Failed to process ready frame");
                            } else {
                                self.record_target_or_after_frame_published(pts, generation)?;
                            }
                        }
                        DecoderEventDrainPolicy::Discard { reason } => {
                            self.discard_ready_frame(handle, reason)?;
                        }
                    }
                }
                VaapiDecoderEvent::FormatChanged => {
                    report.format_changed = true;
                    self.handle_format_changed_event()?;
                }
            }
        }

        self.reclaim_ready_suppressed_surfaces()?;
        Ok(report)
    }
}
