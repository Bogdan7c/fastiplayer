//! Сканер WebM (W3C «WebM Byte Stream Format»).
//!
//! Init-сегмент — EBML-заголовок, заголовок `Segment` и элементы до первого
//! `Cluster`; из них демуксеру нужны только `Info` и `Tracks`. Медиасегмент —
//! один `Cluster`. Дальше `Cues`, `Tags` и прочее служебное выбрасывается.
//!
//! Два переписывания заголовков, без которых эпизоды не работают:
//! - размер `Segment` в init-сегменте всегда становится «неизвестен». Эпизод
//!   кончается раньше, чем объявленный конец исходного файла, и Matroska-разборщик
//!   принял бы такой конец за обрыв (проверено разведкой 03a);
//! - кластер «неизвестного размера» (живые потоки) получает известный размер,
//!   когда его конец найден: демуксер всегда видит обычные кластеры.

use bytes::{Buf, Bytes, BytesMut};

use super::ebml::{
    EbmlElementHeader, UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field, read_element_header,
};
use super::{FramedUnit, FramingError, FramingLimits, SegmentKind, ensure_within_limit};

/// EBML-заголовок документа.
const EBML_HEADER_ID: u32 = 0x1A45_DFA3;
/// `Segment` — корень документа Matroska.
const SEGMENT_ID: u32 = 0x1853_8067;
/// `Cluster` — медиасегмент.
const CLUSTER_ID: u32 = 0x1F43_B675;
/// `Info` — параметры сегмента (TimestampScale), нужен демуксеру.
const INFO_ID: u32 = 0x1549_A966;
/// `Tracks` — описание дорожек, нужен демуксеру.
pub(crate) const TRACKS_ID: u32 = 0x1654_AE6B;
/// Остальные элементы уровня `Segment`: конец кластера неизвестного размера.
const OTHER_SEGMENT_LEVEL_IDS: [u32; 5] = [
    0x114D_9B74, // SeekHead
    0x1C53_BB6B, // Cues
    0x1254_C367, // Tags
    0x1043_A770, // Chapters
    0x1941_A469, // Attachments
];
/// Длина заголовка `Segment`/`Cluster`, который пишет сканер: 4 байта ID + 8 байтов размера.
const REWRITTEN_HEADER_BYTES: usize = 12;

/// Начинает ли элемент новую часть потока (а значит, кончает кластер неизвестного размера).
fn starts_segment_level_element(id: u32) -> bool {
    matches!(
        id,
        EBML_HEADER_ID | SEGMENT_ID | CLUSTER_ID | INFO_ID | TRACKS_ID
    ) || OTHER_SEGMENT_LEVEL_IDS.contains(&id)
}

/// Где сейчас сканер.
#[derive(Debug)]
enum MatroskaState {
    /// Ждём EBML-заголовок нового init-сегмента.
    ExpectEbmlHeader,
    /// EBML-заголовок есть, ждём заголовок `Segment`.
    ExpectSegment {
        /// Целый EBML-заголовок.
        ebml_header: Bytes,
    },
    /// Собираем init-сегмент до первого `Cluster`.
    InitializationBody {
        /// Целый EBML-заголовок.
        ebml_header: Bytes,
        /// Сохранённые `Info` и `Tracks`.
        kept_elements: BytesMut,
        /// Пришёл ли `Tracks`.
        has_tracks: bool,
    },
    /// Init-сегмент выдан, ждём кластеры (или новый init).
    Clusters,
    /// Внутри кластера неизвестного размера: копим его детей до элемента верхнего уровня.
    UnknownSizeCluster {
        /// Тело кластера.
        body: BytesMut,
    },
}

/// Сканер одной полосы WebM.
#[derive(Debug)]
pub(crate) struct MatroskaFramer {
    /// Пределы размера сегментов.
    limits: FramingLimits,
    /// Принятые, но ещё не разобранные байты.
    input: BytesMut,
    /// Сколько байтов ненужного элемента ещё пропустить.
    skip_remaining: u64,
    /// Положение в потоке.
    state: MatroskaState,
    /// Выдан ли хоть один init: после `abort()` поток может продолжиться кластером.
    has_initialization: bool,
}

impl MatroskaFramer {
    /// Пустой сканер.
    pub(crate) fn new(limits: FramingLimits) -> Self {
        Self {
            limits,
            input: BytesMut::new(),
            skip_remaining: 0,
            state: MatroskaState::ExpectEbmlHeader,
            has_initialization: false,
        }
    }

    /// Принимает кусок и выдаёт ставшие целыми сегменты.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        self.input.extend_from_slice(chunk);
        loop {
            if !self.consume_skipped_bytes() {
                return Ok(());
            }
            let Some(header) = read_element_header(&self.input)? else {
                return Ok(());
            };
            let state = std::mem::replace(&mut self.state, MatroskaState::Clusters);
            let step = match state {
                MatroskaState::ExpectEbmlHeader => self.on_ebml_header(header)?,
                MatroskaState::ExpectSegment { ebml_header } => {
                    self.on_segment_header(header, ebml_header)?
                }
                MatroskaState::InitializationBody {
                    ebml_header,
                    kept_elements,
                    has_tracks,
                } => self.on_initialization_element(
                    header,
                    ebml_header,
                    kept_elements,
                    has_tracks,
                    completed,
                )?,
                MatroskaState::Clusters => self.on_cluster_level_element(header, completed)?,
                MatroskaState::UnknownSizeCluster { body } => {
                    self.on_unknown_size_cluster_child(header, body, completed)?
                }
            };
            if step == Step::NeedMoreBytes {
                return Ok(());
            }
        }
    }

    /// `abort()`: недособранное выбрасывается. После выданного init поток может
    /// продолжиться прямо кластером, поэтому сканер ждёт кластер или новый init.
    pub(crate) fn reset(&mut self) {
        self.input.clear();
        self.skip_remaining = 0;
        self.state = if self.has_initialization {
            MatroskaState::Clusters
        } else {
            MatroskaState::ExpectEbmlHeader
        };
    }

    /// Конец потока: кластер неизвестного размера становится целым, прочее выбрасывается.
    pub(crate) fn finish(&mut self, completed: &mut Vec<FramedUnit>) -> usize {
        if let MatroskaState::UnknownSizeCluster { body } =
            std::mem::replace(&mut self.state, MatroskaState::Clusters)
            && !body.is_empty()
        {
            completed.push(FramedUnit::Media(finished_unknown_size_cluster(body)));
        }
        let dropped = self.held_bytes();
        self.reset();
        dropped
    }

    /// Сколько байтов удерживается до целого сегмента.
    pub(crate) fn held_bytes(&self) -> usize {
        let state_bytes = match &self.state {
            MatroskaState::ExpectEbmlHeader | MatroskaState::Clusters => 0,
            MatroskaState::ExpectSegment { ebml_header } => ebml_header.len(),
            MatroskaState::InitializationBody {
                ebml_header,
                kept_elements,
                ..
            } => ebml_header.len() + kept_elements.len(),
            MatroskaState::UnknownSizeCluster { body } => body.len(),
        };
        self.input.len() + state_bytes
    }

    /// Первый элемент init-сегмента — EBML-заголовок известного размера.
    fn on_ebml_header(&mut self, header: EbmlElementHeader) -> Result<Step, FramingError> {
        if header.id != EBML_HEADER_ID {
            return Err(FramingError::MissingEbmlHeader);
        }
        let total_len = known_total_len(header)?;
        ensure_within_limit(
            SegmentKind::Initialization,
            total_len,
            self.limits.max_initialization_bytes,
        )?;
        let Some(ebml_header) = self.take_whole(total_len) else {
            self.state = MatroskaState::ExpectEbmlHeader;
            return Ok(Step::NeedMoreBytes);
        };
        self.state = MatroskaState::ExpectSegment { ebml_header };
        Ok(Step::Continue)
    }

    /// Заголовок `Segment` выбрасывается: init получит свой, с размером «неизвестен».
    fn on_segment_header(
        &mut self,
        header: EbmlElementHeader,
        ebml_header: Bytes,
    ) -> Result<Step, FramingError> {
        if header.id != SEGMENT_ID {
            return Err(FramingError::MissingSegment);
        }
        self.input.advance(header.header_len);
        self.state = MatroskaState::InitializationBody {
            ebml_header,
            kept_elements: BytesMut::new(),
            has_tracks: false,
        };
        Ok(Step::Continue)
    }

    /// Элементы между `Segment` и первым `Cluster`.
    fn on_initialization_element(
        &mut self,
        header: EbmlElementHeader,
        ebml_header: Bytes,
        mut kept_elements: BytesMut,
        has_tracks: bool,
        completed: &mut Vec<FramedUnit>,
    ) -> Result<Step, FramingError> {
        if matches!(header.id, CLUSTER_ID | EBML_HEADER_ID) {
            if !has_tracks {
                return Err(FramingError::MissingTracks);
            }
            completed.push(FramedUnit::Initialization(initialization_segment(
                &ebml_header,
                &kept_elements,
            )));
            self.has_initialization = true;
            // Сам элемент не потребляем: его разберёт следующее состояние.
            self.state = if header.id == CLUSTER_ID {
                MatroskaState::Clusters
            } else {
                MatroskaState::ExpectEbmlHeader
            };
            return Ok(Step::Continue);
        }
        let total_len = known_total_len(header)?;
        if !matches!(header.id, INFO_ID | TRACKS_ID) {
            self.skip_remaining = total_len;
            self.state = MatroskaState::InitializationBody {
                ebml_header,
                kept_elements,
                has_tracks,
            };
            return Ok(Step::Continue);
        }
        let accumulated = (ebml_header.len() + REWRITTEN_HEADER_BYTES + kept_elements.len()) as u64;
        ensure_within_limit(
            SegmentKind::Initialization,
            accumulated.saturating_add(total_len),
            self.limits.max_initialization_bytes,
        )?;
        let Some(element) = self.take_whole(total_len) else {
            self.state = MatroskaState::InitializationBody {
                ebml_header,
                kept_elements,
                has_tracks,
            };
            return Ok(Step::NeedMoreBytes);
        };
        kept_elements.extend_from_slice(&element);
        self.state = MatroskaState::InitializationBody {
            ebml_header,
            kept_elements,
            has_tracks: has_tracks || header.id == TRACKS_ID,
        };
        Ok(Step::Continue)
    }

    /// Уровень `Segment` после init: кластеры, новый init или служебное.
    fn on_cluster_level_element(
        &mut self,
        header: EbmlElementHeader,
        completed: &mut Vec<FramedUnit>,
    ) -> Result<Step, FramingError> {
        self.state = MatroskaState::Clusters;
        match (header.id, header.total_len()) {
            (EBML_HEADER_ID, _) => {
                self.state = MatroskaState::ExpectEbmlHeader;
                Ok(Step::Continue)
            }
            (CLUSTER_ID, Some(total_len)) => {
                ensure_within_limit(SegmentKind::Media, total_len, self.limits.max_media_bytes)?;
                let Some(cluster) = self.take_whole(total_len) else {
                    return Ok(Step::NeedMoreBytes);
                };
                completed.push(FramedUnit::Media(cluster));
                Ok(Step::Continue)
            }
            (CLUSTER_ID, None) => {
                self.input.advance(header.header_len);
                self.state = MatroskaState::UnknownSizeCluster {
                    body: BytesMut::new(),
                };
                Ok(Step::Continue)
            }
            (_, Some(total_len)) => {
                self.skip_remaining = total_len;
                Ok(Step::Continue)
            }
            (id, None) => Err(FramingError::UnknownSizeNotAllowed { id }),
        }
    }

    /// Ребёнок кластера неизвестного размера или конец этого кластера.
    fn on_unknown_size_cluster_child(
        &mut self,
        header: EbmlElementHeader,
        mut body: BytesMut,
        completed: &mut Vec<FramedUnit>,
    ) -> Result<Step, FramingError> {
        if starts_segment_level_element(header.id) {
            completed.push(FramedUnit::Media(finished_unknown_size_cluster(body)));
            self.state = MatroskaState::Clusters;
            return Ok(Step::Continue);
        }
        let total_len = known_total_len(header)?;
        let accumulated = (REWRITTEN_HEADER_BYTES + body.len()) as u64;
        ensure_within_limit(
            SegmentKind::Media,
            accumulated.saturating_add(total_len),
            self.limits.max_media_bytes,
        )?;
        let Some(child) = self.take_whole(total_len) else {
            self.state = MatroskaState::UnknownSizeCluster { body };
            return Ok(Step::NeedMoreBytes);
        };
        body.extend_from_slice(&child);
        self.state = MatroskaState::UnknownSizeCluster { body };
        Ok(Step::Continue)
    }

    /// Пропускает уже пришедшие байты ненужного элемента. `false` — пропуск не окончен.
    fn consume_skipped_bytes(&mut self) -> bool {
        if self.skip_remaining == 0 {
            return true;
        }
        let skipped = usize::try_from(self.skip_remaining)
            .unwrap_or(usize::MAX)
            .min(self.input.len());
        self.input.advance(skipped);
        self.skip_remaining -= skipped as u64;
        self.skip_remaining == 0
    }

    /// Забирает элемент из буфера, если он пришёл целиком.
    fn take_whole(&mut self, total_len: u64) -> Option<Bytes> {
        let total_len = usize::try_from(total_len).ok()?;
        (self.input.len() >= total_len).then(|| self.input.split_to(total_len).freeze())
    }
}

/// Что делать дальше после одного элемента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Разбирать следующий элемент.
    Continue,
    /// Ждать следующий кусок.
    NeedMoreBytes,
}

/// Полная длина элемента, которому размер обязан быть известен.
fn known_total_len(header: EbmlElementHeader) -> Result<u64, FramingError> {
    header
        .total_len()
        .ok_or(FramingError::UnknownSizeNotAllowed { id: header.id })
}

/// Init-сегмент: EBML-заголовок, `Segment` неизвестного размера, `Info` и `Tracks`.
fn initialization_segment(ebml_header: &[u8], kept_elements: &[u8]) -> Bytes {
    let mut segment =
        BytesMut::with_capacity(ebml_header.len() + REWRITTEN_HEADER_BYTES + kept_elements.len());
    segment.extend_from_slice(ebml_header);
    segment.extend_from_slice(&encode_id(SEGMENT_ID));
    segment.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    segment.extend_from_slice(kept_elements);
    segment.freeze()
}

/// Кластер неизвестного размера с найденным концом — как обычный кластер.
fn finished_unknown_size_cluster(body: BytesMut) -> Bytes {
    let mut cluster = BytesMut::with_capacity(REWRITTEN_HEADER_BYTES + body.len());
    cluster.extend_from_slice(&encode_id(CLUSTER_ID));
    // Тело ограничено пределом медиасегмента, поэтому всегда помещается в поле размера.
    let size_field = encode_known_size_field(body.len() as u64).unwrap_or(UNKNOWN_SIZE_FIELD);
    cluster.extend_from_slice(&size_field);
    cluster.extend_from_slice(&body);
    cluster.freeze()
}

#[cfg(test)]
#[path = "matroska_tests.rs"]
mod tests;
