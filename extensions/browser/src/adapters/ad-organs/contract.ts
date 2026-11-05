// Контракт «органов рекламы» сайта (сессия 09d): что адаптер умеет сказать о рекламе на экране и как
// нажать кнопку пропуска. Контракт узкий намеренно: он войдёт в адаптеры сайтов сессии 14, и остальной
// код (наблюдение во фрейме, сводка в фоне) знает о сайте только то, что написано здесь.
//
// Инвариант 1 (страница — враг): наружу из читателя выходят только числа и метки `PageAdState`.
// Текст страницы дальше читателя не уходит.

import type { PageAdState } from "../../protocol/index.ts";
import type { PressSkipOutcome } from "../../shared/ad-watch-contract.ts";

export type { PressSkipOutcome };

/** Элемент страницы, как его видят читатели: единственное, что с ним можно сделать напрямую, — нажать. */
export interface AdElement {
  click(): void;
}

/** Прямоугольник элемента в координатах вьюпорта, CSS-пиксели. */
export interface ElementRect {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

/** Где находится фрейм: по этому адаптер решает, его ли это фрейм. */
export interface AdFramePlace {
  /** Имя хоста документа фрейма. */
  readonly hostname: string;
  /** Есть ли в документе элемент по селектору. */
  contains(selector: string): boolean;
}

/**
 * Узкий взгляд читателей на документ. Единственное место, где читатели касаются DOM: в тестах его
 * заменяет маленькая подделка, в продукте — `dom-view.ts`.
 */
export interface AdPageView extends AdFramePlace {
  /** Корень документа: за ним следим, когда своего корня у органов ещё нет. */
  documentRoot(): AdElement;
  /** Первый элемент по селектору (внутри `within`, если задан). */
  find(selector: string, within?: AdElement): AdElement | undefined;
  /** Все элементы по селектору. */
  findAll(selector: string, within?: AdElement): readonly AdElement[];
  /** Видимый текст элемента как есть: нормализует и проверяет его читатель. */
  textOf(element: AdElement): string;
  hasClass(element: AdElement, className: string): boolean;
  /** Есть ли у элемента дочерние ЭЛЕМЕНТЫ (текстовые узлы не в счёт): лист дерева — их нет. */
  hasChildElements(element: AdElement): boolean;
  /** Показан ли элемент: не скрыт сам и не скрыт предком (`display:none`, `visibility:hidden`). */
  isRendered(element: AdElement): boolean;
  rectOf(element: AdElement): ElementRect;
  /** Размер видимой области документа. */
  viewportSize(): { readonly width: number; readonly height: number };
}

/** Органы рекламы одного сайта или плеера. */
export interface AdOrgans {
  /** Имя семейства: только для тестов и журнала разработчика. */
  readonly family: "youtube" | "ima" | "playerjs";
  /** Эти органы возможны в этом фрейме? Дёшево; зовётся при каждом продлении аренды. */
  matches(place: AdFramePlace): boolean;
  /** За каким деревом следить мутациями: корень органов или, пока их нет, весь документ. */
  observationRoot(): AdElement;
  /**
   * Что сейчас с рекламой. Мусор со страницы (числа вне лимитов протокола, номер больше размера блока,
   * незнакомый текст) даёт `unknown` или `unavailable`, а не ошибку.
   */
  readAdState(): PageAdState;
  /** Нажать кнопку пропуска, именно её и только если сайт её сейчас показывает. */
  pressSkip(): PressSkipOutcome;
}
