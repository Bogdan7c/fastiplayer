//! Владелец активного codec adapter-а VA-API декодера.
//!
//! До первого `configure_stream` декодеру неизвестен кодек потока, поэтому adapter
//! не создаётся вовсе (`AwaitingStreamConfig`). Раньше на этом месте создавался
//! VP9-adapter «по умолчанию»: он был лишним для любого не-VP9 файла и давал в логе
//! вводящую в заблуждение строку `backend_name="VA-API VP9"` ещё до открытия медиа.
//!
//! Инварианты модуля:
//! - adapter создаётся только в [`ActiveCodecAdapter::recreate_for_config`] и только
//!   когда текущий adapter не может быть переиспользован под новый stream config;
//! - если создание не удалось, прежнее состояние (прежний adapter или его отсутствие)
//!   сохраняется — решение «остановить поток» остаётся у decoder thread-а;
//! - пакет до настройки потока — типизированная фатальная ошибка
//!   [`VaapiAdapterDecodeError::StreamNotConfigured`] (решение владельца, вариант A1);
//! - flush, EOF drain, events и stream info без adapter-а — безопасные no-op:
//!   у ненастроенного декодера нет codec state, который нужно сбрасывать;
//! - модуль не трогает surface pool, resource pool, hardware reservation и preroll:
//!   ими владеет `VaapiVideoDecoder`.

use std::rc::Rc;

use anyhow::Result;
use codec_core::VideoCodec;
use tracing::info;
use video_core::VideoStreamDecodeConfig;

use crate::codec_adapter::{
    VaapiAdapterDecodeError, VaapiAdapterStreamInfo, VaapiCodecAdapter, VaapiCodecAdapterFactory,
    VaapiDecoderEvent,
};

/// Имя backend-а, пока кодек потока ещё не известен.
///
/// Совпадает с именем, которое `VideoDecodeThread` отдаёт наружу до настройки потока.
const UNCONFIGURED_BACKEND_NAME: &str = "VA-API";

/// Короткая метка для логов decode loop-а, пока adapter не создан.
const UNCONFIGURED_CODEC_LABEL: &str = "unconfigured";

/// Фатальная ошибка декодера: пакет пришёл раньше настройки потока.
///
/// Отдельный тип нужен, чтобы `is_fatal_decoder_error` распознал её через downcast
/// и decoder thread остановился по обычному fatal-пути, без ACK этого пакета.
#[derive(Debug)]
pub(crate) struct VaapiStreamNotConfiguredError {
    /// Время пакета (мс) — чтобы в логе было видно, какой пакет нарушил порядок.
    pts_ms: u64,
}

impl VaapiStreamNotConfiguredError {
    /// Фиксирует пакет, отправленный до `configure_stream`.
    pub(super) const fn for_packet(pts_ms: u64) -> Self {
        Self { pts_ms }
    }
}

impl std::fmt::Display for VaapiStreamNotConfiguredError {
    /// Человекочитаемое описание нарушения порядка вызовов.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "VA-API packet at {} ms submitted before configure_stream: codec adapter is not created",
            self.pts_ms
        )
    }
}

impl std::error::Error for VaapiStreamNotConfiguredError {}

/// Шов создания codec adapter-ов: в production — VA display, в тестах — fake со счётчиком.
pub(super) trait CodecAdapterSource {
    /// Создаёт adapter под уже принятый stream config.
    fn create_for_config(
        &mut self,
        config: &VideoStreamDecodeConfig,
    ) -> Result<Box<dyn VaapiCodecAdapter>>;
}

/// Production-источник: создаёт cros-codecs adapters через единственный VA display декодера.
pub(super) struct DisplayCodecAdapterSource {
    /// Тот же display, на котором живут выходные surfaces декодера.
    display: Rc<cros_codecs::libva::Display>,
}

impl DisplayCodecAdapterSource {
    /// Оборачивает уже открытый VA display; второй `VADisplay` не открывается.
    pub(super) fn new(display: Rc<cros_codecs::libva::Display>) -> Self {
        Self { display }
    }
}

impl CodecAdapterSource for DisplayCodecAdapterSource {
    /// Делегирует выбор concrete adapter-а internal factory (она же валидирует config).
    fn create_for_config(
        &mut self,
        config: &VideoStreamDecodeConfig,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        VaapiCodecAdapterFactory::create_adapter_for_config(self.display.clone(), config)
    }
}

/// Чем закончилась настройка adapter-а под поток.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodecAdapterConfigureOutcome {
    /// Текущий adapter подошёл под новый config и остался без пересоздания.
    Reused {
        /// Кодек переиспользованного adapter-а.
        codec: VideoCodec,
        /// Имя backend-а для diagnostics/логов (`"VA-API VP9"` и т.п.).
        backend_name: &'static str,
    },
    /// Создан новый adapter (первая настройка или смена несовместимого кодека).
    Created {
        /// Кодек нового adapter-а.
        codec: VideoCodec,
        /// Имя backend-а для diagnostics/логов.
        backend_name: &'static str,
    },
}

impl CodecAdapterConfigureOutcome {
    /// Пишет факт «декодер настроен под этот поток» в обоих исходах.
    ///
    /// Контракт строки: `backend_name` — ПЕРВОЕ поле после сообщения. Бенчмарк-сборщик
    /// (`docs/benchmarks/tools/local/evidence.py`) ищет подстроку
    /// `configured for stream backend_name="VA-API <codec>"` и по ней доказывает
    /// кодек потока; при переиспользовании (VP9) строка тоже обязана появиться.
    pub(super) fn log_configured_for_stream(self) {
        match self {
            Self::Reused {
                codec,
                backend_name,
            } => info!(
                backend_name,
                codec = %codec,
                reused_adapter = true,
                "VA-API codec adapter configured for stream"
            ),
            Self::Created {
                codec,
                backend_name,
            } => info!(
                backend_name,
                codec = %codec,
                reused_adapter = false,
                "VA-API codec adapter configured for stream"
            ),
        }
    }
}

/// Состояние слота: adapter либо ещё не нужен, либо настроен под конкретный поток.
enum ActiveCodecAdapterState {
    /// Декодер открыт, но stream config ещё не приходил — adapter не создан.
    AwaitingStreamConfig,
    /// Adapter создан под принятый stream config.
    Configured(Box<dyn VaapiCodecAdapter>),
}

/// Единственный владелец активного codec adapter-а внутри `VaapiVideoDecoder`.
pub(super) struct ActiveCodecAdapter {
    /// Текущее состояние; наружу не раскрывается, только intent-методы ниже.
    state: ActiveCodecAdapterState,
}

impl ActiveCodecAdapter {
    /// Слот нового декодера: adapter появится только на первом `configure_stream`.
    pub(super) const fn awaiting_stream_config() -> Self {
        Self {
            state: ActiveCodecAdapterState::AwaitingStreamConfig,
        }
    }

    /// Кодек активного adapter-а; `None`, пока поток не настроен.
    pub(super) fn codec(&self) -> Option<VideoCodec> {
        match &self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => None,
            ActiveCodecAdapterState::Configured(adapter) => Some(adapter.codec()),
        }
    }

    /// Имя backend-а для UI/diagnostics; до настройки — нейтральное `"VA-API"`.
    pub(super) fn backend_name(&self) -> &'static str {
        match &self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => UNCONFIGURED_BACKEND_NAME,
            ActiveCodecAdapterState::Configured(adapter) => adapter.backend_name(),
        }
    }

    /// Короткая метка кодека для логов decode loop-а.
    pub(super) fn codec_label(&self) -> &'static str {
        match &self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => UNCONFIGURED_CODEC_LABEL,
            ActiveCodecAdapterState::Configured(adapter) => adapter.codec_label(),
        }
    }

    /// Возвращает `Reused`, если текущий adapter сам разрешает reuse под этот config.
    ///
    /// Решение о reuse остаётся за adapter-ом (`can_reuse_for_config`): VP9 разрешает
    /// тот же кодек, H.264/H.265 пересоздаются. Без adapter-а переиспользовать нечего.
    /// Метод ничего не меняет: подготовку к пересозданию делает decoder.
    pub(super) fn reuse_for_config(
        &self,
        config: &VideoStreamDecodeConfig,
    ) -> Option<CodecAdapterConfigureOutcome> {
        match &self.state {
            ActiveCodecAdapterState::Configured(adapter)
                if adapter.can_reuse_for_config(config) =>
            {
                Some(CodecAdapterConfigureOutcome::Reused {
                    codec: adapter.codec(),
                    backend_name: adapter.backend_name(),
                })
            }
            _ => None,
        }
    }

    /// Создаёт adapter под config и делает его активным.
    ///
    /// При ошибке источника прежнее состояние не меняется, ошибка возвращается как есть.
    pub(super) fn recreate_for_config(
        &mut self,
        config: &VideoStreamDecodeConfig,
        source: &mut dyn CodecAdapterSource,
    ) -> Result<CodecAdapterConfigureOutcome> {
        let adapter = source.create_for_config(config)?;
        let outcome = CodecAdapterConfigureOutcome::Created {
            codec: adapter.codec(),
            backend_name: adapter.backend_name(),
        };
        self.state = ActiveCodecAdapterState::Configured(adapter);
        Ok(outcome)
    }

    /// Даёт adapter для submit-а пакета либо фатальную ошибку «поток не настроен».
    pub(super) fn adapter_for_packet_submit(
        &mut self,
    ) -> std::result::Result<&mut (dyn VaapiCodecAdapter + 'static), VaapiAdapterDecodeError> {
        match &mut self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => {
                Err(VaapiAdapterDecodeError::StreamNotConfigured)
            }
            ActiveCodecAdapterState::Configured(adapter) => Ok(adapter.as_mut()),
        }
    }

    /// Flush codec state; без adapter-а сбрасывать нечего.
    pub(super) fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        match &mut self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => Ok(()),
            ActiveCodecAdapterState::Configured(adapter) => adapter.flush(),
        }
    }

    /// Запускает EOF/DPB drain; у ненастроенного декодера хвоста нет.
    pub(super) fn begin_end_of_stream_drain(
        &mut self,
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        match &mut self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => Ok(()),
            ActiveCodecAdapterState::Configured(adapter) => adapter.begin_end_of_stream_drain(),
        }
    }

    /// Следующий pending event adapter-а; без adapter-а событий нет.
    pub(super) fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
        match &mut self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => None,
            ActiveCodecAdapterState::Configured(adapter) => adapter.next_event(),
        }
    }

    /// Последний stream info adapter-а; без adapter-а он неизвестен.
    pub(super) fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
        match &self.state {
            ActiveCodecAdapterState::AwaitingStreamConfig => None,
            ActiveCodecAdapterState::Configured(adapter) => adapter.stream_info(),
        }
    }
}

#[cfg(test)]
mod tests;
