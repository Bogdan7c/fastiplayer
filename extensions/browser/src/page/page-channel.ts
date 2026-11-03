// MAIN-сторона участка 1: рукопожатие (page-channel-handshake.ts) и личный порт.
//
// Рукопожатие: слушатель `message` на window в фазе захвата ставится СИНХРОННО при старте скрипта
// (раньше любых скриптов страницы). Принимается только ПЕРВОЕ сообщение от своего окна с меткой
// договора нужной версии и ровно одним портом; для него зовётся `stopImmediatePropagation` —
// обработчики страницы его не видят. После этого слушатель снимается: всё остальное (второе
// рукопожатие, чужие сообщения) идёт странице нетронутым.
//
// Все вызовы — оригиналами, снятыми на document_start: страница позже может подменить
// `MessagePort.prototype.postMessage` и подслушать наш поток, если бы мы звали его по имени.
//
// Встреча (договор page-channel-handshake.ts): сразу после слушателя `message` MAIN-мир слушает
// OFFER изолированного мира и шлёт READY. Так порт приходит, только когда слушатель уже стоит, —
// в каком бы порядке и с какой паузой браузер ни исполнил два наших скрипта (Firefox исполняет их
// не в одном синхронном отрезке).

import {
  HANDSHAKE_MARKER_FIELD,
  HANDSHAKE_VERSION,
  PAGE_CHANNEL_OFFER_EVENT,
  PAGE_CHANNEL_READY_EVENT,
} from "../shared/page-channel-handshake.ts";
import type { CandidatesReply, PageEvent } from "../shared/handoff-contract.ts";
import { parseIncomingPageMessage, type IncomingPageMessage } from "./page-command-guard.ts";
import type { OddityReporter } from "./prototype-wrap.ts";

/** Окно с событиями и портами (в тестах — фейк поверх EventTarget/MessagePort Node). */
export interface ChannelWindow {
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { readonly prototype: object };
  readonly MessageEvent: { readonly prototype: object };
  readonly MessagePort: { readonly prototype: object };
}

/** Оригиналы событий и портов. */
export interface ChannelOriginals {
  addListener(target: object, type: string, listener: (event: unknown) => void, capture: boolean): void;
  removeListener(target: object, type: string, listener: (event: unknown) => void, capture: boolean): void;
  stopImmediatePropagation(event: unknown): void;
  eventData(event: unknown): unknown;
  eventSource(event: unknown): unknown;
  eventPorts(event: unknown): unknown;
  postToPort(port: object, message: unknown, transfer: readonly Transferable[]): void;
  startPort(port: object): void;
}

/** Что уходит по порту из MAIN-мира. */
export type OutgoingPageMessage = PageEvent | CandidatesReply;

/** Личный порт MAIN-мира после рукопожатия. */
export interface PagePort {
  /** Отправить сообщение; `transfer` — буферы, которые передаются, а не копируются. */
  send(message: OutgoingPageMessage, transfer?: readonly Transferable[]): void;
}

function findOwnOrInherited(prototype: object, name: string, part: "get" | "value"): unknown {
  let current: object | null = prototype;
  while (current !== null) {
    const descriptor = Object.getOwnPropertyDescriptor(current, name);
    const found: unknown = part === "get" ? descriptor?.get : descriptor?.value;
    if (found !== undefined) return found;
    current = Object.getPrototypeOf(current) as object | null;
  }
  throw new TypeError(`page channel: ${name} is missing`);
}

/** Снять оригиналы событий и портов. Зовётся один раз на document_start. */
export function captureChannelOriginals(win: ChannelWindow): ChannelOriginals {
  const addEventListener = findOwnOrInherited(win.EventTarget.prototype, "addEventListener", "value") as Function;
  const removeEventListener = findOwnOrInherited(win.EventTarget.prototype, "removeEventListener", "value") as Function;
  const stopImmediate = findOwnOrInherited(win.Event.prototype, "stopImmediatePropagation", "value") as Function;
  const data = findOwnOrInherited(win.MessageEvent.prototype, "data", "get") as Function;
  const source = findOwnOrInherited(win.MessageEvent.prototype, "source", "get") as Function;
  const ports = findOwnOrInherited(win.MessageEvent.prototype, "ports", "get") as Function;
  const postMessage = findOwnOrInherited(win.MessagePort.prototype, "postMessage", "value") as Function;
  const start = findOwnOrInherited(win.MessagePort.prototype, "start", "value") as Function;
  return {
    addListener: (target, type, listener, capture) => Reflect.apply(addEventListener, target, [type, listener, capture]),
    removeListener: (target, type, listener, capture) => Reflect.apply(removeEventListener, target, [type, listener, capture]),
    stopImmediatePropagation: (event) => Reflect.apply(stopImmediate, event, []),
    eventData: (event) => Reflect.apply(data, event, []),
    eventSource: (event) => Reflect.apply(source, event, []),
    eventPorts: (event) => Reflect.apply(ports, event, []),
    postToPort: (port, message, transfer) => Reflect.apply(postMessage, port, [message, transfer]),
    startPort: (port) => Reflect.apply(start, port, []),
  };
}

/** Окно для событий встречи: нужен конструктор `Event` (в тестах — `Event` Node). */
export interface RendezvousWindow {
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { readonly prototype: object; new (type: string): object };
}

/** Оригиналы для событий встречи (снимаются на document_start, как и оригиналы канала). */
export interface RendezvousOriginals {
  /** Синхронно разослать событие слушателям цели (`EventTarget.prototype.dispatchEvent`). */
  dispatch(target: object, event: object): void;
  /** Отменить событие (`Event.prototype.preventDefault`): ответ «я слушаю» на OFFER. */
  preventDefault(event: unknown): void;
  /** Новое событие без данных (оригинальный конструктор `Event`). */
  createEvent(type: string): object;
}

/** Снять оригиналы событий встречи. Зовётся один раз на document_start. */
export function captureRendezvousOriginals(win: RendezvousWindow): RendezvousOriginals {
  const dispatchEvent = findOwnOrInherited(win.EventTarget.prototype, "dispatchEvent", "value") as Function;
  const preventDefault = findOwnOrInherited(win.Event.prototype, "preventDefault", "value") as Function;
  const EventConstructor = win.Event;
  return {
    dispatch: (target, event) => void Reflect.apply(dispatchEvent, target, [event]),
    preventDefault: (event) => Reflect.apply(preventDefault, event, []),
    createEvent: (type) => Reflect.construct(EventConstructor, [type]),
  };
}

/** Порт из события, если это наше рукопожатие; иначе `null`. */
function handshakePortOf(win: object, originals: ChannelOriginals, event: unknown): object | null {
  if (originals.eventSource(event) !== win) return null;
  const message = originals.eventData(event);
  if (typeof message !== "object" || message === null) return null;
  const fieldNames = Object.keys(message);
  if (fieldNames.length !== 1 || fieldNames[0] !== HANDSHAKE_MARKER_FIELD) return null;
  if ((message as Record<string, unknown>)[HANDSHAKE_MARKER_FIELD] !== HANDSHAKE_VERSION) return null;
  const ports = originals.eventPorts(event) as { readonly length: number; readonly [index: number]: unknown } | null;
  if (ports === null || ports.length !== 1) return null;
  const port = ports[0];
  return typeof port === "object" && port !== null ? port : null;
}

/** Обработчики входящего по порту. */
export interface PageChannelHandlers {
  /** Порт установлен (один раз за жизнь документа). */
  onConnected(port: PagePort): void;
  /** Пришло сообщение правильной формы. */
  onMessage(message: IncomingPageMessage): void;
}

/**
 * Поставить слушатель рукопожатия (синхронно, на document_start) и позвать порт событием READY.
 * Всё, что приходит по порту, проверяется по форме; ни одно исключение не уходит странице.
 */
export function listenForPageChannel(
  win: object,
  originals: ChannelOriginals,
  rendezvous: RendezvousOriginals,
  handlers: PageChannelHandlers,
  reportOddity: OddityReporter,
): void {
  const onPortMessage = (event: unknown): void => {
    try {
      const message = parseIncomingPageMessage(originals.eventData(event));
      if (message !== null) handlers.onMessage(message);
    } catch {
      reportOddity("page channel message handling failed");
    }
  };

  // OFFER изолированного мира, стартовавшего ПОСЛЕ нас: отмена = «слушатель стоит, присылай порт».
  // Слушатель живёт до рукопожатия: чужое OFFER до него безвредно (отмена ничего не отдаёт).
  const onOffer = (event: unknown): void => {
    try {
      originals.stopImmediatePropagation(event);
      rendezvous.preventDefault(event);
    } catch {
      reportOddity("page channel offer handling failed");
    }
  };

  const onWindowMessage = (event: unknown): void => {
    try {
      const port = handshakePortOf(win, originals, event);
      if (port === null) return; // не наше — странице как есть
      originals.stopImmediatePropagation(event);
      originals.removeListener(win, "message", onWindowMessage, true);
      // Канал есть: встреча окончена, OFFER больше не отменяем (страница не узнает о нас по отмене).
      originals.removeListener(win, PAGE_CHANNEL_OFFER_EVENT, onOffer, true);
      originals.addListener(port, "message", onPortMessage, false);
      originals.startPort(port);
      handlers.onConnected({ send: (message, transfer = []) => originals.postToPort(port, message, transfer) });
    } catch {
      reportOddity("page channel handshake failed");
    }
  };

  // Порядок важен: сначала слушатель `message`, только потом зов порта — иначе порт мог бы прийти
  // раньше слушателя (та самая гонка Firefox).
  originals.addListener(win, "message", onWindowMessage, true);
  originals.addListener(win, PAGE_CHANNEL_OFFER_EVENT, onOffer, true);
  try {
    // Изолированный мир, стартовавший РАНЬШЕ нас, ждёт этого события и отвечает рукопожатием.
    rendezvous.dispatch(win, rendezvous.createEvent(PAGE_CHANNEL_READY_EVENT));
  } catch {
    reportOddity("page channel ready signal failed");
  }
}
