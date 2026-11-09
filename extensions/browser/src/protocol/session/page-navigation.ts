// Навигация страницы (протокол 1.7, сессия 14a): есть ли на странице следующая серия. Зеркало
// session/page_navigation.rs.
//
// Сообщение `page_navigation` не привязано к поколению потока, последнее слово главнее. До первого сообщения
// плеер считает, что следующей серии нет; с экстеншеном старше 1.7 кнопки Next нет вовсе.

import { stringEnum, type Codec } from "../schema.ts";

/** Есть ли на странице следующая серия. */
export type NextEpisodeAvailability = "available" | "unavailable";

/** Оба значения - один список для разбора. */
export const NEXT_EPISODE_AVAILABILITIES: readonly NextEpisodeAvailability[] = ["available", "unavailable"];

export const nextEpisodeAvailabilityCodec: Codec<NextEpisodeAvailability> =
  stringEnum<NextEpisodeAvailability>(NEXT_EPISODE_AVAILABILITIES);
