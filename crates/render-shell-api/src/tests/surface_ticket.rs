use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::*;

/// Буфер-шпион: отмечает, что его уронили (у wgpu так буфер возвращается swapchain-у).
#[derive(Debug)]
struct SpyBuffer {
    sequence: u32,
    dropped: Rc<Cell<bool>>,
}

impl Drop for SpyBuffer {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

/// Свой рендер открывает свой билет и получает тот же буфер и время ожидания.
#[test]
fn owner_opens_its_ticket_and_gets_the_same_buffer() {
    let owner = RendererInstanceId::allocate();
    let dropped = Rc::new(Cell::new(false));
    let ticket = AcquiredSurfaceFrame::new(
        owner,
        SpyBuffer {
            sequence: 7,
            dropped: dropped.clone(),
        },
        Duration::from_millis(3),
    );
    assert_eq!(ticket.acquire_elapsed(), Duration::from_millis(3));

    let opened = ticket
        .into_buffer::<SpyBuffer>(owner)
        .expect("owner must open its own ticket");

    assert_eq!(opened.buffer.sequence, 7);
    assert_eq!(opened.acquire_elapsed, Duration::from_millis(3));
    assert!(
        !dropped.get(),
        "opening must hand the buffer over, not drop it"
    );
}

/// Билет другого экземпляра того же типа не открывается, а его буфер возвращается своим drop-ом.
#[test]
fn another_instance_cannot_open_the_ticket() {
    let owner = RendererInstanceId::allocate();
    let other_instance = RendererInstanceId::allocate();
    let dropped = Rc::new(Cell::new(false));
    let ticket = AcquiredSurfaceFrame::new(
        owner,
        SpyBuffer {
            sequence: 1,
            dropped: dropped.clone(),
        },
        Duration::ZERO,
    );

    let refusal = ticket
        .into_buffer::<SpyBuffer>(other_instance)
        .expect_err("ticket of another renderer instance must be refused");

    assert_eq!(refusal.ticket_owner, owner);
    assert_eq!(refusal.expected_owner, other_instance);
    assert!(
        dropped.get(),
        "refused buffer must be dropped (returned to its swapchain)"
    );
}

/// Свой паспорт, но чужой тип буфера (подделка) — тоже отказ.
#[test]
fn foreign_buffer_type_is_refused_even_with_matching_owner() {
    let owner = RendererInstanceId::allocate();
    let ticket = AcquiredSurfaceFrame::new(owner, "not a renderer buffer", Duration::ZERO);

    assert!(ticket.into_buffer::<SpyBuffer>(owner).is_err());
}

/// Неиспользованный билет при drop-е роняет буфер внутри.
#[test]
fn dropping_unused_ticket_drops_the_buffer() {
    let dropped = Rc::new(Cell::new(false));
    let ticket = AcquiredSurfaceFrame::new(
        RendererInstanceId::allocate(),
        SpyBuffer {
            sequence: 2,
            dropped: dropped.clone(),
        },
        Duration::ZERO,
    );

    drop(ticket);

    assert!(dropped.get());
}

/// Паспорта уникальны: пересозданный рендер получает другой номер.
#[test]
fn renderer_instance_ids_are_unique() {
    let first = RendererInstanceId::allocate();
    let second = RendererInstanceId::allocate();

    assert_ne!(first, second);
    assert_ne!(first, RendererInstanceId::unowned());
    assert_ne!(second.value(), 0);
}
