//! Минимальный extractor Matroska/WebM video metadata, которую Symphonia 0.6 не отдаёт
//! в текущую neutral model.
//!
//! Этот модуль не заменяет demuxer. Он читает только metadata-дерево `Tracks` до decode,
//! чтобы сохранить `Colour`/HDR elements для раннего выбора video path.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients,
    TransferFunction, VideoColorMetadata,
};
use media_core::{TrackId, VideoTrackMetadata};

/// Верхняя граница pre-scan: metadata обычно лежит перед первым Cluster.
const MATROSKA_METADATA_SCAN_LIMIT_BYTES: u64 = 4 * 1024 * 1024;

/// Верхняя граница чтения `Cues`: index нужен только для bounded seek-anchor lookup.
pub(crate) const MATROSKA_CUES_SCAN_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

/// Matroska default `TimestampScale`: один tick равен одной миллисекунде.
const MATROSKA_DEFAULT_TIMESTAMP_SCALE_NS: u64 = 1_000_000;

const ID_SEGMENT: u64 = 0x1853_8067;
const ID_SEEK_HEAD: u64 = 0x114D_9B74;
const ID_SEEK: u64 = 0x4DBB;
const ID_SEEK_ID: u64 = 0x53AB;
const ID_SEEK_POSITION: u64 = 0x53AC;
const ID_INFO: u64 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u64 = 0x002A_D7B1;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u64 = 0xAE;
const ID_TRACK_NUMBER: u64 = 0xD7;
const ID_TRACK_TYPE: u64 = 0x83;
const ID_CODEC_ID: u64 = 0x86;
const ID_VIDEO: u64 = 0xE0;
const ID_PIXEL_WIDTH: u64 = 0xB0;
const ID_PIXEL_HEIGHT: u64 = 0xBA;
const ID_COLOUR: u64 = 0x55B0;
const ID_MATRIX_COEFFICIENTS: u64 = 0x55B1;
const ID_BITS_PER_CHANNEL: u64 = 0x55B2;
const ID_CHROMA_SUBSAMPLING_HORZ: u64 = 0x55B3;
const ID_CHROMA_SUBSAMPLING_VERT: u64 = 0x55B4;
const ID_RANGE: u64 = 0x55B9;
const ID_TRANSFER_CHARACTERISTICS: u64 = 0x55BA;
const ID_PRIMARIES: u64 = 0x55BB;
const ID_MAX_CLL: u64 = 0x55BC;
const ID_MAX_FALL: u64 = 0x55BD;
const ID_MASTERING_METADATA: u64 = 0x55D0;
const ID_LUMINANCE_MAX: u64 = 0x55D9;
const ID_LUMINANCE_MIN: u64 = 0x55DA;
const ID_CUES: u64 = 0x1C53_BB6B;
const ID_CUE_POINT: u64 = 0xBB;
const ID_CUE_TIME: u64 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u64 = 0xB7;
const ID_CUE_TRACK: u64 = 0xF7;

/// Matroska/WebM metadata одного video track-а, доступная до основного demux loop.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatroskaVideoTrack {
    /// Container `CodecID`, например `V_VP9`; нужен, когда Symphonia отдаёт `CODEC_TYPE_NULL`.
    pub codec_id: Option<String>,

    /// Нормализованная video metadata, если в TrackEntry были полезные поля.
    pub metadata: Option<VideoTrackMetadata>,
}

/// Результат prefix scan-а: video tracks плюс факт, что дерево `Tracks` уже найдено.
pub(crate) struct MatroskaVideoTrackScan {
    /// Video tracks, извлечённые из Matroska/WebM `Tracks`.
    pub video_tracks: HashMap<TrackId, MatroskaVideoTrack>,

    /// `true`, если prefix уже содержит `Tracks`, даже когда video tracks там нет.
    pub tracks_found: bool,
}

/// Bounded Matroska/WebM cue index по track number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MatroskaCueIndex {
    /// Список cue timestamps для каждого Matroska TrackNumber.
    cues_by_track: HashMap<TrackId, Vec<Duration>>,
}

impl MatroskaCueIndex {
    /// Возвращает ближайший cue выбранного video track-а не позже target.
    pub(crate) fn nearest_cue_before_or_at(
        &self,
        track_id: TrackId,
        target: Duration,
    ) -> Option<Duration> {
        self.cues_by_track
            .get(&track_id)?
            .iter()
            .rev()
            .copied()
            .find(|cue_timestamp| *cue_timestamp <= target)
    }

    /// Возвращает предыдущий cue выбранного track-а строго раньше указанной backend-позиции.
    pub(crate) fn nearest_cue_before(
        &self,
        track_id: TrackId,
        timestamp: Duration,
    ) -> Option<Duration> {
        self.cues_by_track
            .get(&track_id)?
            .iter()
            .rev()
            .copied()
            .find(|cue_timestamp| *cue_timestamp < timestamp)
    }

    /// Соединяет index-и, прочитанные из prefix-а и из отдельного `Cues` payload-а.
    pub(crate) fn merge(&mut self, other: MatroskaCueIndex) {
        for (track_id, mut cue_timestamps) in other.cues_by_track {
            self.cues_by_track
                .entry(track_id)
                .or_default()
                .append(&mut cue_timestamps);
        }
        self.normalize();
    }

    /// Сообщает, есть ли хотя бы один usable cue.
    pub(crate) fn is_empty(&self) -> bool {
        self.cues_by_track.is_empty()
    }

    /// Создаёт test-only index без EBML parser-а.
    #[cfg(test)]
    pub(crate) fn from_track_cues_for_tests(
        track_id: TrackId,
        cue_timestamps: impl IntoIterator<Item = Duration>,
    ) -> Self {
        let mut cue_index = Self::default();
        for cue_timestamp in cue_timestamps {
            cue_index.insert(track_id, cue_timestamp);
        }
        cue_index.normalize();
        cue_index
    }

    /// Добавляет один cue timestamp для track-а.
    fn insert(&mut self, track_id: TrackId, cue_timestamp: Duration) {
        self.cues_by_track
            .entry(track_id)
            .or_default()
            .push(cue_timestamp);
    }

    /// Сортирует, dedup-ит и удаляет пустые списки, чтобы lookup был простым и быстрым.
    fn normalize(&mut self) {
        self.cues_by_track.retain(|_, cue_timestamps| {
            cue_timestamps.sort_unstable();
            cue_timestamps.dedup();
            !cue_timestamps.is_empty()
        });
    }
}

/// План чтения Cues после prefix scan-а.
pub(crate) struct MatroskaCueReadPlan {
    /// Cues, которые уже полностью/частично попали в prefix.
    pub(crate) cue_index: MatroskaCueIndex,

    /// Абсолютная byte-позиция `Cues`, найденная через `SeekHead`.
    pub(crate) cues_absolute_position: Option<u64>,

    /// Matroska `TimestampScale` в наносекундах.
    pub(crate) timestamp_scale_ns: u64,
}

/// Читает начало файла и возвращает video tracks по Matroska TrackNumber.
pub(crate) fn extract_video_tracks_from_file(
    path: &Path,
) -> io::Result<HashMap<TrackId, MatroskaVideoTrack>> {
    let mut file = File::open(path)?;
    let metadata_prefix = read_prefix(&mut file, MATROSKA_METADATA_SCAN_LIMIT_BYTES)?;

    Ok(extract_video_tracks_from_bytes(&metadata_prefix))
}

/// Читает Matroska/WebM cues из seekable файла и возвращает bounded cue index.
pub(crate) fn extract_cue_index_from_file(path: &Path) -> io::Result<MatroskaCueIndex> {
    let mut file = File::open(path)?;
    let metadata_prefix = read_prefix(&mut file, MATROSKA_METADATA_SCAN_LIMIT_BYTES)?;
    let MatroskaCueReadPlan {
        mut cue_index,
        cues_absolute_position,
        timestamp_scale_ns,
    } = scan_cue_read_plan_from_bytes(&metadata_prefix);

    if let Some(cues_absolute_position) = cues_absolute_position {
        let cues_prefix = read_prefix_at(&mut file, cues_absolute_position)?;
        if let Some(cues_index) =
            extract_cue_index_from_cues_bytes(&cues_prefix, timestamp_scale_ns)
        {
            cue_index.merge(cues_index);
        }
    }

    Ok(cue_index)
}

/// Извлекает video tracks из уже прочитанного EBML prefix-а.
pub(crate) fn extract_video_tracks_from_bytes(
    bytes: &[u8],
) -> HashMap<TrackId, MatroskaVideoTrack> {
    scan_video_tracks_from_bytes(bytes).video_tracks
}

/// Читает cue index из уже загруженного Matroska/WebM prefix-а.
pub(crate) fn scan_cue_read_plan_from_bytes(bytes: &[u8]) -> MatroskaCueReadPlan {
    let mut position = 0;

    while let Some(header) = read_element_header(bytes, position, bytes.len()) {
        if header.id == ID_SEGMENT {
            return parse_segment_cue_read_plan(bytes, header.data_start, header.data_end);
        }
        position = header.next_position;
    }

    MatroskaCueReadPlan {
        cue_index: MatroskaCueIndex::default(),
        cues_absolute_position: None,
        timestamp_scale_ns: MATROSKA_DEFAULT_TIMESTAMP_SCALE_NS,
    }
}

/// Читает cue index из bytes, начинающихся с EBML element-а `Cues`.
pub(crate) fn extract_cue_index_from_cues_bytes(
    bytes: &[u8],
    timestamp_scale_ns: u64,
) -> Option<MatroskaCueIndex> {
    let header = read_element_header(bytes, 0, bytes.len())?;
    if header.id != ID_CUES {
        return None;
    }

    let mut cue_index = parse_cues(
        bytes,
        header.data_start,
        header.data_end,
        timestamp_scale_ns,
    );
    cue_index.normalize();
    (!cue_index.is_empty()).then_some(cue_index)
}

/// Сканирует EBML prefix и сообщает, найдено ли дерево `Tracks`.
pub(crate) fn scan_video_tracks_from_bytes(bytes: &[u8]) -> MatroskaVideoTrackScan {
    let mut video_tracks = HashMap::new();
    let mut tracks_found = false;
    let mut position = 0;

    while let Some(header) = read_element_header(bytes, position, bytes.len()) {
        if header.id == ID_SEGMENT
            && parse_segment(bytes, header.data_start, header.data_end, &mut video_tracks)
        {
            tracks_found = true;
            break;
        }
        position = header.next_position;
    }

    MatroskaVideoTrackScan {
        video_tracks,
        tracks_found,
    }
}

/// Читает bounded prefix из текущей позиции reader-а.
fn read_prefix<R>(reader: &mut R, limit_bytes: u64) -> io::Result<Vec<u8>>
where
    R: Read,
{
    let mut prefix = Vec::new();
    reader.by_ref().take(limit_bytes).read_to_end(&mut prefix)?;
    Ok(prefix)
}

/// Читает bounded `Cues` prefix из абсолютной позиции seekable reader-а.
fn read_prefix_at<R>(reader: &mut R, position: u64) -> io::Result<Vec<u8>>
where
    R: Read + Seek,
{
    reader.seek(SeekFrom::Start(position))?;
    read_prefix(reader, MATROSKA_CUES_SCAN_LIMIT_BYTES)
}

/// Ищет `Tracks` внутри Segment и прекращает scan после первого найденного дерева tracks.
fn parse_segment(
    bytes: &[u8],
    start: usize,
    end: usize,
    video_tracks: &mut HashMap<TrackId, MatroskaVideoTrack>,
) -> bool {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_TRACKS {
            parse_tracks(bytes, header.data_start, header.data_end, video_tracks);
            return true;
        }
        position = header.next_position;
    }

    false
}

/// Ищет `Info/TimestampScale`, `SeekHead -> Cues` и Cues, уже попавшие в prefix.
fn parse_segment_cue_read_plan(bytes: &[u8], start: usize, end: usize) -> MatroskaCueReadPlan {
    let mut timestamp_scale_ns = MATROSKA_DEFAULT_TIMESTAMP_SCALE_NS;
    let mut cues_relative_position = None;
    let mut cue_ranges = Vec::new();
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_INFO => {
                if let Some(scale) =
                    parse_info_timestamp_scale(bytes, header.data_start, header.data_end)
                {
                    timestamp_scale_ns = scale;
                }
            }
            ID_SEEK_HEAD => {
                if cues_relative_position.is_none() {
                    cues_relative_position =
                        parse_seek_head_cues_position(bytes, header.data_start, header.data_end);
                }
            }
            ID_CUES => cue_ranges.push((header.data_start, header.data_end)),
            _ => {}
        }
        position = header.next_position;
    }

    let mut cue_index = MatroskaCueIndex::default();
    for (cue_start, cue_end) in cue_ranges {
        cue_index.merge(parse_cues(bytes, cue_start, cue_end, timestamp_scale_ns));
    }

    MatroskaCueReadPlan {
        cue_index,
        cues_absolute_position: cues_relative_position.and_then(|relative_position| {
            u64::try_from(start)
                .ok()
                .and_then(|segment_data_start| segment_data_start.checked_add(relative_position))
        }),
        timestamp_scale_ns,
    }
}

/// Читает `TimestampScale` из `Info`; отсутствующее значение остаётся Matroska default.
fn parse_info_timestamp_scale(bytes: &[u8], start: usize, end: usize) -> Option<u64> {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_TIMESTAMP_SCALE {
            return read_unsigned(bytes, &header).filter(|timestamp_scale| *timestamp_scale > 0);
        }
        position = header.next_position;
    }

    None
}

/// Ищет в `SeekHead` позицию `Cues` относительно начала Segment payload-а.
fn parse_seek_head_cues_position(bytes: &[u8], start: usize, end: usize) -> Option<u64> {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_SEEK
            && let Some(cues_position) =
                parse_seek_entry_cues_position(bytes, header.data_start, header.data_end)
        {
            return Some(cues_position);
        }
        position = header.next_position;
    }

    None
}

/// Читает один `Seek` entry и возвращает position только для `Cues`.
fn parse_seek_entry_cues_position(bytes: &[u8], start: usize, end: usize) -> Option<u64> {
    let mut seek_id = None;
    let mut seek_position = None;
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_SEEK_ID => seek_id = read_unsigned(bytes, &header),
            ID_SEEK_POSITION => seek_position = read_unsigned(bytes, &header),
            _ => {}
        }
        position = header.next_position;
    }

    (seek_id == Some(ID_CUES)).then_some(seek_position?)
}

/// Читает все `CuePoint` children внутри `Cues`.
fn parse_cues(bytes: &[u8], start: usize, end: usize, timestamp_scale_ns: u64) -> MatroskaCueIndex {
    let mut cue_index = MatroskaCueIndex::default();
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_CUE_POINT {
            parse_cue_point(
                bytes,
                header.data_start,
                header.data_end,
                timestamp_scale_ns,
                &mut cue_index,
            );
        }
        position = header.next_position;
    }

    cue_index.normalize();
    cue_index
}

/// Читает один `CuePoint`: общий `CueTime` плюс один или несколько track positions.
fn parse_cue_point(
    bytes: &[u8],
    start: usize,
    end: usize,
    timestamp_scale_ns: u64,
    cue_index: &mut MatroskaCueIndex,
) {
    let mut cue_time = None;
    let mut cue_tracks = Vec::new();
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_CUE_TIME => cue_time = read_unsigned(bytes, &header),
            ID_CUE_TRACK_POSITIONS => {
                if let Some(track_id) =
                    parse_cue_track_position(bytes, header.data_start, header.data_end)
                {
                    cue_tracks.push(track_id);
                }
            }
            _ => {}
        }
        position = header.next_position;
    }

    let Some(cue_timestamp) =
        cue_time.and_then(|cue_time| duration_from_matroska_ticks(cue_time, timestamp_scale_ns))
    else {
        return;
    };

    for track_id in cue_tracks {
        cue_index.insert(track_id, cue_timestamp);
    }
}

/// Читает Matroska TrackNumber внутри `CueTrackPositions`.
fn parse_cue_track_position(bytes: &[u8], start: usize, end: usize) -> Option<TrackId> {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_CUE_TRACK {
            return read_unsigned(bytes, &header)
                .and_then(|track_number| u32::try_from(track_number).ok())
                .map(TrackId::new);
        }
        position = header.next_position;
    }

    None
}

/// Конвертирует Matroska timestamp ticks в wall-clock `Duration`.
fn duration_from_matroska_ticks(cue_time: u64, timestamp_scale_ns: u64) -> Option<Duration> {
    let timestamp_ns = u128::from(cue_time).checked_mul(u128::from(timestamp_scale_ns))?;
    u64::try_from(timestamp_ns).ok().map(Duration::from_nanos)
}

/// Читает все TrackEntry children внутри Matroska `Tracks`.
fn parse_tracks(
    bytes: &[u8],
    start: usize,
    end: usize,
    video_tracks: &mut HashMap<TrackId, MatroskaVideoTrack>,
) {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        if header.id == ID_TRACK_ENTRY
            && let Some((track_id, metadata)) =
                parse_track_entry(bytes, header.data_start, header.data_end)
        {
            video_tracks.insert(track_id, metadata);
        }
        position = header.next_position;
    }
}

/// Читает один TrackEntry и возвращает metadata только для video track-а.
fn parse_track_entry(
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Option<(TrackId, MatroskaVideoTrack)> {
    let mut track_number = None;
    let mut track_type = None;
    let mut codec_id = None;
    let mut video_metadata = VideoTrackMetadata::empty();
    let mut has_video_element = false;
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_TRACK_NUMBER => track_number = read_unsigned(bytes, &header),
            ID_TRACK_TYPE => track_type = read_unsigned(bytes, &header),
            ID_CODEC_ID => codec_id = read_ascii(bytes, &header),
            ID_VIDEO => {
                has_video_element = true;
                video_metadata = parse_video(bytes, header.data_start, header.data_end);
            }
            _ => {}
        }
        position = header.next_position;
    }

    let track_number = track_number.and_then(|number| u32::try_from(number).ok())?;
    let is_video_track = track_type == Some(1) || has_video_element;
    if !is_video_track {
        return None;
    }

    if codec_id
        .as_deref()
        .is_some_and(|codec_id| codec_id != "V_VP9")
    {
        tracing::debug!(
            track = track_number,
            ?codec_id,
            "Video track metadata не VP9"
        );
    }

    Some((
        TrackId::new(track_number),
        MatroskaVideoTrack {
            codec_id,
            metadata: video_metadata.into_option(),
        },
    ))
}

/// Читает Matroska `Video` metadata, включая nested `Colour`.
fn parse_video(bytes: &[u8], start: usize, end: usize) -> VideoTrackMetadata {
    let mut metadata = VideoTrackMetadata::empty();
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_PIXEL_WIDTH => {
                metadata.coded_width =
                    read_unsigned(bytes, &header).and_then(|value| u32::try_from(value).ok());
            }
            ID_PIXEL_HEIGHT => {
                metadata.coded_height =
                    read_unsigned(bytes, &header).and_then(|value| u32::try_from(value).ok());
            }
            ID_COLOUR => {
                apply_colour_metadata(bytes, header.data_start, header.data_end, &mut metadata);
            }
            _ => {}
        }
        position = header.next_position;
    }

    metadata
}

/// Читает Matroska `Colour` fields и сразу нормализует их в codec-core model.
fn apply_colour_metadata(
    bytes: &[u8],
    start: usize,
    end: usize,
    video_metadata: &mut VideoTrackMetadata,
) {
    let mut colour = MatroskaColourFields::default();
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_MATRIX_COEFFICIENTS => colour.matrix = read_unsigned(bytes, &header),
            ID_BITS_PER_CHANNEL => colour.bits_per_channel = read_unsigned(bytes, &header),
            ID_CHROMA_SUBSAMPLING_HORZ => {
                colour.chroma_subsampling_horz = read_unsigned(bytes, &header);
            }
            ID_CHROMA_SUBSAMPLING_VERT => {
                colour.chroma_subsampling_vert = read_unsigned(bytes, &header);
            }
            ID_RANGE => colour.range = read_unsigned(bytes, &header),
            ID_TRANSFER_CHARACTERISTICS => colour.transfer = read_unsigned(bytes, &header),
            ID_PRIMARIES => colour.primaries = read_unsigned(bytes, &header),
            ID_MAX_CLL => colour.max_cll = read_unsigned(bytes, &header),
            ID_MAX_FALL => colour.max_fall = read_unsigned(bytes, &header),
            ID_MASTERING_METADATA => {
                parse_mastering_metadata(bytes, header.data_start, header.data_end, &mut colour);
            }
            _ => {}
        }
        position = header.next_position;
    }

    video_metadata.bit_depth = colour
        .bits_per_channel
        .and_then(|value| u8::try_from(value).ok())
        .and_then(BitDepth::from_bits);
    video_metadata.chroma = colour
        .chroma_subsampling_horz
        .zip(colour.chroma_subsampling_vert)
        .and_then(|(horizontal, vertical)| {
            ChromaSubsampling::from_matroska_subsampling(horizontal, vertical)
        });
    video_metadata.color = colour.into_color_metadata();
}

/// Читает только luminance fields из SMPTE 2086 mastering metadata.
fn parse_mastering_metadata(
    bytes: &[u8],
    start: usize,
    end: usize,
    colour: &mut MatroskaColourFields,
) {
    let mut position = start;

    while let Some(header) = read_element_header(bytes, position, end) {
        match header.id {
            ID_LUMINANCE_MAX => colour.luminance_max = read_float(bytes, &header),
            ID_LUMINANCE_MIN => colour.luminance_min = read_float(bytes, &header),
            _ => {}
        }
        position = header.next_position;
    }
}

/// Набор сырых Matroska Colour fields до нормализации.
#[derive(Debug, Default)]
struct MatroskaColourFields {
    matrix: Option<u64>,
    bits_per_channel: Option<u64>,
    chroma_subsampling_horz: Option<u64>,
    chroma_subsampling_vert: Option<u64>,
    range: Option<u64>,
    transfer: Option<u64>,
    primaries: Option<u64>,
    max_cll: Option<u64>,
    max_fall: Option<u64>,
    luminance_max: Option<f64>,
    luminance_min: Option<f64>,
}

impl MatroskaColourFields {
    /// Возвращает typed color metadata, если `Colour` содержал хотя бы одно полезное поле.
    fn into_color_metadata(self) -> Option<VideoColorMetadata> {
        let has_color_fields = self.matrix.is_some()
            || self.range.is_some()
            || self.transfer.is_some()
            || self.primaries.is_some()
            || self.max_cll.is_some()
            || self.max_fall.is_some()
            || self.luminance_max.is_some()
            || self.luminance_min.is_some();

        if !has_color_fields {
            return None;
        }

        let primaries = self
            .primaries
            .map(ColorPrimaries::from_h273_value)
            .unwrap_or(ColorPrimaries::Unknown);
        let transfer = self
            .transfer
            .map(TransferFunction::from_h273_value)
            .unwrap_or(TransferFunction::Unknown);
        let hdr_metadata = build_hdr_metadata(&self, primaries, transfer);

        Some(VideoColorMetadata::container(
            self.range
                .map(ColorRange::from_matroska_value)
                .unwrap_or(ColorRange::Unknown),
            self.matrix
                .map(MatrixCoefficients::from_h273_value)
                .unwrap_or(MatrixCoefficients::Unknown),
            primaries,
            transfer,
            hdr_metadata,
        ))
    }
}

/// Собирает HDR side metadata без обязательности для strict core validation.
fn build_hdr_metadata(
    colour: &MatroskaColourFields,
    primaries: ColorPrimaries,
    transfer: TransferFunction,
) -> Option<HdrMetadata> {
    let max_content_light_level_nits = colour.max_cll.and_then(|value| u32::try_from(value).ok());
    let max_frame_average_light_level_nits =
        colour.max_fall.and_then(|value| u32::try_from(value).ok());
    let max_luminance_nits = colour.luminance_max.map(|value| value as f32);
    let min_luminance_nits = colour.luminance_min.map(|value| value as f32);
    let has_hdr_side_metadata = max_content_light_level_nits.is_some()
        || max_frame_average_light_level_nits.is_some()
        || max_luminance_nits.is_some()
        || min_luminance_nits.is_some();

    has_hdr_side_metadata.then_some(HdrMetadata {
        color_primaries: primaries,
        transfer_function: transfer,
        max_luminance_nits,
        min_luminance_nits,
        max_content_light_level_nits,
        max_frame_average_light_level_nits,
    })
}

/// Header одного EBML element-а в пределах уже прочитанного prefix-а.
#[derive(Debug, Clone, Copy)]
struct ElementHeader {
    id: u64,
    data_start: usize,
    data_end: usize,
    data_complete: bool,
    next_position: usize,
}

/// Читает EBML element header без выделений и без рекурсивной state machine.
fn read_element_header(bytes: &[u8], position: usize, parent_end: usize) -> Option<ElementHeader> {
    if position >= parent_end {
        return None;
    }

    let (id, id_width) = read_element_id(bytes, position)?;
    let size_position = position.checked_add(id_width)?;
    let (data_size, size_width) = read_element_size(bytes, size_position)?;
    let data_start = size_position.checked_add(size_width)?;
    let declared_data_end = data_size
        .and_then(|size| usize::try_from(size).ok())
        .and_then(|size| data_start.checked_add(size));

    if data_start > parent_end {
        return None;
    }

    let data_complete = declared_data_end.is_some_and(|data_end| data_end <= parent_end);
    let data_end = declared_data_end.unwrap_or(parent_end).min(parent_end);

    Some(ElementHeader {
        id,
        data_start,
        data_end,
        data_complete,
        next_position: data_end,
    })
}

/// Читает EBML element id, сохраняя marker bits как часть id.
fn read_element_id(bytes: &[u8], position: usize) -> Option<(u64, usize)> {
    let first_byte = *bytes.get(position)?;
    let width = ebml_vint_width(first_byte)?;
    if width > 4 || position.checked_add(width)? > bytes.len() {
        return None;
    }

    let id = bytes[position..position + width]
        .iter()
        .fold(0u64, |accumulator, byte| {
            (accumulator << 8) | u64::from(*byte)
        });
    Some((id, width))
}

/// Читает EBML size vint и удаляет marker bit.
fn read_element_size(bytes: &[u8], position: usize) -> Option<(Option<u64>, usize)> {
    let first_byte = *bytes.get(position)?;
    let width = ebml_vint_width(first_byte)?;
    if width > 8 || position.checked_add(width)? > bytes.len() {
        return None;
    }

    let raw_value = bytes[position..position + width]
        .iter()
        .fold(0u64, |accumulator, byte| {
            (accumulator << 8) | u64::from(*byte)
        });
    let value_bits = 7 * width;
    let value_mask = (1u64 << value_bits) - 1;
    let value = raw_value & value_mask;
    let unknown_size_value = value_mask;

    if value == unknown_size_value {
        Some((None, width))
    } else {
        Some((Some(value), width))
    }
}

/// Возвращает ширину EBML vint по первому byte.
fn ebml_vint_width(first_byte: u8) -> Option<usize> {
    let leading_zeroes = first_byte.leading_zeros() as usize;
    let width = leading_zeroes.checked_add(1)?;
    (width <= 8).then_some(width)
}

/// Читает unsigned integer payload в big-endian форме.
fn read_unsigned(bytes: &[u8], header: &ElementHeader) -> Option<u64> {
    if !header.data_complete {
        return None;
    }

    if header.data_start >= header.data_end || header.data_end - header.data_start > 8 {
        return None;
    }

    Some(
        bytes[header.data_start..header.data_end]
            .iter()
            .fold(0u64, |accumulator, byte| {
                (accumulator << 8) | u64::from(*byte)
            }),
    )
}

/// Читает ASCII payload, который нужен только для CodecID diagnostics.
fn read_ascii(bytes: &[u8], header: &ElementHeader) -> Option<String> {
    if !header.data_complete {
        return None;
    }

    std::str::from_utf8(&bytes[header.data_start..header.data_end])
        .ok()
        .map(ToOwned::to_owned)
}

/// Читает Matroska float payload: 32-bit или 64-bit big-endian.
fn read_float(bytes: &[u8], header: &ElementHeader) -> Option<f64> {
    if !header.data_complete {
        return None;
    }

    match header.data_end.checked_sub(header.data_start)? {
        4 => {
            let value =
                f32::from_be_bytes(bytes[header.data_start..header.data_end].try_into().ok()?);
            Some(f64::from(value))
        }
        8 => Some(f64::from_be_bytes(
            bytes[header.data_start..header.data_end].try_into().ok()?,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bt2020_pq_limited_colour_from_track_entry() {
        let colour = master(
            ID_COLOUR,
            vec![
                unsigned(ID_MATRIX_COEFFICIENTS, 9),
                unsigned(ID_BITS_PER_CHANNEL, 10),
                unsigned(ID_CHROMA_SUBSAMPLING_HORZ, 1),
                unsigned(ID_CHROMA_SUBSAMPLING_VERT, 1),
                unsigned(ID_RANGE, 1),
                unsigned(ID_TRANSFER_CHARACTERISTICS, 16),
                unsigned(ID_PRIMARIES, 9),
                unsigned(ID_MAX_CLL, 1000),
                unsigned(ID_MAX_FALL, 400),
            ],
        );
        let video = master(
            ID_VIDEO,
            vec![
                unsigned(ID_PIXEL_WIDTH, 3840),
                unsigned(ID_PIXEL_HEIGHT, 2160),
                colour,
            ],
        );
        let track_entry = master(
            ID_TRACK_ENTRY,
            vec![
                unsigned(ID_TRACK_NUMBER, 1),
                unsigned(ID_TRACK_TYPE, 1),
                ascii(ID_CODEC_ID, "V_VP9"),
                video,
            ],
        );
        let bytes = master(ID_SEGMENT, vec![master(ID_TRACKS, vec![track_entry])]);

        let video_tracks_by_track = extract_video_tracks_from_bytes(&bytes);
        let video_track = video_tracks_by_track
            .get(&TrackId::new(1))
            .expect("video track должен быть извлечён");
        let metadata = video_track
            .metadata
            .as_ref()
            .expect("video metadata должен быть извлечён");
        let color = metadata
            .color
            .as_ref()
            .expect("Colour metadata должна быть");

        assert_eq!(video_track.codec_id.as_deref(), Some("V_VP9"));
        assert_eq!(metadata.coded_width, Some(3840));
        assert_eq!(metadata.coded_height, Some(2160));
        assert_eq!(metadata.bit_depth, Some(BitDepth::Ten));
        assert_eq!(metadata.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(color.range, ColorRange::Limited);
        assert_eq!(color.transfer, TransferFunction::Pq);
        assert_eq!(color.primaries, ColorPrimaries::Bt2020);
        assert_eq!(color.matrix, MatrixCoefficients::Bt2020);
        assert_eq!(
            color
                .hdr_metadata
                .as_ref()
                .and_then(|metadata| metadata.max_content_light_level_nits),
            Some(1000)
        );
    }

    #[test]
    fn extracts_tracks_when_segment_declared_size_exceeds_prescan_prefix() {
        let colour = master(
            ID_COLOUR,
            vec![
                unsigned(ID_BITS_PER_CHANNEL, 10),
                unsigned(ID_TRANSFER_CHARACTERISTICS, 16),
                unsigned(ID_PRIMARIES, 9),
            ],
        );
        let video = master(ID_VIDEO, vec![unsigned(ID_PIXEL_WIDTH, 3840), colour]);
        let track_entry = master(
            ID_TRACK_ENTRY,
            vec![
                unsigned(ID_TRACK_NUMBER, 1),
                unsigned(ID_TRACK_TYPE, 1),
                ascii(ID_CODEC_ID, "V_VP9"),
                video,
            ],
        );
        let tracks = master(ID_TRACKS, vec![track_entry]);
        let declared_segment_size = tracks.len() + 4096;
        let bytes = element_with_declared_size(ID_SEGMENT, declared_segment_size, &tracks);

        let video_tracks_by_track = extract_video_tracks_from_bytes(&bytes);
        let video_track = video_tracks_by_track
            .get(&TrackId::new(1))
            .expect("video track должен читаться из prefix-а большого Segment");
        let metadata = video_track
            .metadata
            .as_ref()
            .expect("video metadata должна читаться из prefix-а большого Segment");
        let color = metadata
            .color
            .as_ref()
            .expect("Colour metadata должна сохраниться из prefix-а");

        assert_eq!(metadata.coded_width, Some(3840));
        assert_eq!(metadata.bit_depth, Some(BitDepth::Ten));
        assert_eq!(color.transfer, TransferFunction::Pq);
        assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    }

    #[test]
    fn extracts_codec_id_even_without_optional_video_metadata() {
        let video = master(ID_VIDEO, vec![]);
        let track_entry = master(
            ID_TRACK_ENTRY,
            vec![
                unsigned(ID_TRACK_NUMBER, 2),
                unsigned(ID_TRACK_TYPE, 1),
                ascii(ID_CODEC_ID, "V_VP9"),
                video,
            ],
        );
        let bytes = master(ID_SEGMENT, vec![master(ID_TRACKS, vec![track_entry])]);

        let video_tracks_by_track = extract_video_tracks_from_bytes(&bytes);
        let video_track = video_tracks_by_track
            .get(&TrackId::new(2))
            .expect("video track должен быть извлечён по TrackType");

        assert_eq!(video_track.codec_id.as_deref(), Some("V_VP9"));
        assert!(video_track.metadata.is_none());
    }

    #[test]
    fn scan_reports_tracks_found_for_audio_only_tracks() {
        let track_entry = master(
            ID_TRACK_ENTRY,
            vec![
                unsigned(ID_TRACK_NUMBER, 3),
                unsigned(ID_TRACK_TYPE, 2),
                ascii(ID_CODEC_ID, "A_OPUS"),
            ],
        );
        let bytes = master(ID_SEGMENT, vec![master(ID_TRACKS, vec![track_entry])]);

        let scan = scan_video_tracks_from_bytes(&bytes);

        assert!(scan.tracks_found);
        assert!(scan.video_tracks.is_empty());
    }

    #[test]
    fn cue_index_uses_timestamp_scale_and_selected_track() {
        let info = master(ID_INFO, vec![unsigned(ID_TIMESTAMP_SCALE, 1_000_000_000)]);
        let cues = master(
            ID_CUES,
            vec![
                cue_point(2, vec![1, 2]),
                cue_point(5, vec![2]),
                cue_point(8, vec![1]),
            ],
        );
        let bytes = master(ID_SEGMENT, vec![info, cues]);

        let cue_plan = scan_cue_read_plan_from_bytes(&bytes);

        assert_eq!(
            cue_plan
                .cue_index
                .nearest_cue_before_or_at(TrackId::new(1), Duration::from_secs(7)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            cue_plan
                .cue_index
                .nearest_cue_before_or_at(TrackId::new(1), Duration::from_secs(9)),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            cue_plan
                .cue_index
                .nearest_cue_before_or_at(TrackId::new(2), Duration::from_secs(7)),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn cue_read_plan_uses_seek_head_position_for_external_cues() {
        let seek_head = seek_head_to_cues(512);
        let bytes = element_with_declared_size(ID_SEGMENT, 4096, &seek_head);
        let segment_data_start = bytes.len() - seek_head.len();

        let cue_plan = scan_cue_read_plan_from_bytes(&bytes);

        assert_eq!(
            cue_plan.cues_absolute_position,
            Some(u64::try_from(segment_data_start + 512).expect("test offset fits"))
        );
        assert!(cue_plan.cue_index.is_empty());
    }

    #[test]
    fn extract_cue_index_from_cues_bytes_rejects_non_cues_payload() {
        let bytes = master(ID_TRACKS, Vec::new());

        let cue_index =
            extract_cue_index_from_cues_bytes(&bytes, MATROSKA_DEFAULT_TIMESTAMP_SCALE_NS);

        assert!(cue_index.is_none());
    }

    #[test]
    fn extracts_external_cues_from_h265_mkv_fixture() {
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../test-assets/h265/4k60fps/LXb3EKWsInQ_2160p60_sdr_hevc_main_8bit_mkv_118mbps_aac_20s.mkv",
        );

        let cue_index =
            extract_cue_index_from_file(&fixture_path).expect("real H.265 MKV cues should parse");

        assert_eq!(
            cue_index.nearest_cue_before_or_at(TrackId::new(1), Duration::from_millis(9_999),),
            Some(Duration::from_millis(8_045))
        );
    }

    fn master(id: u64, children: Vec<Vec<u8>>) -> Vec<u8> {
        let payload = children.into_iter().flatten().collect::<Vec<_>>();
        element(id, &payload)
    }

    fn cue_point(cue_time: u64, tracks: Vec<u64>) -> Vec<u8> {
        let mut children = vec![unsigned(ID_CUE_TIME, cue_time)];
        children.extend(tracks.into_iter().map(|track_number| {
            master(
                ID_CUE_TRACK_POSITIONS,
                vec![unsigned(ID_CUE_TRACK, track_number)],
            )
        }));
        master(ID_CUE_POINT, children)
    }

    fn seek_head_to_cues(relative_position: u64) -> Vec<u8> {
        master(
            ID_SEEK_HEAD,
            vec![master(
                ID_SEEK,
                vec![
                    binary(ID_SEEK_ID, &encode_id(ID_CUES)),
                    unsigned(ID_SEEK_POSITION, relative_position),
                ],
            )],
        )
    }

    fn unsigned(id: u64, value: u64) -> Vec<u8> {
        let mut payload = Vec::new();
        let mut started = false;
        for byte in value.to_be_bytes() {
            if byte != 0 || started {
                started = true;
                payload.push(byte);
            }
        }
        if payload.is_empty() {
            payload.push(0);
        }
        element(id, &payload)
    }

    fn ascii(id: u64, value: &str) -> Vec<u8> {
        element(id, value.as_bytes())
    }

    fn binary(id: u64, value: &[u8]) -> Vec<u8> {
        element(id, value)
    }

    fn element(id: u64, payload: &[u8]) -> Vec<u8> {
        element_with_declared_size(id, payload.len(), payload)
    }

    fn element_with_declared_size(id: u64, declared_size: usize, payload: &[u8]) -> Vec<u8> {
        let mut bytes = encode_id(id);
        bytes.extend(encode_size(declared_size));
        bytes.extend_from_slice(payload);
        bytes
    }

    fn encode_id(id: u64) -> Vec<u8> {
        let width = if id <= 0xff {
            1
        } else if id <= 0xffff {
            2
        } else if id <= 0xff_ffff {
            3
        } else {
            4
        };
        id.to_be_bytes()[8 - width..].to_vec()
    }

    fn encode_size(size: usize) -> Vec<u8> {
        if size < 0x7f {
            return vec![0x80 | size as u8];
        }
        if size < 0x3fff {
            return ((0x4000 | size) as u16).to_be_bytes().to_vec();
        }
        if size < 0x1f_ffff {
            let value = 0x20_0000 | size;
            return vec![
                ((value >> 16) & 0xff) as u8,
                ((value >> 8) & 0xff) as u8,
                (value & 0xff) as u8,
            ];
        }
        panic!("test EBML payload too large");
    }
}
