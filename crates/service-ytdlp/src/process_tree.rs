//! Ownership app-created process group и bounded shutdown её stdout/stderr pipe-ов.

use std::io::{self, Read};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;

/// Максимальное число spawn-попыток при transient Unix `ETXTBSY`.
const TEXT_FILE_BUSY_SPAWN_ATTEMPTS: usize = 8;

/// Короткая пауза между spawn-попытками без busy-loop.
const TEXT_FILE_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Grace после root exit для получения EOF от обоих pipe-reader-ов.
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Poll interval сохраняет responsive cancellation без busy-loop.
const PIPE_COMPLETION_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Worker после stop обязан подтвердить завершение за bounded cleanup budget.
const PIPE_READER_STOP_TIMEOUT: Duration = Duration::from_millis(500);

/// Ошибка spawn boundary без смешивания cooperative cancellation и OS failure.
#[derive(Debug)]
pub(crate) enum OwnedProcessSpawnError {
    /// Владелец операции отменил запуск до появления child process.
    Cancellation,

    /// OS не позволила запустить executable.
    Process(io::Error),
}

/// Наблюдаемое состояние root process без передачи ownership/reap caller-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedProcessRootState {
    Running,
    Exited,
}

/// Единственный владелец root child и созданной для него process group.
///
/// Явный `finish` нужен для передачи cleanup failure вызывающему коду. `Drop`
/// остаётся аварийной страховкой для panic/раннего возврата и никогда не оставляет
/// живой child намеренно.
pub(crate) struct OwnedProcess {
    child: Option<Child>,
    exit_status: Option<ExitStatus>,
}

struct OwnedProcessTerminationFailure {
    source: io::Error,
    reaped_status: Option<ExitStatus>,
}

impl OwnedProcessTerminationFailure {
    fn unreaped(source: io::Error) -> Self {
        Self {
            source,
            reaped_status: None,
        }
    }
}

impl OwnedProcess {
    fn new(child: Child) -> Self {
        Self {
            child: Some(child),
            exit_status: None,
        }
    }

    /// Передаёт stdout pipe reader-у, сохраняя ownership root child внутри boundary.
    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.as_mut()?.stdout.take()
    }

    /// Передаёт stderr pipe reader-у, сохраняя ownership root child внутри boundary.
    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.as_mut()?.stderr.take()
    }

    /// Наблюдает root exit без Unix reap и без освобождения PID/PGID identity.
    pub(crate) fn poll_root_exit(&mut self) -> io::Result<OwnedProcessRootState> {
        if self.exit_status.is_some() {
            return Ok(OwnedProcessRootState::Exited);
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned process уже завершён"))?;

        #[cfg(unix)]
        {
            poll_unix_root_exit_without_reap(child)
        }

        #[cfg(not(unix))]
        {
            match child.try_wait()? {
                Some(status) => {
                    self.exit_status = Some(status);
                    Ok(OwnedProcessRootState::Exited)
                }
                None => Ok(OwnedProcessRootState::Running),
            }
        }
    }

    /// Завершает оставшуюся process group и reap-ит root child.
    ///
    /// Метод вызывается и после нормального root exit: его descendants могут всё
    /// ещё владеть stdout/stderr pipe-ами и иначе заблокируют reader join.
    pub(crate) fn finish(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let Some(mut child) = self.child.take() else {
            return Err(io::Error::other(
                "owned process не содержит child или сохранённый exit status",
            ));
        };

        match terminate_owned_process_group(&mut child) {
            Ok(status) => {
                self.exit_status = Some(status);
                Ok(status)
            }
            Err(failure) => {
                if let Some(status) = failure.reaped_status {
                    self.exit_status = Some(status);
                } else {
                    self.child = Some(child);
                }
                Err(failure.source)
            }
        }
    }

    #[cfg(all(test, unix))]
    fn root_process_id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("test observes PID before finish")
            .id()
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            // Drop не может вернуть ошибку, но silent cleanup failure здесь недопустим.
            eprintln!("rustiplayer: аварийная очистка owned process завершилась ошибкой: {error}");
        }
    }
}

/// Сохраняет primary failure вместе с дополнительной ошибкой cleanup/join.
#[derive(Debug, thiserror::Error)]
#[error("process owner primary failure: {primary:#}; cleanup failure: {cleanup:#}")]
pub(crate) struct OwnedProcessCleanupFailure {
    /// Исходная typed ошибка операции.
    primary: anyhow::Error,

    /// Ошибка обязательной очистки либо присоединения pipe-reader-а.
    cleanup: anyhow::Error,
}

impl OwnedProcessCleanupFailure {
    pub(crate) fn new(primary: anyhow::Error, cleanup: anyhow::Error) -> Self {
        Self { primary, cleanup }
    }
}

/// Ошибка bounded остановки pipe-reader worker-а.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OwnedPipeStopError {
    /// Reader успел получить настоящий IO failure до/во время stop.
    #[error("pipe reader завершился IO failure")]
    Reader {
        #[source]
        source: io::Error,
    },

    /// Worker завершился без передачи результата, например из-за panic.
    #[error("pipe reader worker завершился без результата")]
    WorkerTerminated,

    /// Stop token не завершил worker в обязательный bounded budget.
    #[error("pipe reader worker не подтвердил bounded stop")]
    StopTimedOut,
}

/// Ошибка bounded drain с отдельной семантикой cancellation/operation deadline.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OwnedPipeDrainError {
    #[error("pipe drain отменён владельцем операции")]
    Cancellation,

    #[error("pipe drain cancellation не смогла bounded остановить worker")]
    CancellationCleanup {
        #[source]
        source: OwnedPipeStopError,
    },

    #[error("pipe drain исчерпал исходный operation timeout")]
    OperationTimedOut,

    #[error("pipe drain timeout не смог bounded остановить worker")]
    OperationTimeoutCleanup {
        #[source]
        source: OwnedPipeStopError,
    },

    #[error("pipe не достиг EOF в bounded drain grace после root exit")]
    DrainTimedOut,

    #[error("pipe drain grace исчерпана, а worker не подтвердил bounded stop")]
    DrainTimeoutCleanup {
        #[source]
        source: OwnedPipeStopError,
    },

    #[error("pipe reader завершился IO failure")]
    Reader {
        #[source]
        source: io::Error,
    },

    #[error("pipe reader worker завершился без результата")]
    WorkerTerminated,
}

/// Private sentinel: в отличие от `Interrupted`, std `read_to_end` не retry-ит его.
#[derive(Debug, thiserror::Error)]
#[error("pipe reader остановлен process owner-ом")]
struct PipeReaderStopped;

/// Bounded completion owner для одного stdout/stderr reader worker-а.
pub(crate) struct OwnedPipeReader<T> {
    completion: Receiver<io::Result<T>>,
    stop_requested: Arc<AtomicBool>,
}

impl<T> OwnedPipeReader<T> {
    /// Ждёт EOF/result не дольше исходного deadline и короткой drain grace.
    pub(crate) fn drain(
        mut self,
        operation_started_at: Instant,
        operation_timeout: Duration,
        drain_started_at: Instant,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<T, OwnedPipeDrainError> {
        loop {
            match self.completion.try_recv() {
                Ok(result) => return map_pipe_completion(result),
                Err(TryRecvError::Disconnected) => {
                    return Err(OwnedPipeDrainError::WorkerTerminated);
                }
                Err(TryRecvError::Empty) => {}
            }

            if is_cancelled() {
                return match self.stop_and_confirm() {
                    Ok(()) => Err(OwnedPipeDrainError::Cancellation),
                    Err(source) => Err(OwnedPipeDrainError::CancellationCleanup { source }),
                };
            }

            let operation_remaining =
                operation_timeout.saturating_sub(operation_started_at.elapsed());
            if operation_remaining.is_zero() {
                return match self.stop_and_confirm() {
                    Ok(()) => Err(OwnedPipeDrainError::OperationTimedOut),
                    Err(source) => Err(OwnedPipeDrainError::OperationTimeoutCleanup { source }),
                };
            }

            let drain_remaining = PIPE_DRAIN_GRACE.saturating_sub(drain_started_at.elapsed());
            if drain_remaining.is_zero() {
                return match self.stop_and_confirm() {
                    Ok(()) => Err(OwnedPipeDrainError::DrainTimedOut),
                    Err(source) => Err(OwnedPipeDrainError::DrainTimeoutCleanup { source }),
                };
            }

            let wait_interval = PIPE_COMPLETION_POLL_INTERVAL
                .min(operation_remaining)
                .min(drain_remaining);
            match self.completion.recv_timeout(wait_interval) {
                Ok(result) => return map_pipe_completion(result),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(OwnedPipeDrainError::WorkerTerminated);
                }
            }
        }
    }

    /// Немедленно просит worker прекратить чтение и bounded подтверждает завершение.
    pub(crate) fn abort(mut self) -> Result<(), OwnedPipeStopError> {
        self.stop_and_confirm()
    }

    fn stop_and_confirm(&mut self) -> Result<(), OwnedPipeStopError> {
        self.stop_requested.store(true, Ordering::Release);
        match self.completion.recv_timeout(PIPE_READER_STOP_TIMEOUT) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) if is_pipe_reader_stopped(&error) => Ok(()),
            Ok(Err(source)) => Err(OwnedPipeStopError::Reader { source }),
            Err(RecvTimeoutError::Disconnected) => Err(OwnedPipeStopError::WorkerTerminated),
            Err(RecvTimeoutError::Timeout) => Err(OwnedPipeStopError::StopTimedOut),
        }
    }
}

impl<T> Drop for OwnedPipeReader<T> {
    fn drop(&mut self) {
        // Явные production paths вызывают `drain`/`abort` и получают cleanup error.
        // Drop остаётся panic/early-return страховкой: он не может вернуть ошибку,
        // но обязан разбудить Unix non-blocking worker и не оставить вечное чтение.
        self.stop_requested.store(true, Ordering::Release);
    }
}

fn map_pipe_completion<T>(result: io::Result<T>) -> Result<T, OwnedPipeDrainError> {
    result.map_err(|source| OwnedPipeDrainError::Reader { source })
}

fn pipe_reader_stopped_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, PipeReaderStopped)
}

fn is_pipe_reader_stopped(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<PipeReaderStopped>())
        .is_some()
}

/// Pipe handle, который можно перевести в cooperative non-blocking read mode.
pub(crate) trait OwnedPipe: Read + Send + 'static {
    fn configure_bounded_read(&self) -> io::Result<()>;
}

#[cfg(unix)]
impl OwnedPipe for ChildStdout {
    fn configure_bounded_read(&self) -> io::Result<()> {
        configure_unix_nonblocking_pipe(self)
    }
}

#[cfg(unix)]
impl OwnedPipe for ChildStderr {
    fn configure_bounded_read(&self) -> io::Result<()> {
        configure_unix_nonblocking_pipe(self)
    }
}

#[cfg(not(unix))]
impl OwnedPipe for ChildStdout {
    fn configure_bounded_read(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(unix))]
impl OwnedPipe for ChildStderr {
    fn configure_bounded_read(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Запускает stop-aware pipe reader и возвращает только bounded completion owner.
pub(crate) fn spawn_owned_pipe_reader<R, T, F>(
    thread_name: &'static str,
    pipe: R,
    read_pipe: F,
) -> io::Result<OwnedPipeReader<T>>
where
    R: OwnedPipe,
    T: Send + 'static,
    F: FnOnce(&mut dyn Read) -> io::Result<T> + Send + 'static,
{
    pipe.configure_bounded_read()?;
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop_requested = Arc::clone(&stop_requested);
    let (completion_sender, completion) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let mut reader = StopAwarePipe {
                pipe,
                stop_requested: worker_stop_requested,
            };
            let result = read_pipe(&mut reader);
            // Completion подтверждает не только выход closure, но и закрытие reader FD.
            drop(reader);
            let _ = completion_sender.send(result);
        })?;
    drop(worker);

    Ok(OwnedPipeReader {
        completion,
        stop_requested,
    })
}

struct StopAwarePipe<R> {
    pipe: R,
    stop_requested: Arc<AtomicBool>,
}

impl<R> Read for StopAwarePipe<R>
where
    R: Read,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop_requested.load(Ordering::Acquire) {
                return Err(pipe_reader_stopped_error());
            }

            match self.pipe.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(PIPE_COMPLETION_POLL_INTERVAL);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

/// Запускает child в собственной process group и ограниченно повторяет Unix `ETXTBSY`.
///
/// Один `Command` безопасно переиспользуется между попытками. Retry не выходит за
/// общий timeout операции: caller передаёт исходный `operation_started_at`, а
/// оставшееся время после успешного spawn использует для ожидания child process.
pub(crate) fn spawn_owned_process(
    command: &mut Command,
    operation_started_at: Instant,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<OwnedProcess, OwnedProcessSpawnError> {
    configure_owned_process_group(command);
    let mut last_text_file_busy = None;

    for attempt_index in 0..TEXT_FILE_BUSY_SPAWN_ATTEMPTS {
        if is_cancelled() {
            return Err(OwnedProcessSpawnError::Cancellation);
        }
        if attempt_index > 0 && operation_started_at.elapsed() >= timeout {
            return Err(OwnedProcessSpawnError::Process(
                last_text_file_busy
                    .take()
                    .expect("retry attempt always follows ETXTBSY"),
            ));
        }

        match command.spawn() {
            Ok(child) => return Ok(OwnedProcess::new(child)),
            Err(error)
                if is_text_file_busy(&error)
                    && attempt_index + 1 < TEXT_FILE_BUSY_SPAWN_ATTEMPTS =>
            {
                let remaining_timeout = timeout.saturating_sub(operation_started_at.elapsed());
                if remaining_timeout.is_zero() {
                    return Err(OwnedProcessSpawnError::Process(error));
                }
                last_text_file_busy = Some(error);
                thread::sleep(TEXT_FILE_BUSY_RETRY_INTERVAL.min(remaining_timeout));
            }
            Err(error) => return Err(OwnedProcessSpawnError::Process(error)),
        }
    }

    unreachable!("spawn retry loop always returns on its final attempt")
}

/// Изолирует новый child в process group, которой владеет текущий запуск.
fn configure_owned_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }
}

#[cfg(unix)]
fn configure_unix_nonblocking_pipe(pipe: &impl AsRawFd) -> io::Result<()> {
    let file_descriptor = pipe.as_raw_fd();
    // SAFETY: fd принадлежит живому ChildStdout/ChildStderr; F_GETFL не меняет state.
    let current_flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFL) };
    if current_flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: тот же живой fd; сохраняем все flags и добавляем только O_NONBLOCK.
    let set_result = unsafe {
        libc::fcntl(
            file_descriptor,
            libc::F_SETFL,
            current_flags | libc::O_NONBLOCK,
        )
    };
    if set_result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(unix)]
fn poll_unix_root_exit_without_reap(child: &Child) -> io::Result<OwnedProcessRootState> {
    let process_id = libc::id_t::try_from(child.id()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "child PID не помещается в waitid identity",
        )
    })?;

    loop {
        // Нулевой siginfo позволяет отличить WNOHANG без события по `si_pid == 0`.
        // SAFETY: zeroed siginfo_t допустим как output buffer для waitid.
        let mut process_info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        // SAFETY: root остаётся нашим waitable child; WNOWAIT намеренно не reap-ит его.
        let wait_result = unsafe {
            libc::waitid(
                libc::P_PID,
                process_id,
                &mut process_info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if wait_result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }

        // SAFETY: waitid успешно инициализировал siginfo_t для SIGCHLD layout.
        let observed_process_id = unsafe { process_info.si_pid() };
        if observed_process_id == 0 {
            return Ok(OwnedProcessRootState::Running);
        }
        if observed_process_id == child.id() as libc::pid_t {
            return Ok(OwnedProcessRootState::Exited);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "waitid вернул событие другого root process",
        ));
    }
}

#[cfg(unix)]
fn is_text_file_busy(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ETXTBSY)
}

#[cfg(not(unix))]
fn is_text_file_busy(_error: &io::Error) -> bool {
    false
}

/// Завершает всё принадлежащее запуску дерево и обязательно reap-ит root child.
fn terminate_owned_process_group(
    child: &mut Child,
) -> Result<ExitStatus, OwnedProcessTerminationFailure> {
    #[cfg(unix)]
    {
        terminate_unix_process_group(child)
    }

    #[cfg(not(unix))]
    {
        terminate_single_process(child)
    }
}

#[cfg(unix)]
fn terminate_unix_process_group(
    child: &mut Child,
) -> Result<ExitStatus, OwnedProcessTerminationFailure> {
    let process_group_id = libc::pid_t::try_from(child.id()).map_err(|_| {
        OwnedProcessTerminationFailure::unreaped(io::Error::new(
            io::ErrorKind::InvalidInput,
            "child PID не помещается в Unix process-group identity",
        ))
    })?;

    // SAFETY: `spawn_owned_process` всегда конфигурирует отдельную process group
    // перед spawn, поэтому PID root child является PGID именно этого запуска.
    let signal_result = unsafe { libc::kill(-process_group_id, libc::SIGKILL) };
    if signal_result == 0 {
        return child
            .wait()
            .map_err(OwnedProcessTerminationFailure::unreaped);
    }

    let signal_error = io::Error::last_os_error();
    if signal_error.raw_os_error() == Some(libc::ESRCH) {
        return child
            .wait()
            .map_err(OwnedProcessTerminationFailure::unreaped);
    }

    // Даже при неожиданной ошибке group signal root child не должен остаться
    // работающим или zombie; исходную ошибку сохраняем как более сильный сигнал.
    match terminate_single_process(child) {
        Ok(reaped_status) => Err(OwnedProcessTerminationFailure {
            source: signal_error,
            reaped_status: Some(reaped_status),
        }),
        Err(root_cleanup_error) => Err(OwnedProcessTerminationFailure::unreaped(io::Error::other(
            OwnedProcessCleanupFailure::new(signal_error.into(), root_cleanup_error.into()),
        ))),
    }
}

fn terminate_single_process(child: &mut Child) -> io::Result<ExitStatus> {
    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(error),
    }

    child.wait()
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Read;
    use std::process::Stdio;

    use super::*;

    /// Unix poll оставляет root waitable до group cleanup и сохраняет настоящий status.
    #[test]
    fn unix_root_exit_poll_does_not_reap_before_group_cleanup() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "exit 7"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut process = spawn_owned_process(
            &mut command,
            Instant::now(),
            Duration::from_secs(2),
            &|| false,
        )
        .expect("spawn WNOWAIT fixture");
        let root_process_id = process.root_process_id();
        let observation_started_at = Instant::now();
        loop {
            if process.poll_root_exit().expect("WNOWAIT poll must succeed")
                == OwnedProcessRootState::Exited
            {
                break;
            }
            assert!(
                observation_started_at.elapsed() < Duration::from_secs(1),
                "root fixture must exit promptly"
            );
            thread::sleep(Duration::from_millis(1));
        }

        // Повторный raw WNOWAIT доказывает, что OwnedProcess ещё не reap-нул root.
        // SAFETY: zeroed siginfo_t является output buffer для waitid.
        let mut process_info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        // SAFETY: root PID всё ещё принадлежит этому process и остаётся waitable.
        let wait_result = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::try_from(root_process_id).expect("test PID fits id_t"),
                &mut process_info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(wait_result, 0);
        // SAFETY: successful waitid инициализировал SIGCHLD payload.
        assert_eq!(
            unsafe { process_info.si_pid() },
            root_process_id as libc::pid_t
        );

        let exit_status = process
            .finish()
            .expect("group cleanup must reap root exactly once");
        assert_eq!(exit_status.code(), Some(7));
    }

    /// Abort получает completion только после выхода worker и закрытия reader FD.
    #[test]
    fn pipe_reader_abort_confirms_worker_and_file_descriptor_teardown() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = spawn_owned_process(
            &mut command,
            Instant::now(),
            Duration::from_secs(2),
            &|| false,
        )
        .expect("spawn pipe-abort fixture");
        let stdout = process.take_stdout().expect("stdout pipe configured");
        let reader = spawn_owned_pipe_reader("pipe-abort-test", stdout, |pipe| {
            let mut captured_bytes = Vec::new();
            pipe.read_to_end(&mut captured_bytes)?;
            Ok(captured_bytes)
        })
        .expect("spawn stop-aware reader");

        let abort_started_at = Instant::now();
        reader
            .abort()
            .expect("owner stop sentinel must complete worker without retry loop");
        assert!(
            abort_started_at.elapsed() < Duration::from_secs(1),
            "reader abort must not consume stop timeout"
        );
        process.finish().expect("cleanup pipe-abort fixture");
    }

    /// Setup abort через Drop завершает root+descendant и закрывает унаследованный pipe.
    #[test]
    fn setup_abort_drop_kills_descendant_and_unblocks_pipe_reader() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & printf ready; wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = spawn_owned_process(
            &mut command,
            Instant::now(),
            Duration::from_secs(2),
            &|| false,
        )
        .expect("spawn owned setup-abort fixture");
        let mut stdout = process.take_stdout().expect("stdout pipe configured");
        let mut ready = [0_u8; 5];
        stdout
            .read_exact(&mut ready)
            .expect("fixture confirms descendant creation");
        assert_eq!(&ready, b"ready");
        assert!(
            process.take_stderr().is_none(),
            "missing stderr simulates setup abort"
        );
        let pipe_reader = thread::spawn(move || {
            let mut remaining_stdout = Vec::new();
            stdout.read_to_end(&mut remaining_stdout)?;
            Ok::<_, io::Error>(remaining_stdout)
        });

        let cleanup_started_at = Instant::now();
        drop(process);
        let remaining_stdout = pipe_reader
            .join()
            .expect("pipe reader thread must not panic")
            .expect("pipe reader reaches EOF after owner drop");

        assert!(remaining_stdout.is_empty());
        assert!(
            cleanup_started_at.elapsed() < Duration::from_secs(2),
            "owner drop must not wait for the descendant sleep"
        );
    }
}
