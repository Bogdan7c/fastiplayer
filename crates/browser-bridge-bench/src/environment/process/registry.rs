//! Реестр групп процессов стенда и аварийная уборка по сигналу (Ctrl-C, SIGTERM, SIGHUP).
//!
//! Проблема: каждый процесс стенда — лидер собственной группы (иначе мы не смогли бы
//! убить потомков), поэтому Ctrl-C в терминале до них не доходит, а сам стенд по сигналу
//! умирает БЕЗ `Drop`. Итог — осиротевшие `kwin_wayland --virtual`, частная шина,
//! активированный ею плеер, браузер и посредник.
//!
//! Решение: статическая таблица номеров групп из атомарных чисел. Обработчик сигнала
//! делает только то, что разрешено в обработчике: читает атомики и вызывает `killpg`,
//! `sigaction`, `raise` (всё это async-signal-safe). Никаких аллокаций, блокировок,
//! печати и `Drop` в обработчике нет.

use std::io;
use std::sync::atomic::{AtomicI32, Ordering};

/// Сколько групп можно держать одновременно. Стенду нужно ~5 (kwin, шина kwin, шина
/// плеера, браузер); запас большой, переполнение — явная ошибка запуска, не молчание.
const MAX_REGISTERED_GROUPS: usize = 32;

/// Значение «ячейка свободна»: настоящий номер группы всегда положителен.
const EMPTY_SLOT: i32 = 0;

/// Сигналы, по которым стенд убирает за собой.
const CLEANUP_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// Таблица номеров групп; `EMPTY_SLOT` — свободная ячейка.
static REGISTERED_GROUPS: [AtomicI32; MAX_REGISTERED_GROUPS] =
    [const { AtomicI32::new(EMPTY_SLOT) }; MAX_REGISTERED_GROUPS];

/// Запись в реестре; освобождается через [`GroupRegistration::release`] (из `Drop` процесса).
#[derive(Debug)]
pub(super) struct GroupRegistration {
    slot: usize,
}

impl GroupRegistration {
    /// Записывает номер группы в первую свободную ячейку.
    pub(super) fn register(process_group: u32) -> io::Result<Self> {
        let group = i32::try_from(process_group)
            .ok()
            .filter(|group| *group > EMPTY_SLOT)
            .ok_or_else(|| io::Error::other("process group id is out of range"))?;
        for (slot, cell) in REGISTERED_GROUPS.iter().enumerate() {
            // CAS: две одновременные регистрации не займут одну ячейку.
            if cell
                .compare_exchange(EMPTY_SLOT, group, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Self { slot });
            }
        }
        Err(io::Error::other("too many registered process groups"))
    }

    /// Освобождает ячейку: группа уже убита и подобрана, убивать по номеру больше нельзя
    /// (номер мог бы достаться чужому процессу).
    pub(super) fn release(&self) {
        REGISTERED_GROUPS[self.slot].store(EMPTY_SLOT, Ordering::Release);
    }
}

/// Номера групп, зарегистрированных сейчас (для тестов и диагностики).
#[cfg(test)]
pub(super) fn registered_groups() -> Vec<i32> {
    REGISTERED_GROUPS
        .iter()
        .map(|cell| cell.load(Ordering::Acquire))
        .filter(|group| *group != EMPTY_SLOT)
        .collect()
}

/// Убивает SIGKILL все зарегистрированные группы.
///
/// Async-signal-safe: только чтение атомиков и `killpg`. Именно эту функцию вызывает
/// обработчик сигнала; тест вызывает её же. Ошибки `killpg` игнорируются осознанно:
/// «группы уже нет» — это и есть желаемое состояние, а печатать из обработчика нельзя.
pub(crate) fn kill_all_registered_groups() {
    for cell in &REGISTERED_GROUPS {
        let group = cell.load(Ordering::Acquire);
        if group != EMPTY_SLOT {
            // SAFETY: `killpg` — простой системный вызов без указателей; номер группы
            // положителен (проверено при регистрации).
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }
}

/// Обработчик SIGINT/SIGTERM/SIGHUP: добить группы, затем умереть по тому же сигналу.
extern "C" fn cleanup_and_die(signal: libc::c_int) {
    kill_all_registered_groups();
    // SA_RESETHAND уже вернул стандартное действие на входе в обработчик, а SA_NODEFER не
    // блокирует сигнал, поэтому повторная отправка завершает процесс сразу и статус выхода
    // получается привычным (130 для Ctrl-C).
    // SAFETY: `raise` async-signal-safe.
    unsafe {
        libc::raise(signal);
    }
}

/// Ставит обработчики аварийной уборки. Вызывать один раз из `main` до запуска процессов.
pub fn install_interrupt_cleanup() -> io::Result<()> {
    for signal in CLEANUP_SIGNALS {
        // SAFETY: структура обнуляется целиком (так велит документация `libc`), затем
        // заполняются поля; указатели в вызовах — на живые локальные значения.
        let result = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = cleanup_and_die as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut())
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
