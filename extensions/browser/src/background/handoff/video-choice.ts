// Какое видео вкладки отдать (решение 8 владельца; 09a — простое правило, 09b — формы источника,
// 09c — отсев рекламы/мусора/DRM и скрытые игроки). Второго правила нет: модель вкладки и команда
// «отдать» вызывают этот выбор.
//
// Пул — только видео с вердиктом `main` (tab-model/video-verdict.ts). Проходы по пулу:
//  1. играет И видно;
//  2. видно (пауза);
//  3. играет, но скрыто (Ashdi: скрытый зеркальный плеер играет, решение 68) — скрытый играющий
//     НЕ побеждает видимый, поэтому он только третий.
// Внутри прохода (решение 8: «самое большое видимое > самое длинное»): больше площадь, затем длиннее
// ролик (длительность неизвестна — последняя), затем форма (живой MediaSource выше файла по http(s)),
// затем фрейм и порядок в документе. Форма только разрешает равенство: иначе беззвучный тизер на MSE
// обгонял бы большой фильм-файл.
// Реклама отдельным файлом, играющая при стоящем фильме, в пул не входит и фильм не обгоняет.
//
// Если главного нет, исход по оставшимся: видимый или играющий под DRM → `protected`; играющая
// реклама → `ad_playing`; видимая или играющая неподдержанная форма → `not_supported_yet` с формой
// (решение 185); иначе `nothing`. DRM-видео не мешает передать другой `main` на той же странице.

import type { CandidateMedia, DocumentTag, ElementKey, UnsupportedMediaForm, VideoCandidate } from "../../shared/handoff-contract.ts";
import { VIDEO_FILTER_RULES, type VideoFilterRules } from "../tab-model/video-filter-rules.ts";
import { classifyVideo, type VideoVerdict } from "../tab-model/video-verdict.ts";

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
  /** Хост документа с элементом и хост файла (`null` — не файл): для проверки адреса (решение 199). */
  readonly frameHost: string;
  readonly fileHost: string | null;
}

/** Итог выбора. */
export type VideoChoice =
  | { readonly kind: "hand_off"; readonly video: ChosenVideo }
  /** Главного нет, лучшее видео вкладки под DRM: не трогаем. */
  | { readonly kind: "protected" }
  /** Главного нет, играет только отдельный рекламный ролик (фильма ещё нет, до 09c2). */
  | { readonly kind: "ad_playing" }
  /** Передать нечего, но видео есть в неподдержанной форме. */
  | { readonly kind: "not_supported_yet"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "nothing" };

/** Кандидат с вердиктом и фреймом. */
export interface ClassifiedVideo {
  readonly frameId: number;
  readonly candidate: VideoCandidate;
  readonly verdict: VideoVerdict;
}

/** Передаваемые формы в порядке предпочтения при равенстве площади и длительности. */
const HANDOFF_FORMS: readonly CandidateMedia["kind"][] = ["media_source", "file"];

/** Проходы выбора (см. шапку файла). */
type PlaybackPass = "playing_visible" | "visible" | "playing_hidden";
const PLAYBACK_PASSES: readonly PlaybackPass[] = ["playing_visible", "visible", "playing_hidden"];

function admits(pass: PlaybackPass, candidate: VideoCandidate): boolean {
  const visible = candidate.visible_area_px > 0;
  switch (pass) {
    case "playing_visible":
      return candidate.playing && visible;
    case "visible":
      return visible;
    case "playing_hidden":
      return candidate.playing && !visible;
  }
}

/** Классификация всех кандидатов вкладки в стабильном порядке: фрейм по возрастанию id, затем порядок документа. */
export function classifySurvey(frames: readonly FrameCandidates[], rules: VideoFilterRules = VIDEO_FILTER_RULES): ClassifiedVideo[] {
  const classified: ClassifiedVideo[] = [];
  for (const frame of [...frames].sort((left, right) => left.frameId - right.frameId)) {
    for (const candidate of frame.candidates) {
      classified.push({ frameId: frame.frameId, candidate, verdict: classifyVideo(candidate, rules) });
    }
  }
  return classified;
}

/** Выбор по правилу. Детерминирован и не зависит от порядка ответов фреймов. */
export function chooseVideo(frames: readonly FrameCandidates[], rules: VideoFilterRules = VIDEO_FILTER_RULES): VideoChoice {
  const classified = classifySurvey(frames, rules);
  const mains = classified.filter((entry) => entry.verdict === "main");
  const handable = mains.filter((entry) => HANDOFF_FORMS.includes(entry.candidate.media.kind));
  for (const pass of PLAYBACK_PASSES) {
    const best = bestOf(handable.filter((entry) => admits(pass, entry.candidate)));
    if (best !== undefined) {
      const { document, element, frame_host: frameHost, file_host: fileHost } = best.candidate;
      return { kind: "hand_off", video: { frameId: best.frameId, document, element, frameHost, fileHost } };
    }
  }
  return choiceWithoutMain(classified);
}

/** Главного видео нет: какой честный отказ дать по остальным (порядок: DRM, реклама, неподдержанное). */
function choiceWithoutMain(classified: readonly ClassifiedVideo[]): VideoChoice {
  if (mostNoticeable(classified.filter((entry) => entry.verdict === "protected")) !== undefined) return { kind: "protected" };
  if (classified.some((entry) => entry.verdict === "ad" && entry.candidate.playing)) return { kind: "ad_playing" };
  const unsupported = mostNoticeable(classified.filter((entry) => entry.verdict === "unsupported"));
  if (unsupported !== undefined && unsupported.candidate.media.kind === "unsupported") {
    return { kind: "not_supported_yet", form: unsupported.candidate.media.form };
  }
  return { kind: "nothing" };
}

/** Сравнение внутри прохода: площадь, затем длительность (неизвестная — последняя), затем форма. */
function isBetter(entry: ClassifiedVideo, incumbent: ClassifiedVideo): boolean {
  const entryArea = entry.candidate.visible_area_px;
  const incumbentArea = incumbent.candidate.visible_area_px;
  if (entryArea !== incumbentArea) return entryArea > incumbentArea;
  const entryDuration = entry.candidate.duration_s ?? -1;
  const incumbentDuration = incumbent.candidate.duration_s ?? -1;
  if (entryDuration !== incumbentDuration) return entryDuration > incumbentDuration;
  return formRank(entry) < formRank(incumbent);
}

/** Место формы в порядке предпочтения: меньше — лучше. */
function formRank(entry: ClassifiedVideo): number {
  return HANDOFF_FORMS.indexOf(entry.candidate.media.kind);
}

/** Лучший из отобранных; при полном равенстве остаётся более ранний (меньший frameId, раньше в документе). */
function bestOf(pool: readonly ClassifiedVideo[]): ClassifiedVideo | undefined {
  let best: ClassifiedVideo | undefined;
  for (const entry of pool) {
    if (best === undefined || isBetter(entry, best)) best = entry;
  }
  return best;
}

/**
 * Для отказов (DRM, неподдержанное): учитываем только играющих, а если играющих нет — видимых;
 * `undefined`, когда никто не играет и никого не видно.
 */
function mostNoticeable(pool: readonly ClassifiedVideo[]): ClassifiedVideo | undefined {
  const playing = pool.filter((entry) => entry.candidate.playing);
  return bestOf(playing.length > 0 ? playing : pool.filter((entry) => entry.candidate.visible_area_px > 0));
}
