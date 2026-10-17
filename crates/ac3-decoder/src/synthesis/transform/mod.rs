//! Обратное преобразование и окно (A/52:2012 §7.9.4): чистая DSP-часть
//! синтеза без состояния между блоками.

pub(crate) mod fft;
pub(crate) mod imdct;
pub(crate) mod window;

#[cfg(test)]
mod tests;
