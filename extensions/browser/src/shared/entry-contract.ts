// Договор «входов» режима 1 (сессия 10c): правый клик по видео и по ссылке, кнопка поверх видео, плашка с
// итогом на странице. Все входы сводятся к одной команде фона `handOffTab`; здесь только форма сообщений
// между мирами и имена функций изолированного мира. Поведения в файле нет.
//
//   MAIN (src/page/pointer-video.ts)  ⇄  изолированный мир (src/content)  ⇄  фон (src/background/entries)
//        участок 1: личный порт            участок 2: runtime.sendMessage / executeScript

import type { DocumentTag, ElementKey } from "./handoff-contract.ts";

/**
 * Глобал ИЗОЛИРОВАННОГО мира для `executeScript` фона: «видео под последним настоящим правым кликом».
 * Возвращает `Promise<RightClickedVideo | null>`. Странице функция не видна.
 */
export const RIGHT_CLICKED_VIDEO_FUNCTION_NAME = "fastiplayerBridgeRightClickedVideo";

/** Глобал изолированного мира только для чтения: что сейчас показывает слой страницы экстеншена (для стенда). */
export const PAGE_UI_PROBE_FUNCTION_NAME = "fastiplayerBridgePageUiProbe";

// ---------------------------------------------------------------------------------------------
// MAIN <-> изолированный мир (участок 1)
// ---------------------------------------------------------------------------------------------

/** Прямоугольник в CSS-пикселях вьюпорта своего фрейма. */
export interface PageUiRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

/** Видео без привязки к фрейму: фрейм знает фон (`sender.frameId` или `info.frameId`). */
export interface RightClickedVideo {
  readonly document: DocumentTag;
  readonly element: ElementKey;
}

/** Что изолированный мир видит под точкой (ответ MAIN, проверенный пограничником). */
export interface VideoAtPoint {
  readonly document: DocumentTag;
  readonly element: ElementKey;
  readonly rect: PageUiRect;
  /** Это видео сейчас под тенью у контроллера передачи (уже играет в плеере). */
  readonly in_player: boolean;
}

/** Изолированный мир -> MAIN: «какое видео под последним правым кликом». */
export interface DescribeRightClickedQuery {
  readonly kind: "describe_right_clicked";
  readonly request_id: number;
}

/** Изолированный мир -> MAIN: «какое видео под точкой (x, y) вьюпорта». */
export interface VideoAtPointQuery {
  readonly kind: "video_at_point";
  readonly request_id: number;
  readonly x: number;
  readonly y: number;
}

/** MAIN -> изолированный мир: ответ на `describe_right_clicked`; `null` — под кликом видео нет. */
export interface RightClickedReply {
  readonly kind: "right_clicked";
  readonly request_id: number;
  readonly video: RightClickedVideo | null;
}

/** MAIN -> изолированный мир: ответ на `video_at_point`; `null` — под точкой видео нет. */
export interface VideoAtPointReply {
  readonly kind: "video_at_point_result";
  readonly request_id: number;
  readonly video: VideoAtPoint | null;
}

/** Запросы к MAIN-миру про указатель. */
export type PointerQuery = DescribeRightClickedQuery | VideoAtPointQuery;

/** Ответы MAIN-мира про указатель. */
export type PointerReply = RightClickedReply | VideoAtPointReply;

// ---------------------------------------------------------------------------------------------
// Фон <-> контент-скрипт
// ---------------------------------------------------------------------------------------------

export const SHOW_ENTRY_RESULT_KIND = "show_entry_result";
export const OVERLAY_BUTTON_PRESSED_KIND = "overlay_button_pressed";

/** Итог входа для человека: плашка на странице. Тексты — английские, готовые. */
export interface EntryReport {
  /** Стабильное имя случая (как `NoticeView.id`): стенд и тесты читают его, не разбирая текст. */
  readonly id: string;
  readonly tone: "success" | "refusal";
  readonly headline: string;
  readonly details: readonly string[];
}

/** Фон -> контент-скрипт верхнего фрейма: показать плашку. */
export interface ShowEntryResultMessage {
  readonly kind: typeof SHOW_ENTRY_RESULT_KIND;
  readonly report: EntryReport;
}

/** Контент-скрипт -> фон: человек нажал кнопку поверх видео. */
export interface OverlayButtonPressedMessage {
  readonly kind: typeof OVERLAY_BUTTON_PRESSED_KIND;
  readonly document: DocumentTag;
  readonly element: ElementKey;
}

/** Что слой страницы экстеншена показывает сейчас (только чтение, для стенда). */
export interface PageUiProbe {
  /** Имя случая показанной плашки; `null` — плашки нет. */
  readonly banner: string | null;
  /** Прямоугольник кнопки в координатах вьюпорта; `null` — кнопки нет. */
  readonly overlay_button: PageUiRect | null;
}
