// DOM заглушки «Playing in Fastiplayer» поверх переданного видео (сессия 11a, решение 236): непрозрачная
// тёмная панель ровно по прямоугольнику видео, по центру — текст и кнопка «Return». Рисуется в закрытой тени
// общего слоя (`ui-layer.ts`). Только вид и положение; когда показывать и что делать по нажатию — решает
// `placeholder-controller.ts`.
import type { PageUiRect } from "../../shared/entry-contract.ts";
import { applyImportantStyles, type UiDocument, type UiElement, type UiEvent } from "./ui-dom.ts";
import type { UiLayer } from "./ui-layer.ts";

export const PLACEHOLDER_TEXT = "Playing in Fastiplayer";
export const RETURN_BUTTON_LABEL = "Return";

/**
 * События, которые панель гасит у себя: клик по заглушке не должен дойти до страницы, иначе её плеер решил
 * бы, что нажали на видео (пауза, полноэкранный режим, реклама). `wheel` НЕ гасится: колесо над заглушкой
 * обязано прокручивать страницу. Слушатели захвата страницы на `window`/`document` браузер зовёт раньше нас —
 * их не остановить, но цель у них — наш хост, а не видео.
 */
export const SWALLOWED_EVENT_TYPES = ["pointerdown", "pointerup", "mousedown", "mouseup", "click", "auxclick", "dblclick", "contextmenu", "touchstart", "touchend"] as const;

/**
 * Панель — ниже плашки и кнопки «Play in Fastiplayer» в том же слое: если видео стоит в правом верхнем углу,
 * плашка итога входа должна оставаться видна поверх заглушки. Отрицательный z-index в контексте наложения
 * хоста кладёт панель под соседей по тени, но всё равно поверх страницы (хост в верхнем слое).
 */
export const PANEL_Z_INDEX = "-1";

/** Непрозрачный фон панели: под заглушкой стоит замерший кадр тени, показывать его рядом с окном плеера незачем. */
const PANEL_BACKGROUND = "#111114";

/**
 * Что сделал `shieldClicksWhileLeaving`: `"shielding"` — панель была на экране и стала невидимым щитом;
 * `"nothing_shown"` — показано ничего не было, щит не нужен (прятать нечего, ждать нечего).
 */
export type LeaveShield = "shielding" | "nothing_shown";

/** Вид заглушки, каким его видит контроллер. */
export interface PlaceholderView {
  /** Поставить панель ровно на прямоугольник видео и показать. `onReturnClicked` — каждое нажатие «Return». */
  show(videoRect: PageUiRect, onReturnClicked: (event: UiEvent) => void): void;
  hide(): void;
  /**
   * Передача кончилась, а человек, может быть, уже жмёт «Return»: панель становится невидимой, но клики
   * по месту видео по-прежнему гасит — пока контроллер не позовёт `hide()`. Иначе запоздалый клик попал
   * бы в родной плеер сайта, и тот сам запустил бы видео, которое должно стоять на паузе (решение 51).
   * Повторный `show()` возвращает обычный вид.
   */
  shieldClicksWhileLeaving(): LeaveShield;
  /** Поднять слой на верх верхнего слоя (после полноэкранного режима страницы). */
  raise(): void;
  /** Кнопка «Return» в состоянии «просьба ушла»: повторно не нажимается до следующей передачи. */
  markReturnRequested(requested: boolean): void;
  /** Что на экране сейчас; `null` — не видно. */
  measure(): { readonly panel: PageUiRect | null; readonly returnButton: PageUiRect | null };
}

export function createPlaceholderView(document: UiDocument, layer: UiLayer): PlaceholderView {
  let panel: UiElement | null = null;
  let content: readonly UiElement[] = [];
  let returnButton: UiElement | null = null;
  let visible = false;
  /** Панель на экране невидимым щитом (см. `shieldClicksWhileLeaving`). */
  let shielding = false;

  /** Показать или спрятать содержимое панели. `visibility` детей не трогает попадание кликов в саму панель. */
  function setContentVisible(contentVisible: boolean): void {
    for (const element of content) applyImportantStyles(element, { visibility: contentVisible ? "visible" : "hidden" });
  }
  let returnListener: ((event: UiEvent) => void) | null = null;

  function buildPanel(): { readonly panel: UiElement; readonly text: UiElement; readonly returnButton: UiElement } {
    const createdPanel = document.createElement("div");
    createdPanel.setAttribute("role", "status");
    applyImportantStyles(createdPanel, {
      all: "initial",
      position: "fixed",
      "z-index": PANEL_Z_INDEX,
      display: "none",
      "box-sizing": "border-box",
      "flex-direction": "column",
      "align-items": "center",
      "justify-content": "center",
      gap: "12px",
      padding: "8px",
      overflow: "hidden",
      background: PANEL_BACKGROUND,
      color: "#f4f4f4",
      font: "15px/1.3 system-ui, sans-serif",
      "text-align": "center",
      "user-select": "none",
      "pointer-events": "auto",
      cursor: "default",
    });
    const text = document.createElement("div");
    text.textContent = PLACEHOLDER_TEXT;
    applyImportantStyles(text, { display: "block", "font-weight": "600" });
    const button = document.createElement("button");
    button.setAttribute("type", "button");
    button.textContent = RETURN_BUTTON_LABEL;
    applyImportantStyles(button, {
      all: "initial",
      display: "block",
      padding: "6px 18px",
      background: "#2d6cdf",
      color: "#ffffff",
      "border-radius": "14px",
      font: "14px/1.2 system-ui, sans-serif",
      cursor: "pointer",
      "pointer-events": "auto",
    });
    createdPanel.append(text, button);
    const swallow = (event: UiEvent): void => {
      event.stopPropagation();
      event.stopImmediatePropagation();
    };
    for (const type of SWALLOWED_EVENT_TYPES) createdPanel.addEventListener(type, swallow);
    return { panel: createdPanel, text, returnButton: button };
  }

  return {
    show(videoRect, onReturnClicked) {
      const shadow = layer.shadowRoot();
      if (shadow === null) return;
      if (panel === null || returnButton === null) {
        const built = buildPanel();
        panel = built.panel;
        returnButton = built.returnButton;
        content = [built.text, built.returnButton];
        shadow.append(panel);
      }
      if (shielding) {
        // Новая передача пришла, пока стоял щит: возвращаем обычный вид.
        shielding = false;
        applyImportantStyles(panel, { background: PANEL_BACKGROUND });
        setContentVisible(true);
      }
      // Обработчик нажатия ставится на кнопку (а гашение — на панель): клик сначала доходит до кнопки,
      // потом до панели, где и останавливается.
      if (returnListener !== onReturnClicked) {
        if (returnListener !== null) returnButton.removeEventListener("click", returnListener);
        returnListener = onReturnClicked;
        returnButton.addEventListener("click", returnListener);
      }
      applyImportantStyles(panel, {
        display: "flex",
        left: `${videoRect.left}px`,
        top: `${videoRect.top}px`,
        width: `${videoRect.width}px`,
        height: `${videoRect.height}px`,
      });
      // Поднимаем только при появлении: снять и показать popover на каждый сдвиг — мерцание и потерянные клики.
      if (!visible) layer.raise();
      visible = true;
    },
    hide() {
      visible = false;
      // Заглушки ещё не было — слой не трогаем: без передачи в документе нет ни одного нашего узла.
      if (panel === null) return;
      applyImportantStyles(panel, { display: "none" });
      if (shielding) {
        // Следующий показ — снова обычная панель.
        shielding = false;
        applyImportantStyles(panel, { background: PANEL_BACKGROUND });
        setContentVisible(true);
      }
    },
    shieldClicksWhileLeaving() {
      if (panel === null || !visible) return "nothing_shown";
      // Для зонда и `raise()` заглушки больше нет; на экране — только прозрачный прямоугольник, гасящий клики.
      visible = false;
      shielding = true;
      applyImportantStyles(panel, { background: "transparent" });
      setContentVisible(false);
      return "shielding";
    },
    raise() {
      if (visible) layer.raise();
    },
    markReturnRequested(requested) {
      if (returnButton === null) return;
      returnButton.setAttribute("aria-disabled", requested ? "true" : "false");
      applyImportantStyles(returnButton, { opacity: requested ? "0.6" : "1", cursor: requested ? "default" : "pointer" });
    },
    measure() {
      // Щит невидим: для стенда заглушки на экране уже нет.
      if (shielding) return { panel: null, returnButton: null };
      return { panel: visibleRectOf(panel), returnButton: visibleRectOf(returnButton) };
    },
  };
}

/** Прямоугольник с экрана; скрытое (`display: none`) даёт нулевой — значит, не видно. */
export function visibleRectOf(element: UiElement | null): PageUiRect | null {
  if (element === null) return null;
  const rect = element.getBoundingClientRect();
  if (rect.width === 0 && rect.height === 0) return null;
  // Простой объект, а не DOMRect: поля DOMRect — геттеры прототипа, и ответ `executeScript` пришёл бы пустым.
  return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
}
