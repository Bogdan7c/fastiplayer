// Запись Media Session страницы: какие обработчики медиакнопок она ставит и когда их вызвали
// (сессия 09b2, решение 194).
//
// Зачем: play/pause зрителя идёт через обработчик страницы, если он есть. Снаружи не видно, ушло
// ли нажатие в обработчик или старым путём через элемент: у браузера обработчики не прочитать, а
// страж держит их у себя. Без этого не отличить «обработчика нет» от «обработчик не помог».
//
// Как: обёртка `setActionHandler` ставится снаружи стража (страница ходит через неё) и передаёт
// дальше обёрнутый обработчик. Его вызов — и браузером, и стражем — пишется как
// `media_session_action`. Страж оборачивание не замечает: для него это обычная функция страницы.
// Обёртка живёт только в записывающей сборке.

import { isObject, type OddityReporter } from "../prototype-wrap.ts";
import type { StreamRecorder } from "./recorder.ts";

/** Окно с `navigator.mediaSession` (в тестах — фейк). */
export interface MediaSessionRecordingScope {
  readonly navigator?: { readonly mediaSession?: object | null };
}

type PageHandler = (this: unknown, ...details: unknown[]) => unknown;
type SetActionHandler = (this: unknown, action: unknown, handler: unknown) => unknown;

/** Обёрнутый обработчик: записать вызов и передать как есть (`this`, аргументы, результат, ошибка). */
function recordingHandler(recorder: StreamRecorder, action: string, handler: PageHandler): PageHandler {
  return function (this: unknown, ...details: unknown[]): unknown {
    recorder.record("media_session_action", { action });
    return Reflect.apply(handler, this, details);
  };
}

/** Поставить запись поверх стража. `false` — у браузера нет Media Session (записывать нечего). */
export function installMediaSessionRecording(scope: MediaSessionRecordingScope, recorder: StreamRecorder, reportOddity: OddityReporter): boolean {
  const mediaSession = scope.navigator?.mediaSession;
  if (!isObject(mediaSession)) return false;
  const prototype = Object.getPrototypeOf(mediaSession) as object | null;
  const descriptor = prototype === null ? undefined : Object.getOwnPropertyDescriptor(prototype, "setActionHandler");
  const original = descriptor?.value as SetActionHandler | undefined;
  if (prototype === null || descriptor === undefined || typeof original !== "function") {
    reportOddity("stream recorder: mediaSession.setActionHandler is not a plain method");
    return false;
  }
  Object.defineProperty(prototype, "setActionHandler", {
    ...descriptor,
    value: function setActionHandler(this: unknown, action: unknown, handler: unknown): unknown {
      const actionName = String(action);
      const isHandler = typeof handler === "function";
      // Сначала оригинал: отказ браузера (неизвестное действие) уходит странице и в запись не попадает.
      const result = Reflect.apply(original, this, [action, isHandler ? recordingHandler(recorder, actionName, handler as PageHandler) : handler]);
      recorder.record("media_session_handler", { action: actionName, set: isHandler });
      return result;
    },
  });
  return true;
}
