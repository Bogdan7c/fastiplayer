// Шов «выбор видео» между фоном и изолированным миром (сессия 09a, пункт 9).
//
// Фон опрашивает фреймы вкладки по одному через `scripting.executeScript` (с сессии 15c). Внедрённая
// функция исполняется в ИЗОЛИРОВАННОМ мире фрейма (мир по умолчанию) и зовёт функцию, которую
// изолированный мир выставил в своём глобале под этим именем. Глобал изолированного мира странице
// не виден (у MAIN-мира свой глобал), поэтому страница не может ни вызвать, ни подменить её.

import type { VideoCandidate } from "./handoff-contract.ts";

/** Имя функции в глобале изолированного мира: `() => Promise<ScriptedFrameAnswer>`. */
export const VIDEO_CANDIDATES_FUNCTION_NAME = "fastiplayerBridgeDescribeVideoCandidates";

/** Фрейм с нашим скриптом: версия скрипта и его видео. Версию фон сверяет со своей (сессия 10d). */
export interface ScriptedFrameAnswer {
  readonly script_version: string;
  readonly candidates: readonly VideoCandidate[];
}

/**
 * Фрейм без нашего скрипта. `on_web_page: false` — `about:blank` и подобное: там скрипта и не должно
 * быть, перезагрузка не поможет. `document_loading: true` — документ ещё разбирается: внедрение без
 * ожидания загрузки (сессия 15c) могло опередить наш скрипт `document_start`, судить о разрыве рано.
 */
export interface ScriptlessFrameAnswer {
  readonly script_absent: true;
  readonly on_web_page: boolean;
  readonly document_loading: boolean;
}
