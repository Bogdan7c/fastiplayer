//! Пошаговая нарезка буфера сырого потока на access unit-ы.
//!
//! Функции здесь чистые: они только смотрят на байты и говорят, что делать
//! дальше (отдать пакет, пропустить байты, дочитать). Буфер, курсор, время и
//! пределы принадлежат demuxer-у. Заголовки и сборку access unit-ов полностью
//! делает `ac3-syncframe`; здесь только политика синхронизации для файла.
//!
//! Поиск синхронизации (старт, после seek, после повреждения) строже, чем в
//! MPEG-TS: найденный заголовок принимается, только если сразу за кадром
//! начинается ещё один корректный заголовок (или поток кончается). Сырой
//! файл не даёт никаких других подсказок о границах, а случайная пара
//! `0x0B 0x77` с правдоподобными полями внутри аудиоданных встречается.

use ac3_syncframe::{
    AccessUnit, AccessUnitScan, StreamContinuation, SyncframeSearch, locate_syncframe,
    parse_syncframe_header, scan_access_unit,
};

/// Синхронизирован ли буфер с границами кадров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncState {
    /// Начало, seek или потеря синхронизации: нужен проверенный поиск кадра.
    Searching,
    /// Буфер начинается ровно на границе кадра.
    Locked,
}

/// Почему байты пропускаются, не став пакетом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// Dependent substream (или программа 1–7) без своего substream-а 0 —
    /// хвост пакета, начатого до точки входа. Декодировать его не с чем.
    OrphanSyncframe,
    /// На ожидаемой границе кадра нет корректного заголовка: поток повреждён,
    /// нужно искать синхронизацию заново.
    LostSync,
}

/// Следующее действие над буфером.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FramingStep {
    /// После `skipped_bytes` мусора начинается целый access unit.
    AccessUnit {
        /// Байты перед пакетом (мусор при поиске синхронизации).
        skipped_bytes: usize,
        /// Собранный пакет; в декодер идёт его основная программа.
        unit: AccessUnit,
    },
    /// Пропустить `bytes` байт с начала буфера.
    Skip {
        /// Сколько байт отбросить.
        bytes: usize,
        /// Причина (для логов и состояния синхронизации).
        reason: SkipReason,
    },
    /// Нужны ещё байты; первые `discardable_bytes` уже точно не начинают кадр.
    NeedMoreData {
        /// Сколько байт с начала буфера можно отбросить до дочитывания.
        discardable_bytes: usize,
    },
    /// Конец потока, а с `offset` лежит оборванный кадр.
    TruncatedTail {
        /// Смещение начала оборванного кадра.
        offset: usize,
    },
}

impl FramingStep {
    /// Состояние синхронизации после выполнения шага.
    pub(crate) const fn sync_state_after(self, current: SyncState) -> SyncState {
        match self {
            // Целый пакет или целый (хоть и бесполезный) кадр доказали границу.
            Self::AccessUnit { .. }
            | Self::Skip {
                reason: SkipReason::OrphanSyncframe,
                ..
            } => SyncState::Locked,
            Self::Skip {
                reason: SkipReason::LostSync,
                ..
            } => SyncState::Searching,
            Self::NeedMoreData { .. } | Self::TruncatedTail { .. } => current,
        }
    }
}

/// Решает, что делать с началом буфера.
pub(crate) fn next_framing_step(
    bytes: &[u8],
    state: SyncState,
    continuation: StreamContinuation,
) -> FramingStep {
    let frame_start = match state {
        SyncState::Locked => 0,
        SyncState::Searching => match locate_verified_syncframe(bytes, continuation) {
            VerifiedSearch::Found { offset } => offset,
            VerifiedSearch::NeedMoreData { discardable_bytes } => {
                return FramingStep::NeedMoreData { discardable_bytes };
            }
        },
    };
    match scan_access_unit(&bytes[frame_start..], continuation) {
        Ok(AccessUnitScan::Complete(unit)) => FramingStep::AccessUnit {
            skipped_bytes: frame_start,
            unit,
        },
        Ok(AccessUnitScan::NeedMoreData) => FramingStep::NeedMoreData {
            discardable_bytes: frame_start,
        },
        Ok(AccessUnitScan::OrphanSyncframe {
            syncframe_length_bytes,
            ..
        }) => FramingStep::Skip {
            bytes: frame_start + syncframe_length_bytes,
            reason: SkipReason::OrphanSyncframe,
        },
        // С `MoreBytesMayFollow` обрыв — это `NeedMoreData`, поэтому ошибка
        // обрыва бывает только в конце потока.
        Err(error) if error.is_truncation() => FramingStep::TruncatedTail {
            offset: frame_start,
        },
        // Битый заголовок на месте кадра (или слишком длинная цепочка
        // dependent-ов): сдвигаемся на байт и ищем синхронизацию заново.
        Err(_) => FramingStep::Skip {
            bytes: frame_start + 1,
            reason: SkipReason::LostSync,
        },
    }
}

/// Результат проверенного поиска кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifiedSearch {
    /// С `offset` начинается кадр, за которым идёт ещё один заголовок (или конец потока).
    Found { offset: usize },
    /// Решения пока нет; первые `discardable_bytes` можно выбросить.
    NeedMoreData { discardable_bytes: usize },
}

/// Ищет syncframe, за которым сразу начинается следующий корректный заголовок.
fn locate_verified_syncframe(bytes: &[u8], continuation: StreamContinuation) -> VerifiedSearch {
    let mut search_from = 0_usize;
    loop {
        let (offset, header) = match locate_syncframe(&bytes[search_from..]) {
            SyncframeSearch::Found { offset, header } => (search_from + offset, header),
            SyncframeSearch::NeedMoreData {
                discardable_prefix_bytes,
            } => {
                return VerifiedSearch::NeedMoreData {
                    discardable_bytes: search_from + discardable_prefix_bytes,
                };
            }
        };
        let next_frame = offset + header.frame_length_bytes();
        let next_header = bytes
            .get(next_frame..)
            .filter(|remainder| !remainder.is_empty())
            .map(parse_syncframe_header);
        match (next_header, continuation) {
            (Some(Ok(_)), _) => return VerifiedSearch::Found { offset },
            // Следующий заголовок не дочитан, но поток кончился: кадр последний
            // (возможно, оборванный — это решит `scan_access_unit`).
            (None, StreamContinuation::EndOfStream) => return VerifiedSearch::Found { offset },
            (Some(Err(error)), StreamContinuation::EndOfStream) if error.is_truncation() => {
                return VerifiedSearch::Found { offset };
            }
            (None, StreamContinuation::MoreBytesMayFollow) => {
                return VerifiedSearch::NeedMoreData {
                    discardable_bytes: offset,
                };
            }
            (Some(Err(error)), StreamContinuation::MoreBytesMayFollow) if error.is_truncation() => {
                return VerifiedSearch::NeedMoreData {
                    discardable_bytes: offset,
                };
            }
            // За «кадром» нет заголовка: ложное синхрослово, ищем дальше.
            (Some(Err(_)), _) => search_from = offset + 1,
        }
    }
}

#[cfg(test)]
mod tests;
