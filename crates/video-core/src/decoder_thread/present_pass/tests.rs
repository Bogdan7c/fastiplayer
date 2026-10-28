use std::time::Duration;

use crossbeam_channel::TryRecvError;

use super::{PresentPassNotifyOutcome, PresentPassSignal};

/// Звонок без подписчиков — честный no-op, а не ошибка.
#[test]
fn notify_without_subscribers_reports_no_subscribers() {
    let signal = PresentPassSignal::new();

    assert_eq!(
        signal.notify_present_pass_submitted(),
        PresentPassNotifyOutcome::NoSubscribers
    );
}

/// Каждый показ кладёт жетон подписчику, а повторные показы до приёма сливаются в один.
#[test]
fn repeated_notifications_coalesce_into_one_pulse() {
    let signal = PresentPassSignal::new();
    let subscription = signal.subscribe().expect("fresh signal is not poisoned");

    for _ in 0..3 {
        assert_eq!(
            signal.notify_present_pass_submitted(),
            PresentPassNotifyOutcome::Delivered { subscribers: 1 }
        );
    }

    assert_eq!(subscription.pulse_receiver().try_recv(), Ok(()));
    assert_eq!(
        subscription.pulse_receiver().try_recv(),
        Err(TryRecvError::Empty)
    );
}

/// Два подписчика (декодер прошлого файла и кандидат нового) не отбирают жетон друг у друга.
#[test]
fn every_subscription_receives_its_own_pulse() {
    let signal = PresentPassSignal::new();
    let first = signal.subscribe().expect("fresh signal is not poisoned");
    let second = signal.subscribe().expect("fresh signal is not poisoned");

    assert_eq!(
        signal.notify_present_pass_submitted(),
        PresentPassNotifyOutcome::Delivered { subscribers: 2 }
    );

    assert_eq!(first.pulse_receiver().try_recv(), Ok(()));
    assert_eq!(second.pulse_receiver().try_recv(), Ok(()));
}

/// Уничтоженная подписка выбрасывается при следующем звонке и не копится в списке.
#[test]
fn dropped_subscription_is_pruned_on_next_notification() {
    let signal = PresentPassSignal::new();
    let kept = signal.subscribe().expect("fresh signal is not poisoned");
    drop(signal.subscribe().expect("fresh signal is not poisoned"));

    assert_eq!(
        signal.notify_present_pass_submitted(),
        PresentPassNotifyOutcome::Delivered { subscribers: 1 }
    );
    assert_eq!(kept.pulse_receiver().try_recv(), Ok(()));

    drop(kept);
    assert_eq!(
        signal.notify_present_pass_submitted(),
        PresentPassNotifyOutcome::NoSubscribers
    );
}

/// Клон источника звонит тем же подписчикам: рендер отдаёт клон приложению и фабрике.
#[test]
fn cloned_signal_shares_subscribers() {
    let signal = PresentPassSignal::new();
    let subscription = signal
        .clone()
        .subscribe()
        .expect("fresh signal is not poisoned");

    signal.notify_present_pass_submitted();

    assert_eq!(subscription.pulse_receiver().try_recv(), Ok(()));
}

/// Источник уничтожен (рендер пересоздан) — подписчик видит разрыв, а не вечное ожидание.
#[test]
fn dropped_signal_disconnects_subscription() {
    let signal = PresentPassSignal::new();
    let subscription = signal.subscribe().expect("fresh signal is not poisoned");

    drop(signal);

    assert_eq!(
        subscription
            .pulse_receiver()
            .recv_timeout(Duration::from_secs(1)),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected)
    );
}

/// Отравленный список не роняет ни рендер, ни декодер: оба получают типизированный исход.
#[test]
fn poisoned_signal_reports_typed_outcomes() {
    let signal = PresentPassSignal::new();
    let poisoning_signal = signal.clone();
    let panic_result = std::thread::spawn(move || {
        let _guard = poisoning_signal
            .subscribers
            .lock()
            .expect("lock before poisoning");
        panic!("poison the subscriber list on purpose");
    })
    .join();
    assert!(
        panic_result.is_err(),
        "helper thread must panic to poison the mutex"
    );

    assert_eq!(
        signal.notify_present_pass_submitted(),
        PresentPassNotifyOutcome::SignalPoisoned
    );
    assert_eq!(
        signal.subscribe().err(),
        Some(super::PresentPassSignalPoisoned)
    );
}
