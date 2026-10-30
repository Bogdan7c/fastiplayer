//! Позднее вступление полосы после перемотки (правило 5 `stream_buffer::seek`).
//!
//! Полоса, у которой нет данных у цели перемотки, вступает со своего первого
//! сегмента после цели — если другие полосы покрывают это время (окно
//! [`LateJoinWindow`]) и сегмент годится по происхождению ([`LateJoinCandidates`]).
//! Здесь — только решение по полке одной полосы; опоры (полки других полос)
//! считает владелец буфера.

use super::{Arrival, FeedChoice, LaneShelf};
use crate::framing::{MediaTime, MediaTimeSpan};

/// Где полоса, у которой нет данных у цели перемотки, может вступить позже.
///
/// Полоса без данных у цели (звук начинается позже видео, или страница прислала
/// звук только со своего сегмента после цели) не ждёт вечно сегмент с целью,
/// которого нет: она вступает со своего ПЕРВОГО сегмента после цели — как на
/// обычном старте первая коробка задаёт курсор. Чтобы не принять за такое
/// вступление байты, которых страница просто ещё не прислала, вступление
/// разрешено только внутри непрерывных данных других полос, покрывающих цель
/// (`covered`): у них это время уже есть, а у полосы — нет. Плеер садится на
/// цель по другим полосам, а эта полоса подхватывает со своего времени.
///
/// Если у полосы есть данные внутри этого куска РАНЬШЕ точки вступления — это
/// дыра посреди полосы (недокачанное или выселенное), а не её позднее начало:
/// вступать нельзя, байты с цели должна прислать страница.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LateJoinWindow {
    /// Цель перемотки: вступать можно только сегментом, начавшимся позже.
    target: MediaTime,
    /// Непрерывные данные других полос вокруг цели: вступать можно только
    /// сегментом, начавшимся раньше конца, и только если до него внутри куска у
    /// полосы данных нет.
    covered: MediaTimeSpan,
}

impl LateJoinWindow {
    /// Окно по куску `covered` других полос; `None` — кусок кончается не позже
    /// цели (вступать не во что).
    pub(crate) fn new(target: MediaTime, covered: MediaTimeSpan) -> Option<Self> {
        (covered.end() > target).then_some(Self { target, covered })
    }

    /// Может ли сегмент, начавшийся в `start`, быть точкой вступления.
    fn admits(self, start: MediaTime) -> bool {
        start > self.target && start < self.covered.end()
    }
}

/// Какие сегменты полосы годятся как точка позднего вступления ([`LateJoinWindow`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateJoinCandidates {
    /// Любые на полке: при перемотке другие полосы уже держали цель, и полка —
    /// то, что страница отдала для этого времени.
    AnyOnShelf,
    /// Только принятые после перемотки: цель попросили у страницы, и лежавшее на
    /// полке раньше — время прежних мест (например, после перемотки чуть назад от
    /// уже скачанного): вступив с него, полоса пропустила бы данные цели, которые
    /// страница ещё несёт.
    ArrivedSinceSeek,
}

impl LaneShelf {
    /// Место сегмента, которым полоса без данных у цели вступает позже
    /// ([`LateJoinWindow`]): ПЕРВЫЙ сегмент, начавшийся после цели, — если он
    /// начинается внутри окна. Более поздние сегменты не рассматриваются: между
    /// целью и ними у полосы есть данные, которые надо играть. `None`, если у
    /// полосы есть данные внутри куска других полос раньше этого сегмента: это
    /// сегмент с целью (подача идёт с него, [`Self::prepare_restart`]) или дыра
    /// посреди полосы (байты должна прислать страница).
    ///
    /// `candidates` — годится ли сегмент, лежавший на полке до перемотки
    /// ([`LateJoinCandidates`]).
    fn late_join_index(
        &self,
        window: LateJoinWindow,
        candidates: LateJoinCandidates,
    ) -> Option<usize> {
        let first_after_target = self
            .entries
            .iter()
            .position(|entry| entry.span().start() > window.target)?;
        let data_earlier_inside_window = self.entries[..first_after_target]
            .iter()
            .any(|entry| entry.span().end() > window.covered.start());
        let candidate = &self.entries[first_after_target];
        let candidate_eligible = match candidates {
            LateJoinCandidates::AnyOnShelf => true,
            LateJoinCandidates::ArrivedSinceSeek => candidate.arrival == Arrival::SinceLastSeek,
        };
        (candidate_eligible
            && !data_earlier_inside_window
            && window.admits(candidate.span().start()))
        .then_some(first_after_target)
    }

    /// Может ли полоса вступить позже в окне `window` (правило — в
    /// [`Self::late_join_index`]).
    pub(crate) fn can_join_late(
        &self,
        window: LateJoinWindow,
        candidates: LateJoinCandidates,
    ) -> bool {
        self.late_join_index(window, candidates).is_some()
    }

    /// Готовит позднее вступление: как [`Self::prepare_restart`], только с первого
    /// сегмента после цели. `None` — вступать нечем, ничего не меняется.
    pub(crate) fn prepare_late_join(
        &mut self,
        window: LateJoinWindow,
        candidates: LateJoinCandidates,
    ) -> Option<FeedChoice> {
        let index = self.late_join_index(window, candidates)?;
        Some(self.restart_feeding_from(index))
    }

    /// Перемотка: всё, что лежит на полке, отныне «пришло до последней перемотки».
    pub(crate) fn note_seek(&mut self) {
        for entry in &mut self.entries {
            entry.arrival = Arrival::BeforeLastSeek;
        }
    }
}
