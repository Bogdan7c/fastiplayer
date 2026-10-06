//! Fake-показ для тестов: настоящая цепочка `deliver_notice`, но каналы
//! записывают текст вместо запуска kdialog/zenity/D-Bus.

use std::cell::RefCell;
use std::rc::Rc;

use super::FatalStartupError;
use super::messages::FatalStartupNotice;
use super::presentation::{
    FatalNoticeChannel, FatalStartupPresenter, NoticeChannelFailure, NoticeDelivery, deliver_notice,
};

/// Что увидел пользователь через конкретный канал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShownNotice {
    pub(crate) channel: &'static str,
    pub(crate) notice: FatalStartupNotice,
}

/// Канал, который записывает попытку показа и отвечает заданным исходом.
pub(super) struct RecordingChannel {
    pub(super) name: &'static str,
    pub(super) outcome: Result<(), NoticeChannelFailure>,
    pub(super) attempts: Rc<RefCell<Vec<ShownNotice>>>,
}

impl FatalNoticeChannel for RecordingChannel {
    fn channel_name(&self) -> &'static str {
        self.name
    }

    fn try_show(&self, notice: &FatalStartupNotice) -> Result<(), NoticeChannelFailure> {
        self.attempts.borrow_mut().push(ShownNotice {
            channel: self.name,
            notice: notice.clone(),
        });
        self.outcome.clone()
    }
}

/// Fake presenter с одним успешным каналом «окно»: фиксирует показанные тексты,
/// stderr-копию и исход доставки.
#[derive(Default)]
pub(crate) struct RecordingPresenter {
    attempts: Rc<RefCell<Vec<ShownNotice>>>,
    text_output: RefCell<Vec<u8>>,
    deliveries: RefCell<Vec<NoticeDelivery>>,
}

impl RecordingPresenter {
    /// Тексты, которые увидел пользователь, в порядке показа.
    pub(crate) fn shown_notices(&self) -> Vec<ShownNotice> {
        self.attempts.borrow().clone()
    }

    /// Всё, что было выведено в stderr.
    pub(crate) fn text_output(&self) -> String {
        String::from_utf8_lossy(&self.text_output.borrow()).into_owned()
    }

    /// Сколько раз вызывался показ.
    pub(crate) fn presentation_count(&self) -> usize {
        self.deliveries.borrow().len()
    }
}

impl FatalStartupPresenter for RecordingPresenter {
    fn present(&self, error: &FatalStartupError) {
        let channels: Vec<Box<dyn FatalNoticeChannel>> = vec![Box::new(RecordingChannel {
            name: "fake-dialog",
            outcome: Ok(()),
            attempts: self.attempts.clone(),
        })];
        let delivery = deliver_notice(error, &channels, &mut *self.text_output.borrow_mut());
        self.deliveries.borrow_mut().push(delivery);
    }
}
