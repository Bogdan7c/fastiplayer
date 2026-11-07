//! Что умеет показывать и декодировать плеер браузерного режима — для выбора варианта
//! HLS (кодек, разрешение, звук), как у обычного плеера.
//!
//! Native HLS выбирает вариант мастер-плейлиста по возможностям системы
//! (`SystemCapabilities`) и декодеров звука (`AudioDecodeCapabilitySnapshot`). У
//! браузерного режима их знает тот, кто играет:
//! - безоконный плеер — сразу при старте (его видеовыход и декодеры звука);
//! - окно — только когда оно поднялось и опросило рендер (окно появляется на первом
//!   сеансе, а ядро процесса стартует раньше). До этого ответ —
//!   [`DecodeCapabilitiesAnswer::NotYetKnown`], и открыватель HLS подождёт.
//!
//! Возможности берутся у плеера, а не опрашиваются заново: иначе HLS выбрал бы вариант,
//! который видеовыход этого процесса не покажет.

use audio::AudioDecodeCapabilitySnapshot;
use capability_core::SystemCapabilities;

/// Возможности плеера, по которым native HLS выбирает вариант.
#[derive(Debug, Clone)]
pub(crate) struct BrowserDecodeCapabilities {
    /// Видео: декодеры и рендер (те же, что плеер отдал своему рабочему потоку).
    pub(crate) system: SystemCapabilities,
    /// Звук: какие кодеки декодирует плеер.
    pub(crate) audio: AudioDecodeCapabilitySnapshot,
}

/// Знает ли плеер свои возможности сейчас.
#[derive(Debug, Clone)]
pub(crate) enum DecodeCapabilitiesAnswer {
    /// Знает.
    Known(Box<BrowserDecodeCapabilities>),
    /// Ещё нет (окно не поднялось): спросить позже.
    NotYetKnown,
}

/// У кого спросить возможности. Настоящие — плеер медиа-части (`MediaPlayback`).
pub(crate) trait DecodeCapabilitiesSource {
    /// Возможности плеера сейчас (копия: вызывается один раз на открытие HLS).
    fn decode_capabilities(&self) -> DecodeCapabilitiesAnswer;
}
