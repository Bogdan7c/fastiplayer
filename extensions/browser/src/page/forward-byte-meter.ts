// Счётчик байтов, которые MAIN-мир или воркер копирует в сеанс (решение 200, атака «гигантские
// сегменты»). Потолок очереди сеанса живёт в фоне, а копия делается здесь, и раньше, чем фон что-либо
// узнает: MAIN-мир и изолированный мир делят один поток. Цикл страницы
// `appendBuffer(огромный); abort();` не отпускает поток, изолированный мир не получает ни одного
// сообщения, и копии копятся в памяти порта без предела. Поэтому первый предел — здесь, ДО копии.
//
// Владеет только счётом. На превышении `sendStreamItem` бросает `StreamBytesRefused`, а передачу честно
// кончает вызывающий (`handoff_ended{page_flood}` → 4003 у плеера и причина `page_flood` в модели
// вкладки). Настоящий `appendBuffer` страницы к этому моменту уже отработал и о нас не знает: мы только
// наблюдаем.

import { MAX_FORWARDED_APPEND_BYTES } from "../shared/page-message-validation.ts";
import { createTokenBucket, type MillisecondClock } from "../shared/token-bucket.ts";

/**
 * Сколько байт в секунду можно скопировать в сеанс. Живые сайты: 2 МиБ/с (замер записей,
 * content/page-event-budget.ts). Самый большой законный всплеск за секунду — повтор журнала 64 МиБ
 * плюс один самый большой кусок 128 МиБ, то есть 192 МиБ: потолок 256 МиБ/с его вмещает.
 */
export const MAX_FORWARDED_BYTES_PER_SECOND = 256 * 1024 * 1024;

/** Почему кусок нельзя копировать. */
export type ForwardVerdict = "admitted" | "append_too_large" | "rate_exceeded";

export interface ForwardByteMeter {
  /**
   * Можно ли копировать кусок длиной `byteLength`; отказанный кусок счёт не тратит. Длина приходит из
   * снятого геттера, но счётчик ей не верит: не конечное число — это `rate_exceeded`, а не пропуск.
   */
  admit(byteLength: number): ForwardVerdict;
}

/**
 * Монотонные часы, снятые в момент вызова. Метод `now` живёт на прототипе `Performance`, и страница
 * может его подменить, поэтому он связывается здесь и дальше через объект не читается.
 */
function captureMonotonicClock(): MillisecondClock {
  return performance.now.bind(performance);
}

/**
 * Счётчик. Зовётся на старте скрипта, раньше кода страницы, поэтому часы — снятый здесь оригинал.
 * Часы монотонные: перевод системного времени назад не замораживает счёт.
 */
export function createForwardByteMeter(now: MillisecondClock = captureMonotonicClock(), bytesPerSecond: number = MAX_FORWARDED_BYTES_PER_SECOND): ForwardByteMeter {
  const bucket = createTokenBucket(bytesPerSecond, now);
  return {
    admit(byteLength) {
      if (byteLength > MAX_FORWARDED_APPEND_BYTES) return "append_too_large";
      return bucket.take(byteLength) ? "admitted" : "rate_exceeded";
    },
  };
}
