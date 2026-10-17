// Rust-часть шейпера, © Fastiplayer contributors.

//! Кэш планов шейпинга `harfrust` одного лица.
//!
//! План (`ShapePlan`) - это разобранный по таблицам GSUB/GPOS список возможностей OpenType для
//! набора «направление + письменность + язык + возможности». Построить его стоит дороже самого
//! шейпинга короткого участка, а HarfBuzz в libass делает то же самое один раз на (шрифт,
//! свойства) и кэширует (`hb_shape_plan_create_cached`). Без кэша каждая реплика строила бы план
//! заново для каждого участка. План зависит только от лица и ключа [`PlanSelector`], поэтому кэш
//! принадлежит записи лица ([`crate::face_cache::FaceEntry`]) и умирает вместе с ней.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use harfrust::{Direction, Script, ShapePlan};

/// Свойства участка, от которых зависит план.
///
/// Язык не входит в ключ: он однозначно выводится из письменности
/// ([`super::script::language_for_script`]). Список возможностей выводится из двух флагов
/// ([`super::run_shaper`]: кернинг и лигатуры), остальные возможности постоянны.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PlanSelector {
    /// Направление письма участка.
    pub(super) direction: Direction,
    /// Письменность участка.
    pub(super) script: Script,
    /// Включён ли кернинг (`kern`).
    pub(super) kerning: bool,
    /// Включены ли лигатуры (`liga`, `clig`).
    pub(super) ligatures: bool,
}

/// Планы шейпинга одного лица по ключу [`PlanSelector`].
///
/// Граница размера (лимита в коде нет, она следует из ключа): направлений два (`shape_run`
/// выбирает только LTR и RTL), письменностей конечное число (набор Unicode, порядка 170),
/// сочетаний флагов четыре, то есть не больше ~1400 планов на лицо, и каждый план - разобранный
/// список возможностей в десятки КБ в худшем случае. Реально на лицо приходится 1-3 плана
/// (письменность реплик). Кэш освобождается вместе с лицом, а число лиц ограничено
/// `shaper_faces`, поэтому общая память плана ограничена сверху числом лиц.
#[derive(Default)]
pub(crate) struct ShapePlanCache {
    plans: Mutex<HashMap<PlanSelector, Arc<ShapePlan>>>,
}

impl ShapePlanCache {
    /// План для `selector`; при отсутствии строится `build` и запоминается.
    ///
    /// Построение идёт под замком: два потока не построят один и тот же план дважды, а отрисовка
    /// одной реплики всё равно идёт в одном потоке, так что ожидания на замке не бывает.
    pub(super) fn plan_for(
        &self,
        selector: PlanSelector,
        build: impl FnOnce() -> ShapePlan,
    ) -> Arc<ShapePlan> {
        // Отравленный замок не опасен: внутри только готовые планы, частично записанных нет.
        let mut plans = self.plans.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(plans.entry(selector).or_insert_with(|| Arc::new(build())))
    }

    /// Сколько планов сохранено (для тестов и диагностики).
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}
