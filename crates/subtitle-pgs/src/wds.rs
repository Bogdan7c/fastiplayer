//! WDS (0x17): окна — прямоугольные области экрана, куда разрешено рисовать.

use crate::error::SegmentParseError;
use crate::reader::ByteReader;
use crate::segment::SegmentKind;

/// Окно экрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowDefinition {
    pub window_id: u8,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Разбирает payload WDS: `count u8`, затем по 9 байт на окно. Объявленное число окон,
/// не помещающееся в данные, — ошибка (частичный список не используется).
pub fn parse_wds(payload: &[u8]) -> Result<Vec<WindowDefinition>, SegmentParseError> {
    let mut reader = ByteReader::new(SegmentKind::WindowDefinition, payload);
    let count = reader.u8()?;
    let mut windows = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        windows.push(WindowDefinition {
            window_id: reader.u8()?,
            x: reader.u16()?,
            y: reader.u16()?,
            width: reader.u16()?,
            height: reader.u16()?,
        });
    }
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_windows() {
        let payload = [2, 0, 0, 10, 0, 20, 0, 30, 0, 40, 1, 1, 2, 0, 3, 0, 4, 0, 5];
        let windows = parse_wds(&payload).expect("valid");
        assert_eq!(
            windows,
            [
                WindowDefinition {
                    window_id: 0,
                    x: 10,
                    y: 20,
                    width: 30,
                    height: 40
                },
                WindowDefinition {
                    window_id: 1,
                    x: 258,
                    y: 3,
                    width: 4,
                    height: 5
                },
            ]
        );
    }

    #[test]
    fn count_larger_than_data_is_an_error() {
        let payload = [2, 0, 0, 10, 0, 20, 0, 30, 0, 40];
        assert!(matches!(
            parse_wds(&payload),
            Err(SegmentParseError::Truncated { .. })
        ));
        assert!(parse_wds(&[]).is_err());
        assert_eq!(parse_wds(&[0]).expect("empty list"), []);
    }
}
