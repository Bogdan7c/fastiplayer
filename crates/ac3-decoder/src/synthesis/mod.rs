//! Синтез PCM из частотных коэффициентов (A/52:2012 §7.5, §7.7, §7.9).
//!
//! | модуль          | ответственность                                   | разделы        |
//! |-----------------|---------------------------------------------------|----------------|
//! | `rematrix`      | восстановление L/R режима 2/0                     | §7.5, Т. 7.25–7.28 |
//! | `dynamic_range` | множители `dynrng`/`compr`, политика DRC          | §7.7, §6.1.9   |
//! | `transform`     | окно KBD, обратное БПФ, IMDCT 512 и 2×256         | §7.9.4, Т. 7.33 |
//! | `overlap`       | overlap-add и хвосты каналов между кадрами        | §7.9.4 шаг 6   |
//! | `transient_pre_noise` | TPNP во временной области                   | Annex E §3.7   |
//! | `program`       | сведение substream-ов и сборка кадра в [`crate::DecodedAudio`] | §6.1.8–6.1.11, Annex E §3.8 |
//!
//! `rematrix`, `dynamic_range`, `transform`, `overlap` и
//! `transient_pre_noise` не знают о синтаксисе; `program` — единственный
//! путь синтеза и для AC-3 (программа из одного substream-а), и для E-AC-3.

pub(crate) mod dynamic_range;
pub(crate) mod overlap;
pub(crate) mod program;
pub(crate) mod rematrix;
pub(crate) mod transform;
pub(crate) mod transient_pre_noise;
