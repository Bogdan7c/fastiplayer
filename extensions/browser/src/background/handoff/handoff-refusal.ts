// Перевод исхода команды «отдать вкладку» в запись об отказе для модели вкладки.
// Закрытый список причин (tab-model/tab-snapshot.ts) вместо свободных строк `reason`: попап и 09d
// различают отказы по виду, а не разбирают текст.

import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import type { TabRefusal } from "../tab-model/tab-snapshot.ts";
import type { HandoffSessionEnd } from "./handoff-session.ts";

/** Отказ, соответствующий исходу; `undefined` для `started` (передача состоялась). */
export function refusalOf(outcome: HandOffTabOutcome): TabRefusal | undefined {
  switch (outcome.kind) {
    case "started":
    // Не отказ, а предложение: попап показывает кнопку сразу, модели вкладки помнить нечего.
    case "reload_needed":
      return undefined;
    case "no_video":
    case "video_gone":
    case "protected":
    case "local_address_refused":
    case "player_unavailable":
      return { kind: outcome.kind };
    case "not_supported_yet":
      return { kind: "not_supported_yet", form: outcome.form };
    case "local_address_needs_consent":
      return { kind: "local_address_needs_consent", host: outcome.host };
    case "failed":
      return { kind: "failed" };
  }
}

/**
 * Отказ для уже начатого сеанса: вина страницы, которую попап должен показать честно. Прочие концы
 * (возврат, уход со страницы, ошибка потока) отказом передачи не считаются; `undefined`.
 */
export function refusalOfSessionEnd(end: HandoffSessionEnd): TabRefusal | undefined {
  switch (end.kind) {
    case "page_flood":
      return { kind: "page_flood" };
    case "local_address_refused":
      return { kind: "local_address_refused" };
    case "source_needs_newer_player":
      return { kind: "not_supported_yet", form: end.form };
    case "player_ended":
      // Переполнение очереди — страница не слушает «полон» (гигантские сегменты), тот же флуд.
      return end.cause.kind === "queue_overflow" ? { kind: "page_flood" } : undefined;
    default:
      return undefined;
  }
}
