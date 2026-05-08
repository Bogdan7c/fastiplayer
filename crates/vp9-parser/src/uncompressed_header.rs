use crate::bit_reader::BitReader;

/// VP9 frame marker: первые два бита каждого frame header.
const FRAME_MARKER: u32 = 0b10;

/// VP9 keyframe/intra-only sync code.
const SYNC_CODE: u32 = 0x498342;

/// Значение color_space для sRGB в VP9 bitstream.
const COLOR_SPACE_SRGB: u32 = 7;

#[derive(Debug, Clone, PartialEq)]
pub struct Vp9FrameInfo {
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub show_frame: bool,
    pub profile: u8,
    pub show_existing_frame: bool,
    /// Bit depth кадра, если standalone header содержит достаточно информации.
    pub bit_depth: Option<u8>,
    /// VP9 chroma subsampling X flag, если он известен из header.
    pub subsampling_x: Option<bool>,
    /// VP9 chroma subsampling Y flag, если он известен из header.
    pub subsampling_y: Option<bool>,
    /// Флаги обновления слотов DPB (Decoded Picture Buffer) после декодирования.
    /// Каждый бит соответствует одному из 8 слотов DPB.
    /// Бит=1 означает, что соответствующий слот нужно обновить этим кадром.
    pub refresh_frame_flags: u8,
    /// Индексы опорных кадров в DPB для межкадрового предсказания.
    /// Используются только для интер-кадров (не keyframe).
    /// Каждый элемент — индекс слота DPB (0..7).
    pub ref_frame_idx: [u8; 3],
    /// Знаковое смещение для опорных кадров.
    /// bias[i]=1 означает, что для i-го типа опорного кадра используется знаковое смещение.
    /// Только для интер-кадров.
    pub ref_frame_sign_bias: [u8; 4],
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Insufficient data")]
    InsufficientData,
    #[error("Invalid frame marker")]
    InvalidFrameMarker,
    #[error("Invalid sync code")]
    InvalidSyncCode,
    #[error("Unsupported profile: {0}")]
    UnsupportedProfile(u8),
    #[error("Bitstream error")]
    BitstreamError,
}

pub fn parse_uncompressed_header(data: &[u8]) -> Result<Vp9FrameInfo, ParseError> {
    if data.len() < 3 {
        return Err(ParseError::InsufficientData);
    }

    let mut br = BitReader::new(data);

    let frame_marker = br.read_bits(2).ok_or(ParseError::BitstreamError)?;
    if frame_marker != FRAME_MARKER {
        return Err(ParseError::InvalidFrameMarker);
    }

    let profile = parse_profile(&mut br)?;

    let show_existing_frame = read_bool(&mut br)?;

    // Если установлен флаг show_existing_frame — показываем уже декодированный кадр из DPB.
    // В этом случае новые поля управления опорными кадрами не имеют смысла, обнуляем их.
    if show_existing_frame {
        let _frame_to_show_map_idx = br.read_bits(3).ok_or(ParseError::BitstreamError)?;
        return Ok(Vp9FrameInfo {
            keyframe: false,
            width: 0,
            height: 0,
            render_width: 0,
            render_height: 0,
            show_frame: true,
            profile,
            show_existing_frame,
            bit_depth: None,
            subsampling_x: None,
            subsampling_y: None,
            refresh_frame_flags: 0,
            ref_frame_idx: [0; 3],
            ref_frame_sign_bias: [0; 4],
        });
    }

    let frame_type = br.read_bit().ok_or(ParseError::BitstreamError)?;
    let show_frame = read_bool(&mut br)?;
    let error_resilient_mode = read_bool(&mut br)?;
    let keyframe = frame_type == 0;

    let mut bit_depth = None;
    let mut subsampling_x = None;
    let mut subsampling_y = None;
    let refresh_frame_flags;
    let mut ref_frame_idx = [0u8; 3];
    let mut ref_frame_sign_bias = [0u8; 4];
    let width;
    let height;
    let render_width;
    let render_height;

    if keyframe {
        parse_frame_sync_code(&mut br)?;
        let color_config = parse_color_config(&mut br, profile)?;
        bit_depth = Some(color_config.bit_depth);
        subsampling_x = color_config.subsampling_x;
        subsampling_y = color_config.subsampling_y;
        (width, height) = parse_frame_size(&mut br)?;
        (render_width, render_height) = parse_render_size(&mut br, width, height)?;
        refresh_frame_flags = 0xff;
    } else {
        let intra_only = if !show_frame {
            read_bool(&mut br)?
        } else {
            false
        };

        if !error_resilient_mode {
            let _reset_frame_context = br.read_bits(2).ok_or(ParseError::BitstreamError)?;
        }

        if intra_only {
            parse_frame_sync_code(&mut br)?;
            if profile == 0 {
                bit_depth = Some(8);
                subsampling_x = Some(true);
                subsampling_y = Some(true);
            } else {
                let color_config = parse_color_config(&mut br, profile)?;
                bit_depth = Some(color_config.bit_depth);
                subsampling_x = color_config.subsampling_x;
                subsampling_y = color_config.subsampling_y;
            }
            refresh_frame_flags = br.read_bits(8).ok_or(ParseError::BitstreamError)? as u8;
            (width, height) = parse_frame_size(&mut br)?;
            (render_width, render_height) = parse_render_size(&mut br, width, height)?;
        } else {
            refresh_frame_flags = br.read_bits(8).ok_or(ParseError::BitstreamError)? as u8;
            for index in 0..3 {
                ref_frame_idx[index] = br.read_bits(3).ok_or(ParseError::BitstreamError)? as u8;
                ref_frame_sign_bias[index + 1] = br.read_bit().ok_or(ParseError::BitstreamError)?;
            }
            (width, height, render_width, render_height) = parse_frame_size_with_refs(&mut br)?;
        }
    };

    Ok(Vp9FrameInfo {
        keyframe,
        width,
        height,
        render_width,
        render_height,
        show_frame,
        profile,
        show_existing_frame,
        bit_depth,
        subsampling_x,
        subsampling_y,
        refresh_frame_flags,
        ref_frame_idx,
        ref_frame_sign_bias,
    })
}

/// Результат чтения VP9 color config.
struct ColorConfig {
    /// Bit depth из profile/color config.
    bit_depth: u8,
    /// Chroma subsampling X flag, если format его задаёт.
    subsampling_x: Option<bool>,
    /// Chroma subsampling Y flag, если format его задаёт.
    subsampling_y: Option<bool>,
}

/// Читает один бит как bool.
fn read_bool(br: &mut BitReader<'_>) -> Result<bool, ParseError> {
    Ok(br.read_bit().ok_or(ParseError::BitstreamError)? != 0)
}

/// Читает VP9 profile: low bit, high bit и reserved bit для profile 3.
fn parse_profile(br: &mut BitReader<'_>) -> Result<u8, ParseError> {
    let low_bit = br.read_bit().ok_or(ParseError::BitstreamError)?;
    let high_bit = br.read_bit().ok_or(ParseError::BitstreamError)?;
    let profile = (high_bit << 1) | low_bit;

    if profile == 3 {
        let _reserved_zero = br.read_bit().ok_or(ParseError::BitstreamError)?;
    }

    if profile > 3 {
        Err(ParseError::UnsupportedProfile(profile))
    } else {
        Ok(profile)
    }
}

/// Проверяет keyframe/intra-only sync code перед чтением color config и размеров.
fn parse_frame_sync_code(br: &mut BitReader<'_>) -> Result<(), ParseError> {
    let sync_code = br.read_bits(24).ok_or(ParseError::BitstreamError)?;
    if sync_code == SYNC_CODE {
        Ok(())
    } else {
        Err(ParseError::InvalidSyncCode)
    }
}

/// Читает VP9 color config ровно в том порядке, в котором он лежит в bitstream.
fn parse_color_config(br: &mut BitReader<'_>, profile: u8) -> Result<ColorConfig, ParseError> {
    let bit_depth = if matches!(profile, 2 | 3) {
        if read_bool(br)? { 12 } else { 10 }
    } else {
        8
    };

    let color_space = br.read_bits(3).ok_or(ParseError::BitstreamError)?;
    let (subsampling_x, subsampling_y) = if color_space != COLOR_SPACE_SRGB {
        let _color_range = br.read_bit().ok_or(ParseError::BitstreamError)?;
        if matches!(profile, 1 | 3) {
            let subsampling_x = read_bool(br)?;
            let subsampling_y = read_bool(br)?;
            let _reserved_zero = br.read_bit().ok_or(ParseError::BitstreamError)?;
            (Some(subsampling_x), Some(subsampling_y))
        } else {
            (Some(true), Some(true))
        }
    } else if matches!(profile, 1 | 3) {
        let _reserved_zero = br.read_bit().ok_or(ParseError::BitstreamError)?;
        (Some(false), Some(false))
    } else {
        (None, None)
    };

    Ok(ColorConfig {
        bit_depth,
        subsampling_x,
        subsampling_y,
    })
}

/// Читает coded frame size.
fn parse_frame_size(br: &mut BitReader<'_>) -> Result<(u32, u32), ParseError> {
    let width = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
    let height = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;

    Ok((width, height))
}

/// Читает render size, используя coded size как fallback.
fn parse_render_size(
    br: &mut BitReader<'_>,
    width: u32,
    height: u32,
) -> Result<(u32, u32), ParseError> {
    let render_and_frame_size_different = read_bool(br)?;
    if render_and_frame_size_different {
        let rw = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
        let rh = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
        Ok((rw, rh))
    } else {
        Ok((width, height))
    }
}

/// Читает frame size для inter frame без reference size cache.
fn parse_frame_size_with_refs(br: &mut BitReader<'_>) -> Result<(u32, u32, u32, u32), ParseError> {
    for _ in 0..3 {
        if read_bool(br)? {
            let (render_width, render_height) = parse_render_size(br, 0, 0)?;
            return Ok((0, 0, render_width, render_height));
        }
    }

    let (width, height) = parse_frame_size(br)?;
    let (render_width, render_height) = parse_render_size(br, width, height)?;
    Ok((width, height, render_width, render_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insufficient_data() {
        let result = parse_uncompressed_header(&[0x00]);
        assert!(matches!(result, Err(ParseError::InsufficientData)));
    }

    #[test]
    fn test_invalid_marker() {
        let result = parse_uncompressed_header(&[0x00, 0x00, 0x00]);
        assert!(matches!(result, Err(ParseError::InvalidFrameMarker)));
    }

    /// Тест парсинга ключевого кадра (keyframe).
    /// Проверяем, что refresh_frame_flags парсится корректно,
    /// а ref_frame_idx и ref_frame_sign_bias заполняются нулями
    /// (т.к. для keyframe межкадровое предсказание не используется).
    #[test]
    fn test_keyframe_parse() {
        let keyframe_data = build_vp9_keyframe(0, 8, 64, 64);

        let result =
            parse_uncompressed_header(&keyframe_data).expect("парсинг keyframe должен succeed");

        assert!(result.keyframe, "должен быть keyframe");
        assert_eq!(
            result.refresh_frame_flags, 0xFF,
            "refresh_frame_flags должен быть 0xFF"
        );
        assert_eq!(
            result.ref_frame_idx, [0; 3],
            "ref_frame_idx для keyframe должен быть [0;3]"
        );
        assert_eq!(
            result.ref_frame_sign_bias, [0; 4],
            "ref_frame_sign_bias для keyframe должен быть [0;4]"
        );
        assert_eq!(result.width, 64, "width должен быть 64");
        assert_eq!(result.height, 64, "height должен быть 64");
        assert_eq!(result.render_width, 64, "render_width должен быть 64");
        assert_eq!(result.render_height, 64, "render_height должен быть 64");
        assert_eq!(result.bit_depth, Some(8), "bit depth должен быть 8");
        assert_eq!(result.subsampling_x, Some(true));
        assert_eq!(result.subsampling_y, Some(true));
    }

    /// Тест парсинга интер-кадра (inter-frame).
    /// Проверяем, что ref_frame_idx и ref_frame_sign_bias
    /// парсятся корректно и содержат ненулевые значения.
    #[test]
    fn test_inter_frame_parse() {
        let inter_data = build_inter_frame_without_refs();

        let result =
            parse_uncompressed_header(&inter_data).expect("парсинг inter-frame должен succeed");

        assert!(!result.keyframe, "должен быть inter-frame");
        assert_eq!(
            result.refresh_frame_flags, 0x01,
            "refresh_frame_flags должен быть 0x01"
        );
        assert_eq!(
            result.ref_frame_sign_bias,
            [0, 1, 0, 1],
            "ref_frame_sign_bias должен быть [0,1,0,1]"
        );
        assert_eq!(
            result.ref_frame_idx,
            [1, 2, 3],
            "ref_frame_idx должен быть [1,2,3]"
        );
    }

    #[test]
    fn test_profile2_keyframe_parse() {
        let keyframe_data = build_vp9_keyframe(2, 10, 128, 72);

        let result =
            parse_uncompressed_header(&keyframe_data).expect("profile2 keyframe должен парситься");

        assert_eq!(result.profile, 2);
        assert_eq!(result.bit_depth, Some(10));
        assert_eq!(result.subsampling_x, Some(true));
        assert_eq!(result.subsampling_y, Some(true));
        assert_eq!(result.width, 128);
        assert_eq!(result.height, 72);
    }

    /// Упаковывает bits в bytes в том же MSB-first порядке, что читает parser.
    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    /// Добавляет `width` бит значения в test bitstream.
    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    /// Добавляет VP9 profile bits: low bit, high bit, reserved bit для profile 3.
    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }

    /// Собирает минимальный валидный VP9 keyframe header для parser tests.
    fn build_vp9_keyframe(profile: u8, bit_depth: u8, width: u32, height: u32) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, profile);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, SYNC_CODE, 24);
        if matches!(profile, 2 | 3) {
            bits.push(u8::from(bit_depth == 12));
        }
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        if matches!(profile, 1 | 3) {
            bits.push(1);
            bits.push(1);
            bits.push(0);
        }
        push_bits(&mut bits, width - 1, 16);
        push_bits(&mut bits, height - 1, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    /// Собирает inter frame, который сам задаёт frame size вместо ссылки на reference size.
    fn build_inter_frame_without_refs() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }
}
