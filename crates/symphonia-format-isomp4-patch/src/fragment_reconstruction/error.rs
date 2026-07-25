//! Типизированные и secret-safe ошибки fragment inspector-а.

use std::fmt;

/// Конкретный лимит, остановивший bounded inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentInspectionLimitKind {
    /// Полный размер входного fragment-а.
    InputBytes,
    /// Число ISO BMFF boxes на всех разобранных уровнях.
    BoxCount,
    /// Глубина вложенности boxes.
    BoxDepth,
    /// Число `traf`.
    TrackFragments,
    /// Число `trun`.
    TrackRuns,
    /// Число samples.
    Samples,
    /// Суммарная owned metadata для sample tables и normalized plan.
    SampleTableBytes,
    /// Payload одного box-а.
    BoxPayloadBytes,
}

/// Обязательный structural box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentBoxKind {
    /// Заголовок fragment-а.
    MovieFragmentHeader,
    /// Контейнер fragment-а.
    MovieFragment,
    /// Единственный track fragment.
    TrackFragment,
    /// Track fragment header.
    TrackFragmentHeader,
    /// Явное decode time начала track fragment.
    TrackFragmentDecodeTime,
    /// Один или несколько sample runs.
    TrackFragmentRun,
    /// Единственный media payload.
    MediaData,
}

/// Безопасный контекст structural truncation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentStructureContext {
    /// Верхний уровень fragment-а.
    TopLevel,
    /// Содержимое `moof`.
    MovieFragment,
    /// Содержимое `traf`.
    TrackFragment,
    /// Поля `mfhd`.
    MovieFragmentHeader,
    /// Поля `tfhd`.
    TrackFragmentHeader,
    /// Поля `tfdt`.
    TrackFragmentDecodeTime,
    /// Поля `trun`.
    TrackFragmentRun,
}

/// Причина fail-closed отказа от layout-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentUnsupportedLayout {
    /// Boxes идут не в разрешённом порядке.
    InvalidTopLevelOrder,
    /// Неизвестный либо неподдерживаемый box.
    UnsupportedBox([u8; 4]),
    /// Известный AtomType не входит в fragment profile.
    UnsupportedKnownAtom,
    /// `styp` не принадлежит признанному Smooth/CMAF семейству.
    UnrecognizedSegmentType,
    /// Box с неизвестным размером не позволяет доказать границы.
    UnknownBoxSize,
    /// Existing atom parser отверг поля box-а.
    AtomParserRejected,
    /// Fragment объявлен пустым, хотя caller ожидает media samples.
    EmptyTrackFragment,
    /// `trun` не содержит samples.
    EmptyTrackRun,
    /// После `mdat` есть непустые непаддинговые байты.
    TrailingNonPadding,
}

/// Тип отсутствующего timing/sample evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentTimingEvidence {
    /// Duration sample-а отсутствует и в `trun`, и в defaults.
    Duration,
    /// Size sample-а отсутствует и в `trun`, и в defaults.
    Size,
    /// Flags первого video sample-а не доказывают RAP.
    Flags,
}

/// Проверенное DRM evidence без содержимого box-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentDrmEvidence {
    /// Стандартный encryption-related fourcc.
    Box([u8; 4]),
    /// PIFF Sample Encryption UUID.
    PiffSampleEncryptionUuid,
}

/// Private/UUID evidence, которое F1A намеренно не интерпретирует.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentPrivateExtension {
    /// Неизвестный UUID.
    UnknownUuid,
    /// PIFF absolute-time `tfxd`; существующий patch его не парсит.
    PiffAbsoluteTime,
}

/// Арифметическая операция, на которой доказательство стало невозможным.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentArithmeticOperation {
    /// Подсчёт sample-ов.
    SampleCount,
    /// Подсчёт metadata bytes.
    SampleMetadataBytes,
    /// Вычисление decode time.
    DecodeTime,
    /// Вычисление presentation time.
    PresentationTime,
    /// Вычисление абсолютного byte offset.
    ByteOffset,
    /// Вычисление конца sample range.
    SampleRange,
}

/// Полный typed error boundary fragment inspection-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragmentInspectionError {
    /// Caller отменил работу.
    Cancelled,
    /// Обязательный лимит исчерпан до опасной работы.
    LimitExceeded {
        /// Какой бюджет исчерпан.
        kind: FragmentInspectionLimitKind,
        /// Настроенный предел.
        limit: u64,
        /// Наблюдаемое значение.
        observed: u64,
    },
    /// Объявленная box-структура оборвана.
    StructuralTruncation {
        /// Безопасный structural context.
        context: FragmentStructureContext,
    },
    /// Layout не входит в узкий F1A profile.
    UnsupportedLayout {
        /// Типизированная причина.
        reason: FragmentUnsupportedLayout,
    },
    /// В fragment-е обнаружены разные track IDs.
    MultiTrack {
        /// Авторитетный track ID caller-а.
        expected_track_id: u32,
        /// Track ID из fragment-а.
        actual_track_id: u32,
    },
    /// Обнаружено encryption/DRM evidence.
    DrmProtected {
        /// Безопасный тип evidence.
        evidence: FragmentDrmEvidence,
    },
    /// Обнаружено private extension, которое нельзя угадывать.
    PrivateExtension {
        /// Категория private extension.
        extension: FragmentPrivateExtension,
    },
    /// Обнаружено live-only `tfrf`.
    LiveMetadata,
    /// Обязательный structural box отсутствует.
    MissingBox {
        /// Отсутствующий box.
        kind: FragmentBoxKind,
    },
    /// Structural box, допустимый ровно один раз, повторён.
    DuplicateBox {
        /// Повторённый box.
        kind: FragmentBoxKind,
    },
    /// `tfdt` противоречит авторитетному времени caller-а.
    TimingConflict {
        /// Время caller-а.
        expected_base_decode_time: u64,
        /// Время fragment-а.
        actual_base_decode_time: u64,
    },
    /// Нельзя вывести обязательное поле sample-а.
    TimingEvidenceMissing {
        /// Какое evidence отсутствует.
        evidence: FragmentTimingEvidence,
        /// Индекс sample-а во всём fragment-е.
        sample_index: u32,
    },
    /// Новый range пересекает уже принятый.
    SampleRangeOverlap {
        /// Конец предыдущего range.
        previous_end: u64,
        /// Начало нового range.
        next_start: u64,
    },
    /// Sample range не помещается целиком в единственный `mdat`.
    SampleRangeOutsideMdat {
        /// Начало sample range.
        sample_start: u64,
        /// Конец sample range.
        sample_end: u64,
        /// Начало payload `mdat`.
        mdat_start: u64,
        /// Конец payload `mdat`.
        mdat_end: u64,
    },
    /// Sample ranges не покрывают `mdat` непрерывно и полностью.
    PayloadMismatch {
        /// Ожидаемая следующая граница.
        expected: u64,
        /// Фактическая граница.
        actual: u64,
    },
    /// Первый video sample не является доказанным RAP.
    RapFailure,
    /// Checked arithmetic обнаружила переполнение.
    ArithmeticOverflow {
        /// Операция без входных secret bytes.
        operation: FragmentArithmeticOperation,
    },
    /// Signed data offset нельзя безопасно применить.
    OffsetOverflow,
}

impl fmt::Display for FragmentInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Не печатаем raw box/sample bytes даже при ошибке.
        write!(
            formatter,
            "Smooth/PIFF fragment inspection failed: {self:?}"
        )
    }
}

impl std::error::Error for FragmentInspectionError {}
