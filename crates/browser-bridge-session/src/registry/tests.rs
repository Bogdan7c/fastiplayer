use super::*;

/// Выдаёт пропуска «0000…0001», «…0002» по порядку; можно сломать.
struct CountingTokens {
    issued: u32,
    broken: bool,
}

impl CountingTokens {
    fn new() -> Self {
        Self {
            issued: 0,
            broken: false,
        }
    }
}

impl SessionTokenSource for CountingTokens {
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError> {
        if self.broken {
            return Err(TokenSourceError::EntropyUnavailable {
                reason: "сломан".to_owned(),
            });
        }
        self.issued += 1;
        Ok(SessionToken::new(format!("{:032}", self.issued))?)
    }
}

fn timings() -> RegistryTimings {
    RegistryTimings {
        first_attach_deadline: Duration::from_secs(15),
        reconnect_grace: Duration::from_secs(10),
    }
}

fn key(text: &str) -> SessionKey {
    SessionKey::new(text).unwrap()
}

fn sref(text: &str, instance: u64) -> SessionRef {
    SessionRef {
        key: key(text),
        instance: SessionInstance(instance),
    }
}

fn token(number: u32) -> SessionToken {
    SessionToken::new(format!("{number:032}")).unwrap()
}

const CONN_1: ConnectionId = ConnectionId(1);
const CONN_2: ConnectionId = ConnectionId(2);

fn registry_with_attached(now: Instant, tokens: &mut CountingTokens) -> SessionRegistry {
    let mut registry = SessionRegistry::new(timings());
    registry.open(&key("a"), now, tokens).unwrap();
    registry.attach(&key("a"), &token(1), CONN_1).unwrap();
    registry
}

#[test]
fn start_new_issues_a_token_and_awaits_attach() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    let opened = registry.open(&key("a"), now, &mut tokens).unwrap();
    assert_eq!(opened.outcome, GrantOutcome::Started);
    assert_eq!(opened.replaced, None);
    assert!(opened.token.matches(&token(1)));
    assert_eq!(registry.active_key(), Some(&key("a")));
    assert_eq!(
        registry.next_deadline(),
        Some(now + Duration::from_secs(15))
    );
}

#[test]
fn repeat_open_before_attach_returns_the_same_token_and_one_session() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    let first = registry.open(&key("a"), now, &mut tokens).unwrap();
    let later = now + Duration::from_secs(3);
    let second = registry.open(&key("a"), later, &mut tokens).unwrap();
    assert_eq!(second.outcome, GrantOutcome::ReusedExisting);
    assert!(second.token.matches(&first.token));
    assert_eq!(tokens.issued, 1, "второй пропуск выдавать не нужно");
    // Повтор продлевает срок первого подключения.
    assert_eq!(
        registry.next_deadline(),
        Some(later + Duration::from_secs(15))
    );
}

#[test]
fn replace_reports_the_old_key_and_its_connection() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    let opened = registry.open(&key("b"), now, &mut tokens).unwrap();
    assert_eq!(opened.outcome, GrantOutcome::ReplacedPrevious);
    assert_eq!(
        opened.replaced,
        Some(ReplacedSession {
            session: sref("a", 1),
            connection: Some(CONN_1)
        })
    );
    assert_eq!(registry.active_key(), Some(&key("b")));
    assert_eq!(registry.attached_connection(), None);
    // Старый пропуск не подходит к новому сеансу.
    assert_eq!(
        registry.attach(&key("a"), &token(1), CONN_2),
        Err(AttachRejection::UnknownSession)
    );
}

#[test]
fn replace_with_failing_token_source_keeps_the_old_session_intact() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    tokens.broken = true;
    assert!(registry.open(&key("b"), now, &mut tokens).is_err());
    assert_eq!(registry.active_key(), Some(&key("a")));
    assert_eq!(registry.attached_connection(), Some(CONN_1));
}

#[test]
fn attach_checks_key_then_token_then_occupancy_and_consumes_the_token() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    assert_eq!(
        registry.attach(&key("a"), &token(1), CONN_1),
        Err(AttachRejection::UnknownSession),
        "сеансов ещё нет"
    );
    registry.open(&key("a"), now, &mut tokens).unwrap();
    assert_eq!(
        registry.attach(&key("zzz"), &token(1), CONN_1),
        Err(AttachRejection::UnknownSession)
    );
    assert_eq!(
        registry.attach(&key("a"), &token(77), CONN_1),
        Err(AttachRejection::InvalidToken)
    );
    assert_eq!(
        registry.attach(&key("a"), &token(1), CONN_1),
        Ok(sref("a", 1))
    );
    // Одноразовость: тот же пропуск второй раз — недействителен, а не «занято».
    assert_eq!(
        registry.attach(&key("a"), &token(1), CONN_2),
        Err(AttachRejection::InvalidToken)
    );
}

#[test]
fn rejected_attach_does_not_change_the_registry() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    registry.open(&key("a"), now, &mut tokens).unwrap();
    let before = registry.active.clone();
    let _ = registry.attach(&key("zzz"), &token(1), CONN_1);
    let _ = registry.attach(&key("a"), &token(5), CONN_1);
    assert_eq!(registry.active, before);
    // И настоящий пропуск после отказов всё ещё работает.
    assert_eq!(
        registry.attach(&key("a"), &token(1), CONN_1),
        Ok(sref("a", 1))
    );
}

#[test]
fn reuse_while_attached_issues_next_token_and_second_attach_gets_already_attached() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    let reopened = registry.open(&key("a"), now, &mut tokens).unwrap();
    assert_eq!(reopened.outcome, GrantOutcome::ReusedExisting);
    assert!(reopened.token.matches(&token(2)));
    assert_eq!(registry.attached_connection(), Some(CONN_1));
    let before = registry.active.clone();
    assert_eq!(
        registry.attach(&key("a"), &token(2), CONN_2),
        Err(AttachRejection::AlreadyAttached)
    );
    assert_eq!(registry.active, before, "отказ 4008 не трогает сеанс");
}

#[test]
fn lost_connection_enters_grace_then_reopen_issues_fresh_token_and_reattach_works() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    let lost_at = now + Duration::from_secs(60);
    assert_eq!(
        registry.connection_lost(CONN_1, lost_at),
        Some(sref("a", 1))
    );
    assert_eq!(
        registry.next_deadline(),
        Some(lost_at + Duration::from_secs(10))
    );
    assert_eq!(registry.expire(lost_at + Duration::from_secs(9)), None);

    let reopened = registry
        .open(&key("a"), lost_at + Duration::from_secs(2), &mut tokens)
        .unwrap();
    assert_eq!(reopened.outcome, GrantOutcome::ReusedExisting);
    assert!(reopened.token.matches(&token(2)));
    assert_eq!(
        registry.attach(&key("a"), &token(2), CONN_2),
        Ok(sref("a", 1))
    );
    assert_eq!(registry.attached_connection(), Some(CONN_2));
}

#[test]
fn connection_lost_of_a_foreign_connection_is_ignored() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    assert_eq!(registry.connection_lost(CONN_2, now), None);
    assert_eq!(registry.attached_connection(), Some(CONN_1));
}

#[test]
fn grace_and_first_attach_deadlines_end_the_session_with_their_own_reason() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    registry.open(&key("a"), now, &mut tokens).unwrap();
    assert_eq!(registry.expire(now + Duration::from_secs(14)), None);
    assert_eq!(
        registry.expire(now + Duration::from_secs(15)),
        Some((sref("a", 1), SessionEndReason::FirstAttachDeadlineExpired))
    );
    assert_eq!(registry.active_key(), None);

    let mut registry = registry_with_attached(now, &mut CountingTokens::new());
    registry.connection_lost(CONN_1, now);
    assert_eq!(
        registry.expire(now + Duration::from_secs(10)),
        Some((sref("a", 1), SessionEndReason::ReconnectGraceExpired))
    );
}

#[test]
fn attached_session_has_no_deadline_and_never_expires() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    assert_eq!(registry.next_deadline(), None);
    assert_eq!(registry.expire(now + Duration::from_secs(86_400)), None);
}

#[test]
fn end_session_of_only_matches_the_attached_connection() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    assert_eq!(registry.end_session_of(CONN_2), None);
    assert_eq!(registry.end_session_of(CONN_1), Some(sref("a", 1)));
    assert_eq!(registry.active_key(), None);
}

#[test]
fn end_active_returns_key_and_connection() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    assert_eq!(
        registry.end_active(),
        Some(EndedSession {
            session: sref("a", 1),
            connection: Some(CONN_1)
        })
    );
    assert_eq!(registry.end_active(), None);
}

#[test]
fn end_instance_ends_only_the_matching_active_instance() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    // Чужой экземпляр того же ключа и чужой ключ того же номера — мимо, реестр цел.
    assert_eq!(registry.end_instance(&sref("a", 2)), None);
    assert_eq!(registry.end_instance(&sref("b", 1)), None);
    assert_eq!(registry.active_key(), Some(&key("a")));
    assert_eq!(registry.attached_connection(), Some(CONN_1));
    // Свой экземпляр — закрыт, вместе с соединением.
    assert_eq!(
        registry.end_instance(&sref("a", 1)),
        Some(EndedSession {
            session: sref("a", 1),
            connection: Some(CONN_1)
        })
    );
    assert_eq!(registry.active_key(), None);
    // Повтор — уже не активен.
    assert_eq!(registry.end_instance(&sref("a", 1)), None);
}

#[test]
fn rejection_close_reasons_use_protocol_codes() {
    assert_eq!(
        AttachRejection::UnknownSession.close_reason().close_code(),
        4005
    );
    assert_eq!(
        AttachRejection::InvalidToken.close_reason().close_code(),
        4004
    );
    assert_eq!(
        AttachRejection::AlreadyAttached.close_reason().close_code(),
        4008
    );
}

#[test]
fn instances_grow_on_start_and_replace_and_stay_on_reuse() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = SessionRegistry::new(timings());
    let first = registry.open(&key("a"), now, &mut tokens).unwrap();
    let reused = registry.open(&key("a"), now, &mut tokens).unwrap();
    assert_eq!(
        reused.instance, first.instance,
        "повтор не создаёт новый экземпляр"
    );
    let replacing = registry.open(&key("b"), now, &mut tokens).unwrap();
    assert!(replacing.instance > first.instance);
    assert_eq!(
        replacing.replaced.map(|replaced| replaced.session.instance),
        Some(first.instance)
    );
}

#[test]
fn same_key_reopened_after_grace_expiry_is_a_new_instance_distinct_from_the_ended_one() {
    let now = Instant::now();
    let mut tokens = CountingTokens::new();
    let mut registry = registry_with_attached(now, &mut tokens);
    registry.connection_lost(CONN_1, now);
    let (ended, _) = registry.expire(now + Duration::from_secs(10)).unwrap();
    let reopened = registry
        .open(&key("a"), now + Duration::from_secs(10), &mut tokens)
        .unwrap();
    assert_eq!(ended.key, key("a"));
    assert_eq!(reopened.outcome, GrantOutcome::Started);
    assert_ne!(
        reopened.instance, ended.instance,
        "конец старого не должен совпасть с новым"
    );
}
