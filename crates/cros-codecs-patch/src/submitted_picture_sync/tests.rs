//! Тесты политики ожидания на фейковом ускорителе.
//!
//! Ускоритель — FIFO, как кольцо GPU: ожидание картинки k завершает и все картинки,
//! отправленные раньше неё. Проверяем результат — сколько картинок остаётся в
//! ускорителе после каждой отправки, — а не сам факт вызова ожидания.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use super::AwaitablePicture;
use super::SubmittedPictureSync;
use crate::BlockingMode;

/// Состояние фейкового ускорителя, общее для всех картинок.
#[derive(Default)]
struct FakeAccelerator {
    /// Номера отправленных, но ещё не законченных картинок.
    in_flight: BTreeSet<u32>,
    /// Номера картинок в порядке, в котором их ждали.
    waited: Vec<u32>,
    /// Картинка, ожидание которой заканчивается ошибкой backend-а.
    failing_wait: Option<u32>,
}

/// Отправленная картинка фейкового ускорителя.
#[derive(Clone)]
struct FakePicture {
    number: u32,
    accelerator: Rc<RefCell<FakeAccelerator>>,
}

/// Ошибка ожидания фейкового backend-а: номер картинки.
#[derive(Debug, PartialEq, Eq)]
struct FakeWaitError(u32);

impl AwaitablePicture for FakePicture {
    type WaitError = FakeWaitError;

    fn wait_until_decoded(&self) -> Result<(), FakeWaitError> {
        let mut accelerator = self.accelerator.borrow_mut();
        accelerator.waited.push(self.number);
        if accelerator.failing_wait == Some(self.number) {
            return Err(FakeWaitError(self.number));
        }
        // FIFO: к концу картинки ускоритель закончил и всё, что стояло перед ней.
        accelerator.in_flight.retain(|&number| number > self.number);
        Ok(())
    }
}

/// Декодер в миниатюре: отправляет картинку в ускоритель и применяет политику.
struct FakeDecoder {
    accelerator: Rc<RefCell<FakeAccelerator>>,
    policy: SubmittedPictureSync<FakePicture>,
    mode: BlockingMode,
    next_number: u32,
}

impl FakeDecoder {
    fn new(mode: BlockingMode) -> Self {
        Self {
            accelerator: Rc::default(),
            policy: SubmittedPictureSync::default(),
            mode,
            next_number: 0,
        }
    }

    /// Отправляет следующую картинку и возвращает итог политики.
    fn submit_next(&mut self) -> Result<(), FakeWaitError> {
        let picture =
            FakePicture { number: self.next_number, accelerator: self.accelerator.clone() };
        self.next_number += 1;
        self.accelerator.borrow_mut().in_flight.insert(picture.number);
        self.policy.after_picture_submitted(self.mode, &picture)
    }

    fn pictures_in_flight(&self) -> usize {
        self.accelerator.borrow().in_flight.len()
    }

    fn waited(&self) -> Vec<u32> {
        self.accelerator.borrow().waited.clone()
    }
}

/// Сколько картинок подряд отправляет декодер в тестах: больше, чем пачка B-кадров
/// после паузы на T410 (до 7), чтобы рост без границы был виден.
const BURST_PICTURES: u32 = 12;

#[test]
fn sync_previous_picture_keeps_at_most_two_pictures_in_the_accelerator() {
    let mut decoder = FakeDecoder::new(BlockingMode::SyncPreviousPicture);

    for _ in 0..BURST_PICTURES {
        decoder.submit_next().expect("fake waits succeed");
        assert!(
            decoder.pictures_in_flight() <= 1,
            "after waiting for k−1 only the just-submitted picture k may stay in flight, got {}",
            decoder.pictures_in_flight()
        );
    }
    // Ждём ровно прошлую картинку на каждой отправке, кроме первой.
    assert_eq!(decoder.waited(), (0..BURST_PICTURES - 1).collect::<Vec<_>>());
}

#[test]
fn non_blocking_mode_leaves_the_whole_burst_in_the_accelerator() {
    // Это поведение, из-за которого показ кадра стоял в очереди GPU за пачкой декода.
    let mut decoder = FakeDecoder::new(BlockingMode::NonBlocking);

    for _ in 0..BURST_PICTURES {
        decoder.submit_next().expect("non-blocking never waits");
    }

    assert_eq!(decoder.pictures_in_flight(), BURST_PICTURES as usize);
    assert!(decoder.waited().is_empty());
}

#[test]
fn blocking_mode_waits_for_the_submitted_picture_itself() {
    let mut decoder = FakeDecoder::new(BlockingMode::Blocking);

    for _ in 0..3 {
        decoder.submit_next().expect("fake waits succeed");
        assert_eq!(decoder.pictures_in_flight(), 0);
    }
    assert_eq!(decoder.waited(), vec![0, 1, 2]);
}

#[test]
fn first_submission_waits_for_nothing() {
    let mut decoder = FakeDecoder::new(BlockingMode::SyncPreviousPicture);

    decoder.submit_next().expect("nothing to wait for");

    assert!(decoder.waited().is_empty());
    assert_eq!(decoder.pictures_in_flight(), 1);
}

#[test]
fn completing_the_awaiting_picture_empties_the_accelerator_and_forgets_it() {
    let mut decoder = FakeDecoder::new(BlockingMode::SyncPreviousPicture);
    decoder.submit_next().expect("first submit");
    decoder.submit_next().expect("second submit");

    decoder.policy.complete_awaiting_picture().expect("fake waits succeed");
    assert_eq!(decoder.pictures_in_flight(), 0);
    assert_eq!(decoder.waited(), vec![0, 1]);

    // Повторная граница ничего не ждёт: картинка уже отпущена.
    decoder.policy.complete_awaiting_picture().expect("nothing to wait for");
    assert_eq!(decoder.waited(), vec![0, 1]);

    // Первая отправка после границы снова ничего не ждёт.
    decoder.submit_next().expect("submit after flush");
    assert_eq!(decoder.waited(), vec![0, 1]);
}

#[test]
fn complete_awaiting_picture_is_a_no_op_outside_sync_previous_picture() {
    let mut decoder = FakeDecoder::new(BlockingMode::NonBlocking);
    decoder.submit_next().expect("non-blocking submit");

    decoder.policy.complete_awaiting_picture().expect("nothing remembered");

    assert!(decoder.waited().is_empty());
    assert_eq!(decoder.pictures_in_flight(), 1);
}

#[test]
fn wait_error_is_returned_and_the_next_submission_waits_for_the_newer_picture() {
    let mut decoder = FakeDecoder::new(BlockingMode::SyncPreviousPicture);
    decoder.accelerator.borrow_mut().failing_wait = Some(0);
    decoder.submit_next().expect("first submit waits for nothing");

    // Ошибка ожидания картинки 0 не глотается.
    assert_eq!(decoder.submit_next(), Err(FakeWaitError(0)));

    // Картинка 0 отпущена, следующая отправка ждёт картинку 1, а не 0 повторно.
    decoder.submit_next().expect("waiting for picture 1 succeeds");
    assert_eq!(decoder.waited(), vec![0, 1]);
    assert_eq!(decoder.pictures_in_flight(), 1);
}
