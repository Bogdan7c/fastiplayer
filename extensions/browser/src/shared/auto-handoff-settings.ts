// Настройка режима 2 «автозапуск» (сессия 12a, решения 9, 10, 15, 246–254) в `storage.local`.
// Общая для фона (судит по правилам), изолированного мира (включает охрану на странице) и страницы настроек
// (запись). Устроена как `entry-settings.ts`: хранилищу не доверяем слепо, битое поле читается значением
// по умолчанию и не сбрасывает остальные.
import type { AutoHandoffWatchState } from "./auto-handoff-contract.ts";
import type { KeyValueStore } from "./codec-storage.ts";

/**
 * Правило сайта:
 * - `always` — нажал play, и видео уходит в плеер (по фильтрам: жест, звук, длина; звук и длину можно
 *   выключить, решение 260);
 * - `never` — на этом сайте режим 2 не срабатывает никогда;
 * - `ask` — видео замирает, а поверх него предложение «Play in Fastiplayer / Play here» (сессия 12b).
 */
export const SITE_RULES = ["always", "never", "ask"] as const;
export type SiteRule = (typeof SITE_RULES)[number];

/** Правило для сайта, у которого своего правила нет (решение 251: в настройке оба варианта). */
export const UNKNOWN_SITE_RULES = ["always", "ask"] as const;
export type UnknownSiteRule = (typeof UNKNOWN_SITE_RULES)[number];

export interface AutoHandoffSettings {
  /** Режим 2 включён для всех сайтов (кроме стоп-листа и `never`). По умолчанию выключен (решение 9). */
  readonly enabled: boolean;
  /** Порог длины ролика, секунды (решение 10): короче — не передаём, длина неизвестна — передаём (решение 50). */
  readonly min_duration_s: number;
  /**
   * Фильтр длины включён (решение 260): `false` — порог не действует, ролик любой длины уходит в плеер
   * (сам порог хранится, чтобы включение вернуло его как было).
   */
  readonly require_min_duration: boolean;
  /**
   * Фильтр звука включён (решение 260): `false` — видео без звука, которое человек сам запустил, тоже уходит
   * в плеер. Жест человека нужен по-прежнему: само по себе беззвучное автовоспроизведение ленты не считается.
   */
  readonly require_sound: boolean;
  /** Что делать на сайте без своего правила, когда режим включён. */
  readonly unknown_site_rule: UnknownSiteRule;
  /** Свои правила сайтов: ключ — `siteKeyOfHost` (хост без `www.`, строчными). */
  readonly site_rules: Readonly<Record<string, SiteRule>>;
  /** Свой стоп-лист (решение 15): записи вида `host` или `host/начало-пути`, см. `normalizeStopListEntry`. */
  readonly own_stop_list: readonly string[];
}

/** Порог длины по умолчанию, секунды (решение 10). */
export const DEFAULT_AUTO_HANDOFF_MIN_DURATION_S = 60;

/**
 * Наибольший порог длины, секунды. Час: длиннее порог бессмыслен (под него не попадёт почти ничего, кроме
 * фильмов, а их человек и так отдаст), а число без предела разрешило бы мусору выключить режим незаметно.
 */
export const MAX_AUTO_HANDOFF_MIN_DURATION_S = 3_600;

/** Предел числа своих правил сайтов: защищает фон от раздутого хранилища. */
export const MAX_SITE_RULES = 1_000;

/** Предел числа записей своего стоп-листа. */
export const MAX_OWN_STOP_LIST_ENTRIES = 500;

/** Предел длины одной записи (хост + путь), символов. Длиннее адресов в стоп-листе не бывает. */
export const MAX_STOP_LIST_ENTRY_LENGTH = 300;

/**
 * По умолчанию: режим выключен (решение 9), порог 60 с, оба фильтра (длина и звук) включены,
 * на новом сайте — сразу передаём (решение 251).
 */
export const DEFAULT_AUTO_HANDOFF_SETTINGS: AutoHandoffSettings = {
  enabled: false,
  min_duration_s: DEFAULT_AUTO_HANDOFF_MIN_DURATION_S,
  require_min_duration: true,
  require_sound: true,
  unknown_site_rule: "always",
  site_rules: {},
  own_stop_list: [],
};

/** Ключ настройки в `storage.local`. */
export const AUTO_HANDOFF_SETTINGS_KEY = "autoHandoffSettings";

/** Хост — только буквы, цифры, дефис и точки (IDN хранится в punycode, как его отдаёт `URL`). */
const HOST_PATTERN = /^[a-z0-9-]+(\.[a-z0-9-]+)*$/;

/**
 * Ключ сайта для правил: хост строчными, без завершающей точки и без ведущего `www.`. `null` — это не хост
 * (пусто, пробелы, порт, путь): такой ключ не записывается и не совпадает ни с чем.
 */
export function siteKeyOfHost(host: string): string | null {
  let normalized = host.trim().toLowerCase();
  if (normalized.endsWith(".")) normalized = normalized.slice(0, -1);
  if (normalized.startsWith("www.")) normalized = normalized.slice("www.".length);
  return normalized.length > 0 && HOST_PATTERN.test(normalized) ? normalized : null;
}

/**
 * Привести запись стоп-листа к виду `host` или `host/путь` (путь без завершающего `/`). Схема `https://`
 * и `www.` отбрасываются: человек может вставить адрес как есть. `null` — запись не разобрать.
 */
export function normalizeStopListEntry(raw: string): string | null {
  const trimmed = raw.trim().toLowerCase().replace(/^[a-z]+:\/\//, "");
  if (trimmed.length === 0 || trimmed.length > MAX_STOP_LIST_ENTRY_LENGTH) return null;
  const slash = trimmed.indexOf("/");
  const host = siteKeyOfHost(slash < 0 ? trimmed : trimmed.slice(0, slash));
  if (host === null) return null;
  const path = slash < 0 ? "" : trimmed.slice(slash).replace(/\/+$/, "");
  // В пути — только печатные символы без пробелов и без запроса/якоря: правило про «раздел сайта».
  if (path.length > 0 && !/^\/[^\s?#]*$/.test(path)) return null;
  return `${host}${path}`;
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Флаг: не булево (нет поля, мусор) — значение по умолчанию. */
function parseFlag(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function parseMinDuration(value: unknown): number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= MAX_AUTO_HANDOFF_MIN_DURATION_S
    ? value
    : DEFAULT_AUTO_HANDOFF_MIN_DURATION_S;
}

/** Правила сайтов: битая запись пропускается, остальные живут; сверх предела — отбрасываются. */
function parseSiteRules(value: unknown): Readonly<Record<string, SiteRule>> {
  if (!isPlainRecord(value)) return {};
  const rules: Record<string, SiteRule> = {};
  let count = 0;
  for (const [site, rule] of Object.entries(value)) {
    if (count >= MAX_SITE_RULES) break;
    const key = siteKeyOfHost(site);
    if (key === null || key !== site || !SITE_RULES.includes(rule as SiteRule)) continue;
    rules[key] = rule as SiteRule;
    count += 1;
  }
  return rules;
}

/** Свой стоп-лист: записи приводятся к единому виду, повторы и мусор отбрасываются. */
function parseOwnStopList(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  const entries = new Set<string>();
  for (const raw of value) {
    if (entries.size >= MAX_OWN_STOP_LIST_ENTRIES) break;
    if (typeof raw !== "string") continue;
    const entry = normalizeStopListEntry(raw);
    if (entry !== null) entries.add(entry);
  }
  return [...entries];
}

/**
 * Разобрать то, что лежит в хранилище. Не объект — значения по умолчанию целиком; битое поле — значение
 * по умолчанию только для этого поля.
 */
export function parseAutoHandoffSettings(stored: unknown): AutoHandoffSettings {
  if (!isPlainRecord(stored)) return DEFAULT_AUTO_HANDOFF_SETTINGS;
  const unknownSiteRule = stored["unknown_site_rule"];
  return {
    enabled: parseFlag(stored["enabled"], DEFAULT_AUTO_HANDOFF_SETTINGS.enabled),
    min_duration_s: parseMinDuration(stored["min_duration_s"]),
    // Настройка, записанная до решения 260, этих полей не знает: фильтры остаются включёнными, как были.
    require_min_duration: parseFlag(stored["require_min_duration"], DEFAULT_AUTO_HANDOFF_SETTINGS.require_min_duration),
    require_sound: parseFlag(stored["require_sound"], DEFAULT_AUTO_HANDOFF_SETTINGS.require_sound),
    unknown_site_rule: UNKNOWN_SITE_RULES.includes(unknownSiteRule as UnknownSiteRule)
      ? (unknownSiteRule as UnknownSiteRule)
      : DEFAULT_AUTO_HANDOFF_SETTINGS.unknown_site_rule,
    site_rules: parseSiteRules(stored["site_rules"]),
    own_stop_list: parseOwnStopList(stored["own_stop_list"]),
  };
}

export async function readAutoHandoffSettings(store: KeyValueStore): Promise<AutoHandoffSettings> {
  return parseAutoHandoffSettings((await store.get([AUTO_HANDOFF_SETTINGS_KEY]))[AUTO_HANDOFF_SETTINGS_KEY]);
}

export async function writeAutoHandoffSettings(store: KeyValueStore, settings: AutoHandoffSettings): Promise<void> {
  await store.set({ [AUTO_HANDOFF_SETTINGS_KEY]: settings });
}

/** Новое значение `storage.onChanged` для нашего ключа (`undefined` — наш ключ в изменениях не участвует). */
export function autoHandoffSettingsFromChange(changes: Readonly<Record<string, { readonly newValue?: unknown }>>): AutoHandoffSettings | undefined {
  const change = changes[AUTO_HANDOFF_SETTINGS_KEY];
  return change === undefined ? undefined : parseAutoHandoffSettings(change.newValue);
}

/**
 * Прочитать настройку там, где сбой хранилища не должен ломать остальное: сбой — значения по умолчанию
 * (режим выключен), причина уходит в `note` (журнал разработчика).
 */
export async function readAutoHandoffSettingsOrDefaults(store: KeyValueStore, note: (message: string, error: unknown) => void): Promise<AutoHandoffSettings> {
  try {
    return await readAutoHandoffSettings(store);
  } catch (error) {
    note("could not read the auto handoff settings, mode 2 stays off", error);
    return DEFAULT_AUTO_HANDOFF_SETTINGS;
  }
}

/**
 * Нужна ли охрана на страницах вообще (изолированный мир решает это без фона и без адреса вкладки).
 * Режим включён — да. Выключен, но какой-то сайт помечен `always` или `ask` — тоже да: своё правило сайта
 * действует и при выключенном режиме, а какой это сайт, судит фон — он один знает адрес вкладки.
 */
export function wantsAutoHandoffWatch(settings: AutoHandoffSettings): boolean {
  return settings.enabled || Object.values(settings.site_rules).some((rule) => rule !== "never");
}

/**
 * Что сказать охране страницы: выключена, включена только для видео со звуком (`on`) или включена и для
 * видео без звука (`on_including_muted`, фильтр звука снят, решение 260). Странице уходит только это —
 * правила сайтов и порог длины она не узнаёт.
 */
export function autoHandoffWatchStateOf(settings: AutoHandoffSettings): AutoHandoffWatchState {
  if (!wantsAutoHandoffWatch(settings)) return "off";
  return settings.require_sound ? "on" : "on_including_muted";
}
