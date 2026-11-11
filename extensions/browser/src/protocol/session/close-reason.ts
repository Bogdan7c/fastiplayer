// Причины закрытия WebSocket-сеанса и их коды. Зеркало SessionCloseReason (session/mod.rs).
//
// Закрывает соединение плеер (или экстеншен) прикладным кодом 4000-4999 (заняты 4000-4011); по коду
// экстеншен понимает, чинить ли связь, показывать ли сообщение или молча уйти.

export type SessionCloseReason =
  | "player_closed"
  | "replaced_by_another_tab"
  | "tab_gone"
  | "protocol_violation"
  | "invalid_token"
  | "unknown_session"
  | "player_shutting_down"
  | "attach_timeout"
  | "already_attached"
  /** Страница честно забрала видео, вкладка на месте (решение 179); закрывает экстеншен. */
  | "page_took_video_back"
  /**
   * Зритель нажал «Return» на заглушке страницы (сессия 11a, решение 236): видео играет на
   * странице дальше, плеер молча закрывает окно. Закрывает экстеншен. Коды закрытия не
   * версионируются: старый плеер видит чужой 4xxx и тоже молча закрывается, как на 1000.
   */
  | "viewer_returned_to_page"
  /**
   * Серия доиграла, страница сняла источник, а новый не поставила за срок ожидания (протокол 1.9,
   * сессия 16a, решение 307): плеер пишет «The page didn't start the next episode» и закрывает окно без
   * досмотра. Закрывает экстеншен, и только при общей версии не ниже 1.9; со старым плеером - прежним
   * `page_took_video_back` (`closeReasonForUnstartedNextEpisode` в `episode-change.ts`).
   */
  | "next_episode_not_started";

/** Код закрытия каждой причины (стабильные значения протокола 1.x). */
export const SESSION_CLOSE_CODES: Readonly<Record<SessionCloseReason, number>> = {
  player_closed: 4000,
  replaced_by_another_tab: 4001,
  tab_gone: 4002,
  protocol_violation: 4003,
  invalid_token: 4004,
  unknown_session: 4005,
  player_shutting_down: 4006,
  attach_timeout: 4007,
  already_attached: 4008,
  page_took_video_back: 4009,
  viewer_returned_to_page: 4010,
  next_episode_not_started: 4011,
};

/** Код закрытия для причины. */
export function closeCodeOf(reason: SessionCloseReason): number {
  return SESSION_CLOSE_CODES[reason];
}

/** Причина по коду закрытия; `undefined` для чужих кодов (например 1006 «обрыв»). */
export function closeReasonFromCode(code: number): SessionCloseReason | undefined {
  for (const [reason, reasonCode] of Object.entries(SESSION_CLOSE_CODES)) {
    if (reasonCode === code) {
      return reason as SessionCloseReason;
    }
  }
  return undefined;
}
