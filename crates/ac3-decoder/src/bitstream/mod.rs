//! Низкоуровневый доступ к битам кадра: ограниченный читатель и CRC-16.
//!
//! Модуль ничего не знает о синтаксисе AC-3 — только о битах и о полиноме
//! A/52 §7.10.1. Его используют и AC-3 (`syntax`), и будущий E-AC-3.

pub(crate) mod crc;
pub(crate) mod reader;

pub(crate) use reader::{BitReader, ReadPastLimit};
