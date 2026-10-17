//! Синтез программы: сведение substream-ов access unit-а (A/52 Annex E
//! §3.8.2, §3.8.5) и PCM всех её каналов — общий путь AC-3 и E-AC-3.
//!
//! | модуль      | ответственность                                              |
//! |-------------|--------------------------------------------------------------|
//! | `substream` | [`substream::SubstreamFrame`]: что синтезу нужно от substream-а |
//! | `layout`    | раскладка программы: замена/добавление каналов (§3.8.2)      |
//! | `frame`     | [`frame::Synthesizer`]: rematrix → DRC → IMDCT → OLA → TPNP  |

pub(crate) mod frame;
pub(crate) mod layout;
pub(crate) mod substream;
