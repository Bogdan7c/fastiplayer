//! Чего ждёт поток, когда отвечает плееру «данных пока нет» (решение владельца 150).
//!
//! Плееру нужен честный ответ ([`media_core::DemuxDataWait`]): ждём ли мы байтов
//! живой страницы — тогда перемотка ждёт сколько угодно (спиннер, как в самом
//! браузере, где YouTube 4K иногда отдаёт новое место через 12 с), — или ждать
//! нечего, и срок перемотки плеера должен стеречь сбой как обычно.
//!
//! **Правило.** Поток отвечает [`DemuxDataWait::LiveProducer`], только если ВСЁ верно:
//! - поток жив (первой ошибки нет);
//! - писатель жив (страница не ушла, канал не оборван — [`ProducerLink::Connected`]);
//! - скачивание не оборвано (`end_download_cut_short` не звали: после него конец
//!   данных идёт путём досмотра, ждать нечего);
//! - вход хоть одной полосы открыт (страница не сказала `endOfStream`, или вход
//!   открыт заново перемоткой туда, где байтов ещё нет);
//! - мы сами не держим страницу: состояние буфера не «полон». «Полон» значит, что
//!   страница по нашей просьбе ждёт; если при этом плееру нечего читать — это наша
//!   взаимная блокировка, а не честное ожидание, и срок плеера должен сработать.
//!
//! Вместе с ответом поток отдаёт счётчик принесённого для ожидаемых данных — что в
//! нём считается, описано в `stream_buffer::delivery_progress` (решение 151: без
//! роста счётчика плеер ждёт не дольше предела молчания).
//!
//! Иначе — [`DemuxDataWait::Unattributed`]. Какой полосе не хватает данных, поток не
//! уточняет: плеер учитывает ответ, только когда его собственное чтение только что
//! вернуло «данных нет», а сборка «видео + звук» отвечает так, лишь пока не хватает
//! данных хотя бы одной полосе. Если данные есть, а застряли у плеера, чтения с
//! «данных нет» не будет, и защита плеера сработает.

use media_core::{DemuxDataWait, LiveProducerProgress};

use crate::buffer_window::BufferIntake;
use crate::download_cut::DownloadCut;
use crate::stream_buffer::StreamBuffer;
use crate::stream_state::{ProducerLink, StreamState};

/// Ответ потока «чего жду» (правило — в описании модуля).
pub(crate) fn stream_data_wait(buffer: &StreamBuffer, state: &StreamState) -> DemuxDataWait {
    let producer_can_deliver = state.failure().is_none()
        && state.producer_link() == ProducerLink::Connected
        && state.download_cut() == DownloadCut::NotCut
        && buffer.any_lane_input_open();
    // «Полон» спрашиваем последним: он считает запас по полкам (дороже остального).
    if producer_can_deliver && buffer.buffer_status().intake == BufferIntake::Accepting {
        DemuxDataWait::LiveProducer {
            progress: LiveProducerProgress::from_delivered_bytes(buffer.awaited_delivery_bytes()),
        }
    } else {
        DemuxDataWait::Unattributed
    }
}

#[cfg(test)]
#[path = "producer_wait_tests.rs"]
mod tests;
