// Реестр элементов документа: какой blob-адрес принадлежит какому MediaSource, где искать `<video>`
// (включая закрытые shadow root) и ключи видеоэлементов для договора (`ElementKey`).
//
// Владеет:
//  - картой «blob-адрес → MediaSource» (обёртка `URL.createObjectURL`, только для MediaSource);
//  - списком ЗАКРЫТЫХ shadow root, созданных через `attachShadow` (слабые ссылки): закрытый
//    shadow root из DOM не достать никак, а `<video>` в нём бывает (плееры на веб-компонентах).
//    Открытые не запоминаем: их находит обход (`host.shadowRoot`), а на страницах с тысячами
//    веб-компонентов список всех root рос бы зря;
//  - ключами элементов: номер выдаётся при первом описании и не меняется, пока элемент жив.
// Всё — слабые ссылки: реестр не держит в памяти ни элементы, ни MediaSource страницы.

import type { ElementKey } from "../shared/handoff-contract.ts";
import type { DomReaders } from "./dom-readers.ts";
import { isObject, wrapMethod, type OddityReporter } from "./prototype-wrap.ts";

/** Окно с тем, что оборачивает реестр (в тестах — фейк). */
export interface RegistryWindow {
  readonly URL: object;
  readonly Element: { readonly prototype: object };
}

/**
 * Сколько записей копится до уборки умерших (адреса MediaSource, закрытые shadow root). Записей
 * немного, уборка — простой проход; порог лишь не даёт делать её на каждый вызов страницы.
 */
const SWEEP_THRESHOLD = 64;

export class ElementRegistry {
  readonly #dom: DomReaders;
  readonly #isMediaSource: (candidate: unknown) => boolean;
  readonly #mediaSourceByUrl = new Map<string, WeakRef<object>>();
  readonly #latestUrlOfMediaSource = new WeakMap<object, string>();
  readonly #closedShadowRoots = new Set<WeakRef<object>>();
  readonly #keyOfElement = new WeakMap<object, ElementKey>();
  readonly #elementOfKey = new Map<ElementKey, WeakRef<Element>>();
  #lastElementKey = 0;

  constructor(dom: DomReaders, isMediaSource: (candidate: unknown) => boolean) {
    this.#dom = dom;
    this.#isMediaSource = isMediaSource;
  }

  /** Обернуть `URL.createObjectURL` и `Element.prototype.attachShadow` (на document_start). */
  installHooks(win: RegistryWindow, reportOddity: OddityReporter): void {
    wrapMethod(win.URL, "createObjectURL", reportOddity, (_target, args, url) => {
      const subject = args[0];
      if (typeof url === "string" && this.#isMediaSource(subject) && isObject(subject)) this.noteObjectUrl(url, subject);
    });
    wrapMethod(win.Element.prototype, "attachShadow", reportOddity, (host, _args, root) => {
      // Только что созданный root не виден через host.shadowRoot — значит, он закрытый.
      if (isObject(host) && isObject(root) && this.#dom.openShadowRootOf(host) === null) this.noteClosedShadowRoot(root);
    });
  }

  /** Страница создала blob-адрес для MediaSource. */
  noteObjectUrl(url: string, mediaSource: object): void {
    if (this.#mediaSourceByUrl.size >= SWEEP_THRESHOLD) this.#sweepDeadUrls();
    this.#mediaSourceByUrl.set(url, new WeakRef(mediaSource));
    this.#latestUrlOfMediaSource.set(mediaSource, url);
  }

  /** Страница создала закрытый shadow root. */
  noteClosedShadowRoot(root: object): void {
    if (this.#closedShadowRoots.size >= SWEEP_THRESHOLD) this.#sweepDeadShadowRoots();
    this.#closedShadowRoots.add(new WeakRef(root));
  }

  /** Последний blob-адрес MediaSource (`null` — адрес не создавался). */
  objectUrlOf(mediaSource: object): string | null {
    return this.#latestUrlOfMediaSource.get(mediaSource) ?? null;
  }

  /** Адрес создан `URL.createObjectURL` для (ещё живого) MediaSource этого документа. */
  isMediaSourceObjectUrl(url: string): boolean {
    return this.mediaSourceAt(url) !== null;
  }

  /** MediaSource за blob-адресом (`null` — адрес не наш или MediaSource уже собран). */
  mediaSourceAt(url: string): object | null {
    return this.#mediaSourceByUrl.get(url)?.deref() ?? null;
  }

  /**
   * Все `<video>` документа: сам документ, открытые shadow root (обходом) и запомненные закрытые
   * (если они в документе), с вложенностью любой глубины.
   */
  findVideoElements(): Element[] {
    this.#sweepDeadShadowRoots();
    const videos = new Set<Element>();
    const visitedRoots = new Set<object>();
    const rootsToVisit: object[] = [];
    for (const reference of this.#closedShadowRoots) {
      const root = reference.deref();
      if (root !== undefined && this.#dom.isConnected(root)) rootsToVisit.push(root);
    }
    const collect = (elements: readonly Element[]): void => {
      for (const element of elements) {
        if (this.#dom.isVideoElement(element)) videos.add(element);
        const openRoot = this.#dom.openShadowRootOf(element);
        if (openRoot !== null) rootsToVisit.push(openRoot);
      }
    };
    collect(this.#dom.elementsInDocument());
    while (rootsToVisit.length > 0) {
      const root = rootsToVisit.pop();
      if (root === undefined || visitedRoots.has(root)) continue;
      visitedRoots.add(root);
      collect(this.#dom.elementsInShadowRoot(root));
    }
    return [...videos];
  }

  /** Ключ элемента: выдаётся один раз и не меняется. */
  keyOf(element: Element): ElementKey {
    const known = this.#keyOfElement.get(element);
    if (known !== undefined) return known;
    this.#sweepDeadKeys();
    this.#lastElementKey += 1;
    this.#keyOfElement.set(element, this.#lastElementKey);
    this.#elementOfKey.set(this.#lastElementKey, new WeakRef(element));
    return this.#lastElementKey;
  }

  /** Элемент по ключу, если он жив и всё ещё в документе. */
  elementByKey(key: ElementKey): Element | null {
    const element = this.#elementOfKey.get(key)?.deref();
    if (element === undefined || !this.#dom.isConnected(element)) return null;
    return element;
  }

  #sweepDeadUrls(): void {
    for (const [url, reference] of this.#mediaSourceByUrl) {
      if (reference.deref() === undefined) this.#mediaSourceByUrl.delete(url);
    }
  }

  #sweepDeadShadowRoots(): void {
    for (const reference of this.#closedShadowRoots) {
      if (reference.deref() === undefined) this.#closedShadowRoots.delete(reference);
    }
  }

  #sweepDeadKeys(): void {
    for (const [key, reference] of this.#elementOfKey) {
      if (reference.deref() === undefined) this.#elementOfKey.delete(key);
    }
  }
}
