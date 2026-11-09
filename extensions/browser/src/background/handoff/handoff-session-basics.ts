// Постоянные, мелкие типы и чистые помощники сеанса передачи (`handoff-session.ts`).
//
// Вынесено без смены поведения в сессии 14b2: сеанс упёрся в предел размера (800 строк), а подключение
// памяти выбора добавляет ему несколько строк проводки. Здесь только то, что не зависит от состояния
// сеанса: значения не меняются, функции чистые.

import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import type { OpenSessionRequest } from "../host-link/open-session-requests.ts";
import type { FileAddressRule } from "./file-address-guard.ts";
import type { OutgoingItem } from "./page-event-translator.ts";

/** Элемент очереди открывает новое поколение (`generation_started`). */
export function startsGeneration(item: OutgoingItem): boolean {
  return item.kind === "message" && item.message.type === "generation_started";
}

/** Где сейчас стоит сеанс. Меняется при переезде. */
export interface SessionPlace {
  readonly frameId: number;
  readonly beginRequestId: number;
  readonly fileAddressRule: FileAddressRule;
}

/** Главный документ вкладки: его порт умирает только вместе со страницей, а не с рекламой. */
export const TOP_FRAME_ID = 0;

/** Ушла ли команда в порт страницы. */
export type PageDelivery = "delivered" | "port_dead";

/** Подсказки для окна до первых байтов: в 09a длительность и позицию узнаём только из потока. */
export const UNKNOWN_HINTS: OpenSessionRequest["hints"] = { duration: { kind: "unknown" }, position: { kind: "unknown" } };

/** Отказ старта: HLS этого вида договорённый плеер ещё не играет. */
export const NATIVE_HLS_NOT_YET: HandOffTabOutcome = { kind: "not_supported_yet", form: "native_hls" };

/** Причина нужды связи (для отладки HostLink). */
export const DEMAND_REASON = "handoff";
