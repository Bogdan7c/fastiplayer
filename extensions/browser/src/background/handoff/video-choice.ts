// Какое видео вкладки отдать (пункт 9 задания 09a — простое правило до угадывания 09c):
// среди всех фреймов — элемент с живым MediaSource, который играет; иначе самый большой видимый
// из живых. Видео без живого MediaSource 09a не передаёт (`<video src>` — 09b).

import type { DocumentTag, ElementKey, VideoCandidate } from "../../shared/handoff-contract.ts";

/** Кандидаты одного фрейма вкладки. */
export interface FrameCandidates {
  readonly frameId: number;
  readonly candidates: readonly VideoCandidate[];
}

/**
 * Выбранный элемент: фрейм, метка документа и ключ элемента в нём. Метка уходит в `begin_handoff`:
 * пока фон ждёт плеер, фрейм мог перейти на другой документ, где ключи элементов снова с 1, —
 * чужой документ по метке ответит `element_not_found`, а не отдаст не то видео.
 */
export interface ChosenVideo {
  readonly frameId: number;
  readonly document: DocumentTag;
  readonly element: ElementKey;
}

interface Ranked {
  readonly frameId: number;
  readonly candidate: VideoCandidate;
}

/**
 * Выбор по правилу. При равенстве площади выигрывает фрейм с меньшим id (верхний документ — 0),
 * затем порядок кандидатов в документе: выбор детерминирован и не зависит от порядка ответов фреймов.
 * `undefined` — передавать нечего (нет живого MediaSource, либо живые не играют и не видны).
 */
export function chooseVideo(frames: readonly FrameCandidates[]): ChosenVideo | undefined {
  const live: Ranked[] = [];
  for (const frame of [...frames].sort((left, right) => left.frameId - right.frameId)) {
    for (const candidate of frame.candidates) if (candidate.has_live_media) live.push({ frameId: frame.frameId, candidate });
  }
  const playing = live.filter((entry) => entry.candidate.playing);
  const pool = playing.length > 0 ? playing : live.filter((entry) => entry.candidate.visible_area_px > 0);
  let best: Ranked | undefined;
  for (const entry of pool) {
    // Строгое «больше»: при равенстве остаётся более ранний (меньший frameId, раньше в документе).
    if (best === undefined || entry.candidate.visible_area_px > best.candidate.visible_area_px) best = entry;
  }
  return best === undefined
    ? undefined
    : { frameId: best.frameId, document: best.candidate.document, element: best.candidate.element };
}
