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
// Реклама на месте плеера, пока фильм стоит, уходит в плеер первой (решение 201): сеанс начинается на
// ней и потом переезжает на фильм (`video-relocation.ts`). Беззвучный баннер рядом с фильмом рекламой
// на месте плеера не считается: баннер не всегда зациклен, поэтому у беззвучного решает ещё и площадь.
//
// Если главного нет, исход по оставшимся: видимый или играющий под DRM → `protected`; видимая или
// играющая неподдержанная форма → `not_supported_yet` с формой (решение 185); иначе `nothing`.
// DRM-видео не мешает передать другой `main` на той же странице.

import type { CandidateMedia, DocumentTag, ElementKey, UnsupportedMediaForm, VideoCandidate, VideoRef } from "../../shared/handoff-contract.ts";
import { VIDEO_FILTER_RULES, type VideoFilterRules } from "../tab-model/video-filter-rules.ts";
import { classifyVideo, unsupportedFormOf, type VideoVerdict } from "../tab-model/video-verdict.ts";

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
  /** Что сейчас на экране: реклама или фильм. Признак уходит сеансу и переживает переезд (решение 204). */
  readonly role: VideoRole;
  /**
   * Короткий файл с вердиктом «фильм» ещё может оказаться рекламой: преролл в своём фрейме на незнакомом
   * домене соседей не имеет. Сторож взводится заранее, а переезд откроется, только когда ролик кончится.
   */
  readonly mayBeAd: boolean;
  /**
   * Длина ролика, секунды (`null` — неизвестна или бесконечна, эфир). Нужна возврату страницы (11b2): по ней
   * фон узнаёт тот же фильм на заново открытой странице (сверка ± 2 с, как у позднего входа).
   */
  readonly durationS: number | null;
}

/** Что на экране плеера. */
export type VideoRole = "ad" | "film";

/** Итог выбора. */
export type VideoChoice =
  | { readonly kind: "hand_off"; readonly video: ChosenVideo }
  /** Главного нет, лучшее видео вкладки под DRM: не трогаем. */
  | { readonly kind: "protected" }
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
const HANDOFF_FORMS: readonly CandidateMedia["kind"][] = ["media_source", "file", "hls"];

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
      // Соседи — только свой документ: преролл и ждущий его фильм живут в одном плеере. Старый
      // документ того же фрейма чужой, даже если ещё не успел исчезнуть из опроса.
      const neighbours = frame.candidates.filter((other) => other !== candidate && other.document === candidate.document);
      classified.push({ frameId: frame.frameId, candidate, verdict: classifyVideo(candidate, rules, neighbours) });
    }
  }
  return classified;
}

/** Выбор по правилу. Детерминирован и не зависит от порядка ответов фреймов. */
export function chooseVideo(frames: readonly FrameCandidates[], rules: VideoFilterRules = VIDEO_FILTER_RULES): VideoChoice {
  const classified = classifySurvey(frames, rules);
  const ad = adInPlayerPlace(classified, handableMains(classified));
  return ad === undefined ? chooseMainVideo(frames, rules) : { kind: "hand_off", video: chosenVideoOf(ad, rules) };
}

/** Передаваемые видео с вердиктом «главное». */
function handableMains(classified: readonly ClassifiedVideo[]): ClassifiedVideo[] {
  return classified.filter((entry) => entry.verdict === "main" && HANDOFF_FORMS.includes(entry.candidate.media.kind));
}

/**
 * Главное видео вкладки: то, ради чего человек пришёл, даже пока на месте плеера идёт реклама. Его
 * показывает модель вкладки (попап 10), а `chooseVideo` отвечает на другой вопрос: что передать сейчас.
 */
export function chooseMainVideo(frames: readonly FrameCandidates[], rules: VideoFilterRules = VIDEO_FILTER_RULES): VideoChoice {
  const classified = classifySurvey(frames, rules);
  const handable = handableMains(classified);
  for (const pass of PLAYBACK_PASSES) {
    const best = bestOf(handable.filter((entry) => admits(pass, entry.candidate)));
    if (best !== undefined) return { kind: "hand_off", video: chosenVideoOf(best, rules) };
  }
  return choiceWithoutMain(classified);
}

/** Итог выбора по ссылке человека: обычный итог или «такого видео в свежем опросе нет». */
export type RequestedVideoChoice = VideoChoice | { readonly kind: "gone" };

/**
 * Выбор по ссылке из списка попапа (сессия 10b). Это не «выбери за человека», а проверка его выбора по
 * СВЕЖЕМУ опросу: подмены другим видео нет.
 * - ссылки нет в опросе → `gone`;
 * - DRM → `protected`, неподдержанная форма → `not_supported_yet`, пустой элемент → `nothing`;
 * - `main`, `ad`, `junk` передаются. Выбор человека главнее вердикта: фильм, ошибочно принятый за рекламу,
 *   иначе получил бы плашку рекламы и запрет перемотки, поэтому роль всегда `film`. `mayBeAd` считается как
 *   обычно: короткий файл всё равно может оказаться роликом, и сторож взведётся.
 */
export function chooseRequestedVideo(frames: readonly FrameCandidates[], ref: VideoRef, rules: VideoFilterRules = VIDEO_FILTER_RULES): RequestedVideoChoice {
  const entry = classifySurvey(frames, rules).find(
    (known) => known.frameId === ref.frame_id && known.candidate.document === ref.document && known.candidate.element === ref.element,
  );
  if (entry === undefined) return { kind: "gone" };
  switch (entry.verdict) {
    case "protected":
      return { kind: "protected" };
    case "unsupported":
      return unsupportedChoiceOf(entry);
    case "empty":
      return { kind: "nothing" };
    case "main":
    case "ad":
    case "junk":
      return { kind: "hand_off", video: { ...chosenVideoOf(entry, rules), role: "film" } };
  }
}

/** Выбранное видео из записи опроса. Единственное место, где собирается `ChosenVideo`. */
export function chosenVideoOf(entry: ClassifiedVideo, rules: VideoFilterRules = VIDEO_FILTER_RULES): ChosenVideo {
  const { document, element, frame_host: frameHost, file_host: fileHost, duration_s: durationSeconds } = entry.candidate;
  // Короткий HLS-ролик (запись) подозрителен так же, как короткий файл.
  const isShortFile = (entry.candidate.media.kind === "file" || entry.candidate.media.kind === "hls") && durationSeconds !== null && durationSeconds <= rules.prerollMaxSeconds;
  return {
    frameId: entry.frameId,
    document,
    element,
    frameHost,
    fileHost,
    role: entry.verdict === "ad" ? "ad" : "film",
    mayBeAd: isShortFile,
    durationS: durationSeconds,
  };
}

/**
 * Реклама на месте плеера: играет, видна, не зациклена, передаваемой формы, а фильм не играет. Со звуком
 * это точно не баннер. Беззвучная (автозапуск без звука) должна занимать хотя бы половину площади плеера:
 * одинокий беззвучный рекламный ролик на странице без плеера — баннер, передавать его незачем.
 */
function adInPlayerPlace(classified: readonly ClassifiedVideo[], handable: readonly ClassifiedVideo[]): ClassifiedVideo | undefined {
  if (handable.some((entry) => entry.candidate.playing && entry.candidate.visible_area_px > 0)) return undefined;
  // Играет защищённое видео: это оно на месте плеера, а реклама рядом ему не преролл.
  if (classified.some((entry) => entry.verdict === "protected" && entry.candidate.playing)) return undefined;
  // Площадь плеера: фильм или пустой элемент, ждущий его. Реклама сама себе плеером не считается.
  const playerArea = Math.max(0, ...classified.filter((entry) => entry.verdict !== "ad").map((entry) => entry.candidate.visible_area_px));
  const inPlace = classified.filter(({ verdict, candidate }) => {
    if (verdict !== "ad" || !candidate.playing || candidate.loop || candidate.visible_area_px === 0) return false;
    if (!HANDOFF_FORMS.includes(candidate.media.kind)) return false;
    return !candidate.muted || (playerArea > 0 && candidate.visible_area_px >= playerArea / 2);
  });
  return bestOf(inPlace);
}

/** Главного видео нет: какой честный отказ дать по остальным (порядок: DRM, неподдержанное). */
function choiceWithoutMain(classified: readonly ClassifiedVideo[]): VideoChoice {
  if (mostNoticeable(classified.filter((entry) => entry.verdict === "protected")) !== undefined) return { kind: "protected" };
  const unsupported = mostNoticeable(classified.filter((entry) => entry.verdict === "unsupported"));
  return unsupported === undefined ? { kind: "nothing" } : unsupportedChoiceOf(unsupported);
}

/** Отказ по неподдержанному видео: его форма; формы нет — «нечего передавать». */
function unsupportedChoiceOf(entry: ClassifiedVideo): VideoChoice {
  const form = unsupportedFormOf(entry.candidate);
  return form === null ? { kind: "nothing" } : { kind: "not_supported_yet", form };
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
