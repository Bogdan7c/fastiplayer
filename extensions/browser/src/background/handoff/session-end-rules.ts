// Правила концов сеанса передачи, добавленные в 11a (решения 236–238). Чистые функции без состояния:
// `handoff-session.ts` (у предела размера) зовёт их одной строкой, а тесты проверяют их напрямую.
//
// - чем снять тень, когда плеер закрыл сокет сам (решение 238): окно закрыто без прощания → пауза;
// - какую причину `tab_gone` послать, когда сломался плеер страницы (решение 237): новое ЗНАЧЕНИЕ протокола
//   1.3 только плееру, который его знает;
// - как закрыть сокет, когда страница подтвердила снятие тени по нашему `end_handoff`.

import { SESSION_CLOSE_CODES, tabGoneReasonIntroducedIn, versionIncludes, type ProtocolVersion, type SessionCloseReason, type TabGoneReason } from "../../protocol/index.ts";
import type { PageCommand } from "../../shared/handoff-contract.ts";
import type { HandoffSessionEnd } from "./session-contract.ts";
import { END_WITHOUT_PLAYER, RETURN_PAUSED_AT_SHADOW } from "./session-page-commands.ts";
import { NORMAL_CLOSE_CODE, type QueueDeliveringCloseReason, type SocketEndCause } from "./session-socket.ts";

/**
 * Коды, которыми плеер закрывает сокет, когда ЗРИТЕЛЬ закрыл окно (или его заменили, или плеер выходит).
 * Такой конец — решение человека, а не авария: видео не должно заиграть на странице само.
 */
const PLAYER_WINDOW_CLOSE_REASONS: readonly SessionCloseReason[] = ["player_closed", "replaced_by_another_tab", "player_shutting_down"];

/**
 * Чем снять тень, когда сокет кончился по стороне плеера, пока передача активна (`return_to_page` не было:
 * иначе сеанс был бы уже в фазе `ending`). Окно закрыто человеком (4000/4001/4006) — позиция тени и пауза
 * (решение 238). Любой другой обрыв (1006 — плеер упал, таймауты, нарушения протокола) — как раньше:
 * позиция тени, играть как тень, страница не должна замолчать из-за нашей аварии.
 */
export function endCommandAfterPlayerSocketEnd(cause: SocketEndCause): Extract<PageCommand, { kind: "end_handoff" }> {
  const closedByViewer = cause.kind === "player_closed" && cause.reason !== undefined && PLAYER_WINDOW_CLOSE_REASONS.includes(cause.reason);
  return closedByViewer ? RETURN_PAUSED_AT_SHADOW : END_WITHOUT_PLAYER;
}

/**
 * Причина `tab_gone`, когда плеер страницы сломался (решение 237). Плееру до 1.3 нового значения не шлём
 * (его строгий разбор отверг бы всё сообщение): он получает ближайшее старое — `navigated_away`, тот же
 * путь «доиграть скачанное».
 */
export function tabGoneReasonForPagePlayerFailure(agreedProtocol: ProtocolVersion): TabGoneReason {
  return versionIncludes(agreedProtocol, tabGoneReasonIntroducedIn("page_player_failed")) ? "page_player_failed" : "navigated_away";
}

/** Как закрыть сокет после подтверждённого снятия тени. */
export type ConfirmedEndClosing =
  /** Дописать очередь страницы и закрыть кодом причины (плеер покажет её своими словами). */
  | { readonly kind: "deliver_queue_then_close"; readonly reason: QueueDeliveringCloseReason }
  /** Закрыть сразу этим кодом, очередь не дописывается. */
  | { readonly kind: "close_now"; readonly code: number };

/**
 * Закрытие сокета после того, как страница подтвердила наш `end_handoff` (или не ответила вовремя):
 * - отказ поколения (`stream_rejected`) — 4003 после очереди: окно не должно гаснуть молча (09a2);
 * - «Return» зрителя (решение 236) — 4010 сразу: окно закрывается мгновенно и молча, без дописывания
 *   очереди (видео уже играет страница, догружать окну нечего). Обычно сокет к этому моменту уже закрыт
 *   тем же кодом в миг нажатия, и повтор ничего не делает;
 * - возврат по просьбе плеера (`return_to_page`) и прочее — обычное 1000: конец решил и объяснил сам плеер.
 */
export function closingAfterConfirmedEnd(end: HandoffSessionEnd): ConfirmedEndClosing {
  switch (end.kind) {
    case "stream_rejected":
      return { kind: "deliver_queue_then_close", reason: "protocol_violation" };
    case "returned_by_viewer":
      return { kind: "close_now", code: SESSION_CLOSE_CODES.viewer_returned_to_page };
    default:
      return { kind: "close_now", code: NORMAL_CLOSE_CODE };
  }
}
