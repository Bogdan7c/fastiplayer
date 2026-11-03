// Какой MediaSource стоит за каким handle, ушедшим из воркера в главный поток (сессия 09b).
//
// Главный поток видит только `MediaSourceHandle` в `srcObject` элемента; связать его с MediaSource
// воркера можно лишь по порядку сообщений. Правило:
//  - геттер `MediaSource.prototype.handle` обёрнут: каждый выданный handle запоминается → его
//    MediaSource (слабо);
//  - `postMessage` воркера обёрнут: каждый
//    УСПЕШНЫЙ вызов получает номер по порядку (1, 2, …), и handle из его списка transfer
//    сообщаются наблюдателю вместе с этим номером;
//  - главный поток считает доверенные события `message`/`messageerror` на объекте Worker в том же
//    порядке (браузер доставляет их 1:1 и по порядку), так «n-е сообщение» совпадает на обеих сторонах.
// Наши собственные донесения идут по личному MessagePort, а не через `postMessage` воркера, поэтому
// счёт не сбивают.

import { isObject, wrapGetter, wrapMethod, type OddityReporter } from "../page/prototype-wrap.ts";

/** Кто узнаёт об ушедших handle. */
export interface HandleTransferObserver {
  /**
   * Успешный `postMessage` номер `messageOrdinal` передал handle; на каждый — его MediaSource
   * (`null` — handle не выдан нашим геттером: чужой, например из вложенного воркера).
   */
  messagePosted(messageOrdinal: number, mediaSourcesOfHandles: readonly (object | null)[]): void;
}

/** Глобал воркера с тем, что оборачиваем (в тестах — фейк). */
export interface HandleTransferScope {
  readonly MediaSource?: { readonly prototype: object };
  readonly MediaSourceHandle?: { readonly prototype: object };
  readonly DedicatedWorkerGlobalScope?: { readonly prototype: object };
}

/**
 * Сколько элементов списка transfer просматриваем. Настоящие сообщения передают единицы объектов;
 * предел не даёт враждебному коду заставить хук перебирать миллионы.
 */
const MAX_TRANSFER_ENTRIES_SCANNED = 64;

/** Поставить обе обёртки. `false` — в воркере нет нужного (тогда сопоставления нет, честно). */
export function installHandleTransferTracking(scope: HandleTransferScope, observer: HandleTransferObserver, reportOddity: OddityReporter): boolean {
  const handlePrototype = scope.MediaSourceHandle?.prototype;
  const mediaSourcePrototype = scope.MediaSource?.prototype;
  const postMessageOwner = postMessageOwnerOf(scope);
  if (handlePrototype === undefined || mediaSourcePrototype === undefined || postMessageOwner === null) return false;
  const isHandle = (candidate: unknown): candidate is object =>
    isObject(candidate) && Reflect.apply(Object.prototype.isPrototypeOf, handlePrototype, [candidate]) === true;

  const mediaSourceOfHandle = new WeakMap<object, object>();
  let messageOrdinal = 0;
  const gotHandle = wrapGetter(mediaSourcePrototype, "handle", reportOddity, (mediaSource, handle) => {
    if (isObject(mediaSource) && isHandle(handle)) mediaSourceOfHandle.set(handle, mediaSource);
  });
  const gotPostMessage = wrapMethod(postMessageOwner, "postMessage", reportOddity, (_scope, args) => {
    // Номер — первым: даже если разбор transfer ниже сорвётся, счёт не разойдётся с главным потоком.
    messageOrdinal += 1;
    const scan = handlesIn(transferListOf(args[1]), isHandle);
    if (scan.truncated) {
      // Список длиннее предела: что в хвосте — не знаем. Честно «не наш», чтобы главный поток
      // не принял найденный там handle за чужой MediaSource с чужим ключом.
      observer.messagePosted(messageOrdinal, [null]);
      return;
    }
    if (scan.handles.length > 0) observer.messagePosted(messageOrdinal, scan.handles.map((handle) => mediaSourceOfHandle.get(handle) ?? null));
  });
  if (!gotHandle || !gotPostMessage) {
    reportOddity("worker handle tracking is partial");
    return false;
  }
  return true;
}

/**
 * Где лежит `postMessage` воркера. По WebIDL операции интерфейса с [Global]
 * (`DedicatedWorkerGlobalScope`) — собственные свойства САМОГО глобала, а не прототипа (так в
 * Chromium: у `DedicatedWorkerGlobalScope.prototype` своего `postMessage` нет). Прототип — запасной
 * путь для окружений, где метод всё же там.
 */
function postMessageOwnerOf(scope: HandleTransferScope): object | null {
  if (typeof Object.getOwnPropertyDescriptor(scope, "postMessage")?.value === "function") return scope;
  const prototype = scope.DedicatedWorkerGlobalScope?.prototype;
  return prototype !== undefined && typeof Object.getOwnPropertyDescriptor(prototype, "postMessage")?.value === "function" ? prototype : null;
}

/** Список transfer из второго аргумента: массив или `{ transfer }` (`StructuredSerializeOptions`). */
function transferListOf(argument: unknown): unknown {
  if (Array.isArray(argument)) return argument;
  return isObject(argument) ? (argument as { readonly transfer?: unknown }).transfer : undefined;
}

/** Handle из списка; `truncated` — список длиннее предела и просмотрен не весь. */
function handlesIn(list: unknown, isHandle: (candidate: unknown) => candidate is object): { readonly handles: object[]; readonly truncated: boolean } {
  if (!Array.isArray(list)) return { handles: [], truncated: false };
  if (list.length > MAX_TRANSFER_ENTRIES_SCANNED) return { handles: [], truncated: true };
  const handles: object[] = [];
  for (let index = 0; index < list.length; index += 1) {
    const entry: unknown = list[index];
    if (isHandle(entry)) handles.push(entry);
  }
  return { handles, truncated: false };
}
