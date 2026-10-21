//! Focused tests владельца активного codec adapter-а (без VA-железа).
//!
//! Fake-источник считает созданные adapters: так проверяется главный эффект
//! ленивого создания — на старте не создаётся ни одного лишнего adapter-а.

use std::cell::Cell;
use std::rc::Rc;

use media_core::TrackId;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;
use crate::codec_adapter::VaapiPacketDecodeHints;
use crate::output_surface_pool::VaapiOutputSurfacePool;

/// Fake adapter: кодек и reuse-политика задаются тестом, flush/drain считаются.
struct FakeCodecAdapter {
    /// Кодек, под который «создан» adapter.
    codec: VideoCodec,
    /// Разрешает ли adapter reuse под config того же кодека (как VP9).
    reuses_same_codec: bool,
    /// Сколько раз вызывали flush — проверка, что slot делегирует, а не глотает вызов.
    flush_calls: Rc<Cell<usize>>,
}

impl VaapiCodecAdapter for FakeCodecAdapter {
    /// Кодек fake adapter-а.
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Reuse только для того же кодека и только если тест это разрешил.
    fn can_reuse_for_config(&self, config: &VideoStreamDecodeConfig) -> bool {
        self.reuses_same_codec && config.codec == self.codec
    }

    /// Имя backend-а повторяет production-формат `"VA-API <codec>"`.
    fn backend_name(&self) -> &'static str {
        match self.codec {
            VideoCodec::Vp9 => "VA-API VP9",
            VideoCodec::H264 => "VA-API H.264",
            _ => "VA-API fake",
        }
    }

    /// Метка для логов.
    fn codec_label(&self) -> &'static str {
        "fake"
    }

    /// Submit в тестах этого модуля не вызывается (нужен реальный surface pool).
    fn submit_packet(
        &mut self,
        _timestamp_us: u64,
        packet_data: &[u8],
        _decode_hints: VaapiPacketDecodeHints,
        _surface_pool: &VaapiOutputSurfacePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        Ok(packet_data.len())
    }

    /// Считает flush-вызовы.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.flush_calls.set(self.flush_calls.get() + 1);
        Ok(())
    }

    /// Fake не держит DPB tail.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        Ok(())
    }

    /// Fake не публикует events.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
        None
    }

    /// Fake не знает stream info.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
        None
    }
}

/// Fake-источник: считает созданные adapters и умеет имитировать отказ создания.
#[derive(Default)]
struct CountingAdapterSource {
    /// Сколько adapters реально создано.
    created_adapters: usize,
    /// Сколько раз источник просили создать adapter (включая неудачные попытки).
    creation_attempts: usize,
    /// Если `true`, следующее создание вернёт ошибку (как отказ VA/cros).
    fail_next_creation: bool,
    /// Общий счётчик flush-ей всех созданных fake adapters.
    flush_calls: Rc<Cell<usize>>,
}

impl CodecAdapterSource for CountingAdapterSource {
    /// Создаёт fake adapter; VP9 разрешает reuse, остальные — нет (как в production).
    fn create_for_config(
        &mut self,
        config: &VideoStreamDecodeConfig,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        self.creation_attempts += 1;
        if self.fail_next_creation {
            self.fail_next_creation = false;
            return Err(anyhow::anyhow!("fake adapter creation failure"));
        }
        self.created_adapters += 1;
        Ok(Box::new(FakeCodecAdapter {
            codec: config.codec,
            reuses_same_codec: config.codec == VideoCodec::Vp9,
            flush_calls: self.flush_calls.clone(),
        }))
    }
}

/// Минимальный stream config нужного кодека.
fn stream_config(codec: VideoCodec) -> VideoStreamDecodeConfig {
    VideoStreamDecodeConfig {
        track_id: TrackId::new(1),
        codec,
        profile: None,
        bit_depth: None,
        chroma: None,
        coded_width: Some(1920),
        coded_height: Some(1080),
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        codec_private: None,
        packetization: None,
    }
}

/// Повторяет порядок `VaapiVideoDecoder::configure_stream`: сначала reuse, потом создание.
fn configure(
    slot: &mut ActiveCodecAdapter,
    source: &mut CountingAdapterSource,
    codec: VideoCodec,
) -> Result<CodecAdapterConfigureOutcome> {
    let config = stream_config(codec);
    match slot.reuse_for_config(&config) {
        Some(reused) => Ok(reused),
        None => slot.recreate_for_config(&config, source),
    }
}

/// Тест 1: новый декодер не создаёт ни одного adapter-а и не притворяется VP9.
/// Поймает возврат VP9-adapter-а «по умолчанию» в конструктор.
#[test]
fn new_slot_creates_no_adapter_and_reports_neutral_backend() {
    let slot = ActiveCodecAdapter::awaiting_stream_config();
    let source = CountingAdapterSource::default();

    assert_eq!(slot.codec(), None);
    assert_eq!(slot.backend_name(), "VA-API");
    assert_eq!(slot.stream_info(), None);
    assert_eq!(source.creation_attempts, 0);
}

/// Тест 2: старт H.264-потока создаёт ровно один adapter (раньше: VP9 + H.264).
#[test]
fn first_h264_configure_creates_exactly_one_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();

    let outcome = configure(&mut slot, &mut source, VideoCodec::H264).expect("configure H.264");

    assert_eq!(
        outcome,
        CodecAdapterConfigureOutcome::Created {
            codec: VideoCodec::H264,
            backend_name: "VA-API H.264",
        }
    );
    assert_eq!(source.created_adapters, 1);
    assert_eq!(slot.codec(), Some(VideoCodec::H264));
    assert_eq!(slot.backend_name(), "VA-API H.264");
}

/// Тест 3: старт VP9-потока тоже создаёт ровно один adapter — именно на configure.
#[test]
fn first_vp9_configure_creates_exactly_one_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();

    let outcome = configure(&mut slot, &mut source, VideoCodec::Vp9).expect("configure VP9");

    assert!(matches!(
        outcome,
        CodecAdapterConfigureOutcome::Created {
            codec: VideoCodec::Vp9,
            ..
        }
    ));
    assert_eq!(source.created_adapters, 1);
}

/// Тест 4: повторная настройка VP9 переиспользует adapter и отдаёт `Reused` с
/// `backend_name = "VA-API VP9"` — из него строится строка `reused_adapter=true`.
#[test]
fn repeated_vp9_configure_reuses_adapter_with_vp9_backend_name() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();
    configure(&mut slot, &mut source, VideoCodec::Vp9).expect("first VP9 configure");

    let outcome = configure(&mut slot, &mut source, VideoCodec::Vp9).expect("second VP9 configure");

    assert_eq!(
        outcome,
        CodecAdapterConfigureOutcome::Reused {
            codec: VideoCodec::Vp9,
            backend_name: "VA-API VP9",
        }
    );
    assert_eq!(
        source.created_adapters, 1,
        "reuse must not create an adapter"
    );
}

/// Тест 5: H.264 не разрешает reuse — повторная настройка пересоздаёт adapter
/// (фиксирует существующую политику `can_reuse_for_config`).
#[test]
fn repeated_h264_configure_recreates_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();
    configure(&mut slot, &mut source, VideoCodec::H264).expect("first H.264 configure");

    let outcome = configure(&mut slot, &mut source, VideoCodec::H264).expect("second configure");

    assert!(matches!(
        outcome,
        CodecAdapterConfigureOutcome::Created { .. }
    ));
    assert_eq!(source.created_adapters, 2);
}

/// Тест 6: отказ создания возвращает ошибку и не трогает прежний adapter.
#[test]
fn creation_failure_keeps_previous_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();
    configure(&mut slot, &mut source, VideoCodec::Vp9).expect("VP9 configure");
    source.fail_next_creation = true;

    let error = configure(&mut slot, &mut source, VideoCodec::H264)
        .expect_err("creation failure must be returned");

    assert!(error.to_string().contains("fake adapter creation failure"));
    assert_eq!(source.creation_attempts, 2);
    assert_eq!(source.created_adapters, 1);
    assert_eq!(
        slot.codec(),
        Some(VideoCodec::Vp9),
        "old adapter stays active"
    );
    assert_eq!(slot.backend_name(), "VA-API VP9");
}

/// Тест 6b: отказ самой первой настройки оставляет декодер ненастроенным.
#[test]
fn first_creation_failure_keeps_slot_unconfigured() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource {
        fail_next_creation: true,
        ..CountingAdapterSource::default()
    };

    let result = configure(&mut slot, &mut source, VideoCodec::H264);

    assert!(result.is_err());
    assert_eq!(slot.codec(), None);
    assert!(matches!(
        slot.adapter_for_packet_submit(),
        Err(VaapiAdapterDecodeError::StreamNotConfigured)
    ));
}

/// Тест 7 (A1): пакет до configure получает `StreamNotConfigured`, adapter не создаётся.
#[test]
fn packet_submit_before_configure_is_rejected_without_creating_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let source = CountingAdapterSource::default();

    let submit_target = slot.adapter_for_packet_submit();

    assert!(matches!(
        submit_target,
        Err(VaapiAdapterDecodeError::StreamNotConfigured)
    ));
    assert_eq!(source.creation_attempts, 0);
    assert_eq!(slot.codec(), None);
}

/// После configure пакет получает именно настроенный adapter.
#[test]
fn packet_submit_after_configure_targets_configured_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();
    configure(&mut slot, &mut source, VideoCodec::H264).expect("configure H.264");

    let submit_target = slot
        .adapter_for_packet_submit()
        .expect("configured slot must expose adapter");

    assert_eq!(submit_target.codec(), VideoCodec::H264);
}

/// Тест 8: flush/EOF drain/events до configure — no-op без ошибок и без создания adapter-а.
#[test]
fn lifecycle_calls_before_configure_are_noops() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let source = CountingAdapterSource::default();

    assert!(matches!(slot.flush(), Ok(())));
    assert!(matches!(slot.begin_end_of_stream_drain(), Ok(())));
    assert!(slot.next_event().is_none());
    assert_eq!(slot.codec(), None);
    assert_eq!(source.creation_attempts, 0);
    assert_eq!(source.flush_calls.get(), 0);
}

/// Настроенный slot делегирует flush adapter-у, а не глотает вызов.
#[test]
fn flush_after_configure_reaches_adapter() {
    let mut slot = ActiveCodecAdapter::awaiting_stream_config();
    let mut source = CountingAdapterSource::default();
    configure(&mut slot, &mut source, VideoCodec::Vp9).expect("configure VP9");

    slot.flush().expect("flush must pass");

    assert_eq!(source.flush_calls.get(), 1);
}
