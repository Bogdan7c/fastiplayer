//! Синтаксис AC-3 (A/52 §5): разбор полей syncframe-а и их смысл.
//!
//! Модуль читает биты в порядке Таблиц 5.1–5.5 и Annex D Таблицы D2.1,
//! проверяет ограничения §5.5/§7.10.2 и отдаёт алгоритмам `spectral`
//! уже разобранные значения. Синтаксис E-AC-3 (Annex E) — соседний модуль
//! `eac3` поверх тех же `spectral`-шагов.

pub(crate) mod annex_d;
pub(crate) mod audblk;
pub(crate) mod bsi;
pub(crate) mod bsi_values;
pub(crate) mod eac3;
pub(crate) mod syncframe;
pub(crate) mod syncinfo;
