// Шов окружения тени (A1↔A2): всё внешнее подаётся контроллеру снаружи, чтобы тесты шли на фейках.
// Вынесено из `shadow-session.ts` без изменений смысла — только типы, поведения здесь нет.

import type { MediaSessionGuard } from "../media-session-guard.ts";
import type { TimeoutScheduler } from "./file-return.ts";
import type { ElementOriginals } from "./originals.ts";
import type { MediaSourceObjectUrlCheck } from "./source-reset-policy.ts";

/** Когда снимать файл с настоящего элемента: сразу или когда фон взял задержанное видео. */
export type FileStopMoment = "stop_file_now" | "keep_file_until_commit";

/** Окружение тени (шов A1↔A2): всё внешнее подаётся снаружи, чтобы тесты шли на фейках. */
export interface ShadowEnvironment {
  readonly win: Window;
  readonly originals: ElementOriginals;
  readonly mediaSession: MediaSessionGuard;
  /**
   * `performance.now` (мс). Своих таймеров тень не заводит: подтягивание и проверки идут на тиках
   * часов плеера (таймеры скрытой вкладки Chromium будит раз в минуту).
   */
  readonly now: () => number;
  /**
   * Адрес — blob-адрес MediaSource этого документа (точный ответ реестра MAIN-мира, A1). Нужен,
   * чтобы отличить новое поколение на том же элементе от blob-адреса обычного файла.
   */
  readonly isMediaSourceObjectUrl: MediaSourceObjectUrlCheck;
  /** Таймер окна — только страховка возврата файла (`FILE_RETURN_TIMEOUT_MS`). */
  readonly scheduleTimeout: TimeoutScheduler;
}
