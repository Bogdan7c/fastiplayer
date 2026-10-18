//! Распаковка HDMV RLE в палитровые индексы.
//!
//! Коды (построчно, пиксели слева направо):
//! - `C` (C != 0) — один пиксель цвета `C`;
//! - `00 00` — конец строки;
//! - `00 00LLLLLL` (L = 1..63) — L пикселей цвета 0;
//! - `00 01LLLLLL LLLLLLLL` — 14-битное L пикселей цвета 0;
//! - `00 10LLLLLL CC` — L пикселей цвета `CC`;
//! - `00 11LLLLLL LLLLLLLL CC` — 14-битное L пикселей цвета `CC`.
//!
//! Декодер устойчив к мусору: результат всегда ровно `width * height` индексов, цикл
//! ограничен длиной входа, паник нет. Все отклонения от идеала считаются в [`RleReport`].

/// Как закончились входные данные относительно картинки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RleDataEnd {
    /// Все строки картинки получены.
    Complete,
    /// Данные кончились раньше: недостающая часть заполнена индексом 0.
    Truncated,
}

/// Отчёт о нарушениях формата при распаковке (всё «мягкое»: картинка получена).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RleReport {
    /// Сколько раз пиксели не поместились в строку и были отброшены.
    pub overshoot_runs: u32,
    /// Сколько строк закончились раньше ширины (дополнены нулями).
    pub short_lines: u32,
    /// Сколько байт осталось после того, как все строки уже получены (кроме EOL).
    pub ignored_trailing_bytes: u32,
    pub data_end: RleDataEnd,
}

impl RleReport {
    /// `true`, если поток соответствует формату без отклонений.
    pub fn is_clean(&self) -> bool {
        self.overshoot_runs == 0
            && self.short_lines == 0
            && self.ignored_trailing_bytes == 0
            && self.data_end == RleDataEnd::Complete
    }
}

/// Результат распаковки: индексы построчно (stride = width) и отчёт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedIndices {
    pub indices: Vec<u8>,
    pub report: RleReport,
}

/// Состояние записи: текущая позиция в картинке.
struct Canvas {
    indices: Vec<u8>,
    width: usize,
    height: usize,
    column: usize,
    row: usize,
    report: RleReport,
}

impl Canvas {
    fn is_full(&self) -> bool {
        self.row >= self.height
    }

    /// Пишет серию пикселей, обрезая её по правому краю строки.
    fn put_run(&mut self, count: usize, color: u8) {
        if self.is_full() {
            return;
        }
        let room = self.width - self.column;
        let written = count.min(room);
        if written < count {
            self.report.overshoot_runs += 1;
        }
        if color != 0 {
            let start = self.row * self.width + self.column;
            self.indices[start..start + written].fill(color);
        }
        self.column += written;
    }

    /// Конец строки: недостающие пиксели остаются нулями (буфер инициализирован нулями).
    fn end_line(&mut self) {
        if self.is_full() {
            return;
        }
        if self.column < self.width {
            self.report.short_lines += 1;
        }
        self.column = 0;
        self.row += 1;
    }
}

/// Распаковывает `data` в картинку `width` x `height`. Вызывающий обязан заранее проверить,
/// что `width * height` допустим по лимиту (см. `picture_decode`).
pub fn decode_hdmv_rle(data: &[u8], width: usize, height: usize) -> DecodedIndices {
    let mut canvas = Canvas {
        indices: vec![0; width.saturating_mul(height)],
        width,
        height,
        column: 0,
        row: 0,
        report: RleReport {
            overshoot_runs: 0,
            short_lines: 0,
            ignored_trailing_bytes: 0,
            data_end: RleDataEnd::Complete,
        },
    };
    let mut position = 0usize;
    let mut truncated = false;
    while position < data.len() && !canvas.is_full() {
        match next_code(data, position) {
            Some((code, next_position)) => {
                apply_code(&mut canvas, code);
                position = next_position;
            }
            None => {
                truncated = true;
                position = data.len();
            }
        }
    }
    // Последняя строка без завершающего EOL — допустимо (не считается ошибкой).
    if !canvas.is_full() {
        if canvas.column > 0 {
            canvas.end_line();
        }
        if !canvas.is_full() {
            truncated = true;
        }
    }
    canvas.report.ignored_trailing_bytes = count_ignored_tail(&data[position..]);
    if truncated {
        canvas.report.data_end = RleDataEnd::Truncated;
    }
    DecodedIndices {
        indices: canvas.indices,
        report: canvas.report,
    }
}

/// Хвост после заполнения картинки: пары `00 00` (лишние EOL) безвредны и не считаются.
fn count_ignored_tail(tail: &[u8]) -> u32 {
    let meaningful = tail.chunks(2).any(|pair| pair != [0, 0] && pair != [0]);
    if meaningful {
        u32::try_from(tail.len()).unwrap_or(u32::MAX)
    } else {
        0
    }
}

/// Один разобранный код RLE.
enum Code {
    EndOfLine,
    Run { count: usize, color: u8 },
}

/// Разбирает код, начинающийся в `position`. `None` — код оборван концом данных.
fn next_code(data: &[u8], position: usize) -> Option<(Code, usize)> {
    let first = *data.get(position)?;
    if first != 0 {
        return Some((
            Code::Run {
                count: 1,
                color: first,
            },
            position + 1,
        ));
    }
    let marker = *data.get(position + 1)?;
    let short_len = usize::from(marker & 0x3F);
    match marker >> 6 {
        0b00 if short_len == 0 => Some((Code::EndOfLine, position + 2)),
        0b00 => Some((
            Code::Run {
                count: short_len,
                color: 0,
            },
            position + 2,
        )),
        0b01 => {
            let low = *data.get(position + 2)?;
            Some((
                Code::Run {
                    count: (short_len << 8) | usize::from(low),
                    color: 0,
                },
                position + 3,
            ))
        }
        0b10 => {
            let color = *data.get(position + 2)?;
            Some((
                Code::Run {
                    count: short_len,
                    color,
                },
                position + 3,
            ))
        }
        _ => {
            let low = *data.get(position + 2)?;
            let color = *data.get(position + 3)?;
            Some((
                Code::Run {
                    count: (short_len << 8) | usize::from(low),
                    color,
                },
                position + 4,
            ))
        }
    }
}

fn apply_code(canvas: &mut Canvas, code: Code) {
    match code {
        Code::EndOfLine => canvas.end_line(),
        // Серия нулевой длины (например `00 80 CC`) ничего не пишет и не зацикливает:
        // позиция чтения уже продвинулась.
        Code::Run { count, color } => canvas.put_run(count, color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pixels_and_end_of_line() {
        let out = decode_hdmv_rle(&[1, 2, 0, 0, 3, 4, 0, 0], 2, 2);
        assert_eq!(out.indices, [1, 2, 3, 4]);
        assert!(out.report.is_clean());
    }

    #[test]
    fn all_run_forms() {
        // Строка 1: 3 нуля (короткая форма) + 2 пикселя цвета 7 (форма 10).
        // Строка 2: 300 нулей (14 бит, 0x012C) — шире строки? Нет: ширина 5, поэтому отдельно.
        let data = [0, 0x03, 0, 0x82, 7, 0, 0];
        let out = decode_hdmv_rle(&data, 5, 1);
        assert_eq!(out.indices, [0, 0, 0, 7, 7]);
        assert!(out.report.is_clean());

        // 14-битный нулевой прогон: 300 нулей, затем 260 пикселей цвета 9 (форма 11).
        let data = [0, 0x41, 0x2C, 0, 0xC1, 0x04, 9, 0, 0];
        let out = decode_hdmv_rle(&data, 560, 1);
        assert!(out.indices[..300].iter().all(|&i| i == 0));
        assert!(out.indices[300..].iter().all(|&i| i == 9));
        assert_eq!(out.indices.len(), 560);
        assert!(out.report.is_clean());
    }

    #[test]
    fn overshoot_is_clamped_and_counted() {
        let out = decode_hdmv_rle(&[0, 0x85, 4, 0, 0, 1, 1, 0, 0], 3, 2);
        assert_eq!(out.indices, [4, 4, 4, 1, 1, 0]);
        assert_eq!(out.report.overshoot_runs, 1);
        assert_eq!(out.report.short_lines, 1);
        assert_eq!(out.report.data_end, RleDataEnd::Complete);
    }

    #[test]
    fn short_line_is_padded_with_zero_and_counted() {
        let out = decode_hdmv_rle(&[5, 0, 0, 6, 6, 6, 0, 0], 3, 2);
        assert_eq!(out.indices, [5, 0, 0, 6, 6, 6]);
        assert_eq!(out.report.short_lines, 1);
    }

    #[test]
    fn missing_final_end_of_line_is_tolerated() {
        let out = decode_hdmv_rle(&[1, 2, 0, 0, 3, 4], 2, 2);
        assert_eq!(out.indices, [1, 2, 3, 4]);
        assert!(out.report.is_clean());
    }

    #[test]
    fn truncated_data_yields_partial_image() {
        // Вторая строка обрывается посреди кода `00 C0 ..`.
        let out = decode_hdmv_rle(&[1, 2, 0, 0, 3, 0, 0xC0, 0x01], 2, 3);
        assert_eq!(out.indices, [1, 2, 3, 0, 0, 0]);
        assert_eq!(out.report.data_end, RleDataEnd::Truncated);
        let empty = decode_hdmv_rle(&[], 2, 2);
        assert_eq!(empty.indices, [0; 4]);
        assert_eq!(empty.report.data_end, RleDataEnd::Truncated);
    }

    #[test]
    fn zero_length_runs_do_not_loop_or_write() {
        // `00 40 00` — 14-битный прогон длиной 0; `00 80 05` — длина 0 цвета 5.
        let out = decode_hdmv_rle(&[0, 0x40, 0, 0, 0x80, 5, 7, 0, 0], 1, 1);
        assert_eq!(out.indices, [7]);
        assert!(out.report.is_clean());
    }

    #[test]
    fn extra_data_after_last_row_is_reported_but_extra_eol_is_not() {
        let out = decode_hdmv_rle(&[1, 0, 0, 0, 0], 1, 1);
        assert_eq!(out.indices, [1]);
        assert_eq!(out.report.ignored_trailing_bytes, 0);
        let noisy = decode_hdmv_rle(&[1, 0, 0, 9, 9, 9], 1, 1);
        assert_eq!(noisy.indices, [1]);
        assert_eq!(noisy.report.ignored_trailing_bytes, 3);
    }
}
