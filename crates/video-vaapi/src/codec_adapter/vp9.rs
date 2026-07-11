use super::*;
/// Production VP9 adapter поверх существующего cros-codecs decoder-а.
pub(super) struct Vp9VaapiCodecAdapter {
    /// cros-codecs stateless decoder спрятан за adapter trait-object.
    inner: cros_codecs::decoder::stateless::DynStatelessVideoDecoder<InternalVaapiFrame>,
}

/// VP9 сохраняет старую configure-семантику: same-codec config не пересоздаёт decoder.
pub(super) fn vp9_can_reuse_for_config(config: &VideoStreamDecodeConfig) -> bool {
    config.codec == VideoCodec::Vp9
}

impl Vp9VaapiCodecAdapter {
    /// Создаёт VP9 decoder для уже открытого VA display.
    pub(super) fn new(display: Rc<Display>) -> Result<Self> {
        type VaapiVp9Decoder = cros_codecs::decoder::stateless::StatelessDecoder<
            cros_codecs::decoder::stateless::vp9::Vp9,
            cros_codecs::backend::vaapi::decoder::VaapiBackend<InternalVaapiFrame>,
        >;

        let decoder = VaapiVp9Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|error| anyhow::anyhow!("Failed to create VA-API VP9 decoder: {error:?}"))?;

        Ok(Self {
            inner: decoder.into_trait_object(),
        })
    }
}

impl VaapiCodecAdapter for Vp9VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::Vp9
    }

    /// Явно владеет VP9 reuse intent вместо внешней проверки codec-а в decoder-е.
    fn can_reuse_for_config(&self, config: &VideoStreamDecodeConfig) -> bool {
        vp9_can_reuse_for_config(config)
    }

    /// Возвращает старое имя backend-а для сохранения UI/log совместимости.
    fn backend_name(&self) -> &'static str {
        "VA-API VP9"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "VP9"
    }

    /// Делегирует packet submit в cros-codecs и управляет output surface allocation.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        _decode_hints: VaapiPacketDecodeHints,
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        let mut alloc_cb = || {
            let frame = frame_pool.alloc_or_allocate();
            if frame.is_none() {
                tracing::warn!("Frame pool exhausted; decoder needs more output buffers");
            }
            frame
        };

        self.inner
            .decode(timestamp_us, packet_data, &mut alloc_cb)
            .map_err(VaapiAdapterDecodeError::from)
    }

    /// Flush-ит текущий VP9 decoder state.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// VP9 stateless path публикует готовые frames во время обычного decode loop-а.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        Ok(())
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
