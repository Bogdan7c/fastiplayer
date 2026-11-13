//! Сон машины глазами сервера сеанса (сессия 16d2).
//!
//! Перед сном ядро Linux прерывает каждый поток процесса, чтобы его заморозить.
//! Блокирующее чтение сокета с тайм-аутом (у сервера сеанса это шаг опроса) в этот
//! момент возвращается с `EINTR`. На T480s так после каждого пробуждения рвалась
//! живая связь с экстеншеном: сервер принимал `EINTR` за уход браузера.
//!
//! Тест делает с процессом то же, что заморозка: много раз шлёт сигнал каждому его
//! потоку. Обработчик сигнала пустой и стоит с `SA_RESTART`, как у обычной
//! программы: вызовы без тайм-аута ядро перезапускает само, а вызовы сокета с
//! тайм-аутом всё равно кончаются `EINTR` — ровно случай сна. Результат: связь
//! жива (ping от страницы получает pong сервера), сеанс не терял соединения.
//!
//! Тест — отдельный исполняемый файл: обработчик сигнала ставится на весь процесс
//! и не должен задевать другие тесты.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

#[path = "loopback/support.rs"]
#[allow(
    dead_code,
    reason = "общая обвязка петлевых тестов; этому файлу нужна только её часть"
)]
mod support;

use std::time::{Duration, Instant};

use browser_bridge_session::SessionLifecycleEvent;

use support::{Env, attach, round_trip_ping};

/// Сколько длится «буря» прерываний: много шагов опроса сервера (5 мс в тестах).
const INTERRUPT_STORM: Duration = Duration::from_millis(400);
/// Пауза между волнами сигналов: поток соединения успевает снова уйти в чтение.
const BETWEEN_WAVES: Duration = Duration::from_millis(2);
/// Нижняя граница волн: иначе тест мог бы пройти, ничего толком не прервав.
const MIN_WAVES: usize = 50;

/// Пустой обработчик: сигнал нужен только ради прерванного системного вызова.
extern "C" fn ignore_signal(_signal: libc::c_int) {}

/// Ставит пустой обработчик `SIGUSR1` с `SA_RESTART` на весь процесс.
fn install_noop_handler() {
    // SAFETY: структура обнулена и заполнена до вызова; обработчик ничего не
    // делает, поэтому безопасен для асинхронного сигнала.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ignore_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0,
            "обработчик сигнала не поставился"
        );
    }
}

/// Номера всех потоков процесса, кроме вызывающего.
fn other_thread_ids() -> Vec<libc::pid_t> {
    // SAFETY: `gettid` без аргументов и без побочных эффектов.
    let own = unsafe { libc::gettid() };
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|thread| *thread != own)
        .collect()
}

/// Шлёт `SIGUSR1` каждому потоку процесса волнами, как заморозка перед сном.
/// Возвращает число волн.
fn interrupt_every_thread_for(storm: Duration) -> usize {
    // SAFETY: `getpid` без аргументов и без побочных эффектов.
    let process = unsafe { libc::getpid() };
    let started = Instant::now();
    let mut waves = 0;
    while started.elapsed() < storm {
        for thread in other_thread_ids() {
            // SAFETY: `tgkill` шлёт сигнал потоку этого же процесса; поток мог уже
            // кончиться — тогда вызов вернёт ошибку, и это не важно для теста.
            unsafe {
                libc::syscall(libc::SYS_tgkill, process, thread, libc::SIGUSR1);
            }
        }
        waves += 1;
        std::thread::sleep(BETWEEN_WAVES);
    }
    waves
}

#[test]
fn a_machine_sleep_interrupting_every_thread_does_not_cut_the_page_link() {
    let env = Env::start();
    let token = env.open("sleep").token;
    let mut socket = attach(env.port(), "sleep", &token);
    // До «сна» связь работает: сервер читает и отвечает.
    round_trip_ping(&mut socket, b"before-sleep");

    install_noop_handler();
    let waves = interrupt_every_thread_for(INTERRUPT_STORM);
    assert!(waves >= MIN_WAVES, "прерываний слишком мало: {waves}");

    // После «пробуждения» та же связь жива: ping страницы дошёл, pong сервера вернулся.
    round_trip_ping(&mut socket, b"after-sleep");
    let lifecycle = env.events.seen.get().lifecycle;
    assert!(
        !lifecycle
            .iter()
            .any(|event| matches!(event, SessionLifecycleEvent::ConnectionLost { .. })),
        "сервер счёл связь потерянной: {lifecycle:?}"
    );
    assert!(
        !lifecycle
            .iter()
            .any(|event| matches!(event, SessionLifecycleEvent::Ended { .. })),
        "сеанс закончился: {lifecycle:?}"
    );
}
