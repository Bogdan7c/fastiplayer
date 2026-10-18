//! Кодировщик потоков PGS для тестов и генерации фикстур.
//!
//! Нужен, чтобы проверять декодер на потоках, собранных вне самого декодера: сегменты
//! PCS/WDS/PDS/ODS/END, палитра RGBA -> YCbCr, простой RLE-кодировщик, нарезка ODS на
//! фрагменты. Результат — payload блоков Matroska ([`PgsStreamBuilder::block_payloads`])
//! или байты файла `.sup` ([`PgsStreamBuilder::to_sup_bytes`]).
//! Производственный путь воспроизведения этот модуль не использует.

use subtitle_core::{PictureForcedFlag, SubtitleTime};

use crate::color::PgsColorMatrix;
use crate::pcs::{CompositionObject, CompositionState, CropRect};
use crate::segment::SegmentKind;
use crate::wds::WindowDefinition;

/// Оборачивает payload в сегмент: `[тип][размер u16 BE][payload]`.
///
/// Payload длиннее 65535 байт не помещается в сегмент; кодировщик такие случаи не создаёт
/// (ODS режется на фрагменты заранее), поэтому лишнее обрезается с `debug_assert`.
pub fn encode_segment(kind: SegmentKind, payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= usize::from(u16::MAX));
    let size = u16::try_from(payload.len()).unwrap_or(u16::MAX);
    let mut out = vec![kind.to_byte()];
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&payload[..usize::from(size)]);
    out
}

/// Цвет записи палитры для кодировщика: RGB + альфа (straight).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteColorSpec {
    pub index: u8,
    pub rgba: [u8; 4],
}

/// Описание палитры набора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteSpec {
    pub palette_id: u8,
    pub version: u8,
    pub colors: Vec<PaletteColorSpec>,
}

/// Описание картинки объекта: палитровые индексы построчно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectImageSpec {
    pub object_id: u16,
    pub width: u16,
    pub height: u16,
    pub indices: Vec<u8>,
    /// Максимум RLE-байт в одном сегменте ODS; `None` — один сегмент на объект.
    pub max_rle_bytes_per_fragment: Option<usize>,
}

/// Описание одного набора отображения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySetSpec {
    reference_width: u16,
    reference_height: u16,
    state: CompositionState,
    palette_only: bool,
    palette_id: u8,
    windows: Vec<WindowDefinition>,
    palettes: Vec<PaletteSpec>,
    images: Vec<ObjectImageSpec>,
    composition: Vec<CompositionObject>,
}

impl DisplaySetSpec {
    /// Пустой набор заданного состояния в системе координат `width` x `height`.
    pub fn new(reference_width: u16, reference_height: u16, state: CompositionState) -> Self {
        Self {
            reference_width,
            reference_height,
            state,
            palette_only: false,
            palette_id: 0,
            windows: Vec::new(),
            palettes: Vec::new(),
            images: Vec::new(),
            composition: Vec::new(),
        }
    }

    /// Какая палитра используется композицией.
    pub fn with_palette_id(mut self, palette_id: u8) -> Self {
        self.palette_id = palette_id;
        self
    }

    /// Композиция только обновляет палитру (объекты наследуются).
    pub fn palette_update_only(mut self) -> Self {
        self.palette_only = true;
        self
    }

    pub fn with_window(mut self, window: WindowDefinition) -> Self {
        self.windows.push(window);
        self
    }

    pub fn with_palette(mut self, palette: PaletteSpec) -> Self {
        self.palettes.push(palette);
        self
    }

    pub fn with_image(mut self, image: ObjectImageSpec) -> Self {
        self.images.push(image);
        self
    }

    pub fn with_composition_object(mut self, object: CompositionObject) -> Self {
        self.composition.push(object);
        self
    }
}

/// Простой RLE-кодировщик HDMV: серии цвета, строки завершаются `00 00`.
/// Лишние пиксели сверх `width * height` игнорируются; недостающие считаются нулями.
pub fn encode_rle(indices: &[u8], width: u16, height: u16) -> Vec<u8> {
    const MAX_RUN: usize = 0x3FFF;
    let width = usize::from(width);
    let mut out = Vec::new();
    for row in 0..usize::from(height) {
        let start = row * width;
        let line: Vec<u8> = (start..start + width)
            .map(|position| indices.get(position).copied().unwrap_or(0))
            .collect();
        let mut column = 0;
        while column < line.len() {
            let color = line[column];
            let mut run = 1;
            while column + run < line.len() && line[column + run] == color && run < MAX_RUN {
                run += 1;
            }
            push_run(&mut out, color, run);
            column += run;
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

fn push_run(out: &mut Vec<u8>, color: u8, run: usize) {
    // run <= 0x3FFF по построению.
    let high = ((run >> 8) & 0x3F) as u8;
    let low = (run & 0xFF) as u8;
    match (color, run) {
        (0, 1..=63) => out.extend_from_slice(&[0, low]),
        (0, _) => out.extend_from_slice(&[0, 0x40 | high, low]),
        (_, 1) => out.push(color),
        (_, 2..=63) => out.extend_from_slice(&[0, 0x80 | low, color]),
        (_, _) => out.extend_from_slice(&[0, 0xC0 | high, low, color]),
    }
}

fn encode_pcs_payload(spec: &DisplaySetSpec, composition_number: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&spec.reference_width.to_be_bytes());
    out.extend_from_slice(&spec.reference_height.to_be_bytes());
    out.push(0x10); // Код частоты кадров (24 fps); декодер его игнорирует.
    out.extend_from_slice(&composition_number.to_be_bytes());
    out.push(spec.state.to_byte());
    out.push(if spec.palette_only { 0x80 } else { 0x00 });
    out.push(spec.palette_id);
    out.push(u8::try_from(spec.composition.len()).unwrap_or(u8::MAX));
    for object in spec.composition.iter().take(usize::from(u8::MAX)) {
        out.extend_from_slice(&object.object_id.to_be_bytes());
        out.push(object.window_id);
        let mut flags = 0u8;
        if object.crop.is_some() {
            flags |= 0x80;
        }
        if object.forced == PictureForcedFlag::Forced {
            flags |= 0x40;
        }
        out.push(flags);
        out.extend_from_slice(&object.x.to_be_bytes());
        out.extend_from_slice(&object.y.to_be_bytes());
        if let Some(CropRect {
            x,
            y,
            width,
            height,
        }) = object.crop
        {
            for value in [x, y, width, height] {
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    out
}

fn encode_wds_payload(windows: &[WindowDefinition]) -> Vec<u8> {
    let mut out = vec![u8::try_from(windows.len()).unwrap_or(u8::MAX)];
    for window in windows.iter().take(usize::from(u8::MAX)) {
        out.push(window.window_id);
        for value in [window.x, window.y, window.width, window.height] {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
    out
}

fn encode_pds_payload(palette: &PaletteSpec, matrix: PgsColorMatrix) -> Vec<u8> {
    let mut out = vec![palette.palette_id, palette.version];
    for color in &palette.colors {
        let [red, green, blue, alpha] = color.rgba;
        let (luma, cb, cr) = matrix.rgb_to_ycbcr(red, green, blue);
        // В потоке Cr идёт перед Cb.
        out.extend_from_slice(&[color.index, luma, cr, cb, alpha]);
    }
    out
}

/// ODS-сегменты объекта: один или несколько фрагментов.
fn encode_ods_segments(image: &ObjectImageSpec) -> Vec<Vec<u8>> {
    let rle = encode_rle(&image.indices, image.width, image.height);
    let chunk_len = image
        .max_rle_bytes_per_fragment
        .unwrap_or(usize::MAX)
        .max(1);
    // Первый фрагмент несёт ещё 7 служебных байт ODS-заголовка; следим за лимитом сегмента.
    let chunk_len = chunk_len.min(usize::from(u16::MAX) - 11);
    let chunks: Vec<&[u8]> = if rle.is_empty() {
        vec![&[][..]]
    } else {
        rle.chunks(chunk_len).collect()
    };
    let declared_length = u32::try_from(rle.len() + 4).unwrap_or(0x00FF_FFFF);
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(position, chunk)| {
            let flags = match (position == 0, position == last) {
                (true, true) => 0xC0,
                (true, false) => 0x80,
                (false, true) => 0x40,
                (false, false) => 0x00,
            };
            let mut payload = image.object_id.to_be_bytes().to_vec();
            payload.push(0); // Версия объекта.
            payload.push(flags);
            if position == 0 {
                payload.extend_from_slice(&declared_length.to_be_bytes()[1..]);
                payload.extend_from_slice(&image.width.to_be_bytes());
                payload.extend_from_slice(&image.height.to_be_bytes());
            }
            payload.extend_from_slice(chunk);
            encode_segment(SegmentKind::ObjectDefinition, &payload)
        })
        .collect()
}

/// Все сегменты набора по порядку PCS, WDS, PDS, ODS, END (плоским списком).
pub fn encode_display_set_segments(spec: &DisplaySetSpec, composition_number: u16) -> Vec<Vec<u8>> {
    let matrix = PgsColorMatrix::for_reference_height(u32::from(spec.reference_height));
    let mut segments = vec![encode_segment(
        SegmentKind::PresentationComposition,
        &encode_pcs_payload(spec, composition_number),
    )];
    if !spec.windows.is_empty() {
        segments.push(encode_segment(
            SegmentKind::WindowDefinition,
            &encode_wds_payload(&spec.windows),
        ));
    }
    for palette in &spec.palettes {
        segments.push(encode_segment(
            SegmentKind::PaletteDefinition,
            &encode_pds_payload(palette, matrix),
        ));
    }
    for image in &spec.images {
        segments.extend(encode_ods_segments(image));
    }
    segments.push(encode_segment(SegmentKind::EndOfDisplaySet, &[]));
    segments
}

/// Собирает поток из наборов отображения, привязанных ко времени.
#[derive(Debug, Default)]
pub struct PgsStreamBuilder {
    display_sets: Vec<(SubtitleTime, Vec<Vec<u8>>)>,
}

impl PgsStreamBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет набор на момент `time`; номер композиции присваивается по порядку.
    pub fn push_display_set(&mut self, time: SubtitleTime, spec: &DisplaySetSpec) -> &mut Self {
        let number = u16::try_from(self.display_sets.len() % (usize::from(u16::MAX) + 1))
            .unwrap_or(u16::MAX);
        self.display_sets
            .push((time, encode_display_set_segments(spec, number)));
        self
    }

    /// Payload блоков Matroska: ровно один блок на набор, внутри все его сегменты подряд.
    pub fn block_payloads(&self) -> Vec<(SubtitleTime, Vec<u8>)> {
        self.display_sets
            .iter()
            .map(|(time, segments)| (*time, segments.concat()))
            .collect()
    }

    /// Файл `.sup`: перед каждым сегментом 13-байтный заголовок (`PG`, PTS, DTS = 0).
    pub fn to_sup_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (time, segments) in &self.display_sets {
            // 90 кГц: микросекунды * 9 / 100; значения > u32 насыщаются.
            let ticks = u32::try_from(time.as_micros().max(0) * 9 / 100).unwrap_or(u32::MAX);
            for segment in segments {
                out.extend_from_slice(b"PG");
                out.extend_from_slice(&ticks.to_be_bytes());
                out.extend_from_slice(&0u32.to_be_bytes());
                out.extend_from_slice(segment);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rle::decode_hdmv_rle;

    #[test]
    fn rle_roundtrips_all_run_forms() {
        let mut indices = vec![0u8; 100]; // Ноль, 100 подряд (14-битная форма).
        indices.extend(std::iter::repeat_n(5u8, 100));
        // Длинная серия цвета (100), одиночный пиксель, пара, одиночный ноль.
        indices.extend([7, 8, 8, 0]);
        let width = u16::try_from(indices.len()).unwrap();
        let encoded = encode_rle(&indices, width, 1);
        let decoded = decode_hdmv_rle(&encoded, usize::from(width), 1);
        assert_eq!(decoded.indices, indices);
        assert!(decoded.report.is_clean());
    }
}
