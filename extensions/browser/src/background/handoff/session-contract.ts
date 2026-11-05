// Договор сеанса передачи (сессия 09d вынесла его из `handoff-session.ts`, который упёрся в предел размера):
// чем кончается сеанс и что владелец сеанса может у него спросить. Только типы, поведения здесь нет.

import type { HandOffTabOutcome, HeldVideoReply, HeldVideoReport, PageEventBudgetKind } from "../../shared/handoff-contract.ts";
import type { PageAdState, TabGoneReason } from "../../protocol/index.ts";
import type { PageStreamProblem } from "./page-event-translator.ts";
import type { SocketEndCause } from "./session-socket.ts";

/** Чем закончилась передача (для владельца и тестов; журнала в продукте нет). */
export type HandoffSessionEnd =
  | { readonly kind: "start_failed"; readonly outcome: HandOffTabOutcome }
  | { readonly kind: "page_gone"; readonly reason: TabGoneReason }
  /** Страница сама забрала видео, вкладка на месте (`handoff_ended{element_released}`, 4009). */
  | { readonly kind: "page_took_video_back" }
  /** Страница сняла тень сама без нашей просьбы — сбой внутри страницы (4003, Д-6). */
  | { readonly kind: "page_side_failure" }
  /** Тень замолчала дольше предела пульса (4003, решение 180). */
  | { readonly kind: "shadow_silent" }
  /** Страница превысила бюджет событий или счётчик копий байтов (4003, решение 200); вид — только для журнала. */
  | { readonly kind: "page_flood"; readonly overBudget: PageEventBudgetKind | "forwarded_bytes" }
  /** Страница назвала адрес файла, который правило запрещает (4003, решение 199). */
  | { readonly kind: "local_address_refused" }
  | { readonly kind: "returned_to_page" }
  | { readonly kind: "stream_rejected" }
  | { readonly kind: "player_ended"; readonly cause: SocketEndCause }
  | { readonly kind: "page_stream_problem"; readonly problem: PageStreamProblem };

export interface HandoffSession {
  /** Исход старта: `started` или причина отказа. Всегда завершается. */
  readonly outcome: Promise<HandOffTabOutcome>;
  /** Браузер сообщил, что вкладка закрыта (`tabs.onRemoved`). */
  notifyTabRemoved(): void;
  /** Сторож задержал видео в этом фрейме: переехать на него или отпустить. */
  heldVideo(frameId: number, report: HeldVideoReport): HeldVideoReply;
  /** На экране плеера реклама (решение 204; сессия 09d читает только это). */
  adOnScreen(): boolean;
  /** Донесение фрейма о рекламе (сессия 09d); строгая форма проверена приёмником, вкладка — владельцем. */
  adWatchReport(frameId: number, ad: PageAdState): void;
}

