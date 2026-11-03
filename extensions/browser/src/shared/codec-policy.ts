// Политика видеокодеков для страниц (решения 43, 64, 189): общие типы фона, изолированного
// скрипта-«курьера» и MAIN-мира. Только чистые данные и разбор — ни браузерного API, ни состояния.
//
// Политика отвечает на один вопрос: какие семейства ВИДЕО-кодеков страница должна услышать как
// «не поддерживается». Модель «отказать этим» (а не «разрешить только эти»): всё, чего политика не
// назвала, остаётся за браузером. Звук политика не трогает никогда. Плейлисты HLS (решение 64)
// отвечают «нет» всегда и от политики не зависят (см. src/page/codec-policy/mime-classifier.ts).
import type { VideoCodecFamily } from "../protocol/capability.ts";

export type { VideoCodecFamily };

/** Настройка владельца на странице настроек. */
export type CodecSetting = "auto" | "prefer_vp9" | "always_h264" | "off";

export const DEFAULT_CODEC_SETTING: CodecSetting = "auto";
export const CODEC_SETTINGS: readonly CodecSetting[] = ["auto", "prefer_vp9", "always_h264", "off"];

/** Семейство, которому политика может отказать (H.264 не отвергается никогда). */
export type RefusableFamily = "av1" | "vp9" | "hevc" | "vp8";

/** Канонический порядок отказов: от него зависят и имена файлов-курьеров, и полезная нагрузка. */
export const CANONICAL_REFUSAL_ORDER: readonly RefusableFamily[] = ["av1", "vp9", "hevc", "vp8"];

/**
 * Действующая политика.
 *  - `transparent` — браузер отвечает как обычно (кроме HLS);
 *  - `refuse`      — перечисленным видео-семействам страница получает «не поддерживается».
 *    `families` непусто, без повторов, в каноническом порядке `CANONICAL_REFUSAL_ORDER`.
 */
export type CodecPolicy =
  | { readonly kind: "transparent" }
  | { readonly kind: "refuse"; readonly families: readonly RefusableFamily[] };

export const TRANSPARENT_POLICY: CodecPolicy = { kind: "transparent" };

/** Политика «отказать этим»: пустой набор — прозрачность, остальное приводится к канону. */
export function refuse(families: Iterable<RefusableFamily>): CodecPolicy {
  const wanted = new Set(families);
  const canonical = CANONICAL_REFUSAL_ORDER.filter((family) => wanted.has(family));
  return canonical.length === 0 ? TRANSPARENT_POLICY : { kind: "refuse", families: canonical };
}

// ---------------------------------------------------------------- полезная нагрузка «курьер → страница»

/** Нагрузка события: отказываемые семейства через запятую в каноническом порядке, например `av1,vp9`. */
export function encodeRefusedFamilies(families: readonly RefusableFamily[]): string {
  return families.join(",");
}

/**
 * Разбор нагрузки. Страница — враг (инвариант 1): принимается только строка из известных семейств,
 * разделённых запятыми, в каноническом порядке и без повторов; всё остальное — `undefined`.
 */
export function decodeRefusedFamilies(payload: unknown): readonly RefusableFamily[] | undefined {
  if (typeof payload !== "string" || payload.length === 0 || payload.length > 64) return undefined;
  const families: RefusableFamily[] = [];
  for (const part of payload.split(",")) {
    const family = CANONICAL_REFUSAL_ORDER.find((candidate) => candidate === part);
    if (family === undefined) return undefined;
    families.push(family);
  }
  // Канонический порядок и отсутствие повторов: ровно то, что получится после приведения к канону.
  const policy = refuse(families);
  const canonical = policy.kind === "refuse" ? encodeRefusedFamilies(policy.families) : "";
  return canonical === payload ? families : undefined;
}

// ---------------------------------------------------------------- события-рукопожатие и файлы-курьеры

/** Имена DOM-событий рукопожатия «курьер ⇄ MAIN-мир». Фиксированные и не секретные: события синхронные. */
export const CODEC_POLICY_ASK_EVENT = "fastiplayer-bridge:codec-policy-ask";
export const CODEC_POLICY_TELL_EVENT = "fastiplayer-bridge:codec-policy-tell";

/** Идентификатор динамического контент-скрипта-курьера (он всегда один). */
export const CARRIER_SCRIPT_ID = "fastiplayer-codec-policy";

/**
 * Все наборы отказов, для которых собирается свой файл-курьер: файл не принимает параметров, поэтому
 * набор «запекается» в него при сборке. 15 непустых подмножеств четырёх семейств.
 */
export function allCarrierRefusalSets(): readonly (readonly RefusableFamily[])[] {
  const sets: RefusableFamily[][] = [];
  for (let mask = 1; mask < 1 << CANONICAL_REFUSAL_ORDER.length; mask += 1) {
    sets.push(CANONICAL_REFUSAL_ORDER.filter((_, index) => (mask & (1 << index)) !== 0));
  }
  return sets;
}

/** Имя собранного файла-курьера для набора, например `codec-policy-av1-vp9.js`. */
export function carrierFileName(families: readonly RefusableFamily[]): string {
  return `codec-policy-${families.join("-")}.js`;
}
