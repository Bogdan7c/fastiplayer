// Логика кнопки поверх видео. DOM здесь нет — кнопку рисует `OverlayButtonView`, события приходят через
// `PointerSource`. Так логику можно проверять фейками.
//
// Состояние покоя: настройка выключена -> НИ ОДНОГО слушателя и ни одного узла (кнопка «спит»).
// Включена: пассивный `mousemove` на документе, запрос «видео под точкой» не чаще раза в `HOVER_POLL_MS`.
//
// Как не мигать и не врать:
//  - каждое движение мыши (вне нашей кнопки) повышает «версию точки»; ответ на запрос показывается, только
//    если за время ответа мышь не сдвинулась (устаревший ответ молча выбрасывается);
//  - с видео на нашу кнопку мышь переходит без скрытия: событие над кнопкой приходит с целью = наш хост;
//  - ушли с видео (ответ «ничего»), покинули окно, прокрутили страницу -> кнопка скрывается;
//  - видео уже в плеере (`in_player`) -> кнопки нет;
//  - нажатие принимается только от настоящего человека (`isTrusted`) и не доходит до страницы.
import type { OverlayButtonPressedMessage, PageUiRect, VideoAtPoint } from "../../shared/entry-contract.ts";
import { OVERLAY_BUTTON_PRESSED_KIND } from "../../shared/entry-contract.ts";
import type { UiEvent } from "./ui-dom.ts";

/** Не чаще одного вопроса к MAIN-миру за столько мс (против дребезга `mousemove`). */
export const HOVER_POLL_MS = 100;

/** Источник событий мыши документа; подписка возвращает функцию отписки. */
export interface PointerSource {
  /** Пассивный `mousemove`. */
  onMouseMove(listener: (event: PointerMoveEvent) => void): () => void;
  /** Мышь покинула окно документа или страница прокручена: прямоугольник видео устарел. */
  onPointerLost(listener: () => void): () => void;
}

export interface PointerMoveEvent {
  readonly x: number;
  readonly y: number;
  /** Цель события как её видит документ (над нашей тенью — наш хост: тень закрытая, цель ретаргетится). */
  readonly target: unknown;
}

/** Рисование кнопки. */
export interface OverlayButtonView {
  /** Показать/передвинуть кнопку к видео; `onPressed` зовётся на нажатие (любое, фильтр — в контроллере). */
  show(videoRect: PageUiRect, onPressed: (event: UiEvent) => void): void;
  hide(): void;
  /** Прямоугольник кнопки, какой он СЕЙЧАС на экране; `null` — не показана. */
  measure(): PageUiRect | null;
}

export interface OverlayButtonTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface OverlayButtonDependencies {
  readonly pointer: PointerSource;
  readonly view: OverlayButtonView;
  readonly timers: OverlayButtonTimers;
  readonly now: () => number;
  /** Цель события — наш собственный хост (мышь над нашей кнопкой). */
  readonly isOwnElement: (target: unknown) => boolean;
  /** Вопрос к MAIN-миру (через пограничник). Не отклоняется: «ничего»/таймаут = `null`. */
  readonly videoAtPoint: (x: number, y: number) => Promise<VideoAtPoint | null>;
  /** Донести нажатие фону. Отказ доставки обрабатывает вызывающий. */
  readonly sendPressed: (message: OverlayButtonPressedMessage) => Promise<unknown>;
  /** Отметка для разработчика о сбое доставки (не пользователю). */
  readonly note: (message: string, error: unknown) => void;
}

export interface OverlayButtonController {
  /** Включить/выключить кнопку (идемпотентно). Выключение снимает слушатели, таймер и кнопку. */
  setEnabled(enabled: boolean): void;
  /** Где кнопка на экране сейчас (для зонда стенда). */
  measureButton(): PageUiRect | null;
}

export function createOverlayButtonController(dependencies: OverlayButtonDependencies): OverlayButtonController {
  const { pointer, view, timers } = dependencies;
  let unsubscribe: (() => void)[] = [];
  /** Растёт с каждым движением мыши вне кнопки; устаревший ответ отличается версией. */
  let pointVersion = 0;
  let lastPollAt = Number.NEGATIVE_INFINITY;
  let pendingPoll: unknown = null;
  let latestPoint: { x: number; y: number } | null = null;
  /** Видео, над которым кнопка показана сейчас; её нажатие относится к нему. */
  let current: VideoAtPoint | null = null;

  function hideButton(): void {
    current = null;
    view.hide();
  }

  function cancelPendingPoll(): void {
    if (pendingPoll === null) return;
    timers.clearTimeout(pendingPoll);
    pendingPoll = null;
  }

  async function poll(): Promise<void> {
    const point = latestPoint;
    if (point === null) return;
    lastPollAt = dependencies.now();
    const versionAtRequest = pointVersion;
    const answer = await dependencies.videoAtPoint(point.x, point.y);
    // Выключили настройку или мышь сдвинулась, пока шёл ответ: он про точку, которой уже нет.
    if (unsubscribe.length === 0 || versionAtRequest !== pointVersion) return;
    if (answer === null || answer.in_player) {
      hideButton();
      return;
    }
    current = answer;
    view.show(answer.rect, handlePress);
  }

  /** Опрос не должен давать необработанное отклонение, что бы ни случилось в показе кнопки. */
  function guardedPoll(): Promise<void> {
    return poll().catch((error: unknown) => dependencies.note("overlay button poll failed", error));
  }

  function handleMove(event: PointerMoveEvent): void {
    // Над своей кнопкой: ничего не спрашиваем и не скрываем — человек идёт нажать.
    if (dependencies.isOwnElement(event.target)) return;
    pointVersion += 1;
    latestPoint = { x: event.x, y: event.y };
    if (pendingPoll !== null) return; // уже ждём «хвостовой» опрос: он возьмёт самую свежую точку
    const sinceLastPoll = dependencies.now() - lastPollAt;
    if (sinceLastPoll >= HOVER_POLL_MS) {
      void guardedPoll();
      return;
    }
    pendingPoll = timers.setTimeout(() => {
      pendingPoll = null;
      void guardedPoll();
    }, HOVER_POLL_MS - sinceLastPoll);
  }

  function handlePointerLost(): void {
    pointVersion += 1; // ответ на вопрос, заданный до потери, уже не про это положение
    cancelPendingPoll();
    hideButton();
  }

  function handlePress(event: UiEvent): void {
    // Страница не должна получить наше нажатие ни при каких условиях (иначе клик по «видео» запустит её плеер).
    event.stopPropagation();
    event.stopImmediatePropagation();
    event.preventDefault();
    // Поддельное событие (скрипт страницы не достанет закрытую тень, но эта проверка дешёвая) — игнор.
    if (!event.isTrusted || current === null) return;
    const pressed: OverlayButtonPressedMessage = { kind: OVERLAY_BUTTON_PRESSED_KIND, document: current.document, element: current.element };
    hideButton(); // двойное нажатие не должно слать второй раз
    pointVersion += 1; // летящий ответ про прежнюю точку не должен вернуть кнопку и дать второй вход
    dependencies.sendPressed(pressed).catch((error: unknown) => dependencies.note("overlay button press was not delivered", error));
  }

  /** Подписка атомарна: сбой второй части снимает первую, иначе следующее включение повесило бы дубль. */
  function start(): void {
    const added: (() => void)[] = [];
    try {
      added.push(pointer.onMouseMove(handleMove));
      added.push(pointer.onPointerLost(handlePointerLost));
    } catch (error) {
      for (const remove of added) remove();
      dependencies.note("overlay button listeners were not installed", error);
      return;
    }
    unsubscribe = added;
  }

  function stop(): void {
    for (const remove of unsubscribe) remove();
    unsubscribe = [];
    pointVersion += 1; // ответ, летящий в момент выключения, будет выброшен
    cancelPendingPoll();
    latestPoint = null;
    lastPollAt = Number.NEGATIVE_INFINITY;
    hideButton();
  }

  return {
    setEnabled(enabled) {
      const isEnabled = unsubscribe.length > 0;
      if (enabled === isEnabled) return;
      if (enabled) start();
      else stop();
    },
    measureButton: () => view.measure(),
  };
}
