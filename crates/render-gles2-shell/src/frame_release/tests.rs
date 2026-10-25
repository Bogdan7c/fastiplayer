//! Тесты освобождения кадров по fence. Fake-fence сигналится вручную, fake-провайдер считает
//! освобождения, поэтому порядок и количество проверяются детерминированно.

mod binding_behavior;
mod fakes;
mod renderer_detach;
mod trait_coverage;
mod wrapper_forwarding;
