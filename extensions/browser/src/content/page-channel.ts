// Участок 1, сторона изолированного мира: личный MessagePort с MAIN-миром (договор
// src/shared/page-channel-handshake.ts).
//
// На document_start изолированный мир создаёт MessageChannel, оставляет себе port1 и передаёт port2
// MAIN-миру одним `window.postMessage`. Дальше весь обмен — по порту: обработчики `message` страницы
// не видят ни сообщений, ни байтов.
//
// Firefox (живая разведка 09a): сообщение, пришедшее по порту, живёт в отсеке СТРАНИЦЫ и видно
// изолированному миру через Xray: `instanceof Uint8Array` ложно, побайтовое чтение ~275 мс/МиБ.
// `structuredClone` в изолированном мире делает родную копию (~0,5 мс/МиБ) — её и проверяем.
// В Chromium сообщение сразу родное, копия не нужна.
//
// Когда уходит порт (встреча, договор page-channel-handshake.ts). Прежде порт уходил слепо, на старте,
// в расчёте на то, что оба document_start-скрипта исполняются в одном синхронном отрезке и задача
// доставки `message` идёт уже после MAIN-скрипта. В Firefox это не так: он ждёт компиляцию КАЖДОГО
// скрипта отдельно, и задача доставки успевала выполниться в паузе между нашими скриптами (стенд после
// 09b: content.js отправил порт, page.js встал через ~5 мс, рукопожатие пропало — «no_video»).
// Теперь порт уходит только к уже стоящему слушателю MAIN-мира:
//   - на старте — отменяемое OFFER: MAIN-мир, если уже стоит, отменяет его → отправляем сразу;
//   - иначе ждём READY от MAIN-мира (он шлёт его сразу после своего слушателя) → отправляем.
// Отправка одна за жизнь документа: второе сообщение с меткой увидели бы обработчики `message`
// страницы (MAIN-мир принимает и прячет только первое). Сообщения, отправленные по своему порту до
// встречи, не теряются: порт копит их, пока второй конец не начнёт приём.

import { HANDSHAKE_MESSAGE, PAGE_CHANNEL_OFFER_EVENT, PAGE_CHANNEL_READY_EVENT } from "../shared/page-channel-handshake.ts";

/** Порт канала в той мере, в какой он нам нужен (MessagePort или фейк в тестах). */
export interface PageChannelPort {
  postMessage(message: unknown): void;
  onmessage: ((event: { readonly data: unknown }) => void) | null;
}

/** Цель событий встречи — своё окно (в тестах — любой EventTarget). */
export interface RendezvousTarget {
  addEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  removeEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  dispatchEvent(event: Event): boolean;
}

/** Окружение изолированного мира (в продукте — настоящий window, в тестах — фейки). */
export interface PageChannelEnvironment {
  /** Новый MessageChannel. */
  createChannel(): { readonly port1: PageChannelPort; readonly port2: Transferable };
  /** `window.postMessage(message, "*", transfer)` своего окна. */
  postToOwnWindow(message: unknown, transfer: Transferable[]): void;
  /** Где встречаются миры: своё окно (оба мира видят одни и те же события DOM). */
  readonly rendezvous: RendezvousTarget;
  /** ОТМЕНЯЕМОЕ событие без данных (`cancelable: true`) — по отмене узнаём, что MAIN-мир слушает. */
  createCancelableEvent(type: string): Event;
}

/** Отправка MAIN-миру по личному порту. */
export interface PageChannel {
  send(message: unknown): void;
}

/**
 * Значение, пришедшее из MAIN-мира, в виде объекта ЭТОГО мира. Чужой отсек (Firefox) узнаётся по
 * тому, что объект не наследует `Object` этого мира; тогда делаем родную структурную копию.
 * `undefined` — копия не удалась (страница подсунула то, что не клонируется): это мусор.
 */
export function adoptPageValue(raw: unknown): unknown {
  if (typeof raw !== "object" || raw === null || raw instanceof Object) return raw;
  try {
    return structuredClone(raw);
  } catch {
    return undefined;
  }
}

/**
 * Открывает канал: создаёт порт, вешает приёмник и передаёт второй конец MAIN-миру — как только
 * его слушатель точно стоит (встреча OFFER/READY, см. шапку файла).
 * `onPageMessage` получает каждое сообщение уже «усыновлённым» (`adoptPageValue`), но НЕ проверенным.
 */
export function openPageChannel(environment: PageChannelEnvironment, onPageMessage: (message: unknown) => void): PageChannel {
  const channel = environment.createChannel();
  const ownPort = channel.port1;
  // Присвоение onmessage само запускает порт (start()). Ранние сообщения не теряются в обе стороны:
  // MAIN-мир пишет только после рукопожатия, а наше `send` до встречи копится во втором конце, пока
  // MAIN-мир не начнёт приём (`start()` в его рукопожатии).
  ownPort.onmessage = (event) => onPageMessage(adoptPageValue(event.data));

  const { rendezvous } = environment;
  const listenerOptions = { capture: true } as const;
  /**
   * Отдать порт MAIN-миру. Ровно один раз: зовётся либо по отменённому OFFER (один раз, на старте),
   * либо из слушателя READY, который здесь же снимается, — второго READY уже никто не услышит.
   */
  const handOverPort = (): void => {
    rendezvous.removeEventListener(PAGE_CHANNEL_READY_EVENT, onMainReady, listenerOptions);
    environment.postToOwnWindow(HANDSHAKE_MESSAGE, [channel.port2]);
  };
  /** READY: MAIN-мир встал позже нас, его слушатель `message` уже стоит. */
  const onMainReady = (event: Event): void => {
    event.stopImmediatePropagation();
    handOverPort();
  };

  // Сначала слушатель READY, потом OFFER: если MAIN-мир стоит раньше нас, он отменит OFFER.
  rendezvous.addEventListener(PAGE_CHANNEL_READY_EVENT, onMainReady, listenerOptions);
  const offerAccepted = !rendezvous.dispatchEvent(environment.createCancelableEvent(PAGE_CHANNEL_OFFER_EVENT));
  if (offerAccepted) handOverPort();
  return { send: (message) => ownPort.postMessage(message) };
}
