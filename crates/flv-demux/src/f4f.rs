use bytes::Bytes;
use demux_api::OrderedSegmentKind;

use crate::{FlvDemuxError, FlvDemuxOptions};

const BOX_HEADER_BYTES: usize = 8;
const LARGE_BOX_HEADER_BYTES: usize = 16;
const FULL_BOX_HEADER_BYTES: usize = 4;

/// Проверенный результат одного F4F fragment-а; наружу выходит только mdat payload.
pub(crate) struct ParsedF4fSegment {
    pub(crate) media_payloads: Vec<Bytes>,
}

#[derive(Clone, Copy)]
struct IsoBox<'a> {
    box_type: [u8; 4],
    payload: &'a [u8],
    payload_offset: usize,
}

/// Валидирует bounded ISO envelope доставленного HDS media fragment.
///
/// Bootstrap (`abst`) принадлежит HDS provider и обычно приходит отдельно от media
/// fragment. Старые/spec-complete источники могут повторять его внутри fragment — такой
/// вариант остаётся допустимым, но inline bootstrap обязательно проходит полную проверку.
pub(crate) fn parse_f4f_segment(
    sequence: u64,
    kind: OrderedSegmentKind,
    bytes: Bytes,
    options: FlvDemuxOptions,
) -> Result<ParsedF4fSegment, FlvDemuxError> {
    if kind != OrderedSegmentKind::Media {
        return Err(malformed(
            sequence,
            "standalone initialization не является F4F media fragment",
        ));
    }
    if bytes.len() > options.fragment_bytes.get() {
        return Err(FlvDemuxError::FragmentTooLarge {
            sequence,
            actual_bytes: bytes.len(),
            limit_bytes: options.fragment_bytes.get(),
        });
    }

    let mut box_budget = options.fragment_boxes.get();
    let boxes = parse_boxes(sequence, &bytes, &mut box_budget)?;
    if !matches!(boxes.len(), 3 | 4)
        || boxes.first().map(|parsed_box| parsed_box.box_type) != Some(*b"afra")
    {
        return Err(malformed(
            sequence,
            "F4F fragment требует afra первым, обязательные moof/mdat и не более одного optional abst",
        ));
    }
    let find_unique = |box_type: [u8; 4]| {
        let mut matches = boxes
            .iter()
            .copied()
            .filter(|parsed_box| parsed_box.box_type == box_type);
        let first = matches.next();
        first.filter(|_| matches.next().is_none())
    };
    let afra = find_unique(*b"afra");
    let abst = find_unique(*b"abst");
    let moof = find_unique(*b"moof");
    let mdat = find_unique(*b"mdat");
    let (Some(afra), Some(moof), Some(mdat)) = (afra, moof, mdat) else {
        return Err(malformed(
            sequence,
            "F4F fragment требует ровно по одному afra, moof и mdat",
        ));
    };

    // Любой четвёртый box обязан быть единственным `abst`: так unknown и duplicate
    // top-level boxes не маскируются под поддерживаемую HDS topology.
    let expected_box_count = 3 + usize::from(abst.is_some());
    if boxes.len() != expected_box_count {
        return Err(malformed(
            sequence,
            "F4F fragment допускает только один optional abst поверх afra/moof/mdat",
        ));
    }

    validate_afra(sequence, afra.payload, options)?;
    if let Some(abst) = abst {
        validate_abst(sequence, abst.payload, options, &mut box_budget)?;
    }
    validate_moof(sequence, moof.payload, options, &mut box_budget)?;
    if mdat.payload.is_empty() {
        return Err(malformed(sequence, "пустой mdat не содержит FLV tags"));
    }

    let payload_end = mdat
        .payload_offset
        .checked_add(mdat.payload.len())
        .ok_or_else(|| malformed(sequence, "mdat payload end overflow"))?;
    Ok(ParsedF4fSegment {
        media_payloads: vec![bytes.slice(mdat.payload_offset..payload_end)],
    })
}

fn parse_boxes<'a>(
    sequence: u64,
    bytes: &'a [u8],
    box_budget: &mut usize,
) -> Result<Vec<IsoBox<'a>>, FlvDemuxError> {
    let mut cursor = 0_usize;
    let mut boxes = Vec::new();
    while cursor < bytes.len() {
        consume_box_budget(sequence, box_budget)?;
        let (parsed_box, box_end) = parse_box_at(sequence, bytes, cursor)?;
        boxes.push(parsed_box);
        cursor = box_end;
    }
    Ok(boxes)
}

fn consume_box_budget(sequence: u64, box_budget: &mut usize) -> Result<(), FlvDemuxError> {
    *box_budget = box_budget
        .checked_sub(1)
        .ok_or_else(|| malformed(sequence, "слишком много ISO boxes во всём fragment"))?;
    Ok(())
}

fn parse_box_at<'a>(
    sequence: u64,
    bytes: &'a [u8],
    cursor: usize,
) -> Result<(IsoBox<'a>, usize), FlvDemuxError> {
    let header = bytes
        .get(cursor..cursor.saturating_add(BOX_HEADER_BYTES))
        .ok_or_else(|| malformed(sequence, "ISO box header обрезан"))?;
    let size32 = u32::from_be_bytes(header[..4].try_into().expect("exact slice"));
    let box_type = header[4..8].try_into().expect("exact slice");
    let (box_size, header_size) = match size32 {
        0 => {
            return Err(malformed(
                sequence,
                "box size=0 запрещён в bounded fragment",
            ));
        }
        1 => {
            let large = bytes
                .get(cursor + BOX_HEADER_BYTES..cursor + LARGE_BOX_HEADER_BYTES)
                .ok_or_else(|| malformed(sequence, "large-size box header обрезан"))?;
            let size64 = u64::from_be_bytes(large.try_into().expect("exact slice"));
            let converted = usize::try_from(size64)
                .map_err(|_| malformed(sequence, "64-bit box size не помещается в usize"))?;
            (converted, LARGE_BOX_HEADER_BYTES)
        }
        value => (
            usize::try_from(value).expect("u32 fits usize"),
            BOX_HEADER_BYTES,
        ),
    };
    if box_size < header_size {
        return Err(malformed(sequence, "box size меньше собственного header"));
    }
    let box_end = cursor
        .checked_add(box_size)
        .ok_or_else(|| malformed(sequence, "box end overflow"))?;
    let payload_offset = cursor + header_size;
    let payload = bytes
        .get(payload_offset..box_end)
        .ok_or_else(|| malformed(sequence, "box выходит за fragment boundary"))?;
    Ok((
        IsoBox {
            box_type,
            payload,
            payload_offset,
        },
        box_end,
    ))
}

fn validate_afra(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
) -> Result<(), FlvDemuxError> {
    let mut cursor = PayloadCursor::new(sequence, "afra", payload);
    cursor.read_full_box(&[0, 1], 0)?;
    let shape = cursor.read_u8()?;
    if shape & 0x1f != 0 {
        return Err(malformed(
            sequence,
            "afra reserved bits должны быть нулевыми",
        ));
    }
    let long_ids = shape & 0x80 != 0;
    let long_offsets = shape & 0x40 != 0;
    let has_global_entries = shape & 0x20 != 0;
    if cursor.read_u32()? == 0 {
        return Err(malformed(sequence, "afra TimeScale не может быть нулём"));
    }
    let local_count =
        cursor.read_bounded_count(options.index_entries.get(), "afra local entries")?;
    let local_entry_bytes = if long_offsets { 16 } else { 12 };
    cursor.skip_repeated(local_count, local_entry_bytes)?;
    if has_global_entries {
        let global_count =
            cursor.read_bounded_count(options.index_entries.get(), "afra global entries")?;
        let identifier_bytes = if long_ids { 8 } else { 4 };
        let offset_bytes = if long_offsets { 16 } else { 8 };
        cursor.skip_repeated(global_count, 8 + identifier_bytes + offset_bytes)?;
    }
    cursor.finish()
}

fn validate_abst(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
    box_budget: &mut usize,
) -> Result<(), FlvDemuxError> {
    let mut cursor = PayloadCursor::new(sequence, "abst", payload);
    cursor.read_full_box(&[0, 1], 0)?;
    cursor.skip(4)?;
    let presentation_flags = cursor.read_u8()?;
    if presentation_flags & 0x0f != 0 {
        return Err(malformed(
            sequence,
            "abst reserved bits должны быть нулевыми",
        ));
    }
    if presentation_flags >> 6 > 1 {
        return Err(malformed(sequence, "abst Profile зарезервирован"));
    }
    if cursor.read_u32()? == 0 {
        return Err(malformed(sequence, "abst TimeScale не может быть нулём"));
    }
    cursor.skip(16)?;
    cursor.read_string(options.metadata_string_bytes.get())?;
    let server_count = usize::from(cursor.read_u8()?);
    cursor.read_strings(server_count, options.metadata_string_bytes.get())?;
    let quality_count = usize::from(cursor.read_u8()?);
    cursor.read_strings(quality_count, options.metadata_string_bytes.get())?;
    cursor.read_string(options.metadata_string_bytes.get())?;
    cursor.read_string(options.metadata_string_bytes.get())?;

    let segment_table_count = usize::from(cursor.read_u8()?);
    validate_table_count(sequence, segment_table_count, options, "asrt")?;
    for _ in 0..segment_table_count {
        let table = cursor.read_box(box_budget)?;
        if table.box_type != *b"asrt" {
            return Err(malformed(sequence, "abst ожидает asrt table"));
        }
        validate_asrt(sequence, table.payload, options)?;
    }

    let fragment_table_count = usize::from(cursor.read_u8()?);
    validate_table_count(sequence, fragment_table_count, options, "afrt")?;
    for _ in 0..fragment_table_count {
        let table = cursor.read_box(box_budget)?;
        if table.box_type != *b"afrt" {
            return Err(malformed(sequence, "abst ожидает afrt table"));
        }
        validate_afrt(sequence, table.payload, options)?;
    }
    cursor.finish()
}

fn validate_table_count(
    sequence: u64,
    count: usize,
    options: FlvDemuxOptions,
    name: &'static str,
) -> Result<(), FlvDemuxError> {
    if count == 0 || count > options.fragment_boxes.get() {
        return Err(malformed(
            sequence,
            format!("abst {name} table count вне bounded policy"),
        ));
    }
    Ok(())
}

fn validate_asrt(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
) -> Result<(), FlvDemuxError> {
    let mut cursor = PayloadCursor::new(sequence, "asrt", payload);
    cursor.read_full_box(&[0, 1], 1)?;
    let quality_count = usize::from(cursor.read_u8()?);
    cursor.read_strings(quality_count, options.metadata_string_bytes.get())?;
    let entry_count = cursor.read_bounded_count(options.index_entries.get(), "asrt entries")?;
    if entry_count == 0 {
        return Err(malformed(sequence, "asrt требует хотя бы одну run entry"));
    }
    cursor.skip_repeated(entry_count, 8)?;
    cursor.finish()
}

fn validate_afrt(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
) -> Result<(), FlvDemuxError> {
    let mut cursor = PayloadCursor::new(sequence, "afrt", payload);
    cursor.read_full_box(&[0, 1], 1)?;
    if cursor.read_u32()? == 0 {
        return Err(malformed(sequence, "afrt TimeScale не может быть нулём"));
    }
    let quality_count = usize::from(cursor.read_u8()?);
    cursor.read_strings(quality_count, options.metadata_string_bytes.get())?;
    let entry_count = cursor.read_bounded_count(options.index_entries.get(), "afrt entries")?;
    if entry_count == 0 {
        return Err(malformed(sequence, "afrt требует хотя бы одну run entry"));
    }
    for _ in 0..entry_count {
        cursor.skip(12)?;
        if cursor.read_u32()? == 0 {
            cursor.skip(1)?;
        }
    }
    cursor.finish()
}

fn validate_moof(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
    box_budget: &mut usize,
) -> Result<(), FlvDemuxError> {
    let children = parse_boxes(sequence, payload, box_budget)?;
    let Some((movie_header, tracks)) = children.split_first() else {
        return Err(malformed(sequence, "moof не содержит mfhd"));
    };
    if movie_header.box_type != *b"mfhd" || tracks.is_empty() {
        return Err(malformed(sequence, "moof требует mfhd и хотя бы один traf"));
    }
    validate_fixed_full_box(sequence, "mfhd", movie_header.payload, &[0], 0, 4)?;
    for track in tracks {
        if track.box_type != *b"traf" {
            return Err(malformed(
                sequence,
                "после mfhd в moof разрешены только traf",
            ));
        }
        validate_traf(sequence, track.payload, options, box_budget)?;
    }
    Ok(())
}

fn validate_traf(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
    box_budget: &mut usize,
) -> Result<(), FlvDemuxError> {
    let children = parse_boxes(sequence, payload, box_budget)?;
    let Some(track_header) = children.first() else {
        return Err(malformed(sequence, "traf не содержит tfhd"));
    };
    if track_header.box_type != *b"tfhd" {
        return Err(malformed(sequence, "первым child в traf должен быть tfhd"));
    }
    validate_tfhd(sequence, track_header.payload)?;
    let mut saw_run = false;
    for child in &children[1..] {
        match &child.box_type {
            b"tfhd" => return Err(malformed(sequence, "traf содержит повторный tfhd")),
            b"trun" => {
                validate_trun(sequence, child.payload, options)?;
                saw_run = true;
            }
            _ => {}
        }
    }
    if !saw_run {
        return Err(malformed(sequence, "traf не содержит trun"));
    }
    Ok(())
}

fn validate_tfhd(sequence: u64, payload: &[u8]) -> Result<(), FlvDemuxError> {
    const OPTIONAL_FIELD_FLAGS: u32 = 0x000001 | 0x000002 | 0x000008 | 0x000010 | 0x000020;
    const SEMANTIC_FLAGS: u32 = 0x010000 | 0x020000;
    let mut cursor = PayloadCursor::new(sequence, "tfhd", payload);
    let flags = cursor.read_full_box(&[0], OPTIONAL_FIELD_FLAGS | SEMANTIC_FLAGS)?;
    if cursor.read_u32()? == 0 {
        return Err(malformed(sequence, "tfhd TrackID не может быть нулём"));
    }
    let mut optional_bytes = 0_usize;
    for (flag, field_bytes) in [
        (0x000001, 8),
        (0x000002, 4),
        (0x000008, 4),
        (0x000010, 4),
        (0x000020, 4),
    ] {
        if flags & flag != 0 {
            optional_bytes += field_bytes;
        }
    }
    cursor.skip(optional_bytes)?;
    cursor.finish()
}

fn validate_trun(
    sequence: u64,
    payload: &[u8],
    options: FlvDemuxOptions,
) -> Result<(), FlvDemuxError> {
    const HEADER_FLAGS: u32 = 0x000001 | 0x000004;
    const SAMPLE_FLAGS: u32 = 0x000100 | 0x000200 | 0x000400 | 0x000800;
    let mut cursor = PayloadCursor::new(sequence, "trun", payload);
    let flags = cursor.read_full_box(&[0, 1], HEADER_FLAGS | SAMPLE_FLAGS)?;
    let sample_count = cursor.read_bounded_count(options.index_entries.get(), "trun samples")?;
    if flags & 0x000001 != 0 {
        cursor.skip(4)?;
    }
    if flags & 0x000004 != 0 {
        cursor.skip(4)?;
    }
    let per_sample_bytes = [0x000100, 0x000200, 0x000400, 0x000800]
        .into_iter()
        .filter(|flag| flags & flag != 0)
        .count()
        * 4;
    cursor.skip_repeated(sample_count, per_sample_bytes)?;
    cursor.finish()
}

fn validate_fixed_full_box(
    sequence: u64,
    name: &'static str,
    payload: &[u8],
    versions: &[u8],
    allowed_flags: u32,
    remaining_bytes: usize,
) -> Result<(), FlvDemuxError> {
    let mut cursor = PayloadCursor::new(sequence, name, payload);
    cursor.read_full_box(versions, allowed_flags)?;
    cursor.skip(remaining_bytes)?;
    cursor.finish()
}

struct PayloadCursor<'a> {
    sequence: u64,
    name: &'static str,
    bytes: &'a [u8],
    position: usize,
}

impl<'a> PayloadCursor<'a> {
    fn new(sequence: u64, name: &'static str, bytes: &'a [u8]) -> Self {
        Self {
            sequence,
            name,
            bytes,
            position: 0,
        }
    }

    fn read_full_box(
        &mut self,
        supported_versions: &[u8],
        allowed_flags: u32,
    ) -> Result<u32, FlvDemuxError> {
        let header = self.read(FULL_BOX_HEADER_BYTES)?;
        if !supported_versions.contains(&header[0]) {
            return Err(malformed(
                self.sequence,
                format!(
                    "{} full-box version {} не поддерживается",
                    self.name, header[0]
                ),
            ));
        }
        let flags = u32::from_be_bytes([0, header[1], header[2], header[3]]);
        if flags & !allowed_flags != 0 {
            return Err(malformed(
                self.sequence,
                format!("{} содержит неизвестные full-box flags", self.name),
            ));
        }
        Ok(flags)
    }

    fn read_u8(&mut self) -> Result<u8, FlvDemuxError> {
        Ok(self.read(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, FlvDemuxError> {
        Ok(u32::from_be_bytes(
            self.read(4)?.try_into().expect("exact slice"),
        ))
    }

    fn read_bounded_count(
        &mut self,
        maximum: usize,
        field: &'static str,
    ) -> Result<usize, FlvDemuxError> {
        let count = usize::try_from(self.read_u32()?).expect("u32 fits usize");
        if count > maximum {
            return Err(malformed(
                self.sequence,
                format!("{field} превышает bounded policy"),
            ));
        }
        Ok(count)
    }

    fn read_string(&mut self, maximum_bytes: usize) -> Result<(), FlvDemuxError> {
        let remaining = &self.bytes[self.position..];
        let Some(length) = remaining.iter().position(|byte| *byte == 0) else {
            return Err(malformed(
                self.sequence,
                format!("{} string не завершён нулём", self.name),
            ));
        };
        if length > maximum_bytes {
            return Err(malformed(
                self.sequence,
                format!("{} string превышает bounded policy", self.name),
            ));
        }
        std::str::from_utf8(&remaining[..length]).map_err(|_| {
            malformed(
                self.sequence,
                format!("{} string содержит невалидный UTF-8", self.name),
            )
        })?;
        self.position += length + 1;
        Ok(())
    }

    fn read_strings(&mut self, count: usize, maximum_bytes: usize) -> Result<(), FlvDemuxError> {
        for _ in 0..count {
            self.read_string(maximum_bytes)?;
        }
        Ok(())
    }

    fn read_box(&mut self, box_budget: &mut usize) -> Result<IsoBox<'a>, FlvDemuxError> {
        consume_box_budget(self.sequence, box_budget)?;
        let (parsed_box, box_end) = parse_box_at(self.sequence, self.bytes, self.position)?;
        self.position = box_end;
        Ok(parsed_box)
    }

    fn skip_repeated(&mut self, count: usize, bytes_each: usize) -> Result<(), FlvDemuxError> {
        let total = count
            .checked_mul(bytes_each)
            .ok_or_else(|| malformed(self.sequence, "repeated field size overflow"))?;
        self.skip(total)
    }

    fn skip(&mut self, count: usize) -> Result<(), FlvDemuxError> {
        self.read(count).map(|_| ())
    }

    fn read(&mut self, count: usize) -> Result<&'a [u8], FlvDemuxError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| malformed(self.sequence, "payload cursor overflow"))?;
        let output = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| malformed(self.sequence, format!("{} payload обрезан", self.name)))?;
        self.position = end;
        Ok(output)
    }

    fn finish(self) -> Result<(), FlvDemuxError> {
        if self.position != self.bytes.len() {
            return Err(malformed(
                self.sequence,
                format!("{} payload содержит trailing bytes", self.name),
            ));
        }
        Ok(())
    }
}

fn malformed(sequence: u64, reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::MalformedF4f {
        sequence,
        reason: reason.into(),
    }
}
