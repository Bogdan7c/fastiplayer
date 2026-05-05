use crate::bit_reader::BitReader;

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

    // frame_marker: 2 bits, must be 0b10
    let frame_marker = br.read_bits(2).ok_or(ParseError::BitstreamError)?;
    if frame_marker != 0b10 {
        return Err(ParseError::InvalidFrameMarker);
    }

    // profile: 1 bit, if 1 then read 1 more bit
    let mut profile = br.read_bit().ok_or(ParseError::BitstreamError)?;
    if profile == 1 {
        let profile_extra = br.read_bit().ok_or(ParseError::BitstreamError)?;
        profile = 1 + profile_extra;
    }

    if profile > 2 {
        return Err(ParseError::UnsupportedProfile(profile));
    }

    let show_existing_frame = if br.read_bit().ok_or(ParseError::BitstreamError)? == 1 {
        true
    } else {
        false
    };

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
            refresh_frame_flags: 0,
            ref_frame_idx: [0; 3],
            ref_frame_sign_bias: [0; 4],
        });
    }

    let frame_type = br.read_bit().ok_or(ParseError::BitstreamError)?;
    let show_frame = br.read_bit().ok_or(ParseError::BitstreamError)? == 1;
    // Читаем флаг error_resilient_mode (1 бит), только если show_frame=true.
    // Этот флаг влияет на поведение при ошибках в потоке.
    let _error_resilient_mode = if show_frame {
        br.read_bit().ok_or(ParseError::BitstreamError)? == 1
    } else {
        false
    };

    // Определяем тип кадра: 0 — keyframe (внутрикадровое кодирование), 1 — inter-frame.
    let keyframe = frame_type == 0;

    // После error_resilient_mode читаем refresh_frame_flags (8 бит).
    // Этот байт определяет, какие слоты DPB обновить после декодирования текущего кадра.
    // Каждый бит соответствует одному из 8 слотов.
    let refresh_frame_flags = br.read_bits(8).ok_or(ParseError::BitstreamError)? as u8;

    // Для интер-кадров читаем знаковые смещения опорных кадров.
    // Для ключевых кадров смещения не используются — заполняем нулями.
    let ref_frame_sign_bias = if keyframe {
        [0u8; 4]
    } else {
        let mut bias = [0u8; 4];
        // Читаем 4 бита: по одному на каждый тип опорного кадра (LAST, GOLDEN, ALTREF, и т.д.).
        for i in 0..4 {
            bias[i] = br.read_bit().ok_or(ParseError::BitstreamError)? as u8;
        }
        bias
    };

    // Для интер-кадров читаем индексы опорных кадров в DPB (по 3 бита каждый).
    // Для ключевых кадров межкадровое предсказание не используется — заполняем нулями.
    let ref_frame_idx = if keyframe {
        [0u8; 3]
    } else {
        let mut idx = [0u8; 3];
        // Читаем 3 индекса опорных кадров (LAST, GOLDEN, ALTREF).
        // Каждый индекс указывает на слот DPB (0..7).
        for i in 0..3 {
            idx[i] = br.read_bits(3).ok_or(ParseError::BitstreamError)? as u8;
        }
        idx
    };

    // Парсим размер кадра и размер отображения.
    let (width, height, render_width, render_height) = if keyframe {
        parse_frame_size_and_render_size(&mut br, None)?
    } else {
        let intra_only = if show_frame {
            false
        } else {
            br.read_bit().ok_or(ParseError::BitstreamError)? == 1
        };
        if intra_only {
            // Пропускаем 8 бит (зарезервировано для intra-only кадров).
            br.read_bits(8).ok_or(ParseError::BitstreamError)?;
            parse_frame_size_and_render_size(&mut br, None)?
        } else {
            (0, 0, 0, 0)
        }
    };

    // Возвращаем структуру с полной информацией о кадре, включая новые поля для Vulkan Video.
    Ok(Vp9FrameInfo {
        keyframe,
        width,
        height,
        render_width,
        render_height,
        show_frame,
        profile,
        show_existing_frame,
        refresh_frame_flags,
        ref_frame_idx,
        ref_frame_sign_bias,
    })
}

fn parse_frame_size_and_render_size(
    br: &mut BitReader,
    _ref_size: Option<(u32, u32)>,
) -> Result<(u32, u32, u32, u32), ParseError> {
    let width = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
    let height = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;

    let render_and_frame_size_different = br.read_bit().ok_or(ParseError::BitstreamError)? == 1;
    let (render_width, render_height) = if render_and_frame_size_different {
        let rw = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
        let rh = br.read_literal(16).ok_or(ParseError::BitstreamError)? as u32 + 1;
        (rw, rh)
    } else {
        (width, height)
    };

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
        // Собираем битовый поток для keyframe:
        // frame_marker=10, profile=0, show_existing=0, frame_type=0 (key),
        // show_frame=1, error_resilient=0, refresh_frame_flags=0xFF,
        // width=64 (0x003F+1), height=64 (0x003F+1), render_diff=0
        let keyframe_data = [
            0x85, // 10000101: frame_marker=10, profile=0, show_existing=0, frame_type=0, show_frame=1, error_resilient=0, refresh[7]=1
            0xFE, // 11111110: refresh[6:0]=1111111, width[15]=0
            0x00, // 00000000: width[14:7]=00000000
            0x7E, // 01111110: width[6:0]=0111111, height[15]=0
            0x00, // 00000000: height[14:7]=00000000
            0x7E, // 01111110: height[6:0]=0111111, render_diff=0
        ];

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
    }

    /// Тест парсинга интер-кадра (inter-frame).
    /// Проверяем, что ref_frame_idx и ref_frame_sign_bias
    /// парсятся корректно и содержат ненулевые значения.
    #[test]
    fn test_inter_frame_parse() {
        // Собираем битовый поток для inter-frame:
        // frame_marker=10, profile=0, show_existing=0, frame_type=1 (inter),
        // show_frame=1, error_resilient=0, refresh_frame_flags=0x01,
        // ref_frame_sign_bias=[1,0,1,0], ref_frame_idx=[1,2,3]
        let inter_data = [
            0x8C, // 10001100: frame_marker=10, profile=0, show_existing=0, frame_type=1, show_frame=1, error_resilient=0, refresh[7]=0
            0x03, // 00000011: refresh[6:0]=0000001, bias[0]=1
            0x45, // 01000101: bias[1]=0, bias[2]=1, bias[3]=0, idx[0]=001, idx[1][2:1]=01
            0x30, // 00110000: idx[1][0]=0, idx[2]=011, padding=0000
        ];

        let result =
            parse_uncompressed_header(&inter_data).expect("парсинг inter-frame должен succeed");

        assert!(!result.keyframe, "должен быть inter-frame");
        assert_eq!(
            result.refresh_frame_flags, 0x01,
            "refresh_frame_flags должен быть 0x01"
        );
        assert_eq!(
            result.ref_frame_sign_bias,
            [1, 0, 1, 0],
            "ref_frame_sign_bias должен быть [1,0,1,0]"
        );
        assert_eq!(
            result.ref_frame_idx,
            [1, 2, 3],
            "ref_frame_idx должен быть [1,2,3]"
        );
    }
}
