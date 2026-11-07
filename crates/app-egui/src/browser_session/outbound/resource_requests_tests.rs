//! `fetch_resource`: версия протокола, надёжность, предел неотправленных, учёт.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::rc::Rc;
use std::time::Instant;

use browser_bridge_protocol::limits::{MAX_CONCURRENT_RESOURCE_FETCHES, MAX_JSON_SAFE_INTEGER};
use browser_bridge_protocol::{
    ByteOffset, PlayerSessionMessage, ProtocolVersion, RangeRequestId as PageRangeRequestId,
    ResourceByteRange as PageResourceByteRange, ResourceFetchLength, ResourceHandle,
};
use browser_bridge_session::SendToPageError;
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestId, ResourceByteRange, ResourceExtent, ResourceRequest,
};

use super::{FETCH_RESOURCE_SINCE, ResourceQueueOutcome, ResourceRequestPermission};
use crate::browser_session::outbound::{PageOutbound, PageSender};

/// Страница-подделка: запоминает поставленное, может отказать заранее заданным отказом.
#[derive(Clone, Default)]
struct FakePage {
    sent: Rc<RefCell<Vec<PlayerSessionMessage>>>,
    refusals: Rc<RefCell<VecDeque<SendToPageError>>>,
}

impl PageSender for FakePage {
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        if let Some(reason) = self.refusals.borrow_mut().pop_front() {
            return Err(reason);
        }
        self.sent.borrow_mut().push(message.clone());
        Ok(())
    }
}

fn outbound_speaking(version: ProtocolVersion) -> (PageOutbound, FakePage) {
    let page = FakePage::default();
    let mut outbound = PageOutbound::new(Box::new(page.clone()));
    outbound.note_agreed_protocol(version);
    (outbound, page)
}

fn whole(id: u32, handle: u32, max_bytes: u64) -> ResourceRequest {
    ResourceRequest {
        id: RangeRequestId::new(id),
        handle: ExternalResourceHandle::new(handle),
        extent: ResourceExtent::Whole,
        max_bytes: NonZeroU64::new(max_bytes).expect("non-zero"),
    }
}

fn slice(id: u32, offset: u64, length: u64, max_bytes: u64) -> ResourceRequest {
    ResourceRequest {
        extent: ResourceExtent::Range(
            ResourceByteRange::new(offset, NonZeroU64::new(length).expect("non-zero"))
                .expect("range"),
        ),
        ..whole(id, 4, max_bytes)
    }
}

/// Порог версии — тот же, что объявляет сообщение протокола.
#[test]
fn the_version_threshold_is_the_one_of_the_protocol_message() {
    let message = PlayerSessionMessage::FetchResource {
        request_id: PageRangeRequestId(1),
        resource: ResourceHandle(0),
        byte_range: PageResourceByteRange::Whole {},
        max_bytes: ResourceFetchLength::new(1).expect("length"),
    };
    assert_eq!(message.introduced_in(), FETCH_RESOURCE_SINCE);
}

/// Страница 1.1: просьба уходит надёжно, в форме протокола, и считается.
#[test]
fn a_1_1_page_gets_the_request_in_protocol_form_and_it_is_counted() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_1);
    assert_eq!(
        outbound.resource_request_permission(),
        ResourceRequestPermission::Allowed
    );
    assert_eq!(
        outbound.request_resource(whole(3, 7, 4096)),
        ResourceQueueOutcome::Queued
    );
    assert_eq!(
        outbound.request_resource(slice(4, 100, 50, 50)),
        ResourceQueueOutcome::Queued
    );
    outbound.flush(Instant::now());
    assert_eq!(
        *page.sent.borrow(),
        vec![
            PlayerSessionMessage::FetchResource {
                request_id: PageRangeRequestId(3),
                resource: ResourceHandle(7),
                byte_range: PageResourceByteRange::Whole {},
                max_bytes: ResourceFetchLength::new(4096).expect("length"),
            },
            PlayerSessionMessage::FetchResource {
                request_id: PageRangeRequestId(4),
                resource: ResourceHandle(4),
                byte_range: PageResourceByteRange::Slice {
                    offset: ByteOffset::new(100).expect("offset"),
                    length: ResourceFetchLength::new(50).expect("length"),
                },
                max_bytes: ResourceFetchLength::new(50).expect("length"),
            },
        ]
    );
    let counters = outbound.resource_counters();
    assert_eq!((counters.sent, counters.bytes_limit_requested), (2, 4146));
    // Счётчики запросов байтов файла не тронуты.
    assert_eq!(outbound.range_counters().sent, 0);
}

/// Страница 1.0 (страховка за сторожем): просьба НЕ уходит, отказ явный и учтён.
#[test]
fn a_1_0_page_never_gets_a_resource_request() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_0);
    assert_eq!(
        outbound.resource_request_permission(),
        ResourceRequestPermission::ProtocolTooOld
    );
    assert_eq!(
        outbound.request_resource(whole(1, 0, 1024)),
        ResourceQueueOutcome::ProtocolTooOld
    );
    outbound.flush(Instant::now());
    assert!(page.sent.borrow().is_empty());
    assert_eq!(outbound.resource_counters().refused_old_protocol, 1);
}

/// До `Attached` версия неизвестна — подождать; новый сеанс снова «неизвестно».
#[test]
fn the_version_is_unknown_before_the_attach_and_again_for_a_new_session() {
    let page = FakePage::default();
    let mut outbound = PageOutbound::new(Box::new(page));
    assert_eq!(
        outbound.resource_request_permission(),
        ResourceRequestPermission::NotAttachedYet
    );
    outbound.note_agreed_protocol(ProtocolVersion::V1_1);
    assert_eq!(
        outbound.resource_request_permission(),
        ResourceRequestPermission::Allowed
    );
    outbound.reset_for_new_session();
    assert_eq!(
        outbound.resource_request_permission(),
        ResourceRequestPermission::NotAttachedYet
    );
}

/// Полная очередь к странице и переподключение: просьбы остаются и уходят позже, не
/// сливаясь; сверх мест у страницы — явный отказ, а не тихая потеря.
#[test]
fn requests_survive_a_full_queue_without_merging_and_the_limit_is_explicit() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_1);
    for id in 1..=MAX_CONCURRENT_RESOURCE_FETCHES as u32 {
        assert_eq!(
            outbound.request_resource(whole(id, 5, 1024)),
            ResourceQueueOutcome::Queued
        );
    }
    let overflow = MAX_CONCURRENT_RESOURCE_FETCHES as u32 + 1;
    assert_eq!(
        outbound.request_resource(whole(overflow, 5, 1024)),
        ResourceQueueOutcome::QueueFull
    );
    assert_eq!(outbound.resource_counters().refused_queue_full, 1);

    page.refusals
        .borrow_mut()
        .push_back(SendToPageError::QueueFull);
    outbound.flush(Instant::now());
    assert!(page.sent.borrow().is_empty(), "a full queue keeps them");
    page.refusals
        .borrow_mut()
        .push_back(SendToPageError::NoAttachedSession);
    outbound.flush(Instant::now());
    assert!(
        page.sent.borrow().is_empty(),
        "a reconnecting page keeps them"
    );
    outbound.flush(Instant::now());
    let ids: Vec<u32> = page
        .sent
        .borrow()
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::FetchResource { request_id, .. } => Some(request_id.0),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        (1..=MAX_CONCURRENT_RESOURCE_FETCHES as u32).collect::<Vec<_>>()
    );
    assert_eq!(
        outbound.resource_counters().sent,
        MAX_CONCURRENT_RESOURCE_FETCHES as u64
    );
}

/// Чего протокол не записывает (смещение за 2⁵³), — явный отказ без отправки.
#[test]
fn an_unencodable_request_is_refused_explicitly() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_1);
    assert_eq!(
        outbound.request_resource(slice(1, MAX_JSON_SAFE_INTEGER + 1, 10, 10)),
        ResourceQueueOutcome::NotEncodable
    );
    outbound.flush(Instant::now());
    assert!(page.sent.borrow().is_empty());
    assert_eq!(outbound.resource_counters().unencodable, 1);
}
