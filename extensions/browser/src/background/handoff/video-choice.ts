// Какое видео вкладки отдать (пункт 9 задания 09a — простое правило до угадывания 09c, расширено
// в 09b формами источника).
//
// Порядок (решение 8 владельца: «играет сейчас > самое большое видимое»; находка 9 ревью 09b):
//  1. ИГРАЮЩИЕ — раньше любых неиграющих, какой бы формы они ни были;
//  2. среди равных по «играет» — ярус формы: живой MediaSource выше обычного файла по http(s)
//     (`<video src>`, сессия 09b);
//  3. внутри яруса — самый большой видимый; иначе (никто не играет) — только видимые.
// Прежнее правило «MSE всегда выше файла» отдавало стоящий видимый MSE-превью вместо играющего
// фильма-файла. Обратная сторона: рекламу отдельным `<video src>` (Tortuga/Ashdi), играющую, пока
// фильм в MSE стоит, правило теперь выберет — её отсев по признакам рекламы — задача 09c
// (решения 66, 84), а не порядка форм.
// Если передавать нечего, но видео в неподдержанной форме есть (играет или видно) — честное
// «not supported yet» с этой формой (решение 185 для нативного HLS), а не «видео нет».

import type { CandidateMedia, DocumentTag, ElementKey, UnsupportedMediaForm, VideoCandidate } from "../../shared/handoff-contract.ts";

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

/** Итог выбора. */
export type VideoChoice =
  | { readonly kind: "hand_off"; readonly video: ChosenVideo }
  /** Передать нечего, но видео есть в неподдержанной форме. */
  | { readonly kind: "not_supported_yet"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "nothing" };

interface Ranked {
  readonly frameId: number;
  readonly candidate: VideoCandidate;
}

/** Ярусы передаваемых форм — в порядке предпочтения (при равенстве по «играет»). */
const HANDOFF_TIERS: readonly CandidateMedia["kind"][] = ["media_source", "file"];

/** Проходы выбора: сначала только играющие, затем видимые. */
type PlaybackPass = "playing" | "visible";
const PLAYBACK_PASSES: readonly PlaybackPass[] = ["playing", "visible"];

/** Подходит ли кандидат проходу. */
function admits(pass: PlaybackPass, candidate: VideoCandidate): boolean {
  return pass === "playing" ? candidate.playing : candidate.visible_area_px > 0;
}

/**
 * Выбор по правилу. При равенстве площади выигрывает фрейм с меньшим id (верхний документ — 0),
 * затем порядок кандидатов в документе: выбор детерминирован и не зависит от порядка ответов фреймов.
 */
export function chooseVideo(frames: readonly FrameCandidates[]): VideoChoice {
  const ranked: Ranked[] = [];
  for (const frame of [...frames].sort((left, right) => left.frameId - right.frameId)) {
    for (const candidate of frame.candidates) ranked.push({ frameId: frame.frameId, candidate });
  }
  for (const pass of PLAYBACK_PASSES) {
    for (const tier of HANDOFF_TIERS) {
      const best = biggestOf(ranked.filter((entry) => entry.candidate.media.kind === tier && admits(pass, entry.candidate)));
      if (best !== undefined) {
        return { kind: "hand_off", video: { frameId: best.frameId, document: best.candidate.document, element: best.candidate.element } };
      }
    }
  }
  const unsupported = bestOf(ranked.filter((entry) => entry.candidate.media.kind === "unsupported"));
  if (unsupported !== undefined && unsupported.candidate.media.kind === "unsupported") {
    return { kind: "not_supported_yet", form: unsupported.candidate.media.form };
  }
  return { kind: "nothing" };
}

/** Самый большой видимый из отобранных (при равенстве — более ранний); `undefined` — пусто. */
function biggestOf(pool: readonly Ranked[]): Ranked | undefined {
  let best: Ranked | undefined;
  for (const entry of pool) {
    // Строгое «больше»: при равенстве остаётся более ранний (меньший frameId, раньше в документе).
    if (best === undefined || entry.candidate.visible_area_px > best.candidate.visible_area_px) best = entry;
  }
  return best;
}

/**
 * Лучший среди неподдержанных: играющие, иначе видимые; среди них — с наибольшей видимой площадью.
 * `undefined` — никто не играет и никого не видно.
 */
function bestOf(tier: readonly Ranked[]): Ranked | undefined {
  const playing = tier.filter((entry) => entry.candidate.playing);
  return biggestOf(playing.length > 0 ? playing : tier.filter((entry) => entry.candidate.visible_area_px > 0));
}
