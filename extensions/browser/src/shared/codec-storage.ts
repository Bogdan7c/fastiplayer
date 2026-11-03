// Что экстеншен хранит в `storage.local` про кодеки, и как это читать/писать. Общий для фона
// (политика) и страницы настроек (запись настройки). Значения из хранилища не доверяем слепо:
// чужая правка или старая версия не должны ронять фон — мусор читается как «нет значения».
import { CODEC_SETTINGS, DEFAULT_CODEC_SETTING, type CodecSetting, type VideoCodecFamily } from "./codec-policy.ts";

/** Семейства метки плеера в каноническом порядке (порядок записи в кэш). */
const LABEL_FAMILY_ORDER: readonly VideoCodecFamily[] = ["h264", "hevc", "vp9", "av1"];

/** Ключ настройки владельца (`auto | prefer_vp9 | always_h264 | off`; незнакомое значение читается как `auto`). */
export const CODEC_SETTING_KEY = "codecSetting";
/** Ключ «дешёвых» кодеков плеера из его последнего `hello_accepted` (решение 162: плеер будится лишь по делу). */
export const CHEAP_CODECS_KEY = "cheapVideoCodecs";

/** Минимум от `storage.local`, нужный политике (в тестах — словарь в памяти). */
export interface KeyValueStore {
  get(keys: readonly string[]): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
}

export async function readCodecSetting(store: KeyValueStore): Promise<CodecSetting> {
  const stored = (await store.get([CODEC_SETTING_KEY]))[CODEC_SETTING_KEY];
  return CODEC_SETTINGS.find((setting) => setting === stored) ?? DEFAULT_CODEC_SETTING;
}

export async function writeCodecSetting(store: KeyValueStore, setting: CodecSetting): Promise<void> {
  await store.set({ [CODEC_SETTING_KEY]: setting });
}

/** Кэш «дешёвых» кодеков плеера; пусто — плеер ещё ни разу не отвечал (или кэш испорчен). */
export async function readCachedCheapCodecs(store: KeyValueStore): Promise<readonly VideoCodecFamily[]> {
  const stored = (await store.get([CHEAP_CODECS_KEY]))[CHEAP_CODECS_KEY];
  if (!Array.isArray(stored)) return [];
  return LABEL_FAMILY_ORDER.filter((family) => stored.includes(family));
}

export async function writeCachedCheapCodecs(store: KeyValueStore, families: readonly VideoCodecFamily[]): Promise<void> {
  await store.set({ [CHEAP_CODECS_KEY]: LABEL_FAMILY_ORDER.filter((family) => families.includes(family)) });
}
