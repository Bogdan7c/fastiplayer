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

/// Метка строки «поколение впервые показано» (16b3b): `generation=<N> start_ms=<мс>`. `start_ms` — секунда
/// ролика, с которой окно его показало: у ролика, вернувшегося в ленте «назад к живому элементу», это
/// секунда, где страница его остановила, а не нуль.
pub const STREAM_GENERATION_SHOWN_MARKER: &str = "Browser stream generation shown";

/// Поколение, которое окно впервые показало, и секунда, с которой оно идёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownGeneration {
    pub generation: u64,
    /// Секунда старта поколения, миллисекунды.
    pub start_ms: u64,
}

/// Разбор строки `Browser stream generation shown generation=<N> start_ms=<мс>`; `None`, если строка не
/// такая или в ней нет одного из полей.
pub fn parse_stream_generation_shown(line: &str) -> Option<ShownGeneration> {
    if !line.contains(STREAM_GENERATION_SHOWN_MARKER) {
        return None;
    }
    Some(ShownGeneration {
        generation: number_field(line, "generation")?,
        start_ms: number_field(line, "start_ms")?,
    })
}

/// Сколько миллисекунд новое поколение прождало данных у цели (строка плеера «у цели данных нет …
/// `waited_ms=<N>`»); `None`, если поля нет.
pub fn parse_target_wait_ms(line: &str) -> Option<u64> {
    number_field(line, "waited_ms")
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

    #[test]
    fn the_shown_line_gives_the_generation_and_its_start_second() {
        let line = "2026-11-19T10:00:00.1Z  INFO Browser stream generation shown generation=3 start_ms=3921";
        assert_eq!(
            parse_stream_generation_shown(line),
            Some(ShownGeneration {
                generation: 3,
                start_ms: 3921
            })
        );
        // Строка «загружено» несёт только номер поколения и «показано» не является.
        assert_eq!(
            parse_stream_generation_shown("INFO Browser stream generation loaded generation=3"),
            None
        );
        assert_eq!(
            parse_stream_generation_shown("INFO Browser stream generation shown generation=3"),
            None
        );
    }

    #[test]
    fn the_wait_line_gives_the_milliseconds_waited() {
        let line = "WARN The new stream generation got no data at its target; it starts where data exists waited_ms=10003";
        assert_eq!(parse_target_wait_ms(line), Some(10003));
        assert_eq!(parse_target_wait_ms("INFO nothing to see"), None);
    }
}
