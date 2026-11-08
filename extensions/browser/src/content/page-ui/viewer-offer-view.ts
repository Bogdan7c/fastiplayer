// DOM предложения «Ask» поверх замершего видео (сессия 12b, решения 255–257). Панель ровно по прямоугольнику
// видео, по центру — вопрос, две главные кнопки («Play in Fastiplayer», «Play here») и строка помельче
// («Always for this site · Never for this site»). Рисуется в закрытой тени общего слоя (`ui-layer.ts`), как
// заглушка «Playing in Fastiplayer»: страница до кнопок не дотянется, а её скрипт не подделает клик.
//
// Только вид и положение; когда показывать и что делать по нажатию — решает `viewer-offer-controller.ts`.
// Доводка вида — на финальной полировке интерфейса (решение владельца: визуальная доводка UI отложена).
import type { PageUiRect } from "../../shared/entry-contract.ts";
import { VIEWER_OFFER_CHOICES, type ViewerOfferChoice } from "../../shared/viewer-offer-contract.ts";
import { PANEL_Z_INDEX, SWALLOWED_EVENT_TYPES, visibleRectOf, type LeaveShield } from "./placeholder-view.ts";
import { applyImportantStyles, type UiDocument, type UiElement, type UiEvent } from "./ui-dom.ts";
import type { UiLayer } from "./ui-layer.ts";

export const VIEWER_OFFER_QUESTION = "Play this video in Fastiplayer?";

/** Надписи кнопок по выбору. */
export const VIEWER_OFFER_BUTTON_LABELS: Readonly<Record<ViewerOfferChoice, string>> = {
  fastiplayer: "Play in Fastiplayer",
  here: "Play here",
  always: "Always for this site",
  never: "Never for this site",
};

/** Строка после выбора «в плеер»: плеер стартует до нескольких секунд, панель не должна выглядеть мёртвой. */
export const VIEWER_OFFER_OPENING_TEXT = "Opening in Fastiplayer…";

/**
 * Фон панели обязан быть НЕПРОЗРАЧНЫМ, как у заглушки «Playing in Fastiplayer». Firefox на нажатии мыши ищет
 * видео под курсором (`PictureInPictureChild`: `nodesFromRect(..., onlyVisible = true, порог видимости)`, порог
 * `media.videocontrols.picture-in-picture.video-toggle.visibility-threshold` = 1.0): перекрытием он считает только
 * полностью непрозрачное. Сквозь полупрозрачную панель он «видит» видео, и клик в зоне своей кнопки «Картинка в
 * картинке» (правый край видео) забирает себе раньше страницы — так стенд 12b терял «Never for this site».
 */
const PANEL_BACKGROUND = "#111114";

/** Что видно на экране (для стенда): панель и каждая кнопка; `null` — не видно. */
export interface ViewerOfferMeasure {
  readonly panel: PageUiRect | null;
  readonly buttons: Readonly<Record<ViewerOfferChoice, PageUiRect | null>> | null;
}

/** Состояние после нажатия: `asking` — кнопки живы; `answered` — выбор ушёл; `opening` — ещё и «Opening…». */
export type ViewerOfferViewState = "asking" | "answered" | "opening";

export interface ViewerOfferView {
  /**
   * Поставить панель на прямоугольник видео и показать. `onChoice` — каждое нажатие любой кнопки.
   * `no_layer` — нашего слоя в документе нет (документ без корня): панель не нарисовать.
   */
  show(videoRect: PageUiRect, onChoice: (choice: ViewerOfferChoice, event: UiEvent) => void): "shown" | "no_layer";
  hide(): void;
  /** Как у заглушки: панель становится невидимым щитом от запоздалого клика, пока контроллер не позовёт `hide()`. */
  shieldClicksWhileLeaving(): LeaveShield;
  raise(): void;
  setState(state: ViewerOfferViewState): void;
  measure(): ViewerOfferMeasure;
}

/** Собранная панель: сама панель, всё её видимое содержимое, кнопки по выбору, строка «Opening…». */
interface BuiltPanel {
  readonly panel: UiElement;
  readonly content: readonly UiElement[];
  readonly buttons: Readonly<Record<ViewerOfferChoice, UiElement>>;
  readonly opening: UiElement;
}

/** Главная кнопка (синяя), второстепенная (серая) и «ссылка» (текст без рамки). */
type ButtonLook = "primary" | "secondary" | "link";

const BUTTON_LOOKS: Readonly<Record<ViewerOfferChoice, ButtonLook>> = { fastiplayer: "primary", here: "secondary", always: "link", never: "link" };

function styleButton(button: UiElement, look: ButtonLook): void {
  const background = look === "primary" ? "#2d6cdf" : look === "secondary" ? "#3a3a42" : "transparent";
  applyImportantStyles(button, {
    all: "initial",
    display: "inline-block",
    padding: look === "link" ? "2px 4px" : "6px 16px",
    background,
    color: look === "link" ? "#c9d4ff" : "#ffffff",
    "border-radius": "14px",
    font: look === "link" ? "13px/1.2 system-ui, sans-serif" : "14px/1.2 system-ui, sans-serif",
    "text-decoration": look === "link" ? "underline" : "none",
    cursor: "pointer",
    "pointer-events": "auto",
  });
}

/** Ряд кнопок с переносом: на маленьком видео кнопки встают друг под другом, а не вылезают за панель. */
function createRow(document: UiDocument, children: readonly UiElement[]): UiElement {
  const row = document.createElement("div");
  applyImportantStyles(row, { display: "flex", "flex-wrap": "wrap", "justify-content": "center", gap: "8px" });
  row.append(...children);
  return row;
}

function buildPanel(document: UiDocument): BuiltPanel {
  const panel = document.createElement("div");
  panel.setAttribute("role", "dialog");
  applyImportantStyles(panel, {
    all: "initial",
    position: "fixed",
    "z-index": PANEL_Z_INDEX,
    display: "none",
    "box-sizing": "border-box",
    "flex-direction": "column",
    "align-items": "center",
    "justify-content": "center",
    gap: "10px",
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
  const question = document.createElement("div");
  question.textContent = VIEWER_OFFER_QUESTION;
  applyImportantStyles(question, { display: "block", "font-weight": "600" });
  const buttons = {} as Record<ViewerOfferChoice, UiElement>;
  for (const choice of VIEWER_OFFER_CHOICES) {
    const button = document.createElement("button");
    button.setAttribute("type", "button");
    button.textContent = VIEWER_OFFER_BUTTON_LABELS[choice];
    styleButton(button, BUTTON_LOOKS[choice]);
    buttons[choice] = button;
  }
  const mainRow = createRow(document, [buttons.fastiplayer, buttons.here]);
  const ruleRow = createRow(document, [buttons.always, buttons.never]);
  const opening = document.createElement("div");
  opening.textContent = VIEWER_OFFER_OPENING_TEXT;
  applyImportantStyles(opening, { display: "none", "font-size": "13px", opacity: "0.85" });
  panel.append(question, mainRow, ruleRow, opening);
  const swallow = (event: UiEvent): void => {
    event.stopPropagation();
    event.stopImmediatePropagation();
  };
  for (const type of SWALLOWED_EVENT_TYPES) panel.addEventListener(type, swallow);
  return { panel, content: [question, mainRow, ruleRow, opening], buttons, opening };
}

export function createViewerOfferView(document: UiDocument, layer: UiLayer): ViewerOfferView {
  let built: BuiltPanel | null = null;
  let visible = false;
  let shielding = false;
  let choiceListener: ((choice: ViewerOfferChoice, event: UiEvent) => void) | null = null;

  function setContentVisible(contentVisible: boolean): void {
    if (built === null) return;
    for (const element of built.content) applyImportantStyles(element, { visibility: contentVisible ? "visible" : "hidden" });
  }

  function leaveShield(): void {
    if (built === null || !shielding) return;
    shielding = false;
    applyImportantStyles(built.panel, { background: PANEL_BACKGROUND });
    setContentVisible(true);
  }

  /** Панель строится один раз; нажатия кнопок идут в текущий `choiceListener` (его меняет `show`). */
  function ensureBuilt(): BuiltPanel | null {
    if (built !== null) return built;
    const shadow = layer.shadowRoot();
    if (shadow === null) return null;
    const created = buildPanel(document);
    for (const choice of VIEWER_OFFER_CHOICES) created.buttons[choice].addEventListener("click", (event) => choiceListener?.(choice, event));
    shadow.append(created.panel);
    built = created;
    return created;
  }

  return {
    show(videoRect, onChoice) {
      const panel = ensureBuilt();
      if (panel === null) return "no_layer";
      leaveShield();
      choiceListener = onChoice;
      applyImportantStyles(panel.panel, {
        display: "flex",
        left: `${videoRect.left}px`,
        top: `${videoRect.top}px`,
        width: `${videoRect.width}px`,
        height: `${videoRect.height}px`,
      });
      if (!visible) layer.raise();
      visible = true;
      return "shown";
    },
    hide() {
      visible = false;
      if (built === null) return;
      applyImportantStyles(built.panel, { display: "none" });
      leaveShield();
    },
    shieldClicksWhileLeaving() {
      if (built === null || !visible) return "nothing_shown";
      visible = false;
      shielding = true;
      applyImportantStyles(built.panel, { background: "transparent" });
      setContentVisible(false);
      return "shielding";
    },
    raise() {
      if (visible) layer.raise();
    },
    setState(state) {
      if (built === null) return;
      const asking = state === "asking";
      for (const choice of VIEWER_OFFER_CHOICES) {
        const button = built.buttons[choice];
        button.setAttribute("aria-disabled", asking ? "false" : "true");
        applyImportantStyles(button, { opacity: asking ? "1" : "0.5", cursor: asking ? "pointer" : "default" });
      }
      applyImportantStyles(built.opening, { display: state === "opening" ? "block" : "none" });
    },
    measure() {
      if (built === null || shielding) return { panel: null, buttons: null };
      const panel = visibleRectOf(built.panel);
      if (panel === null) return { panel: null, buttons: null };
      const buttons = built.buttons;
      return {
        panel,
        buttons: { fastiplayer: visibleRectOf(buttons.fastiplayer), here: visibleRectOf(buttons.here), always: visibleRectOf(buttons.always), never: visibleRectOf(buttons.never) },
      };
    },
  };
}
