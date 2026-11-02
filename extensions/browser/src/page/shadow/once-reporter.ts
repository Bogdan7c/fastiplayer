// Единственная отладочная строка за всё время жизни документа.
//
// В продукте журнала экстеншена нет (поправка 00c), а MAIN-мир не должен шуметь в консоли
// страницы. Ошибки, важные для сеанса, становятся событиями (`handoff_ended` и т.п.), а мелкие
// сбои (не удалось восстановить mediaSession, отказ `play()` при возврате) — одной строкой
// `console.debug` за всё время, чтобы при разборе было с чего начать, но без спама.

/** Куда пишется единственная строка (в тестах — фейк). */
export type DebugSink = (message: string, problem: unknown) => void;

export interface OnceReporter {
  /** Сообщить о сбое; в журнал попадает только самый первый за всё время жизни. */
  report(context: string, problem: unknown): void;
}

export function createOnceReporter(sink: DebugSink): OnceReporter {
  let alreadyReported = false;
  return {
    report(context, problem) {
      if (alreadyReported) return;
      alreadyReported = true;
      try {
        sink(`[fastiplayer] ${context}`, problem);
      } catch {
        // Подменённая страницей консоль бросила — молчим: это уже единственная попытка.
      }
    },
  };
}

/**
 * Консоль и её `debug`, снятые при загрузке модуля — то есть на document_start, когда `page.js`
 * исполняется раньше скриптов страницы. Позже страница может подменить `console.debug` и
 * подслушать нашу строку или бросить из неё; снятый оригинал этого не позволяет.
 */
const consoleAtStart = console;
const debugAtStart = console.debug;

/** Общий на документ репортёр тени и стража mediaSession. */
export const shadowProblemReporter: OnceReporter = createOnceReporter((message, problem) => {
  Reflect.apply(debugAtStart, consoleAtStart, [message, problem]);
});
