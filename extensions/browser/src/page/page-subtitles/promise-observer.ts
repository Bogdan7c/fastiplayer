// Подписка на промис страницы снятым оригиналом `Promise.prototype.then`.
//
// Страница может подменить `Promise.prototype.then` (полифилы, трассировщики), и тогда наш «наблюдатель»
// увидел бы её код вместо браузерного. Поэтому `then` снимается на document_start и зовётся через
// `Reflect.apply`. Подписка ставится синхронно, ДО того как промис вернулся странице: реакции промиса
// выполняются в порядке подписки, и наша сработает раньше страничной (для `fetch` это важно: `clone()`
// должен успеть до того, как страница начнёт читать тело).

/** Окно с `Promise` (в тестах — Promise Node). */
export interface PromiseWindow {
  readonly Promise: { readonly prototype: object };
}

/** Позвать `onFulfilled` с результатом промиса; ошибки наблюдателя и отказ промиса странице не видны. */
export type PromiseObserver = (promise: unknown, onFulfilled: (value: unknown) => void) => void;

export function capturePromiseObserver(win: PromiseWindow): PromiseObserver {
  const then = Object.getOwnPropertyDescriptor(win.Promise.prototype, "then")?.value as Function | undefined;
  if (typeof then !== "function") throw new TypeError("Promise.prototype.then is missing");
  return (promise, onFulfilled) => {
    // Оба обработчика не бросают: производный промис `then` всегда выполняется и не даёт странице
    // события `unhandledrejection` ни по нашей вине, ни по вине отказа исходного промиса (его увидит сама страница).
    const fulfilled = (value: unknown): void => {
      try {
        onFulfilled(value);
      } catch {
        // Сбой наблюдения не должен быть виден странице; причину фиксирует сам `onFulfilled` через репортёр.
      }
    };
    Reflect.apply(then, promise, [fulfilled, () => undefined]);
  };
}
