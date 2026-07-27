use super::*;
/// H.264 stream metadata, доказанная на configure boundary.
pub(super) struct H264VaapiStreamConfig {
    /// Packetization player/demuxer уже подтвердили через `codec-core`.
    packetization: H264Packetization,

    /// SPS NAL units из codec-private без Annex B start code.
    sequence_parameter_sets: Vec<Vec<u8>>,

    /// PPS NAL units из codec-private без Annex B start code.
    picture_parameter_sets: Vec<Vec<u8>>,
}

impl H264VaapiStreamConfig {
    /// Строит backend-local config только из уже принятого neutral stream config-а.
    pub(super) fn from_decode_config(
        config: &VideoStreamDecodeConfig,
    ) -> std::result::Result<Self, VideoStreamConfigRejection> {
        let packetization = match config.packetization {
            Some(VideoStreamPacketization::H264(packetization)) => packetization,
            _ => {
                return Err(VideoStreamConfigRejection::MissingPacketization {
                    codec: VideoCodec::H264,
                });
            }
        };

        let codec_private = config
            .codec_private
            .as_deref()
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| VideoStreamConfigRejection::InvalidCodecPrivate {
                codec: VideoCodec::H264,
                reason: "H.264 VA-API adapter requires avcC codec_private with SPS/PPS".to_string(),
            })?;

        let decoder_config =
            parse_avc_decoder_configuration_record(codec_private).map_err(|error| {
                VideoStreamConfigRejection::InvalidCodecPrivate {
                    codec: VideoCodec::H264,
                    reason: error.to_string(),
                }
            })?;

        if let H264Packetization::AvccLengthPrefixed { nal_length_size } = packetization
            && decoder_config.nal_length_size != nal_length_size
        {
            return Err(VideoStreamConfigRejection::InvalidCodecPrivate {
                codec: VideoCodec::H264,
                reason: format!(
                    "H.264 packetization length size {nal_length_size:?} does not match avcC {:?}",
                    decoder_config.nal_length_size
                ),
            });
        }

        Ok(Self {
            packetization,
            sequence_parameter_sets: decoder_config.sequence_parameter_sets().to_vec(),
            picture_parameter_sets: decoder_config.picture_parameter_sets().to_vec(),
        })
    }

    /// Конвертирует один access unit в caller-owned Annex B buffer по явному intent.
    fn access_unit_to_annex_b_into(
        &self,
        packet_data: &[u8],
        inject_parameter_sets: bool,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        let parameter_set_injection = if inject_parameter_sets {
            H264ParameterSetInjection::BeforeAccessUnit {
                sequence_parameter_sets: &self.sequence_parameter_sets,
                picture_parameter_sets: &self.picture_parameter_sets,
            }
        } else {
            H264ParameterSetInjection::None
        };

        h264_access_unit_to_annex_b_into(
            packet_data,
            self.packetization,
            parameter_set_injection,
            output,
        )
        .map_err(|error| VaapiAdapterDecodeError::ParseFrameError(error.to_string()))
    }
}

/// Готовит H.264 AU к submit-у и владеет reusable Annex B scratch buffer.
pub(super) struct H264AccessUnitPreparer {
    /// Configure-time packetization и SPS/PPS state.
    stream_config: H264VaapiStreamConfig,

    /// Reusable output buffer для AVCC/Annex B перепаковки.
    pub(super) annex_b_scratch: Vec<u8>,

    /// Lifecycle flag: первый AU после configure/flush должен получить SPS/PPS.
    inject_parameter_sets_on_next_au: bool,
}

impl H264AccessUnitPreparer {
    /// Создаёт preparer после configure; первый AU остаётся decode-safe.
    pub(super) fn new(stream_config: H264VaapiStreamConfig) -> Self {
        Self {
            stream_config,
            annex_b_scratch: Vec::new(),
            inject_parameter_sets_on_next_au: true,
        }
    }

    /// Собирает pending AU, перемещая reusable scratch в ownership pending state-а.
    pub(super) fn prepare_pending_access_unit(
        &mut self,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<H264PendingAccessUnit, VaapiAdapterDecodeError> {
        let inject_parameter_sets =
            decode_hints.inject_parameter_sets || self.inject_parameter_sets_on_next_au;

        self.stream_config.access_unit_to_annex_b_into(
            packet_data,
            inject_parameter_sets,
            &mut self.annex_b_scratch,
        )?;
        self.inject_parameter_sets_on_next_au = false;

        let annex_b_bytes = std::mem::take(&mut self.annex_b_scratch);
        Ok(H264PendingAccessUnit::new(annex_b_bytes, packet_data.len()))
    }

    /// Возвращает полностью consumed AU buffer в scratch и сохраняет его capacity.
    pub(super) fn recycle_completed_access_unit(
        &mut self,
        pending_access_unit: H264PendingAccessUnit,
    ) {
        debug_assert_eq!(
            pending_access_unit.consumed_bytes,
            pending_access_unit.annex_b_bytes.len(),
            "H.264 AU buffer can return to scratch only after full consume"
        );
        self.annex_b_scratch = pending_access_unit.into_reusable_annex_b_bytes();
        self.annex_b_scratch.clear();
    }

    /// Сбрасывает lifecycle policy после seek flush/reconfigure cleanup.
    pub(super) fn reset_after_flush(&mut self) {
        self.annex_b_scratch.clear();
        self.inject_parameter_sets_on_next_au = true;
    }
}

/// Annex B access unit, который может быть partially consumed cros H.264 decoder-ом.
pub(super) struct H264PendingAccessUnit {
    /// Полный Annex B payload с injected parameter sets.
    pub(super) annex_b_bytes: Vec<u8>,

    /// Размер исходного packet-а на external adapter boundary.
    pub(super) source_packet_len: usize,

    /// Сколько Annex B bytes уже принято cros decoder-ом.
    pub(super) consumed_bytes: usize,
}

impl H264PendingAccessUnit {
    /// Создаёт pending AU до первого NAL submit-а.
    pub(super) fn new(annex_b_bytes: Vec<u8>, source_packet_len: usize) -> Self {
        Self {
            annex_b_bytes,
            source_packet_len,
            consumed_bytes: 0,
        }
    }

    /// Возвращает owned bytes adapter-у после полного consume.
    fn into_reusable_annex_b_bytes(self) -> Vec<u8> {
        self.annex_b_bytes
    }

    /// Кормит cros decoder NAL-ами до полного AU или первого backpressure/error.
    pub(super) fn feed_until_blocked(
        &mut self,
        mut decode_next_nal: impl FnMut(&[u8]) -> std::result::Result<usize, VaapiAdapterDecodeError>,
    ) -> std::result::Result<Option<usize>, VaapiAdapterDecodeError> {
        while self.consumed_bytes < self.annex_b_bytes.len() {
            let remaining_bytes = &self.annex_b_bytes[self.consumed_bytes..];
            let consumed_now = decode_next_nal(remaining_bytes)?;

            if consumed_now == 0 {
                return Err(VaapiAdapterDecodeError::Decoder(
                    "H.264 decoder accepted a NAL but reported 0 consumed bytes".to_string(),
                ));
            }
            if consumed_now > remaining_bytes.len() {
                return Err(VaapiAdapterDecodeError::Decoder(format!(
                    "H.264 decoder reported {consumed_now} consumed bytes for {} available bytes",
                    remaining_bytes.len()
                )));
            }

            self.consumed_bytes += consumed_now;
        }

        Ok(Some(self.source_packet_len))
    }
}

/// Production H.264 adapter поверх cros-codecs VAAPI decoder-а.
pub(super) struct H264VaapiCodecAdapter {
    /// Concrete cros decoder остаётся private implementation detail adapter-а.
    inner: StatelessDecoder<H264, VaapiBackend<InternalVaapiFrame>>,

    /// Готовит AU, выбирает SPS/PPS injection policy и переиспользует scratch.
    access_unit_preparer: H264AccessUnitPreparer,

    /// Unconsumed AU после `CheckEvents` или output-buffer backpressure.
    pending_access_unit: Option<H264PendingAccessUnit>,
}

impl H264VaapiCodecAdapter {
    /// Создаёт H.264 decoder только для уже валидированного stream config-а.
    pub(super) fn new(display: Rc<Display>, config: &VideoStreamDecodeConfig) -> Result<Self> {
        let stream_config = H264VaapiStreamConfig::from_decode_config(config)
            .map_err(|rejection| anyhow::anyhow!("Invalid H.264 VA-API config: {rejection}"))?;
        let inner = StatelessDecoder::<H264, VaapiBackend<InternalVaapiFrame>>::new_vaapi(
            display,
            BlockingMode::Blocking,
        )
        .map_err(|error| anyhow::anyhow!("Failed to create VA-API H.264 decoder: {error:?}"))?;

        Ok(Self {
            inner,
            access_unit_preparer: H264AccessUnitPreparer::new(stream_config),
            pending_access_unit: None,
        })
    }
}

impl VaapiCodecAdapter for H264VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    /// Возвращает backend label для H.264 diagnostics.
    fn backend_name(&self) -> &'static str {
        "VA-API H.264"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "H.264"
    }

    /// Конвертирует AU в Annex B и отправляет все NAL units в cros decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        if self.pending_access_unit.is_none() {
            self.pending_access_unit = Some(
                self.access_unit_preparer
                    .prepare_pending_access_unit(packet_data, decode_hints)?,
            );
        }

        let Some(pending_access_unit) = self.pending_access_unit.as_mut() else {
            return Err(VaapiAdapterDecodeError::Backend(
                "H.264 pending access unit missing after preparation".to_string(),
            ));
        };
        let feed_result = pending_access_unit.feed_until_blocked(|remaining_bytes| {
            let mut alloc_cb = || {
                let frame = frame_pool.alloc_or_allocate();
                if frame.is_none() {
                    tracing::warn!("Frame pool exhausted; H.264 decoder needs output buffers");
                }
                frame
            };

            self.inner
                .decode(timestamp_us, remaining_bytes, &mut alloc_cb)
                .map_err(VaapiAdapterDecodeError::from)
        });

        match feed_result {
            Ok(Some(source_packet_len)) => {
                let Some(completed_access_unit) = self.pending_access_unit.take() else {
                    return Err(VaapiAdapterDecodeError::Backend(
                        "H.264 completed access unit missing from adapter state".to_string(),
                    ));
                };
                self.access_unit_preparer
                    .recycle_completed_access_unit(completed_access_unit);
                Ok(source_packet_len)
            }
            Ok(None) => unreachable!("H.264 feed loop always completes or returns an error"),
            Err(error) => {
                settle_pending_access_unit_after_submit_error(
                    &mut self.pending_access_unit,
                    &error,
                );
                Err(error)
            }
        }
    }

    /// Flush-ит H.264 decoder state и забывает partially consumed AU.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.pending_access_unit = None;
        self.access_unit_preparer.reset_after_flush();
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// Дожимает H.264 DPB tail; cros-codecs отдаёт tail frames через events.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        if self.pending_access_unit.is_some() {
            return Err(VaapiAdapterDecodeError::Decoder(
                "cannot drain H.264 while an access unit is partially submitted".to_string(),
            ));
        }

        self.access_unit_preparer.reset_after_flush();
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// Возвращает следующий cros event в локальном wrapper-е.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
        self.inner.next_event().map(VaapiDecoderEvent::from)
    }

    /// Возвращает stream info без раскрытия cros type-а наружу module-а.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
        self.inner.stream_info().map(VaapiAdapterStreamInfo::from)
    }
}

/// Сохраняет partially consumed AU только когда cros-codecs явно требует retry тех же bytes.
pub(super) fn settle_pending_access_unit_after_submit_error(
    pending_access_unit: &mut Option<H264PendingAccessUnit>,
    error: &VaapiAdapterDecodeError,
) {
    let same_access_unit_retry = matches!(
        error,
        VaapiAdapterDecodeError::CheckEvents | VaapiAdapterDecodeError::NotEnoughOutputBuffers(_)
    );
    if !same_access_unit_retry {
        *pending_access_unit = None;
    }
}
