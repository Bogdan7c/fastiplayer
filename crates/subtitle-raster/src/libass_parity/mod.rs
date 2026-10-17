//! Тесты паритета с libass 0.17.5: порт растеризатора против эталонов системного libass.
//!
//! Модуль существует только в тестовой сборке. Эталоны (`tests/fixtures/libass/expected/*.png`
//! и `*.json`) генерирует `scripts/subtitle-reference/generate.py` по кейсам `cases/*.ass`.
//! Для каждого кейса [`pipeline`] повторяет путь libass от ASS-рисунка до списка картинок
//! (позиция и квантование, обводка, фильтры заливки/тени), но все тяжёлые шаги выполняет
//! настоящий код порта: [`crate::outline`], [`crate::raster`], [`crate::bitmap`].

mod ass_case;
#[cfg(test)]
mod ass_event_tests;
mod ass_tolerances;
#[cfg(test)]
mod ass_tolerances_animation;
#[cfg(test)]
mod ass_tolerances_layer2;
#[cfg(test)]
mod ass_tolerances_layer3;
mod pipeline;
#[cfg(test)]
mod tests;
mod text_case;
#[cfg(test)]
mod text_tests;
