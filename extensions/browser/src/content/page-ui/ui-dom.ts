// Узкий срез DOM, нужный слою страницы экстеншена (плашка и кнопка поверх видео). Логика слоя пишется
// против этих интерфейсов, а не против настоящего `HTMLElement`: так её можно проверить фейковыми элементами
// в node:test без браузера. Настоящий документ подключается одним переходником (`real-ui-document.ts`).

import type { PageUiRect } from "../../shared/entry-contract.ts";
import type { PageScrollBlock } from "./wheel-pass-through.ts";

/** Событие, как его видит наш код: только то, что нам нужно прочитать или остановить. */
export interface UiEvent {
  /** `true` только у событий, рождённых настоящим действием человека (скрипт страницы подделать не может). */
  readonly isTrusted: boolean;
  readonly target: unknown;
  stopPropagation(): void;
  stopImmediatePropagation(): void;
  preventDefault(): void;
}

/** Событие колеса мыши: где курсор и на сколько крутят (единицы — `deltaMode` стандарта DOM). */
export interface UiWheelEvent extends UiEvent {
  readonly clientX: number;
  readonly clientY: number;
  readonly deltaX: number;
  readonly deltaY: number;
  readonly deltaMode: number;
}

export type UiEventListener = (event: UiEvent) => void;

/** Настройки слушателя: `passive: false` — слушатель вправе погасить родное действие (`preventDefault`). */
export interface UiListenerOptions {
  readonly passive: boolean;
}

/** Элемент слоя страницы. */
export interface UiElement {
  /** Инлайновый стиль (через CSSOM: страничный CSP его не блокирует, в отличие от `<style>`). */
  readonly style: { setProperty(name: string, value: string, priority?: string): void };
  textContent: string | null;
  readonly isConnected: boolean;
  setAttribute(name: string, value: string): void;
  append(...children: UiElement[]): void;
  replaceChildren(...children: UiElement[]): void;
  remove(): void;
  addEventListener(type: string, listener: UiEventListener, options?: UiListenerOptions): void;
  removeEventListener(type: string, listener: UiEventListener): void;
  getBoundingClientRect(): PageUiRect;
}

/** Закрытый shadow-корень: ссылка на него живёт только в изолированном мире. */
export interface UiShadowRoot {
  append(...children: UiElement[]): void;
}

/** Хост слоя: неопределённый custom-тег с закрытой тенью и (если браузер умеет) popover. */
export interface UiHostElement extends UiElement {
  attachShadow(init: { readonly mode: "closed" }): UiShadowRoot;
  /** Нет у браузеров без Popover API: тогда слой держится на `position: fixed`. */
  showPopover?(): void;
  hidePopover?(): void;
}

/** Корень документа, куда добавляется хост. */
export interface UiDocumentRoot {
  append(...children: UiElement[]): void;
}

/** Документ как фабрика элементов. */
export interface UiDocument {
  /** `null`, пока документ ещё не построил корневой элемент (document_start). */
  documentRoot(): UiDocumentRoot | null;
  createHost(tagName: string): UiHostElement;
  createElement(tagName: string): UiElement;
  /** Маленькая иконка «играть» (инлайновый SVG: `web_accessible_resources` не нужен). */
  createPlayIcon(): UiElement;
  /**
   * Блок СТРАНИЦЫ под точкой окна, если убрать наш слой `layerHost` (колесо над заглушкой, 16b2); `null` —
   * под точкой нет ничего, кроме нашего слоя. Откуда колесо поедет дальше, решает `wheel-pass-through.ts`.
   */
  pageScrollBlockUnder(clientX: number, clientY: number, layerHost: UiHostElement | null): PageScrollBlock | null;
}

/** Задать набор инлайновых свойств с `!important`: стили сайта (`div { ... !important }`) их не переопределят. */
export function applyImportantStyles(element: UiElement, styles: Readonly<Record<string, string>>): void {
  for (const [name, value] of Object.entries(styles)) element.style.setProperty(name, value, "important");
}
