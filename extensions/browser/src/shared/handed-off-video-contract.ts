// Договор «якоря переданного видео» (сессия 11a, решение 236): как изолированный мир узнаёт, НАД КАКИМ
// элементом рисовать заглушку «Playing in Fastiplayer», хотя ссылок на элементы у него нет.
//
// Изолированный мир знает только метку документа и ключ элемента; сам элемент живёт в MAIN-мире. По порту
// участка 1 элемент не передать (структурное клонирование DOM-узлов не умеет). Зато DOM общий для обоих
// миров: событие, разосланное на элементе, слышат слушатели изолированного мира на своём `window`.
//
// Как устроено:
//  1. Изолированный мир выбирает СЛУЧАЙНОЕ имя события (`HANDED_OFF_VIDEO_EVENT_NAME_BYTES` байт крипто-
//     генератора, ≥ 128 бит), ставит на своё `window` слушатель этого имени в фазе ЗАХВАТА и шлёт имя MAIN-
//     миру по личному порту (`watch_handed_off_video`). Фону это сообщение не уходит никогда.
//  2. MAIN-мир, когда тень на элементе встала, рассылает на этом элементе `new Event(name, {composed: true})`
//     ОРИГИНАЛЬНЫМИ `Event`/`dispatchEvent`, снятыми на document_start: подмены страницы его не перехватят.
//     Захват идёт от `window` вниз всегда (даже без всплытия), а `composed` выводит событие из теневого
//     дерева, поэтому слушатель изолированного мира получает его первым и гасит.
//  3. Передача на элементе кончилась (любая причина) — MAIN-мир рассылает то же событие на САМОМ `window`.
//     Оба сигнала — синхронные события одного вида, поэтому порядок «передано → ушло → передано» не может
//     перепутаться (сообщение порта пришло бы позже следующего события и сняло бы НОВУЮ заглушку).
// Страница имени не знает (оно идёт по личному порту), поэтому ни подслушать, ни подделать сигнал не может.
//
// Известное ограничение: у элемента внутри ЗАКРЫТОГО теневого дерева изолированный мир видит только его
// хозяина (так браузер перенаправляет `target`, а `composedPath()` закрытые узлы скрывает): заглушка
// рисуется над хозяином. Внутри открытого дерева `composedPath()[0]` — сам элемент.

/** Изолированный → MAIN: «вот имя события якоря». */
export const WATCH_HANDED_OFF_VIDEO_KIND = "watch_handed_off_video";

/**
 * Случайных байтов в имени события: 16 (128 бит). Столько же, сколько у метки документа: угадать имя
 * перебором странице не под силу, а длина (32 шестнадцатеричные цифры) не стоит ничего.
 */
export const HANDED_OFF_VIDEO_EVENT_NAME_BYTES = 16;

/** Имя события якоря — ровно столько шестнадцатеричных цифр (строчных), сколько даёт генератор. */
const EVENT_NAME_PATTERN = new RegExp(`^[0-9a-f]{${HANDED_OFF_VIDEO_EVENT_NAME_BYTES * 2}}$`);

/** Изолированный → MAIN: имя события якоря (принимается только первое за жизнь документа). */
export interface WatchHandedOffVideoMessage {
  readonly kind: typeof WATCH_HANDED_OFF_VIDEO_KIND;
  readonly event_name: string;
}

/** Заполнить байты криптографическим генератором своего мира (`crypto.getRandomValues`). */
export type RandomBytesFill = (bytes: Uint8Array<ArrayBuffer>) => void;

/** Новое случайное имя события якоря: шестнадцатеричная запись байтов генератора. */
export function createHandedOffVideoEventName(fill: RandomBytesFill): string {
  const bytes = new Uint8Array(HANDED_OFF_VIDEO_EVENT_NAME_BYTES);
  fill(bytes);
  let name = "";
  for (const byte of bytes) name += byte.toString(16).padStart(2, "0");
  return name;
}

/** Имя события якоря по форме договора. */
export function isHandedOffVideoEventName(value: unknown): value is string {
  return typeof value === "string" && EVENT_NAME_PATTERN.test(value);
}

/** Разобрать сообщение порта как `watch_handed_off_video`; `null` — не оно или не по форме. */
export function parseWatchHandedOffVideo(candidate: unknown): WatchHandedOffVideoMessage | null {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) return null;
  const fields = candidate as Readonly<Record<string, unknown>>;
  const ownNames = Object.keys(fields);
  if (ownNames.length !== 2 || fields["kind"] !== WATCH_HANDED_OFF_VIDEO_KIND || !Object.hasOwn(fields, "event_name")) return null;
  const eventName = fields["event_name"];
  return isHandedOffVideoEventName(eventName) ? { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: eventName } : null;
}

/**
 * Имя события предложения «Ask» (сессия 12b): своё, но производное от тайного имени якоря. Отдельного сообщения
 * по порту не нужно, а странице оно так же неизвестно: без тайной части его не построить. Канал отдельный от
 * заглушки, чтобы конец одной передачи не снимал предложение над другим видео той же страницы.
 */
export function viewerOfferEventNameOf(anchorEventName: string): string {
  return `${anchorEventName}-offer`;
}
