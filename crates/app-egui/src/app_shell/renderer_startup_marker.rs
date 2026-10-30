//! Метка «запуск рендера не дошёл до первого кадра» (сессия 09 плана gles2-renderer).
//!
//! Зачем: драйвер может не вернуть ошибку, а повесить или уронить процесс при создании
//! рендера. Тогда следующий запуск не должен слепо повторять то же самое. Протокол:
//! 1. ПЕРЕД пробой и созданием рендера семейства X (проба поднимает драйвер и тоже может его
//!    уронить) в файл метки атомарно пишется `X.stable_id()`; тот же X второй раз не пишется;
//! 2. после ПЕРВОГО показанного кадра любого вида метка удаляется — ровно один раз;
//! 3. при штатной остановке до первого кадра (закрытие, suspend, фатальная ошибка уже после
//!    создания) метка тоже удаляется, иначе свёрнутое на старте окно дало бы ложный фолбэк;
//!    ИСКЛЮЧЕНИЕ: если рендер отказывал кадрам (`Failed`) и не показал ни одного — старт не
//!    завершён, метка остаётся;
//! 4. если процесс упал между 1 и 2, файл остаётся, и следующий запуск прочитает его как
//!    [`RendererStartupMarker::UnfinishedStartup`].
//!
//! Модуль владеет файлом и фазой «ждём первый кадр»; выбор рендера читает метку и сообщает о
//! начале создания через [`UnfinishedStartupTracker`] и ничего не знает о файлах. Пересоздание
//! рендера внутри семейства (Apply настроек) метку НЕ пишет: оно идёт через
//! `render_backend::create_renderer`, минуя старт.

use std::fs;
use std::io;
use std::path::PathBuf;

use atomic_file_store::{AtomicFileWriteOutcome, replace_file_atomically};
use render_shell_api::RendererFamily;
use tracing::{info, warn};

use crate::render_backend::{RendererStartupMarker, UnfinishedStartupTracker};
use crate::telemetry::RenderFrameTotals;

/// Все семейства, чьи идентификаторы допустимы в файле метки.
const KNOWN_FAMILIES: [RendererFamily; 2] = [RendererFamily::WgpuVulkan, RendererFamily::Gles2];

/// Файл метки: чтение, атомарная запись и удаление. Фазу старта не знает.
struct MarkerFile {
    /// Путь из `ConfigPaths::renderer_startup_marker_file`.
    path: PathBuf,
}

impl MarkerFile {
    /// Читает метку. Нет файла — прошлый запуск завершился штатно; мусор и ошибки чтения —
    /// предупреждение и «как без метки» (ложный фолбэк хуже, чем потерянная защита).
    fn read(&self) -> RendererStartupMarker {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return RendererStartupMarker::NoUnfinishedStartup;
            }
            Err(error) => {
                warn!(%error, "Could not read the renderer startup marker; ignoring it");
                return RendererStartupMarker::NoUnfinishedStartup;
            }
        };
        let stable_id = text.trim();
        match KNOWN_FAMILIES
            .iter()
            .find(|family| family.stable_id() == stable_id)
        {
            Some(family) => RendererStartupMarker::UnfinishedStartup(*family),
            None => {
                warn!(
                    marker = stable_id,
                    "The renderer startup marker names an unknown renderer; ignoring it"
                );
                RendererStartupMarker::NoUnfinishedStartup
            }
        }
    }

    /// Атомарно записывает метку семейства. Неудача — предупреждение: старт продолжается без
    /// защиты от повисшего драйвера, но не прерывается.
    fn write(&self, family: RendererFamily) {
        match replace_file_atomically(&self.path, family.stable_id().as_bytes()) {
            AtomicFileWriteOutcome::Durable => {}
            AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(problem) => warn!(
                ?problem,
                "The renderer startup marker was written but its durability is unconfirmed"
            ),
            AtomicFileWriteOutcome::NotReplaced(failure) => warn!(
                ?failure,
                "Could not write the renderer startup marker; continuing without it"
            ),
        }
    }

    /// Удаляет метку. Отсутствие файла — не ошибка; остальное — предупреждение.
    fn remove(&self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => warn!(%error, "Could not remove the renderer startup marker"),
        }
    }
}

/// Метка глазами окна браузерного режима: читает, но никогда не пишет и не снимает.
///
/// Файл метки общий с обычным плеером (один `ConfigPaths`). Окно браузерного режима
/// уважает чужую метку (обычный плеер упал на этом семействе — не повторяем), но своей
/// не ставит: два процесса гонялись бы за одним файлом, а снятие чужой метки отключило
/// бы защиту обычного плеера. Цена — сбой драйвера в окне браузерного режима защитой не
/// запоминается (известное ограничение 04a).
pub(crate) struct ReadOnlyRendererStartupMarker {
    marker: RendererStartupMarker,
}

impl ReadOnlyRendererStartupMarker {
    /// Читает метку по пути из `ConfigPaths::renderer_startup_marker_file`.
    pub(crate) fn read_from(marker_path: PathBuf) -> Self {
        Self {
            marker: MarkerFile { path: marker_path }.read(),
        }
    }

    /// Без файла: метки нет (каталог config-а не найден).
    pub(crate) const fn absent() -> Self {
        Self {
            marker: RendererStartupMarker::NoUnfinishedStartup,
        }
    }
}

impl crate::render_backend::UnfinishedStartupTracker for ReadOnlyRendererStartupMarker {
    fn unfinished_startup(&self) -> RendererStartupMarker {
        self.marker
    }

    fn probe_starting(&mut self, _family: RendererFamily) {}

    fn creation_starting(&mut self, _family: RendererFamily) {}

    fn creation_failed(&mut self, _family: RendererFamily) {}
}

/// Фаза метки внутри процесса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerPhase {
    /// Метка не ожидает снятия: запуск завершён или рендер ещё не создавался.
    Idle,

    /// Метка записана (или должна была быть), ждём первый показанный кадр.
    AwaitingFirstFrame {
        /// Итоги кадров к началу старта этого рендера.
        totals_at_startup: RenderFrameTotals,
    },
}

/// Владелец метки незавершённого старта рендера.
pub(super) struct RendererStartupWatch {
    /// Файл метки.
    file: MarkerFile,

    /// Что ждём сейчас.
    phase: MarkerPhase,

    /// Итоги кадров к началу текущего старта (счётчики телеметрии живут весь процесс, а метка
    /// относится к конкретному созданию рендера).
    totals_at_startup: RenderFrameTotals,

    /// Семейство, чей идентификатор сейчас лежит в файле, записанный ЭТИМ запуском; повторная
    /// запись того же содержимого пропускается (лишний fsync на старте).
    written_family: Option<RendererFamily>,
}

impl RendererStartupWatch {
    /// Владелец метки с файлом по пути `marker_path`.
    pub(super) fn new(marker_path: PathBuf) -> Self {
        Self {
            file: MarkerFile { path: marker_path },
            phase: MarkerPhase::Idle,
            totals_at_startup: RenderFrameTotals::default(),
            written_family: None,
        }
    }

    /// Вызывается перед выбором рендера: запоминает итоги кадров, чтобы «первым кадром»
    /// считался только кадр НОВОГО рендера (после suspend счётчики не нулевые).
    pub(super) fn begin_startup(&mut self, totals_so_far: RenderFrameTotals) {
        self.totals_at_startup = totals_so_far;
    }

    /// Снимает метку после первого показанного кадра нового рендера, ровно один раз.
    ///
    /// `presented_frames_total` вычисляется лениво: пока метка не ждёт кадра, счётчик не
    /// читается (вызов стоит на каждом кадре).
    pub(super) fn settle_after_frame(&mut self, presented_frames_total: impl FnOnce() -> u64) {
        let MarkerPhase::AwaitingFirstFrame { totals_at_startup } = self.phase else {
            return;
        };
        if presented_frames_total() > totals_at_startup.presented {
            self.clear_marker();
            info!("First frame presented; the renderer startup marker is cleared");
        }
    }

    /// Рендер остановлен штатно (закрытие, suspend, фатальная ошибка после создания).
    ///
    /// - кадр был показан (снятие ещё не успело сработать) или не было ни показа, ни отказа
    ///   рендера (окно закрыли до первого кадра) — это не падение драйвера, метка снимается;
    /// - показов не было, но рендер отказывал кадрам (`Failed`) — «живой, но немой» рендер:
    ///   старт НЕ завершён, метка остаётся, и следующий запуск пропустит это семейство.
    pub(super) fn renderer_closed(&mut self, totals_now: RenderFrameTotals) {
        let MarkerPhase::AwaitingFirstFrame { totals_at_startup } = self.phase else {
            return;
        };
        let presented_since_startup = totals_now.presented > totals_at_startup.presented;
        let failed_since_startup = totals_now.failed > totals_at_startup.failed;
        if failed_since_startup && !presented_since_startup {
            warn!("The renderer failed frames and presented none; the startup marker is kept");
            self.phase = MarkerPhase::Idle;
            return;
        }
        self.clear_marker();
    }

    /// Пишет метку семейства, если в файле (по записи этого запуска) лежит другое, и ждёт кадра.
    /// Ждём кадра и при неудачной записи: так первый кадр уберёт устаревшую метку прошлого
    /// запуска, которую не удалось перезаписать.
    fn mark_family(&mut self, family: RendererFamily) {
        if self.written_family != Some(family) {
            self.file.write(family);
            self.written_family = Some(family);
        }
        self.phase = MarkerPhase::AwaitingFirstFrame {
            totals_at_startup: self.totals_at_startup,
        };
    }

    /// Удаляет файл метки и возвращает фазу в покой.
    fn clear_marker(&mut self) {
        self.file.remove();
        self.written_family = None;
        self.phase = MarkerPhase::Idle;
    }
}

impl UnfinishedStartupTracker for RendererStartupWatch {
    fn unfinished_startup(&self) -> RendererStartupMarker {
        self.file.read()
    }

    fn probe_starting(&mut self, family: RendererFamily) {
        self.mark_family(family);
    }

    fn creation_starting(&mut self, family: RendererFamily) {
        self.mark_family(family);
    }

    fn creation_failed(&mut self, _family: RendererFamily) {
        // Ошибка создания — обработанный сбой при живом процессе: метка не нужна.
        self.clear_marker();
    }
}

#[cfg(test)]
#[path = "renderer_startup_marker/tests.rs"]
mod tests;
