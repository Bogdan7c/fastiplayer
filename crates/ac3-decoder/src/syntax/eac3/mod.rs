//! Синтаксис E-AC-3 (A/52:2012 Annex E; ETSI TS 102 366 v1.4.1 Annex E):
//! от байтов syncframe-а до частотных коэффициентов в том же представлении
//! [`crate::syntax::audblk::output::DecodedAudioBlock`], что и у AC-3.
//!
//! | модуль          | ответственность                                        | разделы Annex E |
//! |-----------------|--------------------------------------------------------|-----------------|
//! | `bsi`           | `syncinfo`/`bsi`: тип, длина, частота, блоки, метаданные | §2.2.1–2.2.2, §2.3.1 |
//! | `bsi_metadata`  | метаданные сведения и информационные                   | §2.3.1.9–2.3.1.63 |
//! | `channel_map`   | позиции каналов: `chanmap` / `acmod`                    | §2.3.1.8, Т. E2.5 |
//! | `audfrm`        | параметры кадра: стратегии, AHT, SNR, TPNP, ослабление SPX | §2.2.3, §2.3.2 |
//! | `audblk`        | аудиоблок: SPX, coupling/ECPL, экспоненты, мантиссы/AHT | §2.2.4, §2.3.3, §3.3–3.6 |
//! | `syncframe`     | кадр целиком, CRC, учёт бит хвоста                      | §2.2.5–2.2.6, §3.2 |
//! | `access_unit`   | substream-ы программы 0 и их состав                     | §2.3.1.1–2.3.1.2, §3.8 |
//! | `tool_usage`    | счётчики инструментов (диагностика)                     | — |
//! | `inspection`    | публичный разбор access unit-а без PCM                  | — |
//!
//! Алгоритмы частотной области (экспоненты, битовое распределение, мантиссы,
//! coupling, SPX, ECPL, AHT) — общие шаги `crate::spectral`.

pub(crate) mod access_unit;
pub(crate) mod audblk;
pub(crate) mod audfrm;
pub(crate) mod bsi;
pub(crate) mod bsi_metadata;
pub(crate) mod channel_map;
pub(crate) mod inspection;
pub(crate) mod syncframe;
pub(crate) mod tool_usage;
