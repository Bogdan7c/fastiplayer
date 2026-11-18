//! Разбор строки журнала плеера «поколение потока загружено» (сессия 16b2).
//!
//! Плеер пишет `Browser stream generation loaded generation=<N>` каждый раз, когда окно начинает показывать
//! НОВЫЙ источник сеанса (новый MediaSource страницы, переезд на следующий ролик ленты). В отличие от
//! `loads=` в сводке (раз в 10 с) строка приходит сразу, поэтому по ней стенд видит переезд в момент
//! события, а не с опозданием на сводку.
//!
//! Всё здесь — чистые функции над строками.

use super::player_log::number_field;

/// Метка строки «поколение потока загружено».
pub const STREAM_GENERATION_LOADED_MARKER: &str = "Browser stream generation loaded";

/// Метка строки «пул поверхностей окна перестроен под размер потока»: плеер пишет её, когда новый поток
/// (новый ролик ленты) имеет другой размер кадра, `width=<ширина> height=<высота с выравниванием>`.
pub const STREAM_SURFACE_RECONFIGURED_MARKER: &str = "Output surface pool reconfigured for stream";

/// Ширина кадра потока из строки `Output surface pool reconfigured for stream width=<W> height=<H> …`;
/// `None`, если строка не такая или ширина потеряна. Берём ширину, а не высоту: высота в строке
/// выровнена декодером (368 вместо 360), ширина роликов стенда кратна 16 и пишется как есть.
pub fn parse_stream_surface_width(line: &str) -> Option<u64> {
    if !line.contains(STREAM_SURFACE_RECONFIGURED_MARKER) {
        return None;
    }
    number_field(line, "width")
}

/// Номер поколения из строки `Browser stream generation loaded generation=<N>`; `None`, если строка
/// не такая или номер потерян.
pub fn parse_stream_generation_loaded(line: &str) -> Option<u64> {
    if !line.contains(STREAM_GENERATION_LOADED_MARKER) {
        return None;
    }
    number_field(line, "generation")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generation_number_is_read_from_the_loaded_line() {
        let line =
            "2026-11-18T09:57:46.149857Z  INFO Browser stream generation loaded generation=2";
        assert_eq!(parse_stream_generation_loaded(line), Some(2));
    }

    #[test]
    fn the_stream_width_is_read_from_the_reconfigured_line_and_not_from_look_alikes() {
        let line = "INFO Output surface pool reconfigured for stream width=512 height=288 rt_format=1 capacity=24";
        assert_eq!(parse_stream_surface_width(line), Some(512));
        // Строка про другой кадр (экспорт DMA-BUF) тоже несёт `width=`, но потоком не является.
        let export = "INFO First VA NV12 DMA-BUF export descriptor fourcc=1 width=640 height=368";
        assert_eq!(parse_stream_surface_width(export), None);
        assert_eq!(
            parse_stream_surface_width(
                "INFO Output surface pool reconfigured for stream height=288"
            ),
            None
        );
    }

    #[test]
    fn other_lines_and_lines_without_a_number_are_not_generations() {
        // Строка про поколение при seek (`generation=1 pipeline_generation=1`) — не загрузка источника.
        let seek =
            "INFO Demux seek transaction accepted kind=\"seek\" generation=1 pipeline_generation=1";
        assert_eq!(parse_stream_generation_loaded(seek), None);
        let broken = "INFO Browser stream generation loaded";
        assert_eq!(parse_stream_generation_loaded(broken), None);
    }
}
