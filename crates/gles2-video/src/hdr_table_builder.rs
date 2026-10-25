//! Фоновая сборка таблицы HDR → SDR (решение владельца 38, сессия 09 плана gles2-renderer).
//!
//! Сборка таблицы — чистый CPU: 16 мс на 780M и 41–50 мс на T410 (отчёт сессии 06). В потоке
//! рисования это пропущенные кадры на каждую смену «перенос + уровни», поэтому, когда на GPU уже
//! есть старая таблица, новую считает отдельный поток, а видеослой тем временем рисует со старой.
//! Загрузка готовых байт в GL остаётся в потоке рисования: контекст GL принадлежит ему.
//!
//! # Почему поток «по требованию», а не постоянный
//! Смена таблицы — редкое событие (ползунок настроек, смена типа HDR), постоянный поток годами
//! простаивал бы. Поток живёт только на время одной сборки (десятки миллисекунд) и ни разу не
//! трогает GL. Из этого же следует уничтожение: [`Drop`] отсоединяет поток (join не нужен и мог бы
//! задержать закрытие на время сборки); результат уходит в закрытый канал, и это штатно.
//!
//! # Инварианты
//! - Не больше одной сборки в полёте.
//! - Пока сборка идёт, новые параметры запоминаются как «последние» (`queued`): промежуточные
//!   значения (перетаскивание ползунка) заменяют друг друга и не строятся никогда.
//! - Готовый результат отдаётся, только если его ключ равен последнему запрошенному; устаревший
//!   отбрасывается и считается в [`HdrTableBuilder::superseded_requests`].
//! - Модуль ничего не знает про GL: он отдаёт байты, кому и как их загрузить — решает владелец
//!   текстуры ([`crate::hdr_table_texture`]).

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread;

use crate::frame_color::RequiredHdrTable;
use crate::hdr::lut::{HdrLutKey, HdrLutTable};
use crate::hdr::{HdrReferenceLevels, HdrTransfer};

/// Функция сборки таблицы. В production — [`HdrLutTable::build`]; тест подставляет свою
/// (с воротами), чтобы доказать порядок событий без гонок по времени.
pub(crate) type BuildTable = dyn Fn(HdrTransfer, HdrReferenceLevels) -> HdrLutTable + Send + Sync;

/// Готовая в фоне таблица и ключ, для которого она посчитана.
pub(crate) struct BuiltHdrTable {
    pub(crate) key: HdrLutKey,
    pub(crate) table: HdrLutTable,
}

/// Что сделал [`HdrTableBuilder::request`].
#[derive(Debug)]
pub(crate) enum BuildRequestOutcome {
    /// Свободный поток: сборка для этого ключа началась.
    Started,
    /// Сборка именно этого ключа уже идёт; запоминать нечего.
    AlreadyInFlight,
    /// Идёт сборка другого ключа: этот запомнен как последний (прежний запомненный, если был,
    /// заменён и не будет построен).
    Queued,
    /// Поток запустить не удалось: вызывающий строит таблицу сам, синхронно.
    SpawnFailed(io::Error),
}

/// Итог [`HdrTableBuilder::poll`].
pub(crate) enum BuildPoll {
    /// Сборки нет или ещё идёт.
    NotReady,
    /// Таблица для последнего запрошенного ключа готова.
    Ready(BuiltHdrTable),
    /// Готовая таблица устарела (с тех пор запросили другую или отозвали запрос) и выброшена.
    Superseded,
    /// Поток сборки умер, не отправив результат (паника в нём). Ключ не построен.
    WorkerLost,
}

/// Сборка в полёте.
struct InFlight {
    key: HdrLutKey,
    receiver: Receiver<BuiltHdrTable>,
}

/// Фоновый сборщик таблиц HDR → SDR. Владеет потоком сборки и очередью «последних параметров».
pub(crate) struct HdrTableBuilder {
    build: Arc<BuildTable>,
    in_flight: Option<InFlight>,
    /// Последние запрошенные параметры, пока идёт другая сборка.
    queued: Option<RequiredHdrTable>,
    /// Ключ, которого сейчас ждёт вызывающий (последний запрошенный, не отозванный).
    wanted: Option<HdrLutKey>,
    /// Запросов, которые не дошли до GPU: заменённых в очереди и устаревших готовых.
    superseded_requests: u64,
}

impl std::fmt::Debug for HdrTableBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HdrTableBuilder")
            .field("in_flight", &self.in_flight.as_ref().map(|job| job.key))
            .field("queued", &self.queued.map(|required| required.key))
            .field("wanted", &self.wanted)
            .field("superseded_requests", &self.superseded_requests)
            .finish_non_exhaustive()
    }
}

impl HdrTableBuilder {
    /// Сборщик с настоящей сборкой таблицы.
    pub(crate) fn new() -> Self {
        Self::with_build(Arc::new(HdrLutTable::build))
    }

    /// Сборщик с подставленной функцией сборки (тесты).
    pub(crate) fn with_build(build: Arc<BuildTable>) -> Self {
        Self {
            build,
            in_flight: None,
            queued: None,
            wanted: None,
            superseded_requests: 0,
        }
    }

    /// Запросить таблицу `required` как последнюю нужную. Не блокирует.
    pub(crate) fn request(&mut self, required: RequiredHdrTable) -> BuildRequestOutcome {
        self.wanted = Some(required.key);
        match &self.in_flight {
            None => match self.spawn(required) {
                Ok(()) => BuildRequestOutcome::Started,
                Err(error) => {
                    self.wanted = None;
                    BuildRequestOutcome::SpawnFailed(error)
                }
            },
            Some(job) if job.key == required.key => {
                // Нужное уже строится: запомненный «хвост» (если был) устарел.
                self.drop_queued();
                BuildRequestOutcome::AlreadyInFlight
            }
            Some(_) => {
                if self.queued.is_some_and(|queued| queued.key == required.key) {
                    // Тот же ключ уже запомнен: повтор ничего не заменяет и ничего не теряет.
                    return BuildRequestOutcome::Queued;
                }
                // Прежний запомненный ключ (другой) заменяется и не построится никогда.
                self.drop_queued();
                self.queued = Some(required);
                BuildRequestOutcome::Queued
            }
        }
    }

    /// Отозвать запрос: нужная таблица уже на GPU (или кадру она не нужна). Идущая сборка
    /// закончится, но её результат будет выброшен.
    pub(crate) fn withdraw(&mut self) {
        self.wanted = None;
        self.drop_queued();
    }

    /// Забрать готовое, если есть, и при необходимости запустить запомненное. Не блокирует.
    pub(crate) fn poll(&mut self) -> BuildPoll {
        let Some(job) = &self.in_flight else {
            return BuildPoll::NotReady;
        };
        let received = job.receiver.try_recv();
        let built = match received {
            Err(TryRecvError::Empty) => return BuildPoll::NotReady,
            Ok(built) => Some(built),
            Err(TryRecvError::Disconnected) => None,
        };
        self.in_flight = None;
        // Исход решается по состоянию ДО запуска запомненного: тот меняет только поток.
        let outcome = match built {
            Some(built) if self.wanted == Some(built.key) => BuildPoll::Ready(built),
            Some(_) => {
                self.superseded_requests += 1;
                BuildPoll::Superseded
            }
            None => BuildPoll::WorkerLost,
        };
        // Запомненное запускается сразу, чтобы поток не простаивал до следующего кадра.
        if let Some(next) = self.queued.take()
            && let Err(error) = self.spawn(next)
        {
            // Ключ остаётся «нужным»: следующий `request` повторит попытку, а при повторном
            // отказе вызывающий построит таблицу синхронно.
            log::warn!("gles2-video: HDR table worker did not start ({error})");
        }
        outcome
    }

    /// Идёт сборка или есть запомненная, И её результат ещё нужен: картинка со старой таблицей
    /// ещё изменится, видеослою нужна перерисовка, когда таблица будет готова.
    ///
    /// Отозванный запрос ([`Self::withdraw`]) не считается: идущая сборка его закончится и будет
    /// выброшена, но картинка из-за неё уже не изменится, а забрать её некому (`poll` вызывает
    /// только запрос новой таблицы). Иначе приложение перерисовывалось бы вхолостую.
    pub(crate) const fn has_pending_work(&self) -> bool {
        self.wanted.is_some() && (self.in_flight.is_some() || self.queued.is_some())
    }

    /// Сколько запросов не дошло до GPU (заменены новыми или выброшены устаревшими).
    pub(crate) const fn superseded_requests(&self) -> u64 {
        self.superseded_requests
    }

    fn drop_queued(&mut self) {
        if self.queued.take().is_some() {
            self.superseded_requests += 1;
        }
    }

    fn spawn(&mut self, required: RequiredHdrTable) -> io::Result<()> {
        let (sender, receiver) = channel();
        let build = Arc::clone(&self.build);
        thread::Builder::new()
            .name("gles2-hdr-table".to_owned())
            .spawn(move || {
                let table = build(required.transfer, required.levels);
                // Получатель закрыт, если видеослой уничтожен или забыл запрос: результат
                // никому не нужен, это штатный конец потока, а не ошибка.
                let delivered = sender.send(BuiltHdrTable {
                    key: required.key,
                    table,
                });
                if delivered.is_err() {
                    log::debug!("gles2-video: HDR table finished after its video layer was gone");
                }
            })?;
        self.in_flight = Some(InFlight {
            key: required.key,
            receiver,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
