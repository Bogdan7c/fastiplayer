// Метки DRM на видеоэлементах (сессия 09c, инвариант 4, решение 65): защищённое видео экстеншен
// не трогает — ни передачи, ни тени, ни подмен. Этот модуль единственный владелец метки.
//
// Элемент считается защищённым, если выполнено ХОТЬ ОДНО:
//  1. у него есть ключи — оригинальный геттер `mediaKeys` вернул не `null` (читается на лету при
//     вопросе `isProtected`, поэтому ловит и случаи, когда мы пропустили момент);
//  2. на нём сработало ДОВЕРЕННОЕ событие `encrypted` (браузер нашёл в потоке данные шифрования);
//  3. страница УСПЕШНО вызвала `setMediaKeys` с ненулевыми ключами (Vimeo делает так без
//     `encrypted`, поэтому признак нужен отдельно).
// Чего признаком НЕ считаем: `requestMediaKeySystemAccess`. Его зовут «на пробу» (Shaka опрашивает
// 11 систем на любой странице, даже с открытым видео) — защищённым это видео не делает, и мы эту
// функцию не трогаем вовсе.
//
// Как ловим (всё ставится на document_start, до скриптов страницы):
//  - обёртка `HTMLMediaElement.prototype.setMediaKeys` через `prototype-wrap.ts`: сначала оригинал,
//    странице уходит РОВНО его результат (тот же промис) или его же исключение. Метка ставится
//    только когда промис исполнился и ключи были не `null`; отказ и `setMediaKeys(null)` не метят;
//  - слушатель `encrypted` в фазе захвата на window — первый в очереди, поэтому его не заглушит
//    страничный `stopPropagation`. Событие `encrypted` не всплывает и не выходит из shadow root
//    (оно не composed), поэтому такой же слушатель ставится на КАЖДЫЙ shadow root в момент его
//    создания (`attachShadow`, оба режима) — плееры на веб-компонентах прячут `<video>` именно там.
// Обёртки ничего не меняют для страницы: вне передачи это чистое наблюдение.
//
// Известный предел: декларативный shadow root (`<template shadowrootmode>`) создаётся парсером без
// `attachShadow`, его `encrypted` мы не услышим. Защиту такого элемента всё равно видно по ключам
// (признак 1) к моменту, когда страница вызвала `setMediaKeys`.

import type { DomReaders } from "./dom-readers.ts";
import { isObject, observeSafely, wrapMethod, type OddityReporter } from "./prototype-wrap.ts";

/** Окно с тем, что оборачивает модуль (в тестах — фейк). */
export interface DrmWindow {
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { readonly prototype: object };
  readonly HTMLMediaElement: { readonly prototype: object };
  readonly Element: { readonly prototype: object };
  readonly Promise: { readonly prototype: object };
}

/** Кто хочет знать, что элемент стал защищённым (контроллер передачи, освобождение журнала). */
export type ProtectedElementListener = (element: object) => void;

/** Имя события браузера «в потоке найдены данные шифрования». */
const ENCRYPTED_EVENT_TYPE = "encrypted";

/** Настройки слушателя: фаза захвата — раньше любых страничных слушателей на пути события. */
const CAPTURE_LISTENER_OPTIONS = { capture: true } as const;

/** Сколько слабых ссылок копится до уборки умерших элементов (простой проход, не на каждый вызов). */
const SWEEP_THRESHOLD = 64;

export class DrmMarks {
  readonly #dom: DomReaders;
  readonly #reportOddity: OddityReporter;
  /** Элементы, помеченные нами (слабо: метка не держит элемент в памяти). */
  readonly #marked = new WeakSet<object>();
  /** Те же элементы списком — чтобы владельцы журналов могли обойти защищённые (слабо). */
  readonly #markedReferences = new Set<WeakRef<object>>();
  readonly #listeners: ProtectedElementListener[] = [];
  /** Shadow root, на которых слушатель `encrypted` уже стоит. */
  readonly #watchedRoots = new WeakSet<object>();
  readonly #addEventListener: (target: object, ...args: unknown[]) => unknown;
  readonly #eventTargetOf: (event: unknown) => unknown;
  readonly #promiseThen: (promise: object, ...args: unknown[]) => unknown;
  /** Единый обработчик `encrypted` (одна функция на window и на все root). */
  readonly #onEncrypted = (event: unknown): void => {
    observeSafely(this.#reportOddity, ENCRYPTED_EVENT_TYPE, () => this.#handleEncrypted(event));
  };

  constructor(win: DrmWindow, dom: DomReaders, reportOddity: OddityReporter) {
    this.#dom = dom;
    this.#reportOddity = reportOddity;
    // Оригиналы снимаем сейчас: после нас страница вправе менять эти методы как хочет.
    this.#addEventListener = originalMethod(win.EventTarget.prototype, "addEventListener");
    this.#promiseThen = originalMethod(win.Promise.prototype, "then");
    const targetGetter = Object.getOwnPropertyDescriptor(win.Event.prototype, "target")?.get;
    if (targetGetter === undefined) throw new TypeError("no Event.prototype.target");
    this.#eventTargetOf = (event) => Reflect.apply(targetGetter, event, []);
  }

  /** Элемент защищён (признаки 1–3 выше). Чтение ключей может бросить — тогда ответ только по метке. */
  isProtected(element: object): boolean {
    if (this.#marked.has(element)) return true;
    try {
      return this.#dom.hasMediaKeys(element);
    } catch {
      return false;
    }
  }

  /** Подписаться на «элемент стал защищённым» (вызывается один раз на элемент, при первой метке). */
  onProtected(listener: ProtectedElementListener): void {
    this.#listeners.push(listener);
  }

  /** Помеченные (ещё живые) элементы: по ним владельцы журналов отпускают ссылки защищённых MediaSource. */
  markedElements(): object[] {
    const alive: object[] = [];
    for (const reference of this.#markedReferences) {
      const element = reference.deref();
      if (element === undefined) this.#markedReferences.delete(reference);
      else alive.push(element);
    }
    return alive;
  }

  /** Поставить обёртки и слушатель окна. Зовётся один раз на document_start. */
  install(win: DrmWindow): void {
    wrapMethod(win.HTMLMediaElement.prototype, "setMediaKeys", this.#reportOddity, (element, args, outcome) => {
      this.#afterSetMediaKeys(element, args, outcome);
    });
    wrapMethod(win.Element.prototype, "attachShadow", this.#reportOddity, (_host, _args, root) => {
      if (isObject(root)) this.watchShadowRoot(root);
    });
    this.#addEventListener(win, ENCRYPTED_EVENT_TYPE, this.#onEncrypted, CAPTURE_LISTENER_OPTIONS);
  }

  /** Поставить слушатель `encrypted` на shadow root (один раз на root). */
  watchShadowRoot(root: object): void {
    if (this.#watchedRoots.has(root)) return;
    this.#watchedRoots.add(root);
    this.#addEventListener(root, ENCRYPTED_EVENT_TYPE, this.#onEncrypted, CAPTURE_LISTENER_OPTIONS);
  }

  /**
   * После `setMediaKeys`: странице уже отдан результат оригинала (тот же промис). Мы лишь вешаем
   * на него наблюдателя: метка — когда промис исполнился, и только если ключи были не `null`.
   * Отказ промиса наблюдатель гасит у СЕБЯ (производный промис не должен давать лишнего
   * unhandledrejection); сам промис страницы остаётся её собственным.
   */
  #afterSetMediaKeys(element: unknown, args: readonly unknown[], outcome: unknown): void {
    if (!isObject(element) || !isObject(args[0]) || !isObject(outcome)) return;
    this.#promiseThen(
      outcome,
      () => observeSafely(this.#reportOddity, "setMediaKeys", () => this.#mark(element)),
      () => undefined,
    );
  }

  #handleEncrypted(event: unknown): void {
    // Подделанное страницей событие (`dispatchEvent`) не доверенное: оно не должно метить чужие элементы.
    if (!isObject(event) || (event as { readonly isTrusted?: unknown }).isTrusted !== true) return;
    const target = this.#eventTargetOf(event);
    if (isObject(target) && this.#dom.isVideoElement(target)) this.#mark(target);
  }

  #mark(element: object): void {
    if (this.#marked.has(element)) return;
    this.#marked.add(element);
    if (this.#markedReferences.size >= SWEEP_THRESHOLD) this.markedElements();
    this.#markedReferences.add(new WeakRef(element));
    for (const listener of this.#listeners) {
      // Сбой одного подписчика не должен лишать остальных новости (и не доходит до страницы).
      observeSafely(this.#reportOddity, "protected element listener", () => listener(element));
    }
  }
}

/** Оригинальный метод прототипа (по состоянию на сейчас), вызываемый с явным `this`. */
function originalMethod(prototype: object, name: string): (target: object, ...args: unknown[]) => unknown {
  const method: unknown = Object.getOwnPropertyDescriptor(prototype, name)?.value;
  if (typeof method !== "function") throw new TypeError(`no ${name}`);
  return (target, ...args) => Reflect.apply(method, target, args);
}
