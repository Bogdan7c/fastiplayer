// Какие фильтры видео режима 2 действуют на этой вкладке (решения 260–261).
//
// Фильтр действует, если человек включил его везде или добавил сайт вкладки в список фильтра
// (`videoFilterAppliesOn`). Сайт — адрес вкладки: его знает только фон, поэтому списки судятся здесь,
// а охране страницы уходят лишь общие включатели (`autoHandoffWatchOf`).
//
// Здесь же — поправка для общего классификатора видео (`candidatesAsFiltersSee`), см. ниже.
import type { AutoHandoffSettings } from "../../shared/auto-handoff-settings.ts";
import type { ElementKey, VideoCandidate } from "../../shared/handoff-contract.ts";
import { videoFilterAppliesOn } from "../../shared/video-filter-settings.ts";
import { parseWebAddress } from "./stop-list.ts";

/** Фильтры, действующие на вкладке. Каждое поле — «это видео остаётся в браузере». */
export interface VideoFilters {
  /** Отпускать видео без звука. */
  readonly skipMuted: boolean;
  /** Отпускать зацикленное видео. */
  readonly skipLooped: boolean;
  /** Ролик короче порога не уходит сразу, а идёт веткой рекламы (проба, кредит клика). */
  readonly skipShort: boolean;
  /** Порог длины, секунды; действует, только пока `skipShort`. */
  readonly minDurationS: number;
}

/** Фильтры для вкладки с адресом `tabUrl` (как его отдал браузер; не веб-адрес — действуют только общие включатели). */
export function videoFiltersFor(settings: AutoHandoffSettings, tabUrl: string): VideoFilters {
  const siteKey = parseWebAddress(tabUrl)?.siteKey;
  const filters = settings.video_filters;
  return {
    skipMuted: videoFilterAppliesOn(filters.sound, siteKey),
    skipLooped: videoFilterAppliesOn(filters.loop, siteKey),
    skipShort: videoFilterAppliesOn(filters.duration, siteKey),
    minDurationS: settings.min_duration_s,
  };
}

/**
 * Кандидаты донесения такими, какими их должен увидеть общий классификатор при этих фильтрах.
 *
 * Общий классификатор (`classifyVideo`) сам считает беззвучное видео мусором, если оно зацикленное, мелкое
 * или короче 30 с («беззвучное украшение»), — это правило нужно кнопке и передаче вкладки, его не трогаем.
 * Но в режиме 2 задержанное видео запустил сам человек: где фильтр звука не действует, беззвучие не должно
 * его отсеивать. Поэтому ровно задержанный кандидат судится как звучащий; соседи остаются как есть (они
 * влияют на вердикт, например, преролла рядом с фильмом). Фильтр звука действует — кандидаты без изменений.
 */
export function candidatesAsFiltersSee(candidates: readonly VideoCandidate[], heldElement: ElementKey, filters: VideoFilters): readonly VideoCandidate[] {
  if (filters.skipMuted) return candidates;
  return candidates.map((candidate) => (candidate.element === heldElement && candidate.muted ? { ...candidate, muted: false } : candidate));
}
