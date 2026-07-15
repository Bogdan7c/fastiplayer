/// Итог terminal shutdown desktop integration.
///
/// Один enum сохраняет различия, важные process owner-у: истёкший deadline не
/// равен успешному завершению, panic backend-а не маскируется transport error-ом,
/// а повторный drain уже завершённого backend-а остаётся явным no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopIntegrationShutdownOutcome {
    /// Shutdown-запрос принят, backend thread завершён и успешно joined.
    Completed,

    /// Backend был успешно joined более ранним вызовом.
    AlreadyCompleted,

    /// Deadline истёк; join authority всё ещё принадлежит runtime-у.
    TimedOut,

    /// Backend thread завершился panic-ом.
    ThreadPanicked,

    /// Terminal control request не дошёл до platform backend-а.
    TransportFailed(DesktopIntegrationShutdownTransportFailure),
}

/// Transport-класс ошибки terminal shutdown request.
///
/// Тип намеренно не содержит platform/zbus деталей: будущие adapters обязаны
/// отображать собственные transport errors в этот neutral intent boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopIntegrationShutdownTransportFailure {
    /// Control channel закрылся до приёма terminal request.
    ControlChannelDisconnected,
}
