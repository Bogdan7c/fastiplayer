// Где ставить кнопку: правый верхний угол видео с отступом, но не за краем вьюпорта (видео бывает больше
// окна или частично прокручено — кнопка всё равно должна быть видна). Чистая арифметика, без DOM.
import type { PageUiRect } from "../../shared/entry-contract.ts";

/** Отступ кнопки от края видео, CSS-пикселей. */
export const BUTTON_MARGIN_PX = 8;

export interface ViewportSize {
  readonly width: number;
  readonly height: number;
}

/** Точка привязки: правый верхний угол кнопки (сама кнопка растёт влево-вниз от неё). */
export interface ButtonAnchor {
  readonly right: number;
  readonly top: number;
}

export function placeButtonAnchor(videoRect: PageUiRect, viewport: ViewportSize): ButtonAnchor {
  const videoRight = videoRect.left + videoRect.width;
  const visibleRight = Math.min(videoRight, viewport.width);
  const visibleTop = Math.max(videoRect.top, 0);
  return { right: visibleRight - BUTTON_MARGIN_PX, top: visibleTop + BUTTON_MARGIN_PX };
}
