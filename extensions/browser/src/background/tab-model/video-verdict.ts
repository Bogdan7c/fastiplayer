// Классификатор видео вкладки: чистая функция «факты кандидата + правила → вердикт».
// Единственное место, где решается «фильм это, реклама или мусор»: выбор главного видео
// (handoff/video-choice.ts) и модель вкладки используют ровно эту функцию — второго правила нет.

import type { VideoCandidate } from "../../shared/handoff-contract.ts";
import { VIDEO_FILTER_RULES, type VideoFilterRules } from "./video-filter-rules.ts";

/**
 * Вердикт о видео:
 * - `main` — кандидат в главное видео;
 * - `ad` — отдельный рекламный ролик (домен файла или документа рекламный);
 * - `junk` — баннер, превью по наведению, короткий ролик;
 * - `protected` — под DRM, не трогаем (инвариант 4);
 * - `unsupported` — форма источника, которую мы не умеем передавать (not supported yet);
 * - `empty` — источника нет.
 */
export type VideoVerdict = "main" | "ad" | "junk" | "protected" | "unsupported" | "empty";

/** Хост равен рекламному суффиксу или является его поддоменом (сравнение без учёта регистра). */
export function isAdHost(host: string | null, rules: VideoFilterRules): boolean {
  if (host === null || host === "") return false;
  const normalized = host.toLowerCase();
  return rules.adHostSuffixes.some((suffix) => normalized === suffix || normalized.endsWith(`.${suffix}`));
}

/** Беззвучное видео с признаком «не плеер»: цикл, мелкая плитка или короткий ролик. */
function isSilentDecoration(candidate: VideoCandidate, rules: VideoFilterRules): boolean {
  if (!candidate.muted) return false;
  const isShort = candidate.duration_s !== null && candidate.duration_s < rules.shortClipSeconds;
  return candidate.loop || candidate.visible_area_px < rules.smallAreaPx || isShort;
}

/** Классификация одного кандидата. Порядок проверок — часть правила, менять осознанно. */
export function classifyVideo(candidate: VideoCandidate, rules: VideoFilterRules = VIDEO_FILTER_RULES): VideoVerdict {
  if (candidate.drm) return "protected";
  if (candidate.media.kind === "none") return "empty";
  if (candidate.media.kind === "unsupported") return "unsupported";
  if (isAdHost(candidate.file_host, rules) || isAdHost(candidate.frame_host, rules)) return "ad";
  if (isSilentDecoration(candidate, rules)) return "junk";
  // Известно, что ролик короткий, и он стоит: не фильм (играющий короткий со звуком оставляем).
  if (candidate.duration_s !== null && candidate.duration_s < rules.shortClipSeconds && !candidate.playing) return "junk";
  return "main";
}
