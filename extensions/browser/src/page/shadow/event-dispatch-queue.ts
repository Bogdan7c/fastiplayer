// Очередь синтетических событий тени и исходов обещаний `play()`.
//
// Браузер не шлёт медиасобытия изнутри вызова страницы: `play()` возвращается, а `play` и
// `playing` приходят позже (задачей). Страница, которая подписывается сразу после `play()`,
// на это рассчитывает, а синхронная отправка изнутри её же вызова привела бы к повторному
// входу в её код. Поэтому тень копит события и отдаёт их одной микрозадачей — в том же
// порядке, в каком их выпустила модель. Таймеров нет.

import { callOriginal, type ElementOriginals } from "./originals.ts";

/** Одна отложенная операция: отправка события или исход обещаний. */
type DeferredStep = () => void;

export class EventDispatchQueue {
  readonly #element: HTMLMediaElement;
  readonly #originals: ElementOriginals;
  readonly #steps: DeferredStep[] = [];
  #flushScheduled = false;

  constructor(element: HTMLMediaElement, originals: ElementOriginals) {
    this.#element = element;
    this.#originals = originals;
  }

  /** Отправить странице синтетическое медиасобытие (не всплывает, не отменяется — как настоящее). */
  enqueueEvent(type: string): void {
    this.enqueueStep(() => {
      const events = this.#originals.events;
      const event = new events.EventConstructor(type);
      callOriginal(events.dispatchEvent, this.#element, [event]);
    });
  }

  /** Отложить произвольный шаг (исход обещаний `play()`) в общий порядок событий. */
  enqueueStep(step: DeferredStep): void {
    this.#steps.push(step);
    if (this.#flushScheduled) return;
    this.#flushScheduled = true;
    this.#originals.queueMicrotask(() => this.#flush());
  }

  #flush(): void {
    this.#flushScheduled = false;
    // Шаги, добавленные слушателями во время раздачи, раздаются в этом же проходе по порядку.
    while (this.#steps.length > 0) {
      const step = this.#steps.shift();
      step?.();
    }
  }
}
