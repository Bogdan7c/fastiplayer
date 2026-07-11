use super::*;

/// HEVC NAL unit type для video parameter set.
pub(super) const H265_NAL_UNIT_TYPE_VPS: u8 = 32;
/// HEVC NAL unit type для sequence parameter set.
pub(super) const H265_NAL_UNIT_TYPE_SPS: u8 = 33;
/// HEVC NAL unit type для picture parameter set.
pub(super) const H265_NAL_UNIT_TYPE_PPS: u8 = 34;
/// H.265 stream metadata, доказанная на configure boundary и дополненная in-band.
pub(super) struct H265VaapiStreamConfig {
    /// Packetization player/demuxer уже подтвердили через `codec-core`.
    packetization: H265Packetization,

    /// VPS NAL units из hvcC или in-band packets без Annex B start code.
    pub(super) video_parameter_sets: Vec<Vec<u8>>,

    /// SPS NAL units из hvcC или in-band packets без Annex B start code.
    pub(super) sequence_parameter_sets: Vec<Vec<u8>>,

    /// PPS NAL units из hvcC или in-band packets без Annex B start code.
    pub(super) picture_parameter_sets: Vec<Vec<u8>>,
}

impl H265VaapiStreamConfig {
    /// Строит backend-local config без требования canonical полного `hvcC`.
    pub(super) fn from_decode_config(
        config: &VideoStreamDecodeConfig,
    ) -> std::result::Result<Self, VideoStreamConfigRejection> {
        let packetization = match config.packetization {
            Some(VideoStreamPacketization::H265(packetization)) => packetization,
            _ => {
                return Err(VideoStreamConfigRejection::MissingPacketization {
                    codec: VideoCodec::H265,
                });
            }
        };

        let mut stream_config = Self {
            packetization,
            video_parameter_sets: Vec::new(),
            sequence_parameter_sets: Vec::new(),
            picture_parameter_sets: Vec::new(),
        };

        let Some(codec_private) = config
            .codec_private
            .as_deref()
            .filter(|bytes| !bytes.is_empty())
        else {
            return Ok(stream_config);
        };

        let decoder_config =
            parse_hevc_decoder_configuration_record(codec_private).map_err(|error| {
                VideoStreamConfigRejection::InvalidCodecPrivate {
                    codec: VideoCodec::H265,
                    reason: error.to_string(),
                }
            })?;

        if let H265Packetization::HvccLengthPrefixed { nal_length_size } = packetization
            && decoder_config.nal_length_size != nal_length_size
        {
            return Err(VideoStreamConfigRejection::InvalidCodecPrivate {
                codec: VideoCodec::H265,
                reason: format!(
                    "H.265 packetization length size {nal_length_size:?} does not match hvcC {:?}",
                    decoder_config.nal_length_size
                ),
            });
        }

        stream_config
            .video_parameter_sets
            .extend_from_slice(decoder_config.video_parameter_sets());
        stream_config
            .sequence_parameter_sets
            .extend_from_slice(decoder_config.sequence_parameter_sets());
        stream_config
            .picture_parameter_sets
            .extend_from_slice(decoder_config.picture_parameter_sets());

        Ok(stream_config)
    }

    /// Конвертирует AU в Annex B и после успеха запоминает in-band VPS/SPS/PPS.
    fn access_unit_to_annex_b_into(
        &mut self,
        packet_data: &[u8],
        inject_parameter_sets: bool,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        let nal_units = h265_nal_units(packet_data, self.packetization)
            .map_err(|error| VaapiAdapterDecodeError::ParseFrameError(error.to_string()))?;
        let discovered_parameter_sets = self.discover_new_parameter_sets(&nal_units);
        let parameter_set_injection = if inject_parameter_sets {
            H265ParameterSetInjection::BeforeAccessUnit {
                video_parameter_sets: &self.video_parameter_sets,
                sequence_parameter_sets: &self.sequence_parameter_sets,
                picture_parameter_sets: &self.picture_parameter_sets,
            }
        } else {
            H265ParameterSetInjection::None
        };

        h265_access_unit_to_annex_b_into(
            packet_data,
            self.packetization,
            parameter_set_injection,
            output,
        )
        .map_err(|error| VaapiAdapterDecodeError::ParseFrameError(error.to_string()))?;

        self.commit_discovered_parameter_sets(discovered_parameter_sets);
        Ok(())
    }

    /// Собирает новые in-band parameter sets без мутации stream state до успешной конверсии.
    fn discover_new_parameter_sets(&self, nal_units: &[H265NalUnit<'_>]) -> H265ParameterSetUpdate {
        let mut update = H265ParameterSetUpdate::default();
        for nal_unit in nal_units {
            match nal_unit.nal_unit_type() {
                H265_NAL_UNIT_TYPE_VPS => collect_new_parameter_set(
                    &self.video_parameter_sets,
                    &mut update.video_parameter_sets,
                    nal_unit.bytes(),
                ),
                H265_NAL_UNIT_TYPE_SPS => collect_new_parameter_set(
                    &self.sequence_parameter_sets,
                    &mut update.sequence_parameter_sets,
                    nal_unit.bytes(),
                ),
                H265_NAL_UNIT_TYPE_PPS => collect_new_parameter_set(
                    &self.picture_parameter_sets,
                    &mut update.picture_parameter_sets,
                    nal_unit.bytes(),
                ),
                _ => {}
            }
        }
        update
    }

    /// Добавляет только те parameter sets, которые были подтверждены текущим AU.
    fn commit_discovered_parameter_sets(&mut self, update: H265ParameterSetUpdate) {
        self.video_parameter_sets
            .extend(update.video_parameter_sets);
        self.sequence_parameter_sets
            .extend(update.sequence_parameter_sets);
        self.picture_parameter_sets
            .extend(update.picture_parameter_sets);
    }
}

/// Пакет новых HEVC parameter sets, найденных в одном access unit-е.
#[derive(Default)]
struct H265ParameterSetUpdate {
    /// Новые VPS NAL units.
    video_parameter_sets: Vec<Vec<u8>>,

    /// Новые SPS NAL units.
    sequence_parameter_sets: Vec<Vec<u8>>,

    /// Новые PPS NAL units.
    picture_parameter_sets: Vec<Vec<u8>>,
}

/// Копирует NAL в update только если такой parameter set ещё не известен.
fn collect_new_parameter_set(
    known_parameter_sets: &[Vec<u8>],
    discovered_parameter_sets: &mut Vec<Vec<u8>>,
    nal_unit_bytes: &[u8],
) {
    let already_known = known_parameter_sets
        .iter()
        .any(|known_bytes| known_bytes.as_slice() == nal_unit_bytes);
    let already_discovered = discovered_parameter_sets
        .iter()
        .any(|known_bytes| known_bytes.as_slice() == nal_unit_bytes);

    if !already_known && !already_discovered {
        discovered_parameter_sets.push(nal_unit_bytes.to_vec());
    }
}

/// Готовит H.265 AU к submit-у и владеет reusable Annex B scratch buffer.
pub(super) struct H265AccessUnitPreparer {
    /// Configure-time packetization плюс hvcC/in-band VPS/SPS/PPS state.
    pub(super) stream_config: H265VaapiStreamConfig,

    /// Reusable output buffer для HVCC/Annex B перепаковки.
    pub(super) annex_b_scratch: Vec<u8>,

    /// Lifecycle flag: первый AU после configure/flush должен получить VPS/SPS/PPS.
    inject_parameter_sets_on_next_au: bool,
}

impl H265AccessUnitPreparer {
    /// Создаёт preparer после configure; первый AU остаётся decode-safe.
    pub(super) fn new(stream_config: H265VaapiStreamConfig) -> Self {
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
    ) -> std::result::Result<H265PendingAccessUnit, VaapiAdapterDecodeError> {
        let inject_parameter_sets =
            decode_hints.inject_parameter_sets || self.inject_parameter_sets_on_next_au;

        self.stream_config.access_unit_to_annex_b_into(
            packet_data,
            inject_parameter_sets,
            &mut self.annex_b_scratch,
        )?;
        self.inject_parameter_sets_on_next_au = false;

        let annex_b_bytes = std::mem::take(&mut self.annex_b_scratch);
        Ok(H265PendingAccessUnit::new(annex_b_bytes, packet_data.len()))
    }

    /// Возвращает полностью consumed AU buffer в scratch и сохраняет его capacity.
    pub(super) fn recycle_completed_access_unit(
        &mut self,
        pending_access_unit: H265PendingAccessUnit,
    ) {
        debug_assert_eq!(
            pending_access_unit.consumed_bytes,
            pending_access_unit.annex_b_bytes.len(),
            "H.265 AU buffer can return to scratch only after full consume"
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

/// Annex B access unit, который может быть partially consumed cros H.265 decoder-ом.
pub(super) struct H265PendingAccessUnit {
    /// Полный Annex B payload с injected parameter sets.
    pub(super) annex_b_bytes: Vec<u8>,

    /// Размер исходного packet-а на external adapter boundary.
    pub(super) source_packet_len: usize,

    /// Сколько Annex B bytes уже принято cros decoder-ом.
    pub(super) consumed_bytes: usize,
}

impl H265PendingAccessUnit {
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
                    "H.265 decoder accepted a NAL but reported 0 consumed bytes".to_string(),
                ));
            }
            if consumed_now > remaining_bytes.len() {
                return Err(VaapiAdapterDecodeError::Decoder(format!(
                    "H.265 decoder reported {consumed_now} consumed bytes for {} available bytes",
                    remaining_bytes.len()
                )));
            }

            self.consumed_bytes += consumed_now;
        }

        Ok(Some(self.source_packet_len))
    }
}

/// Production-shaped H.265 adapter поверх cros-codecs VAAPI decoder-а.
pub(super) struct H265VaapiCodecAdapter {
    /// Concrete cros decoder остаётся private implementation detail adapter-а.
    inner: StatelessDecoder<H265, VaapiBackend<InternalVaapiFrame>>,

    /// Готовит AU, выбирает VPS/SPS/PPS injection policy и переиспользует scratch.
    access_unit_preparer: H265AccessUnitPreparer,

    /// Unconsumed AU после `CheckEvents` или output-buffer backpressure.
    pending_access_unit: Option<H265PendingAccessUnit>,
}

impl H265VaapiCodecAdapter {
    /// Создаёт H.265 decoder только для уже валидированного stream config-а.
    pub(super) fn new(display: Rc<Display>, config: &VideoStreamDecodeConfig) -> Result<Self> {
        let stream_config = H265VaapiStreamConfig::from_decode_config(config)
            .map_err(|rejection| anyhow::anyhow!("Invalid H.265 VA-API config: {rejection}"))?;
        let inner = StatelessDecoder::<H265, VaapiBackend<InternalVaapiFrame>>::new_vaapi(
            display,
            BlockingMode::Blocking,
        )
        .map_err(|error| anyhow::anyhow!("Failed to create VA-API H.265 decoder: {error:?}"))?;

        Ok(Self {
            inner,
            access_unit_preparer: H265AccessUnitPreparer::new(stream_config),
            pending_access_unit: None,
        })
    }
}

impl VaapiCodecAdapter for H265VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::H265
    }

    /// Возвращает backend label для H.265 diagnostics.
    fn backend_name(&self) -> &'static str {
        "VA-API H.265"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "H.265"
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
                "H.265 pending access unit missing after preparation".to_string(),
            ));
        };
        let feed_result = pending_access_unit.feed_until_blocked(|remaining_bytes| {
            let mut alloc_cb = || {
                let frame = frame_pool.alloc_or_allocate();
                if frame.is_none() {
                    tracing::warn!("Frame pool exhausted; H.265 decoder needs output buffers");
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
                        "H.265 completed access unit missing from adapter state".to_string(),
                    ));
                };
                self.access_unit_preparer
                    .recycle_completed_access_unit(completed_access_unit);
                Ok(source_packet_len)
            }
            Ok(None) => unreachable!("H.265 feed loop always completes or returns an error"),
            Err(error) => Err(error),
        }
    }

    /// Flush-ит H.265 decoder state и забывает partially consumed AU.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.pending_access_unit = None;
        self.access_unit_preparer.reset_after_flush();
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// Дожимает H.265 DPB tail; seek flush остаётся отдельным adapter intent.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        if self.pending_access_unit.is_some() {
            return Err(VaapiAdapterDecodeError::Decoder(
                "cannot drain H.265 while an access unit is partially submitted".to_string(),
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
