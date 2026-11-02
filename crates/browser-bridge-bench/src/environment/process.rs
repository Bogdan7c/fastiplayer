//! Дочерний процесс в собственной группе: гибель стенда убивает всё его потомство.
//!
//! Активированный шиной плеер, посредник браузера и помощники Chromium — потомки
//! процесса, который запустили мы. Если убить только корень, потомки осиротеют
//! (именно так «живут» забытые `fastiplayer --browser-session`). Поэтому каждый
//! процесс стенда — лидер своей группы, а останавливаем мы группу целиком.

mod registry;

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Шаг опроса «процесс уже вышел?».
const WAIT_POLL_STEP: Duration = Duration::from_millis(50);

/// Процесс-лидер группы; при `Drop` вся группа получает SIGKILL.
pub(crate) struct ManagedProcess {
    child: Child,
    /// Запись в реестре аварийной уборки: пока процесс жив, сигнал стенду убьёт его группу.
    registration: registry::GroupRegistration,
}

impl ManagedProcess {
    /// Запускает команду лидером новой группы процессов.
    pub(crate) fn spawn(mut command: Command) -> io::Result<Self> {
        command.process_group(0);
        let mut child = command.spawn()?;
        // Номер группы равен pid лидера (`process_group(0)`).
        match registry::GroupRegistration::register(child.id()) {
            Ok(registration) => Ok(Self {
                child,
                registration,
            }),
            Err(error) => {
                // Без записи в реестре процесс осиротел бы при Ctrl-C: не запускаем вовсе.
                let _kill_result = child.kill();
                let _wait_result = child.wait();
                Err(error)
            }
        }
    }

    /// Забирает канал стандартного вывода дочернего процесса.
    pub(crate) fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    /// Ждёт выход лидера не дольше `timeout`.
    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(WAIT_POLL_STEP);
        }
    }

    /// Шлёт сигнал (`TERM`, `KILL`) всей группе утилитой `kill`.
    pub(crate) fn signal_group(&self, signal: &str) {
        // Отрицательный номер = группа. Ошибку «группы уже нет» глотать безопасно:
        // это и есть желаемое состояние «процессов нет».
        let _already_gone = Command::new("kill")
            .args(["-s", signal, "--"])
            .arg(format!("-{}", self.child.id()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Живы ли ещё процессы группы (сигнал 0 никого не трогает, только проверяет).
    pub(crate) fn group_alive(&self) -> bool {
        Command::new("kill")
            .args(["-s", "0", "--"])
            .arg(format!("-{}", self.child.id()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Ждёт, пока в группе не останется процессов; `true` — группа исчезла сама.
    pub(crate) fn wait_group_gone(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.group_alive() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(WAIT_POLL_STEP);
        }
        true
    }

    /// Мягко останавливает группу: TERM, ждать `grace`, затем KILL. Возвращает статус лидера.
    pub(crate) fn terminate(&mut self, grace: Duration) -> Option<ExitStatus> {
        self.signal_group("TERM");
        let graceful = self.wait_timeout(grace).ok().flatten();
        // Даже если лидер вышел, потомки могли остаться: добиваем группу всегда.
        self.signal_group("KILL");
        graceful.or_else(|| {
            let _kill_result = self.child.kill();
            self.child.wait().ok()
        })
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.signal_group("KILL");
        let _kill_result = self.child.kill();
        let _wait_result = self.child.wait();
        // Группа убита и подобрана: номер больше нельзя убивать (его мог получить чужой).
        self.registration.release();
    }
}

pub use registry::install_interrupt_cleanup;

#[cfg(test)]
mod tests {
    use super::*;

    /// `kill_all_registered_groups` убивает ВСЕ группы реестра, в том числе чужих тестов,
    /// поэтому тесты, которые спавнят процессы, идут строго по очереди.
    static SPAWNING_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serialize_spawning_tests() -> std::sync::MutexGuard<'static, ()> {
        // Отравленный замок (другой тест упал) не мешает: защищаемых данных в нём нет.
        SPAWNING_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Номер группы процесса стенда (он же pid лидера).
    fn group_of(process: &ManagedProcess) -> i32 {
        i32::try_from(process.child.id()).expect("pid fits i32")
    }

    fn proc_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    /// Ждёт, пока `/proc/<pid>` исчезнет (зомби до подбора init-ом ещё виден).
    fn wait_proc_gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc_exists(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        !proc_exists(pid)
    }

    /// Функция, которую вызывает обработчик сигнала, убивает лидера и внуков зарегистрированной
    /// группы, ничего не зная о самом `ManagedProcess`.
    #[test]
    fn kill_all_registered_groups_kills_leader_and_grandchildren() {
        let _serial = serialize_spawning_tests();
        let mut command = Command::new("sh");
        // Внук печатает pid и живёт долго; лидер ждёт его.
        command
            .args(["-c", "sleep 30 & echo $!; sleep 30"])
            .stdout(Stdio::piped());
        let mut process = ManagedProcess::spawn(command).expect("spawn sh");
        let stdout = process.take_stdout().expect("stdout");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line)
            .expect("grandchild pid");
        let grandchild_pid: u32 = line.trim().parse().expect("pid is a number");
        let leader_pid = process.child.id();
        assert!(proc_exists(grandchild_pid) && proc_exists(leader_pid));

        registry::kill_all_registered_groups();

        // Лидер подбирается через try_wait: иначе он остался бы зомби.
        let status = process
            .wait_timeout(Duration::from_secs(5))
            .expect("wait leader");
        assert!(
            status.is_some_and(|status| !status.success()),
            "leader was killed"
        );
        assert!(
            wait_proc_gone(grandchild_pid),
            "grandchild died with the group"
        );
    }

    /// После `Drop` номер группы исчезает из реестра, пока живой процесс в нём присутствует.
    #[test]
    fn dropped_process_is_unregistered() {
        let _serial = serialize_spawning_tests();
        let mut command = Command::new("sleep");
        command.arg("30");
        let process = ManagedProcess::spawn(command).expect("spawn sleep");
        let group = group_of(&process);
        assert!(registry::registered_groups().contains(&group));
        drop(process);
        assert!(!registry::registered_groups().contains(&group));
    }

    /// Убийство группы убивает и внука, которого лидер запустил в фоне.
    #[test]
    fn terminate_kills_grandchildren_too() {
        let _serial = serialize_spawning_tests();
        let mut command = Command::new("sh");
        // Внук `sleep` пишет свой pid в stdout и живёт долго; лидер ждёт его.
        command
            .args(["-c", "sleep 300 & echo $!; wait"])
            .stdout(Stdio::piped());
        let mut process = ManagedProcess::spawn(command).expect("spawn sh");
        let stdout = process.take_stdout().expect("stdout");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line)
            .expect("grandchild pid");
        let grandchild_pid: u32 = line.trim().parse().expect("pid is a number");
        assert!(std::path::Path::new(&format!("/proc/{grandchild_pid}")).exists());

        let status = process.terminate(Duration::from_secs(2));
        assert!(status.is_some(), "the leader exited");
        // Внук мог стать зомби до подбора init-ом: ждём исчезновения каталога /proc.
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::path::Path::new(&format!("/proc/{grandchild_pid}")).exists()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{grandchild_pid}")).exists(),
            "the grandchild died with the group"
        );
    }
}
