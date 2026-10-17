//! Сборка access unit-а: одного пакета для декодера из подряд идущих syncframe-ов.
//!
//! Пакет AC-3 — ровно один syncframe. Пакет E-AC-3 — независимый substream 0
//! и все следующие за ним syncframe-ы до следующего независимого substream-а 0
//! (dependent substream-ы с дополнительными каналами, например 7.1, и
//! дополнительные программы 1–7). Так же устроены пакеты Matroska `A_EAC3` и
//! MP4 `ec-3`, и именно такой пакет ожидает декодер.
//!
//! По A/52 Annex E substream-ы идут строго по порядку: независимый 0, его
//! dependent-ы, независимый 1, его dependent-ы и т.д. Поэтому основная
//! программа (substream 0 + её dependent-ы) — всегда непрерывный префикс
//! access unit-а, и её длина отдаётся отдельно: декодер, который умеет только
//! одну программу, получает только этот префикс.

use crate::{
    ChannelLocations, SyncframeCodec, SyncframeHeader, SyncframeHeaderError, parse_syncframe_header,
};

/// Предел syncframe-ов в одном access unit-е: 8 программ × (независимый + 7 dependent).
///
/// Больше в корректном потоке не бывает; предел не даёт повреждённому потоку
/// из одних dependent substream-ов бесконечно растить буфер.
pub const MAX_SYNCFRAMES_PER_ACCESS_UNIT: usize = 64;

/// Может ли за концом буфера прийти ещё данные этого потока.
///
/// От этого зависит, закончен ли последний access unit: E-AC-3 пакет
/// завершается только началом следующего пакета или концом потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamContinuation {
    /// Буфер — не конец потока; недостающие байты ещё могут прийти.
    MoreBytesMayFollow,
    /// Буфер содержит последние байты потока (EOF / конец сегмента).
    EndOfStream,
}

/// Один собранный access unit в начале буфера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessUnit {
    primary_header: SyncframeHeader,
    codec: SyncframeCodec,
    total_length_bytes: usize,
    primary_program_length_bytes: usize,
    syncframe_count: usize,
    primary_program_locations: ChannelLocations,
    has_additional_programs: bool,
}

impl AccessUnit {
    /// Заголовок первого syncframe-а (AC-3 кадр или независимый substream 0).
    ///
    /// Частота, число сэмплов и кодек пакета берутся из него.
    #[must_use]
    pub const fn primary_header(&self) -> &SyncframeHeader {
        &self.primary_header
    }

    /// Кодек основной программы: E-AC-3, если хоть один её syncframe — E-AC-3
    /// (в том числе AC-3 ядро с E-AC-3 dependent substream-ом), иначе AC-3.
    #[must_use]
    pub const fn codec(&self) -> SyncframeCodec {
        self.codec
    }

    /// Сколько байт с начала буфера занимает access unit целиком
    /// (столько нужно отбросить, чтобы перейти к следующему).
    #[must_use]
    pub const fn total_length_bytes(&self) -> usize {
        self.total_length_bytes
    }

    /// Длина префикса основной программы: substream 0 + его dependent-ы.
    ///
    /// Равна [`Self::total_length_bytes`], если дополнительных программ нет.
    #[must_use]
    pub const fn primary_program_length_bytes(&self) -> usize {
        self.primary_program_length_bytes
    }

    /// Сколько syncframe-ов вошло в access unit (всех программ).
    #[must_use]
    pub const fn syncframe_count(&self) -> usize {
        self.syncframe_count
    }

    /// Сэмплов на канал в пакете — длительность пакета для таймстемпов.
    #[must_use]
    pub const fn samples_per_frame(&self) -> u32 {
        self.primary_header.samples_per_frame()
    }

    /// Итоговое число каналов основной программы: объединение позиций
    /// независимого substream-а 0 и его dependent-ов (5.1 + Lrs/Rrs = 7.1).
    #[must_use]
    pub fn presentation_channel_count(&self) -> u32 {
        self.primary_program_locations.channel_count()
    }

    /// Позиции каналов основной программы в нотации chanmap.
    #[must_use]
    pub const fn presentation_channel_locations(&self) -> ChannelLocations {
        self.primary_program_locations
    }

    /// Есть ли в пакете независимые substream-ы 1–7 (дополнительные программы).
    #[must_use]
    pub const fn has_additional_programs(&self) -> bool {
        self.has_additional_programs
    }
}

/// Что лежит в начале буфера с точки зрения сборки пакетов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessUnitScan {
    /// В начале буфера полностью собранный access unit.
    Complete(AccessUnit),
    /// Данных пока не хватает, чтобы собрать или завершить access unit.
    /// Возможно только при [`StreamContinuation::MoreBytesMayFollow`].
    NeedMoreData,
    /// Буфер начинается целым syncframe-ом, который не может открыть пакет
    /// (dependent substream или программа 1–7 без своего substream-а 0 —
    /// бывает после seek/resync посреди access unit-а). Декодировать его без
    /// предшествующего substream-а 0 нельзя; вызывающий пропускает
    /// `syncframe_length_bytes` байт.
    OrphanSyncframe {
        /// Заголовок пропускаемого syncframe-а.
        header: SyncframeHeader,
        /// Его длина в байтах.
        syncframe_length_bytes: usize,
    },
}

/// Собирает access unit, который начинается ровно с первого байта `bytes`.
///
/// Функция без состояния: вызывающий хранит буфер, отбрасывает
/// `total_length_bytes()` после `Complete` и вызывает снова.
///
/// # Errors
///
/// Повреждённый заголовок первого syncframe-а; при
/// [`StreamContinuation::EndOfStream`] — также оборванный первый кадр
/// ([`SyncframeHeaderError::Truncated`]); [`SyncframeHeaderError::AccessUnitTooLong`].
/// Повреждение *следующего* кадра не портит уже собранный пакет: он
/// возвращается как `Complete`, а ошибку вернёт следующий вызов.
pub fn scan_access_unit(
    bytes: &[u8],
    continuation: StreamContinuation,
) -> Result<AccessUnitScan, SyncframeHeaderError> {
    let Some(first_header) = parse_header_or_wait(bytes, continuation)? else {
        return Ok(AccessUnitScan::NeedMoreData);
    };
    if !first_frame_is_complete(bytes, &first_header, continuation)? {
        return Ok(AccessUnitScan::NeedMoreData);
    }
    if !first_header.starts_access_unit() {
        return Ok(AccessUnitScan::OrphanSyncframe {
            header: first_header,
            syncframe_length_bytes: first_header.frame_length_bytes(),
        });
    }
    let mut assembly = AccessUnitAssembly::start(first_header);
    while assembly.total_length_bytes < bytes.len() {
        let remainder = &bytes[assembly.total_length_bytes..];
        let next_header = match parse_syncframe_header(remainder) {
            Ok(header) => header,
            Err(error)
                if error.is_truncation()
                    && continuation == StreamContinuation::MoreBytesMayFollow =>
            {
                return Ok(AccessUnitScan::NeedMoreData);
            }
            // Следующий кадр повреждён или оборван концом потока. Уже собранный
            // пакет от этого не портится: он отдаётся, а ошибка достанется
            // следующему вызову, который начнётся ровно с испорченного кадра.
            Err(_) => return Ok(AccessUnitScan::Complete(assembly.finish())),
        };
        if next_header.starts_access_unit() {
            return Ok(AccessUnitScan::Complete(assembly.finish()));
        }
        if remainder.len() < next_header.frame_length_bytes() {
            return Ok(match continuation {
                StreamContinuation::MoreBytesMayFollow => AccessUnitScan::NeedMoreData,
                // Оборванный dependent в конце потока — тоже ошибка следующего вызова.
                StreamContinuation::EndOfStream => AccessUnitScan::Complete(assembly.finish()),
            });
        }
        assembly.append(next_header)?;
    }
    // Буфер кончился ровно на границе кадра: следующий syncframe ещё может
    // оказаться dependent-ом этого же пакета, если поток продолжается.
    match continuation {
        StreamContinuation::MoreBytesMayFollow => Ok(AccessUnitScan::NeedMoreData),
        StreamContinuation::EndOfStream => Ok(AccessUnitScan::Complete(assembly.finish())),
    }
}

/// Накопитель одного пакета: границы, программы и каналы.
struct AccessUnitAssembly {
    primary_header: SyncframeHeader,
    codec: SyncframeCodec,
    total_length_bytes: usize,
    primary_program_length_bytes: usize,
    syncframe_count: usize,
    primary_program_locations: ChannelLocations,
    has_additional_programs: bool,
}

impl AccessUnitAssembly {
    fn start(primary_header: SyncframeHeader) -> Self {
        Self {
            primary_header,
            codec: primary_header.codec(),
            total_length_bytes: primary_header.frame_length_bytes(),
            primary_program_length_bytes: primary_header.frame_length_bytes(),
            syncframe_count: 1,
            primary_program_locations: primary_header.channel_locations(),
            has_additional_programs: false,
        }
    }

    fn append(&mut self, header: SyncframeHeader) -> Result<(), SyncframeHeaderError> {
        if self.syncframe_count >= MAX_SYNCFRAMES_PER_ACCESS_UNIT {
            return Err(SyncframeHeaderError::AccessUnitTooLong {
                maximum_syncframes: MAX_SYNCFRAMES_PER_ACCESS_UNIT,
            });
        }
        let is_independent_program = header
            .eac3_substream()
            .is_some_and(|substream| substream.is_independent());
        // Первый независимый substream 1–7 закрывает основную программу:
        // всё после него относится к дополнительным программам.
        self.has_additional_programs |= is_independent_program;
        if !self.has_additional_programs {
            self.primary_program_length_bytes += header.frame_length_bytes();
            self.primary_program_locations = self
                .primary_program_locations
                .union(header.channel_locations());
            // AC-3 ядро с E-AC-3 dependent-ом (7.1 «AC-3 + расширение») — это
            // уже E-AC-3 программа: без Annex E декодер потеряет каналы.
            if header.codec() == SyncframeCodec::EnhancedAc3 {
                self.codec = SyncframeCodec::EnhancedAc3;
            }
        }
        self.total_length_bytes += header.frame_length_bytes();
        self.syncframe_count += 1;
        Ok(())
    }

    const fn finish(&self) -> AccessUnit {
        AccessUnit {
            primary_header: self.primary_header,
            codec: self.codec,
            total_length_bytes: self.total_length_bytes,
            primary_program_length_bytes: self.primary_program_length_bytes,
            syncframe_count: self.syncframe_count,
            primary_program_locations: self.primary_program_locations,
            has_additional_programs: self.has_additional_programs,
        }
    }
}

/// `Ok(None)` — заголовок обрезан, но поток продолжается (нужно дочитать).
fn parse_header_or_wait(
    bytes: &[u8],
    continuation: StreamContinuation,
) -> Result<Option<SyncframeHeader>, SyncframeHeaderError> {
    match parse_syncframe_header(bytes) {
        Ok(header) => Ok(Some(header)),
        Err(error)
            if error.is_truncation() && continuation == StreamContinuation::MoreBytesMayFollow =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Доступен ли первый кадр целиком; при конце потока оборванный кадр — ошибка.
fn first_frame_is_complete(
    bytes: &[u8],
    header: &SyncframeHeader,
    continuation: StreamContinuation,
) -> Result<bool, SyncframeHeaderError> {
    if header.frame_length_bytes() <= bytes.len() {
        return Ok(true);
    }
    match continuation {
        StreamContinuation::MoreBytesMayFollow => Ok(false),
        StreamContinuation::EndOfStream => Err(SyncframeHeaderError::Truncated {
            available_bytes: bytes.len(),
            required_bytes: header.frame_length_bytes(),
        }),
    }
}
