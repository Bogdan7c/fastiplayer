// Что сказать человеку об исходе передачи, начатой режимом 2 «автозапуск» (сессия 12a).
// Отдельная чистая функция, чтобы правило было видно целиком и его легко менять.
//
// Человек сам нажал play и не просил открывать плеер, поэтому молчим, когда всё прошло как ожидалось
// или когда отказ ничего ему не даёт:
// - новое окно плеера открылось       -> молчим (он и так видит окно);
// - окно плеера уже открыто (другой сеанс; оно может быть за браузером, поднимать его — сессия 17) -> плашка
//   «Now playing in the Fastiplayer window»: тот же текст, что у входов режима 1;
// - плеер недоступен или внутренний сбой -> плашка отказа: видео не ушло, и человеку надо знать, почему оно не там;
// - любой другой отказ (нет видео, защита, неподдержанная форма, локальный адрес, перезагрузка) -> молчим:
//   видео продолжает играть в браузере, как и играло.

import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import type { EntryResult } from "../entries/entry-report-text.ts";

/** Итог для репортёра входов или `undefined` — сказать нечего. */
export function noticeOfAutoHandoffOutcome(outcome: HandOffTabOutcome): EntryResult | undefined {
  switch (outcome.kind) {
    case "started":
      return outcome.player_window === "already_open" ? { kind: "outcome", outcome } : undefined;
    case "player_unavailable":
    case "failed":
      return { kind: "outcome", outcome };
    case "no_video":
    case "not_supported_yet":
    case "protected":
    case "reload_needed":
    case "video_gone":
    case "local_address_refused":
    case "local_address_needs_consent":
      return undefined;
  }
}
