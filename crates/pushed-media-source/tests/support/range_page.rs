//! Фейковая «страница» для файла по запросам байтов.
//!
//! Поток, который забирает запросы источника (`take_requests`) и отвечает байтами
//! файла рамками разного размера, как экстеншен с `fetch_range`. Умеет то, что
//! нужно тестам: рамки по кругу разных размеров (в т. ч. по 1 байту), «неизвестный
//! размер», сетевые сбои, уход вкладки посреди файла, остановку посреди запроса
//! (чтобы перемотка успела его отменить) и враждебные рамки.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pushed_media_source::{
    PushedFileError, PushedFileOpener, PushedFilePolicy, PushedFileRequests, PushedFileWriter,
    RangeBytesVerdict, RangeFailure, RangeOutcome, RangeRequest, RangeRequestId, RangeRequestIds,
    RangeRequestNotifier, ResourceSizeReport, pushed_file,
};

/// Как часто страница проверяет очередь, если её не разбудили.
const POLL: Duration = Duration::from_millis(5);

/// Что страница сообщает о размере файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    /// Настоящий размер (заголовок `Content-Range`).
    Known,
    /// Размер неизвестен (ответ без длины).
    Unknown,
}

/// Враждебная выходка на ближайшем запросе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFault {
    /// Рамки под чужим (невыданным) номером.
    ForeignId,
    /// Смещение рамки не там, где кончились прежние байты.
    WrongOffset,
    /// Лишние байты за концом запроса.
    ExtraBytes,
    /// Недобор: запрос завершён раньше времени, хотя файл продолжается.
    ShortWithoutEnd,
    /// Размер файла в итоге другой.
    ChangedSize,
    /// Остановиться после первой рамки и ждать `release_stalled` (запрос успеют отменить).
    StallMidway,
}

/// Сценарий страницы.
#[derive(Debug, Clone)]
pub struct PageScript {
    /// Размеры рамок по кругу.
    pub frame_sizes: Vec<usize>,
    /// Сообщаемый размер файла.
    pub size: SizeMode,
    /// Сколько первых запросов закончить сетевым сбоем после первой рамки.
    pub fail_first_requests: usize,
    /// Каким сбоем.
    pub failure: RangeFailure,
    /// Сбойные запросы не успевают отдать ни одного байта (без «прогресса»).
    pub fail_without_bytes: bool,
}

impl Default for PageScript {
    fn default() -> Self {
        Self {
            frame_sizes: vec![4093, 1, 777, 65_536, 511],
            size: SizeMode::Known,
            fail_first_requests: 0,
            failure: RangeFailure::Network,
            fail_without_bytes: false,
        }
    }
}

/// Будильник страницы: будит поток и считает запросы.
struct WakeNotifier {
    sender: Mutex<Sender<()>>,
    calls: AtomicUsize,
}

impl RangeRequestNotifier for WakeNotifier {
    fn range_requested(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.sender.lock().expect("sender").send(());
    }
}

/// Общее состояние страницы и теста.
struct PageShared {
    file: Vec<u8>,
    script: PageScript,
    served: Mutex<Vec<RangeRequest>>,
    fault: Mutex<Option<PageFault>>,
    first_error: Mutex<Option<PushedFileError>>,
    stale_verdicts: AtomicUsize,
    /// Смещение файла, дойдя до которого страница уходит (вкладка закрыта).
    leave_at: AtomicU64,
    stalled: AtomicBool,
    release: AtomicBool,
    shutdown: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("page state")
}

/// Страница: поток-ответчик.
pub struct FakePage {
    shared: Arc<PageShared>,
    notifier: Arc<WakeNotifier>,
    thread: Option<JoinHandle<()>>,
}

impl FakePage {
    /// Создаёт файл и запускает страницу; открывальщик отдаётся тесту.
    pub fn start(
        file: Vec<u8>,
        script: PageScript,
        policy: PushedFilePolicy,
    ) -> (Self, PushedFileOpener) {
        Self::start_with_ids(file, script, policy, RangeRequestIds::new())
    }

    /// Как [`Self::start`], с общим на сеанс выдавателем номеров.
    pub fn start_with_ids(
        file: Vec<u8>,
        script: PageScript,
        policy: PushedFilePolicy,
        ids: RangeRequestIds,
    ) -> (Self, PushedFileOpener) {
        let (sender, receiver) = mpsc::channel();
        let notifier = Arc::new(WakeNotifier {
            sender: Mutex::new(sender),
            calls: AtomicUsize::new(0),
        });
        let (writer, opener, requests) = pushed_file(
            policy,
            ids,
            Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
        );
        let shared = Arc::new(PageShared {
            file,
            script,
            served: Mutex::new(Vec::new()),
            fault: Mutex::new(None),
            first_error: Mutex::new(None),
            stale_verdicts: AtomicUsize::new(0),
            leave_at: AtomicU64::new(u64::MAX),
            stalled: AtomicBool::new(false),
            release: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });
        let worker = PageWorker {
            shared: Arc::clone(&shared),
            writer,
            requests,
            frame_cursor: 0,
            failures_left: shared.script.fail_first_requests,
            gone: false,
        };
        let thread = thread::spawn(move || worker.run(&receiver));
        (
            Self {
                shared,
                notifier,
                thread: Some(thread),
            },
            opener,
        )
    }

    /// Запросы, на которые страница уже начала отвечать, по порядку.
    pub fn served(&self) -> Vec<RangeRequest> {
        lock(&self.shared.served).clone()
    }

    /// Сколько запросов источник выдал за всё время (число будильников).
    pub fn issued_count(&self) -> usize {
        self.notifier.calls.load(Ordering::SeqCst)
    }

    /// Первая ошибка, на которую наткнулась страница (нарушение или отказ источника).
    pub fn failure(&self) -> Option<PushedFileError> {
        lock(&self.shared.first_error).clone()
    }

    /// Сколько рамок источник отбросил как запоздавшие.
    pub fn stale_verdicts(&self) -> usize {
        self.shared.stale_verdicts.load(Ordering::SeqCst)
    }

    /// Страница уйдёт (seal), как только отдаст байты файла до этого смещения.
    pub fn leave_at(&self, offset: u64) {
        self.shared.leave_at.store(offset, Ordering::SeqCst);
    }

    /// Следующий запрос получит выходку.
    pub fn arm(&self, fault: PageFault) {
        *lock(&self.shared.fault) = Some(fault);
    }

    /// Страница стоит посреди запроса (после `StallMidway`).
    pub fn is_stalled(&self) -> bool {
        self.shared.stalled.load(Ordering::SeqCst)
    }

    /// Отпустить остановленный запрос: страница досылает остаток (уже запоздавший).
    pub fn release_stalled(&self) {
        self.shared.release.store(true, Ordering::SeqCst);
    }
}

impl Drop for FakePage {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.release.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Поток-ответчик.
struct PageWorker {
    shared: Arc<PageShared>,
    writer: PushedFileWriter,
    requests: PushedFileRequests,
    frame_cursor: usize,
    failures_left: usize,
    gone: bool,
}

impl PageWorker {
    fn run(mut self, wake: &mpsc::Receiver<()>) {
        while !self.shared.shutdown.load(Ordering::SeqCst) && !self.gone {
            match wake.recv_timeout(POLL) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            for request in self.requests.take_requests() {
                lock(&self.shared.served).push(request);
                self.serve(request);
                if self.gone || self.shared.shutdown.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    }

    fn next_frame_size(&mut self) -> usize {
        let sizes = &self.shared.script.frame_sizes;
        let size = sizes[self.frame_cursor % sizes.len()];
        self.frame_cursor += 1;
        size
    }

    fn record_error(&self, error: PushedFileError) {
        lock(&self.shared.first_error).get_or_insert(error);
    }

    /// Отправляет рамку; `false` — страница мертва (источник отверг).
    fn send(&mut self, id: RangeRequestId, offset: u64, payload: &[u8]) -> bool {
        match self.writer.accept_range_bytes(id, offset, payload) {
            Ok(RangeBytesVerdict::Accepted) => true,
            Ok(RangeBytesVerdict::StaleDropped) => {
                self.shared.stale_verdicts.fetch_add(1, Ordering::SeqCst);
                true
            }
            Err(error) => {
                self.record_error(error);
                false
            }
        }
    }

    fn serve(&mut self, request: RangeRequest) {
        let shared = Arc::clone(&self.shared);
        let fault = lock(&shared.fault).take();
        let file_len = shared.file.len() as u64;
        let start = request.offset.min(file_len);
        let mut end = (request.offset + u64::from(request.length.get())).min(file_len);
        if fault == Some(PageFault::ShortWithoutEnd) && end - start > 2 {
            end = start + (end - start) / 2;
        }
        let wire_id = if fault == Some(PageFault::ForeignId) {
            RangeRequestId::new(request.id.get() + 1000)
        } else {
            request.id
        };
        let fails_this_request = self.failures_left > 0;
        if fails_this_request {
            self.failures_left -= 1;
        }

        let mut position = start;
        let mut frames_sent = 0;
        if fails_this_request && (shared.script.fail_without_bytes || end - start < 2) {
            self.finish(request.id, RangeOutcome::Failed(shared.script.failure));
            return;
        }
        while position < end {
            let mut size = self.next_frame_size() as u64;
            let limit = shared.leave_at.load(Ordering::SeqCst);
            if limit != u64::MAX {
                size = size.min(limit.saturating_sub(position).max(1));
            }
            if fails_this_request && frames_sent == 0 {
                // Сбой приходит посреди запроса, а не после последнего байта.
                size = size.min(((end - start) / 2).max(1));
            }
            let frame_end = (position + size).min(end);
            let offset = if fault == Some(PageFault::WrongOffset) {
                position + 3
            } else {
                position
            };
            let payload = &shared.file[position as usize..frame_end as usize];
            if !self.send(wire_id, offset, payload) {
                self.gone = true;
                return;
            }
            position = frame_end;
            frames_sent += 1;
            if position >= limit {
                // Вкладка ушла посреди файла: запрос не завершён.
                self.writer.seal();
                self.gone = true;
                return;
            }
            if fails_this_request && frames_sent == 1 {
                self.finish(request.id, RangeOutcome::Failed(shared.script.failure));
                return;
            }
            if fault == Some(PageFault::StallMidway) && frames_sent == 1 {
                shared.stalled.store(true, Ordering::SeqCst);
                while !shared.release.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(2));
                }
                shared.stalled.store(false, Ordering::SeqCst);
                shared.release.store(false, Ordering::SeqCst);
            }
        }
        if fails_this_request {
            self.finish(request.id, RangeOutcome::Failed(shared.script.failure));
            return;
        }
        if fault == Some(PageFault::ExtraBytes) && !self.send(wire_id, end, &[0xEE; 16]) {
            self.gone = true;
            return;
        }
        let resource_size = match (shared.script.size, fault) {
            (SizeMode::Unknown, _) => ResourceSizeReport::Unknown,
            (SizeMode::Known, Some(PageFault::ChangedSize)) => {
                ResourceSizeReport::Known(file_len + 1)
            }
            (SizeMode::Known, _) => ResourceSizeReport::Known(file_len),
        };
        self.finish(wire_id, RangeOutcome::Completed { resource_size });
    }

    fn finish(&mut self, id: RangeRequestId, outcome: RangeOutcome) {
        if let Err(error) = self.writer.finish_request(id, outcome) {
            self.record_error(error);
            self.gone = true;
        }
    }
}
