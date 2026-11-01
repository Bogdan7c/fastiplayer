//! Когда новое поколение потока отдать плееру и с какого места (сессия 04d, путь C,
//! решения 146–147).
//!
//! **Корень дефекта, который закрывает модуль.** Новый `MediaSource` страницы (смена
//! качества YouTube) открывается, пока прежнее поколение ещё играет. Зритель кликнул по
//! таймлайну в X, пока новое поколение открывалось: раньше оно сразу уходило плееру,
//! поставленное на X, хотя данных у X ещё не было. Страница потом перематывалась сама
//! (YouTube возвращался на своё место Y) — эта перемотка ждала показа поколения, а
//! показа без данных у X не бывает: взаимная блокировка → `VideoPreflightTimeout` →
//! `playback_failed`.
//!
//! **Правило (путь C, выбран владельцем).** Поколение-сегменты с целью X отдаётся плееру,
//! только когда перемотка к X села бы с полки (`seek_outlook(X) == FromShelf`) — то же
//! правило, что у настоящей перемотки плеера. Пока ждём, прежнее поколение играет, окно
//! показывает спиннер, а часы странице говорят «жду данных в X».
//!
//! ```text
//!  клик зрителя X ─► aim_for_viewer ─┐            (SessionMedia просит страницу один раз)
//!  page_seek Y ────► aim_for_page ───┤ последняя побеждает; Y ≈ цели — не новая цель
//!                                    ▼
//!  poll_pending: arm(now) ─► decide(monitor, now, page)
//!        ├─ полка держит цель ──────────────────► HandOver(PageSeekedTo(цель))
//!        ├─ ждём < срока, страница жива ────────► Wait{recheck_at}
//!        └─ срок вышел или страницы нет ────────► туда, где данные есть:
//!              позиция страницы → первый общий момент всех полос → иначе ждём дальше
//! ```
//!
//! **Кто чем владеет.**
//! - Состояние «куда поставить ждущее поколение» — [`GenerationPlacement`] в
//!   `PendingGeneration` медиа-части; живёт, пока поколение не отдано плееру.
//! - Ответы полки (держит ли цель, где первый общий момент, одна ли это перемотка) —
//!   у источника, монитор `PushedStreamMonitor`; модуль только спрашивает.
//! - Сообщения странице и загрузка в плеер — у `SessionMedia`: модуль ничего не шлёт.
//!
//! **Граница.** `aim_for_viewer` / `aim_for_page` (назвать цель), `arm` (срок ожидания —
//! с первого прохода после смены цели), `decide` (вердикт ворот), `awaited_target`
//! (цель, которую ждём: часы странице и спиннер окна), `named_start` (старт без ворот:
//! файл, эфир, поток без длительности). Методы `SessionMedia` внизу — склейка с
//! медиа-частью (просьба странице, часы, вид ожидания для окна).
//!
//! **Инварианты.**
//! 1. Без данных у цели поколение-сегменты плееру не отдаётся (иначе отказ плеера).
//! 2. Своя перемотка страницы в ДРУГОЕ место меняет цель; совпавшая с целью (допуск
//!    источника) — не меняет и срок не продлевает: повторы страницы не держат ожидание
//!    вечно, а перемена цели странице ничего не шлёт — петель нет.
//! 3. Срок ожидания данных у цели — [`GenerationPlacementPolicy`], тот
//!    же `player.seek.commit_timeout_ms`, что у настоящей перемотки (10 с по умолчанию).
//!    После него — туда, где данные есть; нигде нет — ждём дальше: поставить поколение
//!    без данных значило бы тот же отказ плеера.
//! 4. Страница ушла (доигрывание после закрытия вкладки) — байтов больше не будет:
//!    срок считается вышедшим сразу.
//! 5. Эфир и поток без длительности не позиционируются (правило 03e): ворот у них нет,
//!    поколение уходит плееру сразу и начинается там, где лежат байты страницы — у эфира
//!    это живой край или точка окна DVR, куда ушла сама страница.
//! 6. Поколение-файл (`<video src>`) ворот не имеет: его байты приходят по запросам
//!    `fetch_range`, а старт с цели — перемотка после показа (`AfterLoad::SeekTo`, 03e).
//!
//! **Тесты.** Чистые правила — `generation_placement_tests.rs`; сквозь `SessionMedia` с
//! настоящим источником — `tests/generation_placement.rs`; процессные (настоящий процесс,
//! окно, MPRIS-клики) — `tests/browser_session_process/generation_switch_clicks.rs`.

use std::time::{Duration, Instant};

use fastiplayer_config::PlayerSeekConfig;
use player_core::PlaybackState;
use pushed_media_source::{PageSeekRequest, PushedStreamMonitor, ShelfSeekOutlook};
use tracing::{debug, info, warn};

use super::SessionMedia;
use super::generation_start::{AfterLoad, GenerationStart};
use crate::browser_session::intake_notice::GenerationSource;
use crate::browser_session::playback::PlaybackObservation;

/// Как часто перепроверять полку, пока поколение ждёт данных у цели.
///
/// Главный цикл и так просыпается на каждые новые байты страницы (подсказка «легли
/// байты»), а срок ожидания будит его сам. Перепроверка — страховка от пропущенного
/// пробуждения: вопрос к полке берёт замок буфера на микросекунды, а четыре вопроса в
/// секунду незаметны и по CPU, и для зрителя (реакция ворот — не позже 250 мс).
const PLACEMENT_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

/// Сроки ворот поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GenerationPlacementPolicy {
    /// Сколько ждать данных у цели, прежде чем ставить туда, где данные есть.
    target_data_wait: Duration,
}

impl GenerationPlacementPolicy {
    /// Сроки с явным ожиданием данных у цели.
    pub(crate) const fn new(target_data_wait: Duration) -> Self {
        Self { target_data_wait }
    }

    /// Ожидание данных у цели = срок коммита перемотки плеера
    /// (`player.seek.commit_timeout_ms`): «то же правило, что у настоящей перемотки» —
    /// зритель ждёт клика по таймлайну столько же, сколько ждал бы обычной перемотки.
    pub(crate) fn from_seek_config(seek: &PlayerSeekConfig) -> Self {
        Self::new(Duration::from_millis(seek.commit_timeout_ms))
    }

    /// Умолчание — из умолчаний config-а (10 с), а не своим числом.
    pub(crate) fn recommended() -> Self {
        Self::from_seek_config(&PlayerSeekConfig::default())
    }
}

/// Кто назвал цель (для журнала: только слово, без позиций страницы в тексте).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetOrigin {
    /// Клик или перетаскивание зрителя в окне.
    Viewer,
    /// Перемотка самой страницы (`seek_started{page_seek}`).
    Page,
}

/// Куда поколение должно встать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aim {
    /// Цели нет: с первого момента, где есть данные всех полос (правило 03e).
    FirstCommonData,
    /// Цель названа зрителем или страницей (последняя побеждает).
    Target {
        position: Duration,
        origin: TargetOrigin,
    },
}

/// Что сделала своя перемотка страницы с целью ждущего поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageAim {
    /// Страница ушла в другое место: теперь цель — её позиция, срок — заново.
    Retargeted,
    /// Совпала с целью, которую назвала сама страница (допуск источника): та же
    /// перемотка, но точнее — позиция страницы побеждает (правило 03e «последняя
    /// побеждает»), срок ожидания не продлевается.
    RefinedPageTarget,
    /// Совпала с целью зрителя (эхо нашей просьбы, допуск источника): цель зрителя
    /// остаётся, срок не продлевается.
    EchoOfViewerTarget,
}

/// Жива ли страница, которая может прислать байты у цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageAvailability {
    /// Сеанс идёт: байты у цели ещё могут прийти.
    Listening,
    /// Страница ушла (доигрывание): новых байтов не будет.
    Gone,
}

/// Вердикт ворот для открытого поколения-сегментов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementVerdict {
    /// Отдать плееру с этого старта.
    HandOver(GenerationStart),
    /// Ждать данных; перепроверить не позже `recheck_at` (или раньше, по байтам).
    Wait {
        /// Когда перепроверить без байтов.
        recheck_at: Instant,
    },
}

/// Попросили ли страницу перемотаться к цели зрителя (одна просьба на действие).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageSeekAsk {
    /// Не просили: цель уже была на полке, страница не слушает или цель не зрителя.
    NotAsked,
    /// Просили: источник поколения помечен, вторая просьба к этой цели не уйдёт.
    Asked,
}

/// Что ждущее поколение-сегменты сейчас делает с потоком (для часов и спиннера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingOpenState {
    /// Поток ещё не открылся: удержат ли его ворота, пока неизвестно.
    NotOpenedYet,
    /// Поток открыт, но ворота держат его до данных у цели.
    HeldByGate,
    /// Это файл `<video src>`: ворот у него нет.
    NotSegments,
}

/// Ждёт ли медиа-часть данных для нового поколения (спиннер окна, 04d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationWait {
    /// Не ждёт: нового поколения с целью нет.
    Idle,
    /// Новое поколение ждёт данных у названной цели; прежнее видео играет.
    AwaitingDataAtTarget,
}

/// Куда и когда поставить ждущее поколение (см. описание модуля).
#[derive(Debug, Clone, Copy)]
pub(super) struct GenerationPlacement {
    /// Сроки.
    policy: GenerationPlacementPolicy,
    /// Куда встать.
    aim: Aim,
    /// Последняя своя перемотка страницы — запасной старт «где страница».
    page_position: Option<Duration>,
    /// С какого прохода ждём данных у текущей цели (`None` — цель сменилась, срок ещё
    /// не взведён).
    waiting_since: Option<Instant>,
    /// О том, что данных нет нигде, уже сказано в журнал (раз на цель).
    stall_reported: bool,
    /// Попросили ли страницу перемотаться к текущей цели зрителя.
    page_seek_ask: PageSeekAsk,
}

impl GenerationPlacement {
    /// Цели нет: старт с первого общего момента всех полос.
    pub(super) const fn first_common_data(policy: GenerationPlacementPolicy) -> Self {
        Self {
            policy,
            aim: Aim::FirstCommonData,
            page_position: None,
            waiting_since: None,
            stall_reported: false,
            page_seek_ask: PageSeekAsk::NotAsked,
        }
    }

    /// Чем начнёт поколение, которое страница открыла ВМЕСТО этого, ещё не отданного
    /// плееру (вторая смена качества до передачи, требование 147).
    ///
    /// Цель зрителя переходит к новому поколению: элемент страницы уже получил нашу
    /// перемотку, она не привязана к умершему `MediaSource`, а новой просьбы странице
    /// не будет (её отметку источника нового поколения ставит медиа-часть). Тип цели
    /// сохраняется, чтобы эхо страницы и дальше узнавалось как эхо.
    ///
    /// Своя перемотка страницы НЕ переходит (правило 03e): новый `MediaSource`
    /// сбрасывает время элемента, и страница назовёт позицию заново.
    ///
    /// Срок ожидания данных — заново: новому поколению странице нужно заново наполнить
    /// новые SourceBuffer-ы, и срок, начатый ради умершего поколения, отнял бы у него
    /// время ожидания (запасной старт сработал бы раньше, чем страница успела бы).
    pub(super) const fn carried_to_replacing_generation(&self) -> Self {
        match self.aim {
            Aim::Target {
                position,
                origin: TargetOrigin::Viewer,
            } => Self {
                policy: self.policy,
                aim: Aim::Target {
                    position,
                    origin: TargetOrigin::Viewer,
                },
                page_position: None,
                waiting_since: None,
                stall_reported: false,
                page_seek_ask: self.page_seek_ask,
            },
            Aim::Target {
                origin: TargetOrigin::Page,
                ..
            }
            | Aim::FirstCommonData => Self::first_common_data(self.policy),
        }
    }

    /// Страницу попросили перемотаться к текущей цели зрителя.
    pub(super) fn note_page_asked(&mut self) {
        self.page_seek_ask = PageSeekAsk::Asked;
    }

    /// Цель зрителя, о которой страницу уже просили (её отметку нужно перенести на
    /// источник поколения, сменившего это).
    pub(super) const fn asked_viewer_target(&self) -> Option<Duration> {
        match (self.aim, self.page_seek_ask) {
            (
                Aim::Target {
                    position,
                    origin: TargetOrigin::Viewer,
                },
                PageSeekAsk::Asked,
            ) => Some(position),
            _ => None,
        }
    }

    /// Цель, если её назвал зритель (`None` — цели нет или её назвала страница).
    pub(super) const fn awaited_viewer_target(&self) -> Option<Duration> {
        match self.aim {
            Aim::Target {
                position,
                origin: TargetOrigin::Viewer,
            } => Some(position),
            _ => None,
        }
    }

    /// Что сделать после загрузки эфира, который не позиционируется (03e): клик
    /// зрителя исполняется обычной перемоткой плеера, когда поколение показано
    /// (решение владельца по 04d: клик в окне DVR не теряется). Своя перемотка
    /// страницы — нет: страница уже ушла туда сама, и её байты лежат там, откуда
    /// эфир и начнётся.
    pub(super) const fn after_live_load(&self) -> AfterLoad {
        match self.awaited_viewer_target() {
            Some(target) => AfterLoad::SeekTo(target),
            None => AfterLoad::Nothing,
        }
    }

    /// Зритель кликнул: его цель — новая (каждое действие зрителя — новая перемотка,
    /// срок заново, как у настоящей перемотки плеера).
    pub(super) fn aim_for_viewer(&mut self, target: Duration) {
        self.retarget(target, TargetOrigin::Viewer);
    }

    /// Страница перемоталась сама. В другое место — новая цель (последняя побеждает);
    /// совпала с уже выбранной (`is_same_target` — допуск источника) — та же перемотка:
    /// срок не продлевается (см. [`PageAim`]).
    pub(super) fn aim_for_page(
        &mut self,
        target: Duration,
        is_same_target: impl FnOnce(Duration, Duration) -> bool,
    ) -> PageAim {
        self.page_position = Some(target);
        if let Aim::Target { position, origin } = self.aim
            && is_same_target(position, target)
        {
            return match origin {
                TargetOrigin::Viewer => PageAim::EchoOfViewerTarget,
                TargetOrigin::Page => {
                    // Та же перемотка страницы, но её позиция точнее: срок — прежний.
                    self.aim = Aim::Target {
                        position: target,
                        origin: TargetOrigin::Page,
                    };
                    PageAim::RefinedPageTarget
                }
            };
        }
        self.retarget(target, TargetOrigin::Page);
        PageAim::Retargeted
    }

    /// Взводит срок ожидания данных у цели (идемпотентно): срок идёт с первого прохода
    /// главного цикла после смены цели, даже если поток ещё не открылся.
    pub(super) fn arm(&mut self, now: Instant) {
        if matches!(self.aim, Aim::Target { .. }) {
            self.waiting_since.get_or_insert(now);
        }
    }

    /// Цель, которую поколение ждёт (`None` — цели нет).
    pub(super) const fn awaited_target(&self) -> Option<Duration> {
        match self.aim {
            Aim::FirstCommonData => None,
            Aim::Target { position, .. } => Some(position),
        }
    }

    /// Старт без ворот: для файла, эфира и потока без длительности (они не ждут
    /// данных у цели — см. инварианты 5 и 6).
    pub(super) const fn named_start(&self) -> GenerationStart {
        match self.aim {
            Aim::FirstCommonData => GenerationStart::FirstCommonData,
            Aim::Target { position, .. } => GenerationStart::PageSeekedTo(position),
        }
    }

    /// Вердикт ворот для открытого поколения-сегментов, которое можно позиционировать.
    pub(super) fn decide(
        &mut self,
        monitor: &PushedStreamMonitor,
        now: Instant,
        page: PageAvailability,
    ) -> PlacementVerdict {
        let Aim::Target { position, origin } = self.aim else {
            // Цели нет — правило 03e: там, где есть данные всех полос.
            return PlacementVerdict::HandOver(GenerationStart::FirstCommonData);
        };
        match monitor.seek_outlook(position) {
            ShelfSeekOutlook::FromShelf => {
                return PlacementVerdict::HandOver(GenerationStart::PageSeekedTo(position));
            }
            // Полоса без меток времени (MPEG-TS): по времени не перемотать вовсе — ждать
            // бессмысленно; позиционирование само откатится к началу, как до 04d.
            ShelfSeekOutlook::Unavailable => {
                return PlacementVerdict::HandOver(GenerationStart::PageSeekedTo(position));
            }
            ShelfSeekOutlook::AsksPage => {}
        }
        let waiting_since = *self.waiting_since.get_or_insert(now);
        // Срок из config-а может быть огромным: переполнение `Instant` = «срока нет».
        let deadline = waiting_since.checked_add(self.policy.target_data_wait);
        let next_recheck = now + PLACEMENT_RECHECK_INTERVAL;
        if page == PageAvailability::Listening && deadline.is_none_or(|deadline| now < deadline) {
            return PlacementVerdict::Wait {
                recheck_at: deadline.map_or(next_recheck, |deadline| deadline.min(next_recheck)),
            };
        }
        if let Some(start) = self.fallback_start(monitor, position) {
            info!(
                ?origin,
                ?page,
                waited_ms = u64::try_from(now.saturating_duration_since(waiting_since).as_millis())
                    .unwrap_or(u64::MAX),
                "The new stream generation got no data at its target; it starts where data exists"
            );
            return PlacementVerdict::HandOver(start);
        }
        if !self.stall_reported {
            self.stall_reported = true;
            warn!(
                ?origin,
                ?page,
                "The new stream generation has no data anywhere it could start; it keeps waiting"
            );
        }
        PlacementVerdict::Wait {
            recheck_at: next_recheck,
        }
    }

    /// Запасной старт, когда у цели данных нет: где страница, иначе первый общий момент
    /// всех полос. Только если полка его держит — иначе это та же блокировка.
    fn fallback_start(
        &self,
        monitor: &PushedStreamMonitor,
        target: Duration,
    ) -> Option<GenerationStart> {
        if let Some(page_position) = self.page_position
            && !monitor.is_same_seek_target(page_position, target)
            && monitor.seek_outlook(page_position) == ShelfSeekOutlook::FromShelf
        {
            return Some(GenerationStart::PageSeekedTo(page_position));
        }
        let common_start = monitor.earliest_common_start()?;
        (monitor.seek_outlook(common_start) == ShelfSeekOutlook::FromShelf)
            .then_some(GenerationStart::FirstCommonData)
    }

    /// Новая цель: срок и отметка журнала — заново.
    fn retarget(&mut self, target: Duration, origin: TargetOrigin) {
        self.aim = Aim::Target {
            position: target,
            origin,
        };
        self.waiting_since = None;
        self.stall_reported = false;
        self.page_seek_ask = PageSeekAsk::NotAsked;
    }
}

/// Нашлось ли ждущее поколение для цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingAim {
    /// Ждущего поколения нет: цель относится к загруженному.
    NoPendingGeneration,
    /// Цель досталась ждущему поколению.
    AimedPending,
}

impl SessionMedia {
    /// Та же медиа-часть с другими сроками ворот поколения (процесс берёт их из config-а).
    pub(crate) fn with_placement_policy(mut self, policy: GenerationPlacementPolicy) -> Self {
        self.placement_policy = policy;
        self
    }

    /// Ждёт ли медиа-часть данных для нового поколения — окно показывает спиннер,
    /// хотя прежнее видео ещё играет (решение 146).
    pub(crate) fn generation_wait(&self) -> GenerationWait {
        if self.awaited_generation_target().is_some() {
            GenerationWait::AwaitingDataAtTarget
        } else {
            GenerationWait::Idle
        }
    }

    /// Цель, которую новое поколение действительно ждёт, — для часов страницы и
    /// спиннера окна.
    ///
    /// Только то, что будет удержано или исполнено:
    /// - поток открыт и ворота держат его до данных у цели — любая цель;
    /// - поток ещё не открылся — только цель ЗРИТЕЛЯ: её точно ждут (ворота у ролика
    ///   или перемотка после показа у эфира), а зритель должен видеть, что клик принят.
    ///   Своя перемотка страницы до открытия — обычная смена качества: поколение почти
    ///   всегда откроется сразу с данными у позиции страницы, и мигать «жду в T» ей
    ///   незачем (часы, как до 04d, — прежнего видео);
    /// - файл — никогда: ворот у него нет.
    pub(super) fn awaited_generation_target(&self) -> Option<Duration> {
        let pending = self.pending.as_ref()?;
        match pending.feed.open_state() {
            PendingOpenState::HeldByGate => pending.placement.awaited_target(),
            PendingOpenState::NotOpenedYet => pending.placement.awaited_viewer_target(),
            PendingOpenState::NotSegments => None,
        }
    }

    /// С чем начнёт новое поколение: файл, сменивший поток сегментов того же поколения,
    /// — то же поколение (цель остаётся); поколение, сменившее ещё не отданное, —
    /// с целью зрителя прежнего (см. [`GenerationPlacement::carried_to_replacing_generation`]),
    /// а источник нового поколения помечается: страница уже идёт к цели, вторая просьба
    /// не уйдёт.
    pub(super) fn placement_for_new_generation(
        &self,
        upgrades_pending_to_file: bool,
        source: &GenerationSource,
    ) -> GenerationPlacement {
        let Some(pending) = self.pending.as_ref() else {
            return GenerationPlacement::first_common_data(self.placement_policy);
        };
        if upgrades_pending_to_file {
            return pending.placement;
        }
        let carried = pending.placement.carried_to_replacing_generation();
        if let (Some(target), GenerationSource::Segments { monitor, .. }) =
            (carried.asked_viewer_target(), source)
        {
            monitor.note_page_seek_requested(target);
        }
        carried
    }

    /// Перемотка зрителя, пока новое поколение открывается или ждёт данных: цель — его
    /// старт, прежнее видео не двигается. Странице — ровно одна просьба на действие, и
    /// только если полка цели не держит (то же правило, что у настоящей перемотки);
    /// источник помечает цель, чтобы установка поколения не попросила второй раз.
    pub(super) fn aim_pending_for_viewer(
        &mut self,
        target: Duration,
        page_listens: bool,
    ) -> PendingAim {
        let Some(pending) = self.pending.as_mut() else {
            return PendingAim::NoPendingGeneration;
        };
        pending.placement.aim_for_viewer(target);
        // Цель сменилась — спросить ворота на ближайшем проходе, не дожидаясь срока.
        pending.retry_at = None;
        debug!(
            generation = pending.generation.get(),
            "Viewer seek is the start target of the generation that is not handed over yet"
        );
        // Файлу просьба не нужна: его байты идут по `fetch_range`.
        let Some(monitor) = pending.feed.segments_monitor().cloned() else {
            return PendingAim::AimedPending;
        };
        if !page_listens {
            return PendingAim::AimedPending;
        }
        if monitor.seek_outlook(target) == ShelfSeekOutlook::FromShelf {
            debug!(
                "Viewer seek target is already on the new generation's shelf; the page is not asked"
            );
            return PendingAim::AimedPending;
        }
        monitor.note_page_seek_requested(target);
        pending.placement.note_page_asked();
        let seek_id = self.outbound.request_page_seek(PageSeekRequest { target });
        debug!(
            seek_id = seek_id.0,
            "Asking the page to seek for the viewer"
        );
        PendingAim::AimedPending
    }

    /// Своя перемотка страницы, пока новое поколение открывается или ждёт данных: она
    /// относится к нему (источник нового поколения о ней уже знает), видео прежнего
    /// поколения не двигается.
    pub(super) fn aim_pending_at_page_seek(&mut self, target: Duration) -> PendingAim {
        let Some(pending) = self.pending.as_mut() else {
            return PendingAim::NoPendingGeneration;
        };
        let monitor = pending.feed.segments_monitor().cloned();
        let page_aim = pending.placement.aim_for_page(target, |current, page| {
            monitor
                .as_ref()
                .is_some_and(|monitor| monitor.is_same_seek_target(current, page))
        });
        // Цель сдвинулась — спросить ворота на ближайшем проходе (эхо цели зрителя
        // ничего не меняет).
        if page_aim != PageAim::EchoOfViewerTarget {
            pending.retry_at = None;
        }
        debug!(
            generation = pending.generation.get(),
            ?page_aim,
            "Page seek is the start target of the generation that is not handed over yet; the loaded video is not moved"
        );
        PendingAim::AimedPending
    }
}

/// Что видят часы страницы, пока новое поколение ждёт данных у цели: «жду данных» в
/// самой цели, со скоростью плеера.
///
/// Элемент страницы уже стоит в новом `MediaSource` на цели и сам ждёт там данных.
/// Позиция и конец ПРЕЖНЕГО поколения ему чужие: увидев «вернулись на старое место»
/// или «конец», страница перемоталась бы обратно (новая цель — петля) или решила бы,
/// что видео кончилось.
pub(super) fn clock_while_generation_awaits(
    observation: &PlaybackObservation,
    target: Duration,
) -> PlaybackObservation {
    PlaybackObservation {
        position: target,
        state: PlaybackState::Seeking,
        ..observation.while_media_installs()
    }
}

#[cfg(test)]
#[path = "generation_placement_tests.rs"]
mod tests;
