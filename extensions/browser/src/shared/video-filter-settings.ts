// Фильтры видео режима 2 «автозапуск» (решения 260–261): какие запущенные человеком видео остаются в браузере.
//
// Фильтров три: «без звука», «короче порога» и «зацикленное». Каждый устроен одинаково:
// - `enabled` — фильтр действует на всех сайтах (по умолчанию выключен: решение 261);
// - `sites` — свой список сайтов, где фильтр действует, даже когда он выключен (исключение из общего правила).
// Сайт в списке — хост без `www.`; запись действует и на поддомены (`example.com` -> `video.example.com`),
// как свои правила сайтов. Сайт — это адрес вкладки (то, что в адресной строке), а не хост встроенного плеера.
//
// Модуль — чистые данные и разбор хранилища, без браузера: его читают фон (решение по видео), изолированный мир
// (что сказать охране страницы) и страница настроек (запись).
import { siteKeyOfHost } from "./site-key.ts";

/** Имена фильтров; порядок — порядок на странице настроек. */
export const VIDEO_FILTER_NAMES = ["sound", "duration", "loop"] as const;
export type VideoFilterName = (typeof VIDEO_FILTER_NAMES)[number];

/** Один фильтр: действует ли везде и где действует, даже если выключен. */
export interface VideoFilterSetting {
  readonly enabled: boolean;
  /** Ключи сайтов (`siteKeyOfHost`), без повторов, в порядке добавления. */
  readonly sites: readonly string[];
}

/** Все фильтры. */
export type VideoFilterSettings = Readonly<Record<VideoFilterName, VideoFilterSetting>>;

/** Предел длины списка сайтов одного фильтра: защищает фон от раздутого хранилища. */
export const MAX_VIDEO_FILTER_SITES = 500;

const OFF_EVERYWHERE: VideoFilterSetting = { enabled: false, sites: [] };

/** По умолчанию все фильтры выключены, списков нет (решение 261: настраивает сам человек). */
export const DEFAULT_VIDEO_FILTER_SETTINGS: VideoFilterSettings = {
  sound: OFF_EVERYWHERE,
  duration: OFF_EVERYWHERE,
  loop: OFF_EVERYWHERE,
};

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Привести то, что ввёл человек, к ключу сайта: схема `https://`, путь, запрос и `www.` отбрасываются
 * (можно вставить адрес страницы целиком). `null` — это не адрес сайта.
 */
export function normalizeVideoFilterSite(raw: string): string | null {
  const withoutScheme = raw.trim().toLowerCase().replace(/^[a-z]+:\/\//, "");
  // Хост кончается на первом `/`, `?` или `#`.
  const host = withoutScheme.split(/[/?#]/, 1)[0] ?? "";
  return siteKeyOfHost(host);
}

/** Список сайтов из хранилища: битые записи и повторы уходят, сверх предела — отбрасываются. */
function parseSites(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  const sites = new Set<string>();
  for (const raw of value) {
    if (sites.size >= MAX_VIDEO_FILTER_SITES) break;
    if (typeof raw !== "string") continue;
    const site = normalizeVideoFilterSite(raw);
    if (site !== null) sites.add(site);
  }
  return [...sites];
}

function parseFilter(value: unknown): VideoFilterSetting {
  if (!isPlainRecord(value)) return OFF_EVERYWHERE;
  return {
    enabled: typeof value["enabled"] === "boolean" ? value["enabled"] : OFF_EVERYWHERE.enabled,
    sites: parseSites(value["sites"]),
  };
}

/** Разобрать фильтры из хранилища. Битый фильтр — выключен и без списка; соседи не страдают. */
export function parseVideoFilterSettings(value: unknown): VideoFilterSettings {
  if (!isPlainRecord(value)) return DEFAULT_VIDEO_FILTER_SETTINGS;
  return { sound: parseFilter(value["sound"]), duration: parseFilter(value["duration"]), loop: parseFilter(value["loop"]) };
}

/** Сайт лежит в списке: точный хост или поддомен записанного (`video.example.com` под `example.com`). */
export function siteIsListed(siteKey: string, sites: readonly string[]): boolean {
  return sites.some((listed) => siteKey === listed || siteKey.endsWith(`.${listed}`));
}

/**
 * Действует ли фильтр на сайте вкладки. `siteKey` `undefined` — адрес вкладки неизвестен: тогда действует
 * только общий включатель (списку сопоставлять не с чем).
 */
export function videoFilterAppliesOn(filter: VideoFilterSetting, siteKey: string | undefined): boolean {
  return filter.enabled || (siteKey !== undefined && siteIsListed(siteKey, filter.sites));
}
