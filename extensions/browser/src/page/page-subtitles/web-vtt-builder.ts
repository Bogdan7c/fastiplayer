// Сборка документа WebVTT из реплик `TextTrack` (сессия 13b2).
//
// Нужна для дорожек без файла: hls.js и dash.js создают `TextTrack` через `addTextTrack` и кладут
// реплики сами — скачивать нечего. Реплики превращаются в обычный документ WebVTT, который плеер
// разбирает так же, как любой скачанный.
//
// Чистая функция без обращений к DOM: реплики сюда приходят уже как простые данные
// (`CueSnapshot`), прочитанные снятыми геттерами. Страница — враг, поэтому на выходе только то, что
// заведомо не сломает документ: время в `чч:мм:сс.ммм`, текст без пустых строк (пустая строка в WebVTT
// закрывает реплику) и без `-->`, настройки реплики из закрытого списка, идентификатор — только безопасный.

/** Реплика как простые данные. `settings` — только то, что отличается от умолчаний, уже проверенное читателем. */
export interface CueSnapshot {
  readonly id: string;
  readonly startSeconds: number;
  readonly endSeconds: number;
  readonly text: string;
  readonly settings: CueSettingsSnapshot;
}

/** Настройки реплики VTTCue; `null` — «не задано / по умолчанию». */
export interface CueSettingsSnapshot {
  readonly align: string | null;
  /** Число (строки или процент — смотри `lineIsPercent`) или `null` для `auto`. */
  readonly line: number | null;
  readonly lineIsPercent: boolean;
  readonly position: number | null;
  readonly size: number | null;
  readonly vertical: string | null;
}

export const NO_CUE_SETTINGS: CueSettingsSnapshot = { align: null, line: null, lineIsPercent: false, position: null, size: null, vertical: null };

/** Допустимые значения `align` и `vertical` из спецификации WebVTT; прочие отбрасываются. */
const ALLOWED_ALIGN_VALUES: readonly string[] = ["start", "center", "end", "left", "right"];
const ALLOWED_VERTICAL_VALUES: readonly string[] = ["rl", "lr"];

/** Идентификатор реплики длиннее этого в документ не идёт: настоящие — короткие номера и метки. */
const MAX_SAFE_CUE_ID_CHARS = 100;

const MILLIS_PER_SECOND = 1000;
const SECONDS_PER_MINUTE = 60;
const SECONDS_PER_HOUR = 3600;

/** Время в секундах → `чч:мм:сс.ммм`; `null` — число не годится (NaN, минус, бесконечность). */
export function formatCueTime(seconds: number): string | null {
  if (!Number.isFinite(seconds) || seconds < 0) return null;
  const totalMillis = Math.round(seconds * MILLIS_PER_SECOND);
  const hours = Math.floor(totalMillis / (SECONDS_PER_HOUR * MILLIS_PER_SECOND));
  const minutes = Math.floor(totalMillis / (SECONDS_PER_MINUTE * MILLIS_PER_SECOND)) % SECONDS_PER_MINUTE;
  const wholeSeconds = Math.floor(totalMillis / MILLIS_PER_SECOND) % SECONDS_PER_MINUTE;
  const millis = totalMillis % MILLIS_PER_SECOND;
  const pad = (value: number, width: number): string => String(value).padStart(width, "0");
  return `${pad(hours, 2)}:${pad(minutes, 2)}:${pad(wholeSeconds, 2)}.${pad(millis, 3)}`;
}

/**
 * Текст реплики для документа: переводы строк приведены к `\n`, пустые (и из одних пробелов) строки
 * выброшены, `-->` обезврежен. Пустой результат — реплику пропускаем.
 */
export function sanitizeCueText(text: string): string {
  const lines = text
    .split(/\r\n|\r|\n/)
    .filter((line) => line.trim() !== "")
    .map((line) => line.split("-->").join("--&gt;"));
  return lines.join("\n");
}

/** Идентификатор реплики, если он безопасен: непустой, короткий, без переводов строк и `-->`. */
function safeCueId(id: string): string | null {
  if (id === "" || id.length > MAX_SAFE_CUE_ID_CHARS) return null;
  if (/[\r\n]/.test(id) || id.includes("-->")) return null;
  return id;
}

/** Строка настроек (с ведущим пробелом) или пустая строка, если всё по умолчанию. */
function settingsSuffix(settings: CueSettingsSnapshot): string {
  const parts: string[] = [];
  if (settings.align !== null && ALLOWED_ALIGN_VALUES.includes(settings.align)) parts.push(`align:${settings.align}`);
  if (settings.line !== null && Number.isFinite(settings.line)) parts.push(`line:${settings.line}${settings.lineIsPercent ? "%" : ""}`);
  if (settings.position !== null && Number.isFinite(settings.position)) parts.push(`position:${settings.position}%`);
  if (settings.size !== null && Number.isFinite(settings.size)) parts.push(`size:${settings.size}%`);
  if (settings.vertical !== null && ALLOWED_VERTICAL_VALUES.includes(settings.vertical)) parts.push(`vertical:${settings.vertical}`);
  return parts.length === 0 ? "" : ` ${parts.join(" ")}`;
}

/** Блок одной реплики; `null` — реплика непригодна (время, пустой текст). */
function cueBlock(cue: CueSnapshot): string | null {
  const start = formatCueTime(cue.startSeconds);
  const end = formatCueTime(cue.endSeconds);
  const text = sanitizeCueText(cue.text);
  if (start === null || end === null || cue.endSeconds <= cue.startSeconds || text === "") return null;
  const id = safeCueId(cue.id);
  const header = `${start} --> ${end}${settingsSuffix(cue.settings)}`;
  return `${id === null ? "" : `${id}\n`}${header}\n${text}`;
}

/** Сколько реплик вошло в документ и сам документ. */
export interface BuiltWebVtt {
  readonly cueCount: number;
  readonly text: string;
}

/** Собрать документ WebVTT. Непригодные реплики пропускаются; `cueCount` — сколько вошло. */
export function buildWebVttFromCues(cues: readonly CueSnapshot[]): BuiltWebVtt {
  const blocks: string[] = [];
  for (const cue of cues) {
    const block = cueBlock(cue);
    if (block !== null) blocks.push(block);
  }
  const body = blocks.length === 0 ? "" : `${blocks.join("\n\n")}\n\n`;
  return { cueCount: blocks.length, text: `WEBVTT\n\n${body}` };
}
