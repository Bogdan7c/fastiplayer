//! Тесты разбора и показа аппаратного кадра NV12 без GPU: настоящие дескрипторы с fd на
//! `/dev/null`, подставной провайдер и подставной видеослой (порядок вызовов и исходы).
//! Показ на настоящем EGL — `tests/dma_buf_frame_to_screen.rs`.

mod frame;
mod materialize;
mod passport;
mod support;
