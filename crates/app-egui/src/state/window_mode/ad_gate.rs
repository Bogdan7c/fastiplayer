//! Запрет перемотки и скорости, пока на экране реклама (сессия 09d, решение 206), и
//! последнее слово о плашке рекламы для окна браузерного режима.
//!
//! Окно закрывает запрет у входа: единственная точка, через которую кнопки, таймлайн
//! (клик и перетаскивание), стрелки, колесо, хоткеи скорости и MPRIS отдают транспорт
//! (`AppState::send_transport_command`), спрашивает [`AdTransportGate::judge`] до того,
//! как команда станет намерением зрителя или уйдёт плееру. Так бегунок и значение
//! скорости не прыгают. Последний рубеж (плашка появилась между кликом и проходом
//! медиа-части) — у `SessionMedia`. Пауза, «играть» и громкость не затрагиваются;
//! команды страницы тоже: страница сама ведёт своё время и скорость.
//!
//! Скраб таймлайна — это несколько команд плееру (начать, цели, закончить), и их нельзя
//! рвать посередине. Правило: скраб, начатый до рекламы, дозавершается (иначе плеер
//! застрял бы в «scrub»); скраб, чьё начало отклонено, отклоняется целиком до своего
//! завершения, даже если реклама кончилась на середине жеста.

use player_core::PlayerCommand;
use tracing::{debug, info};

use crate::ad_plate::{AD_RATE_REFUSED_LINE, AD_SEEK_REFUSED_LINE, AdPlate};

/// Что решили про команду транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateVerdict {
    /// Команда идёт дальше.
    Pass,
    /// Команда отклонена: ни плееру, ни странице.
    Refused,
}

/// Последнее слово о плашке и память «скраб отклонён».
#[derive(Debug, Default)]
pub(crate) struct AdTransportGate {
    /// Плашка рекламы (`None` — рекламы нет).
    plate: Option<AdPlate>,
    /// Начало текущего скраба отклонено: остальные его команды тоже отклоняются.
    refusing_current_scrub: bool,
}

impl AdTransportGate {
    /// Принимает слово медиа-части; `true` — что-то изменилось (окну нужен кадр).
    pub(crate) fn note_plate(&mut self, plate: Option<AdPlate>) -> bool {
        let changed = self.plate != plate;
        self.plate = plate;
        changed
    }

    /// Плашка, которую рисует окно.
    pub(crate) const fn plate(&self) -> Option<&AdPlate> {
        self.plate.as_ref()
    }

    /// Заперт ли транспорт: плашка есть, и предохранитель по сроку её запрет не снял.
    fn locks_transport(&self) -> bool {
        self.plate.is_some_and(|plate| plate.locks_transport)
    }

    /// Пропустить ли команду транспорта (см. описание модуля).
    pub(crate) fn judge(&mut self, command: &PlayerCommand) -> GateVerdict {
        match command {
            PlayerCommand::Seek(_) if self.locks_transport() => {
                info!("{AD_SEEK_REFUSED_LINE}");
                GateVerdict::Refused
            }
            PlayerCommand::SetPlaybackRate(_) if self.locks_transport() => {
                info!("{AD_RATE_REFUSED_LINE}");
                GateVerdict::Refused
            }
            PlayerCommand::BeginScrub { .. } if self.locks_transport() => {
                info!("{AD_SEEK_REFUSED_LINE}");
                self.refusing_current_scrub = true;
                GateVerdict::Refused
            }
            PlayerCommand::BeginScrub { .. } => {
                // Новый жест начинается с чистого листа.
                self.refusing_current_scrub = false;
                GateVerdict::Pass
            }
            PlayerCommand::UpdateScrub(_) | PlayerCommand::PreviewScrub { .. } => {
                if self.refusing_current_scrub || self.locks_transport() {
                    debug!("Scrub target dropped while an ad is on screen");
                    GateVerdict::Refused
                } else {
                    GateVerdict::Pass
                }
            }
            PlayerCommand::EndScrub { .. } => {
                // Жест, начатый до рекламы, дозавершается; отклонённый — гаснет.
                if std::mem::take(&mut self.refusing_current_scrub) {
                    debug!("Scrub end dropped: its start was refused during an ad");
                    GateVerdict::Refused
                } else {
                    GateVerdict::Pass
                }
            }
            _ => GateVerdict::Pass,
        }
    }
}
