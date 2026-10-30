//! Способность demuxer-а пересоздать Symphonia reader с физического начала источника.

/// Можно ли перечитать источник с начала (reprobe нового `FormatReader`).
///
/// Seekability описывает seek внутри открытого reader-а, а reprobe требует ещё и доступа к
/// исходному потоку байтов. У production-конструкторов (файл, seekable stream, `ByteSource`)
/// он есть всегда; у reader-а, собранного напрямую из готового `FormatReader` (fake-и тестов),
/// источника нет, и пути, которые перечитывают начало, обязаны это видеть, а не паниковать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceRestart {
    /// Reader можно пересоздать с начала того же источника.
    FromSourceStart,
    /// Источник для повторного чтения недоступен.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "все production-конструкторы знают источник; без него reader собирают только тесты"
        )
    )]
    Unavailable,
}
