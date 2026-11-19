// «Раздел» адреса ленты коротких видео (сессия 16b3, правило F5): форма пути без идентификаторов роликов.
//
// Зачем. Лента меняет адрес на каждом ролике (Instagram `/reels/<id>/`, Shorts `/shorts/<id>`), а уход в другой
// раздел сайта — тоже смена адреса (`/explore/`, `/direct/inbox/`, главная `/`). Отличить одно от другого можно
// только по форме пути: у шага ленты меняется лишь идентификатор ролика, у ухода — сами слова пути.
//
// Правило. Путь режется на сегменты; сегмент-идентификатор ВЫБРАСЫВАЕТСЯ (подстановка «что угодно, в том числе
// ничего»), остальные сравниваются как есть. Идентификатор:
// - длинный код `[A-Za-z0-9_-]{8,}` (Instagram `C7xYz1AbCdE`, YouTube `dQw4w9WgXcQ`);
// - число (TikTok `7350123456789012345`);
// - имя `@кто-то` (профиль TikTok `/@user/video/<id>`).
// Выбрасываем, а не держим место: открытая лента Instagram — `/reels/`, а после первого шага — `/reels/<id>/`;
// с подстановкой «на своём месте» это был бы «другой раздел».
// Хост входит в раздел: другой сайт — тем более другой раздел. Запрос (`?…`) и хеш не входят.
//
// Известные пределы (записаны в отчёте 16b3):
// - длинное СЛОВО пути (`/following`, `/subscriptions`) тоже похоже на идентификатор и выбрасывается: уход
//   `/foryou` → `/following` раздел видит (`foryou` остаётся), а `/feed/subscriptions` → `/feed/history` — нет;
// - лента внутри профиля TikTok, где рядом с роликами лежат фото-посты (`/@user/photo/<id>`): `photo` и
//   `video` — разные слова, шаг на фото-пост выглядит уходом из раздела.

/** Раздел адреса: хост и слова пути без идентификаторов. Сравнивается только целиком. */
export type FeedSection = string & { readonly __brand: "FeedSection" };

/** Длинный код ролика: буквы, цифры, `_` и `-`, не короче 8 знаков. */
const LONG_IDENTIFIER = /^[A-Za-z0-9_-]{8,}$/;
/** Числовой идентификатор любой длины. */
const NUMERIC_IDENTIFIER = /^\d+$/;
/** Имя автора: `@` и что угодно дальше. */
const AUTHOR_HANDLE = /^@./;

/** Сегмент — подстановка (идентификатор ролика, число или имя автора)? */
export function isIdentifierSegment(segment: string): boolean {
  return LONG_IDENTIFIER.test(segment) || NUMERIC_IDENTIFIER.test(segment) || AUTHOR_HANDLE.test(segment);
}

/**
 * Раздел адреса. `undefined` — адрес не разобрать (браузер дал не URL): о разделе судить нельзя, и правило F5
 * для такого адреса молчит, как будто раздел не менялся.
 */
export function feedSectionOf(url: string): FeedSection | undefined {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return undefined;
  }
  const literalSegments = parsed.pathname.split("/").filter((segment) => segment !== "" && !isIdentifierSegment(segment));
  return `${parsed.host}/${literalSegments.join("/")}` as FeedSection;
}
