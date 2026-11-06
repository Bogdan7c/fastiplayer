// «Какое видео под указателем» в MAIN-мире (сессия 10c). Одна причина существовать: браузер сообщает фону
// фрейм правого клика, но не элемент; а кнопке поверх видео нужно знать видео под курсором. Элементы
// видны только здесь (закрытые shadow root, реестр ключей), поэтому ищем тут.
//
// Два вопроса:
//  1. `describeRightClicked` — видео под ПОСЛЕДНИМ НАСТОЯЩИМ (`isTrusted`) `contextmenu`. Слушатель на
//     `window` в фазе захвата ставится на document_start: страница не успеет остановить событие. Каждое новое
//     настоящее событие перезаписывает память, в том числе на «ничего»: старое видео не должно уехать
//     в плеер после правого клика по пустому месту. Событие, созданное скриптом страницы, не настоящее и память не трогает.
//  2. `videoAtPoint` — видео под точкой вьюпорта (для наведения).
//
// Поиск видео:
//  - сначала `composedPath()` события (настоящий путь, виден и сквозь открытые shadow root);
//  - если `<video>` в пути нет — слой сайта поверх видео или ЗАКРЫТЫЙ shadow root (его внутренностей в пути
//    нет) — берём видео из реестра, чей прямоугольник содержит точку. Из нескольких выбирается верхнее
//    по `elementsFromPoint` (оно отдаёт хозяина закрытого root вместо внутреннего элемента, поэтому ранг
//    видео — позиция в стопке первого элемента, который есть само видео или его составной предок);
//    ранг не определился (видео с `pointer-events: none` в стопку не попадает) — наибольшее по площади.
//
// Все чтения — оригинальными методами, снятыми на document_start: страница позже может подменить прототипы.

import type { DocumentTag, ElementKey } from "../shared/handoff-contract.ts";
import type { PageUiRect, RightClickedVideo, VideoAtPoint } from "../shared/entry-contract.ts";
import type { ViewportRect } from "./dom-readers.ts";

/** Оригинальные чтения события правого клика. */
export interface PointerEventReaders {
  /** Событие настоящее (от человека), а не созданное скриптом страницы. */
  isTrusted(event: unknown): boolean;
  /** `event.composedPath()` на момент вызова; бросает, если событие не наше. */
  composedPath(event: unknown): readonly unknown[];
  /** Координаты вьюпорта (`clientX`, `clientY`); `null` — не мышиное событие. */
  position(event: unknown): { readonly x: number; readonly y: number } | null;
}

/** Оригинальные чтения дерева. */
export interface PointerTreeReaders {
  /** `document.elementsFromPoint(x, y)`: сверху вниз; содержимое закрытых root заменено хозяином. */
  elementsFromPoint(x: number, y: number): readonly unknown[];
  /** Составной родитель: `parentNode`, а над корнем shadow root — его хозяин; `null` — выше нет. */
  composedParentOf(node: object): object | null;
}

/** Всё, что поиску нужно от остального MAIN-мира. Узкий шов: тесты подставляют фейки без браузера. */
export interface PointerVideoEnvironment {
  readonly documentTag: DocumentTag;
  readonly events: PointerEventReaders;
  readonly tree: PointerTreeReaders;
  /** Все `<video>` документа (реестр элементов). */
  videoElements(): readonly Element[];
  keyOf(element: Element): ElementKey;
  isVideoElement(candidate: unknown): boolean;
  isConnected(node: object): boolean;
  rectOf(element: Element): ViewportRect;
  /** Элемент сейчас под тенью у контроллера передачи (намерение, а не поле контроллера). */
  isInPlayer(element: Element): boolean;
}

/**
 * Предел подъёма по составным предкам: страницы с вложенностью в сотни уровней бывают, но вечный цикл
 * (повреждённое дерево подмены) нас остановить не должен.
 */
const MAX_ANCESTOR_STEPS = 1024;

/** Потолок видео, среди которых ищем под точкой: враждебная страница с тысячами `<video>` не должна вешать клик. */
const MAX_VIDEOS_PER_POINT_SEARCH = 256;

function containsPoint(rect: ViewportRect, x: number, y: number): boolean {
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

function areaOf(rect: ViewportRect): number {
  return Math.max(0, rect.right - rect.left) * Math.max(0, rect.bottom - rect.top);
}

export class PointerVideo {
  readonly #environment: PointerVideoEnvironment;
  /** Видео последнего настоящего правого клика; `null` — клик пришёлся не на видео. Слабая ссылка: элемент не держим. */
  #rightClicked: WeakRef<Element> | null = null;

  constructor(environment: PointerVideoEnvironment) {
    this.#environment = environment;
  }

  /** Слушатель `contextmenu` (window, захват). Не бросает: сбой чтения = «под кликом ничего». */
  noteContextMenu(event: unknown): void {
    try {
      if (!this.#environment.events.isTrusted(event)) return;
      this.#rightClicked = this.#videoUnderEvent(event);
    } catch {
      // Событие нечитаемо: безопаснее забыть прошлое видео, чем отдать чужое.
      this.#rightClicked = null;
    }
  }

  /** Видео последнего настоящего правого клика; `null` — его не было или оно уже ушло из документа. */
  describeRightClicked(): RightClickedVideo | null {
    const element = this.#rightClicked?.deref();
    if (element === undefined || !this.#environment.isConnected(element)) return null;
    return { document: this.#environment.documentTag, element: this.#environment.keyOf(element) };
  }

  /** Видео под точкой вьюпорта; `null` — под точкой видео нет. */
  videoAtPoint(x: number, y: number): VideoAtPoint | null {
    const element = this.#videoAtPoint(x, y);
    if (element === null) return null;
    return {
      document: this.#environment.documentTag,
      element: this.#environment.keyOf(element),
      rect: this.#asSizedRect(this.#environment.rectOf(element)),
      in_player: this.#environment.isInPlayer(element),
    };
  }

  #asSizedRect(rect: ViewportRect): PageUiRect {
    return { left: rect.left, top: rect.top, width: rect.right - rect.left, height: rect.bottom - rect.top };
  }

  #videoUnderEvent(event: unknown): WeakRef<Element> | null {
    const { events } = this.#environment;
    for (const node of events.composedPath(event)) {
      if (typeof node === "object" && node !== null && this.#environment.isVideoElement(node)) return new WeakRef(node as Element);
    }
    // Видео в пути нет: слой сайта над видео или закрытый shadow root. Ищем по точке.
    const position = events.position(event);
    const found = position === null ? null : this.#videoAtPoint(position.x, position.y);
    return found === null ? null : new WeakRef(found);
  }

  /** Видео реестра под точкой: верхнее по стопке, иначе наибольшее. */
  #videoAtPoint(x: number, y: number): Element | null {
    const rects = new Map<Element, ViewportRect>();
    for (const element of this.#environment.videoElements().slice(0, MAX_VIDEOS_PER_POINT_SEARCH)) {
      try {
        const rect = this.#environment.rectOf(element);
        if (areaOf(rect) > 0 && containsPoint(rect, x, y) && this.#environment.isConnected(element)) rects.set(element, rect);
      } catch {
        // Элемент нечитаем (чужой «бренд»): пропускаем, остальные не страдают.
      }
    }
    if (rects.size === 0) return null;
    const stack = this.#stackAt(x, y);
    const ranked = [...rects.entries()].map(([element, rect]) => ({ element, area: areaOf(rect), rank: this.#rankOf(element, stack) }));
    // Определившийся ранг (ближе к верху стопки) выигрывает у неопределившегося; при равных — больше площадь.
    ranked.sort((left, right) => left.rank - right.rank || right.area - left.area);
    return ranked[0]?.element ?? null;
  }

  #stackAt(x: number, y: number): readonly unknown[] {
    try {
      return this.#environment.tree.elementsFromPoint(x, y);
    } catch {
      return [];
    }
  }

  /** Позиция в стопке первого элемента, который есть само видео или его составной предок; нет — бесконечность. */
  #rankOf(video: Element, stack: readonly unknown[]): number {
    const lineage = new Set<object>([video]);
    let current: object = video;
    for (let step = 0; step < MAX_ANCESTOR_STEPS; step += 1) {
      const parent = this.#environment.tree.composedParentOf(current);
      if (parent === null) break;
      lineage.add(parent);
      current = parent;
    }
    const index = stack.findIndex((layer) => typeof layer === "object" && layer !== null && lineage.has(layer));
    return index === -1 ? Number.POSITIVE_INFINITY : index;
  }
}

// ---------------------------------------------------------------------------------------------
// Снятие оригиналов (document_start)
// ---------------------------------------------------------------------------------------------

/** Окно с тем, что читают оригиналы указателя (в тестах — фейк). */
export interface PointerWindow {
  readonly Event: { readonly prototype: object };
  readonly Document: { readonly prototype: object };
  readonly Node: { readonly prototype: object };
  readonly document: object;
  /** Нет `MouseEvent` — координат не будет: остаётся только `composedPath`. */
  readonly MouseEvent?: { readonly prototype: object };
  /** Нет `ShadowRoot` — составной подъём останавливается на корне. */
  readonly ShadowRoot?: { readonly prototype: object };
}

function findMember(prototype: object | undefined, name: string, part: "get" | "value"): unknown {
  let current: object | null = prototype ?? null;
  while (current !== null) {
    const descriptor = Object.getOwnPropertyDescriptor(current, name);
    const found: unknown = part === "get" ? descriptor?.get : descriptor?.value;
    if (typeof found === "function") return found;
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

/** Оригиналы чтения события и дерева. `null` — браузер (или фейк) не даёт нужного: вход по указателю не включается. */
export function capturePointerReaders(win: PointerWindow): { readonly events: PointerEventReaders; readonly tree: PointerTreeReaders } | null {
  const composedPath = findMember(win.Event.prototype, "composedPath", "value");
  const elementsFromPoint = findMember(win.Document.prototype, "elementsFromPoint", "value");
  const parentNode = findMember(win.Node.prototype, "parentNode", "get");
  if (typeof composedPath !== "function" || typeof elementsFromPoint !== "function" || typeof parentNode !== "function") return null;
  const clientX = findMember(win.MouseEvent?.prototype, "clientX", "get");
  const clientY = findMember(win.MouseEvent?.prototype, "clientY", "get");
  const shadowHost = findMember(win.ShadowRoot?.prototype, "host", "get");
  const shadowRootPrototype = win.ShadowRoot?.prototype;
  const isPrototypeOf = Object.prototype.isPrototypeOf;
  const documentObject = win.document;
  return {
    events: {
      // `isTrusted` — собственное неподменяемое свойство самого события (LegacyUnforgeable).
      isTrusted: (event) => typeof event === "object" && event !== null && (event as { isTrusted?: unknown }).isTrusted === true,
      composedPath: (event) => {
        const path: unknown = Reflect.apply(composedPath, event, []);
        return Array.isArray(path) ? path : [];
      },
      position: (event) => {
        if (typeof clientX !== "function" || typeof clientY !== "function") return null;
        const x: unknown = Reflect.apply(clientX, event, []);
        const y: unknown = Reflect.apply(clientY, event, []);
        return typeof x === "number" && typeof y === "number" ? { x, y } : null;
      },
    },
    tree: {
      elementsFromPoint: (x, y) => {
        const stack: unknown = Reflect.apply(elementsFromPoint, documentObject, [x, y]);
        return Array.isArray(stack) ? stack : [];
      },
      composedParentOf: (node) => {
        const parent: unknown = Reflect.apply(parentNode, node, []);
        if (typeof parent !== "object" || parent === null) return null;
        const isShadowRoot = shadowRootPrototype !== undefined && Reflect.apply(isPrototypeOf, shadowRootPrototype, [parent]) === true;
        if (!isShadowRoot || typeof shadowHost !== "function") return parent;
        const host: unknown = Reflect.apply(shadowHost, parent, []);
        return typeof host === "object" && host !== null ? host : null;
      },
    },
  };
}
