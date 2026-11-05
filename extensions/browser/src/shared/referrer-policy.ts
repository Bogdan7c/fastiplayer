// Политика Referer закачки файла `<video src>` (решения 202, находка 6 ревью 09c): фон представляется
// браузером, поэтому считает заголовок так же, как браузер считал бы его для этого адреса, но никогда
// не отдаёт больше сайта-источника. Путь страницы не уходит даже при `unsafe-url`.
//
// Чистые функции без браузера: MAIN-мир определяет политику документа, фон считает значение на каждый
// адрес запроса (адрес после перенаправления может быть уже с другого сайта).
//
// Честное ограничение: заголовок ответа `Referrer-Policy` из страницы не виден. Сайт, задавший политику
// только им, получит политику по умолчанию, как будто заголовка нет.

/** Политики Referer из стандарта Referrer Policy. */
export type ReferrerPolicyName =
  | "no-referrer"
  | "no-referrer-when-downgrade"
  | "same-origin"
  | "origin"
  | "strict-origin"
  | "origin-when-cross-origin"
  | "strict-origin-when-cross-origin"
  | "unsafe-url";

export const REFERRER_POLICIES: readonly ReferrerPolicyName[] = [
  "no-referrer",
  "no-referrer-when-downgrade",
  "same-origin",
  "origin",
  "strict-origin",
  "origin-when-cross-origin",
  "strict-origin-when-cross-origin",
  "unsafe-url",
];

/** Политика браузеров, когда документ своей не задал. */
export const DEFAULT_REFERRER_POLICY: ReferrerPolicyName = "strict-origin-when-cross-origin";

/** Старые значения `<meta name=referrer>`, которые стандарт HTML приравнивает к новым. */
const LEGACY_META_POLICIES: Readonly<Record<string, ReferrerPolicyName>> = {
  never: "no-referrer",
  default: DEFAULT_REFERRER_POLICY,
  always: "unsafe-url",
  "origin-when-crossorigin": "origin-when-cross-origin",
};

/** Что нужно для расчёта заголовка. */
export interface FileReferrer {
  /** Сайт-источник документа (`location.origin`, без `/` в конце). */
  readonly origin: string;
  readonly policy: ReferrerPolicyName;
}

/** Значение атрибута `referrerpolicy`; незнакомое браузер считает отсутствующим. */
function parseAttributePolicy(raw: string | null): ReferrerPolicyName | undefined {
  const token = raw?.trim().toLowerCase();
  return REFERRER_POLICIES.find((policy) => policy === token);
}

function parseMetaPolicy(raw: string): ReferrerPolicyName | undefined {
  return parseAttributePolicy(raw) ?? LEGACY_META_POLICIES[raw.trim().toLowerCase()];
}

/**
 * Политика для файла: атрибут элемента важнее документа (решение 202), в документе побеждает последний
 * годный `<meta name=referrer>`, незнакомые значения не меняют уже действующую политику.
 */
export function effectiveReferrerPolicy(elementAttribute: string | null, metaContents: readonly string[]): ReferrerPolicyName {
  const fromElement = parseAttributePolicy(elementAttribute);
  if (fromElement !== undefined) return fromElement;
  let policy = DEFAULT_REFERRER_POLICY;
  for (const content of metaContents) policy = parseMetaPolicy(content) ?? policy;
  return policy;
}

function originOfUrl(url: string): string | null {
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

/** Значение Referer для запроса `url`; `null` — браузер на таком запросе Referer не послал бы. */
export function refererValueFor(url: string, referrer: FileReferrer): string | null {
  const value = `${referrer.origin}/`;
  const isDowngrade = referrer.origin.startsWith("https:") && !url.startsWith("https:");
  switch (referrer.policy) {
    case "no-referrer":
      return null;
    case "same-origin":
      return originOfUrl(url) === referrer.origin ? value : null;
    case "origin":
    case "origin-when-cross-origin":
    case "unsafe-url":
      return value;
    case "strict-origin":
    case "strict-origin-when-cross-origin":
    case "no-referrer-when-downgrade":
      return isDowngrade ? null : value;
  }
}
