// Переходник: настоящий элемент страницы -> узкий `PageScrollBlock` для колеса над заглушкой
// (`wheel-pass-through.ts`). Здесь только чтения раскладки и одна команда прокрутки; какое колесо кому
// достаётся, решает чистое правило, а не этот файл.
//
// Документ и чтение стилей приходят параметрами: в браузере это `document` и `getComputedStyle`, в тестах —
// простые объекты той же формы.
import type { PageScrollBlock, ScrollMotion } from "./wheel-pass-through.ts";

/** Значения `overflow`, при которых блок листается колесом (`hidden` листается только из кода страницы). */
const WHEEL_SCROLLABLE_OVERFLOW: readonly string[] = ["auto", "scroll", "overlay"];

/** Стили блока, которые читает поиск. */
export interface ScrollBlockStyle {
  readonly overflowX: string;
  readonly overflowY: string;
  readonly scrollSnapType: string;
}

/** Срез настоящего `Element`, нужный поиску (настоящий элемент подходит как есть). */
export interface ScrollBlockElement {
  readonly assignedSlot: ScrollBlockElement | null;
  readonly parentElement: ScrollBlockElement | null;
  /** Корень дерева элемента: у теневого корня есть `host`, у документа — нет. */
  getRootNode(): { readonly host?: ScrollBlockElement | null };
  readonly scrollTop: number;
  readonly scrollLeft: number;
  readonly scrollHeight: number;
  readonly scrollWidth: number;
  readonly clientHeight: number;
  readonly clientWidth: number;
  scrollBy(options: { left: number; top: number; behavior: ScrollMotion }): void;
}

/** Срез настоящего `Document` и чтение вычисленного стиля. */
export interface ScrollBlockPage {
  readonly documentElement: ScrollBlockElement | null;
  readonly scrollingElement: ScrollBlockElement | null;
  readonly body: ScrollBlockElement | null;
  /** Элементы под точкой окна, сверху вниз (`Document.elementsFromPoint`). */
  elementsFromPoint(clientX: number, clientY: number): readonly ScrollBlockElement[];
  styleOf(element: ScrollBlockElement): ScrollBlockStyle;
}

/** Предок в плоском дереве: слот, обычный родитель или хозяин теневого корня. */
function flatTreeParentOf(element: ScrollBlockElement): ScrollBlockElement | null {
  if (element.assignedSlot !== null) return element.assignedSlot;
  if (element.parentElement !== null) return element.parentElement;
  return element.getRootNode().host ?? null;
}

/**
 * Элемент — это прокрутка самого документа: её ведёт браузер. Корень документа — всегда; `body` — когда у
 * корня `overflow: visible`: тогда по правилу CSS `overflow` у `body` относится к окну, а не к самому `body`
 * (страницы с `body { height: 100%; overflow: auto }`), и прокрутка `body` из кода ничего бы не сдвинула.
 */
function isDocumentScroller(element: ScrollBlockElement, page: ScrollBlockPage): boolean {
  const root = page.documentElement;
  if (element === root || element === page.scrollingElement) return true;
  if (element !== page.body || root === null) return false;
  const rootStyle = page.styleOf(root);
  return rootStyle.overflowX === "visible" && rootStyle.overflowY === "visible";
}

function blockOf(element: ScrollBlockElement, page: ScrollBlockPage): PageScrollBlock {
  // Стиль читается по требованию и один раз: поиск блока идёт на каждое событие колеса.
  let style: ScrollBlockStyle | null = null;
  const computed = (): ScrollBlockStyle => (style ??= page.styleOf(element));
  return {
    get parent() {
      const parent = flatTreeParentOf(element);
      return parent === null ? null : blockOf(parent, page);
    },
    get isDocumentScroller() {
      return isDocumentScroller(element, page);
    },
    get scrollsVertically() {
      return WHEEL_SCROLLABLE_OVERFLOW.includes(computed().overflowY);
    },
    get scrollsHorizontally() {
      return WHEEL_SCROLLABLE_OVERFLOW.includes(computed().overflowX);
    },
    get scrollTop() {
      return element.scrollTop;
    },
    get scrollLeft() {
      return element.scrollLeft;
    },
    get maxScrollTop() {
      return element.scrollHeight - element.clientHeight;
    },
    get maxScrollLeft() {
      return element.scrollWidth - element.clientWidth;
    },
    get clientHeight() {
      return element.clientHeight;
    },
    get clientWidth() {
      return element.clientWidth;
    },
    get snapsToPositions() {
      const snapType = computed().scrollSnapType;
      return snapType !== "" && snapType !== "none";
    },
    scrollBy(step: { readonly left: number; readonly top: number; readonly motion: ScrollMotion }): void {
      element.scrollBy({ left: step.left, top: step.top, behavior: step.motion });
    },
  };
}

/**
 * Блок страницы под точкой окна, не считая нашего слоя `layerHost` (заглушка лежит в его закрытой тени, и
 * снаружи под точкой виден сам хост). `null` — под точкой нет ничего, кроме слоя.
 */
export function pageScrollBlockUnder(page: ScrollBlockPage, clientX: number, clientY: number, layerHost: unknown): PageScrollBlock | null {
  const under = page.elementsFromPoint(clientX, clientY).find((element) => element !== layerHost);
  return under === undefined ? null : blockOf(under, page);
}

/** Настоящий документ как `ScrollBlockPage`. */
export function scrollBlockPageOf(document: Document): ScrollBlockPage {
  return {
    documentElement: document.documentElement as unknown as ScrollBlockElement | null,
    scrollingElement: document.scrollingElement as unknown as ScrollBlockElement | null,
    body: document.body as unknown as ScrollBlockElement | null,
    elementsFromPoint: (clientX, clientY) => document.elementsFromPoint(clientX, clientY) as unknown as ScrollBlockElement[],
    styleOf: (element) => getComputedStyle(element as unknown as Element),
  };
}
