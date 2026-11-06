// Договор «позднего входа» (сессия 10d, решение 210): попап -> фон. Шлют только страницы самого
// экстеншена: страница сайта перезагрузкой распоряжаться не может.
//
// Фон отвечает сразу, а работу ведёт сам: попап закрывается, итог человек видит плашкой на странице.

export const RELOAD_AND_OPEN_KIND = "reload_and_open";
export const LATE_ENTRY_STATE_KIND = "late_entry_state";

/** Человек нажал «Reload and open» на вкладке. Без этой команды фон страницу не перезагружает. */
export interface ReloadAndOpenRequest {
  readonly kind: typeof RELOAD_AND_OPEN_KIND;
  readonly tab_id: number;
}

/** Ведёт ли фон поздний вход на вкладке (попап, открытый заново, не должен ему мешать). */
export interface LateEntryStateRequest {
  readonly kind: typeof LATE_ENTRY_STATE_KIND;
  readonly tab_id: number;
}

/** Ответ на оба запроса: `running` — поздний вход этой вкладки сейчас идёт. */
export interface LateEntryStateReply {
  readonly running: boolean;
}
