// Договор памяти выбора (сессия 14b2, решения 44, 281, 284, 285): что лежит в `storage.local` и как страница
// настроек просит фон о памяти.
//
// Ключ записи — отпечаток SHA-256 «сайт + сериал» (`background/choice-memory/series-fingerprint.ts`): по
// хранилищу нельзя прочитать, какие сериалы смотрел человек. В самой записи — подписи вариантов так, как их
// пишет сайт («Dub B», «720p»), субтитры и время последнего касания: без них память не работала бы.
//
// Хранилищу не доверяем слепо: битая запись выбрасывается целиком, битое поле записи — только это поле.
// Пишет память только фон (`choice-memory-store.ts`); страница настроек читает и просит фон стереть.

/** Ключ памяти в `storage.local`. */
export const REMEMBERED_CHOICES_KEY = "rememberedChoices";

/** Версия формы документа памяти: незнакомая версия читается как пустая память. */
export const REMEMBERED_CHOICES_VERSION = 1;

/** Отпечаток сериала: 64 строчных шестнадцатеричных знака SHA-256. */
export type SeriesFingerprint = string;

/** Длина подписи, которую память согласна хранить (подписи протокола короче — 256 байт). */
export const MAX_REMEMBERED_LABEL_LENGTH = 512;

/** Языковой тег дорожки субтитров (BCP 47 у протокола короткий). */
export const MAX_REMEMBERED_LANGUAGE_LENGTH = 64;

/** Запомненные субтитры: дорожка по языку и подписи или «выключены». */
export type RememberedSubtitles =
  | { readonly kind: "off" }
  | { readonly kind: "track"; readonly language: string | null; readonly label: string | null };

/** Что помнит память об одном сериале. Любое поле выбора может отсутствовать: его ещё не выбирали. */
export interface RememberedChoices {
  /** Подпись озвучки, как её пишет сайт. */
  readonly audio?: string;
  /** Подпись качества, как её пишет сайт. «Auto» сайта сюда не попадает (решение 284). */
  readonly quality?: string;
  readonly subtitles?: RememberedSubtitles;
  /** Когда запись последний раз писали или применяли (мс от эпохи): по нему вытесняются старые. */
  readonly used_at: number;
}

/** Изменение записи: какие поля выбора заменить. */
export interface RememberedChoicePatch {
  readonly audio?: string;
  readonly quality?: string;
  readonly subtitles?: RememberedSubtitles;
}

/** Документ памяти целиком. */
export interface RememberedChoicesDocument {
  readonly version: typeof REMEMBERED_CHOICES_VERSION;
  readonly series: Readonly<Record<SeriesFingerprint, RememberedChoices>>;
}

export const EMPTY_REMEMBERED_CHOICES: RememberedChoicesDocument = { version: REMEMBERED_CHOICES_VERSION, series: {} };

const FINGERPRINT_PATTERN = /^[0-9a-f]{64}$/;

export function isSeriesFingerprint(value: unknown): value is SeriesFingerprint {
  return typeof value === "string" && FINGERPRINT_PATTERN.test(value);
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isRememberedLabel(value: unknown): value is string {
  return typeof value === "string" && value.length >= 1 && value.length <= MAX_REMEMBERED_LABEL_LENGTH;
}

function nullableText(value: unknown, maxLength: number): string | null | undefined {
  if (value === null) return null;
  return typeof value === "string" && value.length >= 1 && value.length <= maxLength ? value : undefined;
}

/** Субтитры записи; негодная форма — `undefined` (поле выбрасывается). */
export function parseRememberedSubtitles(value: unknown): RememberedSubtitles | undefined {
  if (!isPlainRecord(value)) return undefined;
  if (value["kind"] === "off") return { kind: "off" };
  if (value["kind"] !== "track") return undefined;
  const language = nullableText(value["language"], MAX_REMEMBERED_LANGUAGE_LENGTH);
  const label = nullableText(value["label"], MAX_REMEMBERED_LABEL_LENGTH);
  // Дорожка без языка и без подписи неотличима от любой другой: такую не помним.
  if (language === undefined || label === undefined || (language === null && label === null)) return undefined;
  return { kind: "track", language, label };
}

/** Одна запись; без годного времени — `undefined` (запись выбрасывается: её нельзя честно вытеснить). */
export function parseRememberedChoices(value: unknown): RememberedChoices | undefined {
  if (!isPlainRecord(value)) return undefined;
  const usedAt = value["used_at"];
  if (typeof usedAt !== "number" || !Number.isSafeInteger(usedAt) || usedAt < 0) return undefined;
  const audio = isRememberedLabel(value["audio"]) ? value["audio"] : undefined;
  const quality = isRememberedLabel(value["quality"]) ? value["quality"] : undefined;
  const subtitles = parseRememberedSubtitles(value["subtitles"]);
  return {
    ...(audio === undefined ? {} : { audio }),
    ...(quality === undefined ? {} : { quality }),
    ...(subtitles === undefined ? {} : { subtitles }),
    used_at: usedAt,
  };
}

/** Документ памяти из хранилища: не объект, чужая версия — пустая память; битые записи выбрасываются. */
export function parseRememberedChoicesDocument(stored: unknown): RememberedChoicesDocument {
  if (!isPlainRecord(stored) || stored["version"] !== REMEMBERED_CHOICES_VERSION || !isPlainRecord(stored["series"])) {
    return EMPTY_REMEMBERED_CHOICES;
  }
  const series: Record<SeriesFingerprint, RememberedChoices> = {};
  for (const [fingerprint, entry] of Object.entries(stored["series"])) {
    if (!isSeriesFingerprint(fingerprint)) continue;
    const choices = parseRememberedChoices(entry);
    if (choices !== undefined) series[fingerprint] = choices;
  }
  return { version: REMEMBERED_CHOICES_VERSION, series };
}

// ---------------------------------------------------------------- страница настроек → фон

export const FORGET_REMEMBERED_CHOICES_KIND = "forget_remembered_choices";
export const COUNT_REMEMBERED_CHOICES_KIND = "count_remembered_choices";

/** Просьба страницы настроек к фону: стереть память или сказать, сколько сериалов в ней. */
export type ChoiceMemoryRequest = { readonly kind: typeof FORGET_REMEMBERED_CHOICES_KIND } | { readonly kind: typeof COUNT_REMEMBERED_CHOICES_KIND };

/** Ответ фона. `failed` — хранилище отказало; причина — для текста на странице настроек. */
export type ChoiceMemoryReply =
  | { readonly kind: "forgotten" }
  | { readonly kind: "count"; readonly series: number }
  | { readonly kind: "failed"; readonly reason: string };

/** Строгая форма просьбы: ровно `kind` одного из двух видов. */
export function parseChoiceMemoryRequest(message: unknown): ChoiceMemoryRequest | undefined {
  if (!isPlainRecord(message) || Object.keys(message).length !== 1) return undefined;
  const kind = message["kind"];
  if (kind === FORGET_REMEMBERED_CHOICES_KIND || kind === COUNT_REMEMBERED_CHOICES_KIND) return { kind };
  return undefined;
}

/** Ответ фона по форме (разбирает страница настроек); чужое — `undefined`. */
export function parseChoiceMemoryReply(reply: unknown): ChoiceMemoryReply | undefined {
  if (!isPlainRecord(reply)) return undefined;
  if (reply["kind"] === "forgotten") return { kind: "forgotten" };
  if (reply["kind"] === "count") {
    const series = reply["series"];
    return typeof series === "number" && Number.isSafeInteger(series) && series >= 0 ? { kind: "count", series } : undefined;
  }
  if (reply["kind"] === "failed" && typeof reply["reason"] === "string") return { kind: "failed", reason: reply["reason"] };
  return undefined;
}
