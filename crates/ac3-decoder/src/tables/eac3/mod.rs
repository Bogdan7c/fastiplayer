//! Таблицы E-AC-3 (A/52:2012 Annex E; ETSI TS 102 366 v1.4.1 Annex E).
//!
//! | модуль              | таблицы                                   |
//! |---------------------|-------------------------------------------|
//! | `exponent_strategy` | E2.10 — стратегии экспонент кадра          |
//! | `coupling`          | E2.12, E2.13, E3.9, E3.10 — полосы и ECPL  |
//! | `spx`               | E2.11, E3.13, E3.14 — спектральное расширение |
//! | `aht`               | E3.1, E3.2, E3.6 — hebap и GAQ             |
//! | `vq`                | E4.1–E4.7 — кодовые книги VQ               |
//!
//! Все таблицы сверены с текстом обеих спецификаций скриптом
//! `tools/extract_eac3_tables.py`; у каждой есть тест-сверка.

pub(crate) mod aht;
pub(crate) mod coupling;
pub(crate) mod exponent_strategy;
pub(crate) mod spx;
pub(crate) mod vq;
