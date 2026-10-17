//! Рисование ASS/SSA, слой 1: стиль, позиционирование, переносы, обводка и тень, слои.
//!
//! Модуль — порт поведения libass 0.17.5 (ISC) от «реплика ASS» до «план для текстового
//! конвейера». Сам конвейер (шейпинг, раскладка, растеризация) общий с SRT/WebVTT и живёт в
//! остальных модулях crate; ASS добавляет только то, что специфично для формата:
//! - [`options`] — публичные настройки: масштаб текста, чей стиль применять, размер хранения видео;
//! - `frame_scale` — масштаб «координаты сценария -> пиксели холста» (`PlayRes`, `LayoutRes`,
//!   размер хранения, `ScaledBorderAndShadow`, пропорции пикселя);
//! - `explicit` — «явные» реплики (`\pos`, `\move`, `\clip`, `\org`, `\p`, Banner/Scroll),
//!   к которым пользовательский масштаб текста не применяется;
//! - `filter_params` — значения `\blur`, `\be`, `\clip`, `\iclip`, `\pbo` (зажатие и «кто побеждает»);
//! - `clip_plan` — перевод клипов реплики из координат сценария в экранные величины для стадии
//!   `crate::clip` (она накладывается после кэша реплик);
//! - `render_state` — состояние рендера и применение тегов (`\r`, `\fs`, `\c`, `\bord` ...);
//! - `look` — перевод состояния в вид символов в пикселях, автор или пользователь;
//! - `event_plan` — обход текста реплики и сборка общего стиля реплики.
//!
//! - `animation` — время: `\move`, `\fad`/`\fade` и веса `\t` на момент показа, ключ кэша и ближайшее
//!   изменение картинки (владелец всей логики, зависящей от момента).
//!
//! ## Что рисуется в сессии 10
//! Караоке `\k \K \kf \ko \kt`: теги захватываются состоянием рендера (`render_state`) на следующий
//! символ, а расчёт слов, граница заливки и раскраска живут в `crate::karaoke` и стадии после
//! кэша `crate::clip` (буквы рисуются один раз). Анимация `\t` рисуется: вес `k` считает `tag_weight` по часам реплики, обход
//! вложенных тегов с подменой веса — `transform_apply` (общий для `RenderState` и покадрового
//! клипа `clip_animation`), вопросы «что анимируется и когда изменится» — `transform_scan`.
//! Клипы `\clip`/`\iclip` рисуются стадией `crate::clip` (после кэша реплик). Размытие `\blur`/`\be` рисуется: значения попадают в `SpanLook::blur`, само размытие — в `compose` (`run::composite_run`). Рисунки `\p` рисуются: текст в режиме рисунка — один псевдоглиф (см. `event_plan`). Масштаб букв, межбуквенный
//! интервал, синтетическое утолщение/наклон и линии подчёркивания/зачёркивания рисуются
//! общим конвейером по значениям вида символа (`SpanLook`).

mod animation;
mod clip_animation;
mod clip_plan;
mod drawing_scale;
mod event_plan;
mod explicit;
mod filter_params;
mod frame_scale;
mod look;
mod options;
mod render_state;
mod scroll;
mod tag_weight;
#[cfg(test)]
mod test_script;
mod transform_apply;
mod transform_scan;

pub(crate) use animation::{
    AssAnimation, AssAnimationKey, AssAnimationState, AssEventClock, evaluate_animation,
};
pub(crate) use clip_animation::ClipAnimation;
pub(crate) use clip_plan::EventClipPlan;
pub(crate) use event_plan::plan_ass_event;
pub(crate) use frame_scale::AssFrameScale;
pub use options::{
    AssRenderOptions, AssStorageSize, AssStyleSource, AssTextScale, InvalidAssTextScale,
};
