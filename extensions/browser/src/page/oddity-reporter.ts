// Отладочная запись о необычном в MAIN-мире: ОДНА за всё время жизни документа.
//
// Журнала экстеншена в продукте нет (поправка 00c), а консоль страницы видит её владелец и любой
// её скрипт — поэтому никакого спама: первая странность пишется в `console.debug`, остальные
// не пишутся (первой хватает, чтобы понять, что на странице что-то пошло не так).
// Ошибки, важные для сеанса, сюда не идут — они становятся исходами и событиями договора.

import type { OddityReporter } from "./prototype-wrap.ts";

/** Куда писать (оригинальный `console.debug`, снятый на document_start). */
export type DebugSink = (line: string) => void;

/** Префикс, по которому запись узнаётся в консоли. */
const DEBUG_PREFIX = "Fastiplayer Bridge:";

export function createOddityReporter(sink: DebugSink): OddityReporter {
  let reported = false;
  return (what) => {
    if (reported) return;
    reported = true;
    try {
      sink(`${DEBUG_PREFIX} ${what}`);
    } catch {
      // Консоль подменена и бросает — молчим: странице не должно быть хуже из-за нас.
    }
  };
}
