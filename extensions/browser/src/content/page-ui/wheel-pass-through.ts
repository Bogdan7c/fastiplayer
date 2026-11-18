// Колесо мыши над заглушкой «Playing in Fastiplayer» листает страницу так же, как листало бы без заглушки
// (16b2, находка живой проверки Instagram, решение 361).
//
// Почему этого не делает сам браузер. Заглушка живёт в нашем хосте в верхнем слое, прямо под `<html>`. Колесо
// браузер отдаёт цепочке предков того элемента, что под курсором, — у заглушки это только документ. Если
// страницу листает сам документ (обычный сайт, YouTube), всё работает само. Но лента Instagram листается
// внутри блока (`overflow-y: auto` + `scroll-snap-type: y mandatory`), документ у неё неподвижен: колесо над
// заглушкой уходило в документ и ленту не двигало (живой замер: цель события — наш хост, блок ленты не
// получил ни одного `scroll`). Мимо ролика то же колесо листало ленту.
//
// Что делаем. По колесу над заглушкой находим элемент страницы под курсором (под нашим слоем) и идём от него
// вверх по предкам, как это сделал бы браузер: первый блок, который можно прокрутить в сторону колеса, и
// получает прокрутку. Дошли до прокрутки самого документа, не встретив такого блока, — ничего не трогаем:
// документ браузер листает сам, родной плавной прокруткой. Гасим родное действие колеса только когда
// прокрутили блок сами, иначе вместе с блоком поехал бы и документ.
//
// Клики это не открывает (решения 236, 239): заглушка по-прежнему ловит указатель, странице уходит только
// прокрутка — событие мыши до её элементов не доходит.
//
// Блок с прилипанием прокручиваем плавно. Рывок колеса — несколько щелчков за ~0,1 с; браузер для
// `scrollBy` выбирает точку прилипания ВПЕРЕДИ по ходу прокрутки (проверено стендом в Chromium и Firefox,
// строка `feed-halfway-over-plate`: назад к прежнему ролику блок не возвращается), и мгновенная прокрутка
// перелистнула бы по ролику на каждый щелчок. Плавная ещё идёт к следующему ролику, когда приходят остальные
// щелчки рывка, и они ведут к той же точке — рывок листает один ролик, как родное колесо. Блок без
// прилипания двигаем сразу и ровно на величину колеса: там сумма щелчков и есть нужное расстояние.

import type { UiWheelEvent } from "./ui-dom.ts";

/** Единицы `WheelEvent.deltaMode` (стандарт DOM). */
export const WHEEL_DELTA_MODE = { pixel: 0, line: 1, page: 2 } as const;

/**
 * Сколько пикселей в одной «строке» колеса. Живой замер Firefox 160: один щелчок — три строки — 144 px.
 * Нужна редко: браузеры отдают колесо в пикселях, если читать величину раньше единиц (см. `wheelStepOf`).
 */
export const WHEEL_LINE_PX = 48;

/** Меньше этого остатка блок считается докрученным до края (дробные позиции прокрутки), px. */
const SCROLL_EDGE_TOLERANCE_PX = 1;

/** Как двигать блок: плавно (блок с прилипанием) или сразу на всю величину. */
export type ScrollMotion = "smooth" | "instant";

/** Блок страницы, каким его видит поиск «кого прокрутит колесо»: только факты раскладки и одна команда. */
export interface PageScrollBlock {
  /** Предок в плоском дереве (для детей теневого корня — его хозяин); `null` — выше никого. */
  readonly parent: PageScrollBlock | null;
  /** Это прокрутка самого документа: её ведёт браузер, мы не вмешиваемся. */
  readonly isDocumentScroller: boolean;
  /** Блок прокручивается колесом по вертикали / горизонтали (`overflow: auto | scroll`). */
  readonly scrollsVertically: boolean;
  readonly scrollsHorizontally: boolean;
  /** Где блок стоит и докуда может дойти, px. */
  readonly scrollTop: number;
  readonly scrollLeft: number;
  readonly maxScrollTop: number;
  readonly maxScrollLeft: number;
  /** Размер видимой части блока, px (шаг колеса в «страницах»). */
  readonly clientHeight: number;
  readonly clientWidth: number;
  /** У блока включено прилипание прокрутки (`scroll-snap-type` не `none`). */
  readonly snapsToPositions: boolean;
  scrollBy(step: { readonly left: number; readonly top: number; readonly motion: ScrollMotion }): void;
}

/** Что стало с колесом над заглушкой. */
export type WheelPassOutcome =
  /** Блок страницы под заглушкой прокручен нами, родное действие колеса погашено. */
  | "page_block_scrolled"
  /** Прокручивать нечего или листается сам документ: колесо оставлено браузеру. */
  | "left_to_browser";

/** Есть ли у блока куда ехать в сторону колеса хотя бы по одной оси. */
function hasRoomFor(block: PageScrollBlock, deltaX: number, deltaY: number): boolean {
  const roomDown = block.maxScrollTop - block.scrollTop > SCROLL_EDGE_TOLERANCE_PX;
  const roomUp = block.scrollTop > SCROLL_EDGE_TOLERANCE_PX;
  const roomRight = block.maxScrollLeft - block.scrollLeft > SCROLL_EDGE_TOLERANCE_PX;
  const roomLeft = block.scrollLeft > SCROLL_EDGE_TOLERANCE_PX;
  const vertical = block.scrollsVertically && ((deltaY > 0 && roomDown) || (deltaY < 0 && roomUp));
  const horizontal = block.scrollsHorizontally && ((deltaX > 0 && roomRight) || (deltaX < 0 && roomLeft));
  return vertical || horizontal;
}

/**
 * Блок, который прокрутило бы колесо, не будь заглушки: первый от элемента под курсором вверх, у кого есть
 * куда ехать. `null` — такого блока нет до самой прокрутки документа: колесо достаётся браузеру.
 */
export function wheelTargetBlock(under: PageScrollBlock | null, deltaX: number, deltaY: number): PageScrollBlock | null {
  for (let block = under; block !== null; block = block.parent) {
    if (block.isDocumentScroller) return null;
    if (hasRoomFor(block, deltaX, deltaY)) return block;
  }
  return null;
}

/** Величина колеса в пикселях блока. */
function wheelStepOf(event: UiWheelEvent, block: Pick<PageScrollBlock, "clientHeight" | "clientWidth">, deltaX: number, deltaY: number): { left: number; top: number } {
  switch (event.deltaMode) {
    case WHEEL_DELTA_MODE.line:
      return { left: deltaX * WHEEL_LINE_PX, top: deltaY * WHEEL_LINE_PX };
    case WHEEL_DELTA_MODE.page:
      return { left: deltaX * block.clientWidth, top: deltaY * block.clientHeight };
    default:
      return { left: deltaX, top: deltaY };
  }
}

/**
 * Колесо над заглушкой — странице. `blockUnder` находит блок страницы под точкой события (под нашим слоем).
 * Поддельное колесо (скрипт страницы) ничего не листает: прокрутка — действие человека.
 */
export function passWheelToPage(event: UiWheelEvent, blockUnder: (clientX: number, clientY: number) => PageScrollBlock | null): WheelPassOutcome {
  if (!event.isTrusted) return "left_to_browser";
  // Величина читается РАНЬШЕ единиц: Firefox в этом случае отдаёт её в пикселях (совместимость со старыми
  // страницами), а при обратном порядке — в строках.
  const deltaX = event.deltaX;
  const deltaY = event.deltaY;
  const block = wheelTargetBlock(blockUnder(event.clientX, event.clientY), deltaX, deltaY);
  if (block === null) return "left_to_browser";
  // Блок листаем сами — документ под ним от этого же колеса ехать не должен.
  event.preventDefault();
  const step = wheelStepOf(event, block, deltaX, deltaY);
  block.scrollBy({ ...step, motion: block.snapsToPositions ? "smooth" : "instant" });
  return "page_block_scrolled";
}
