// Адаптер timedtext YouTube (решение 276): `/api/timedtext?...&lang=en&kind=asr&tlang=de&fmt=json3`.
//
// Что значат параметры:
// - `lang` — язык исходной дорожки; `tlang` — язык АВТОПЕРЕВОДА, на который YouTube переводит её на лету.
//   Зритель видит текст на `tlang`, поэтому именно он называет дорожку, если задан;
// - `kind=asr` — дорожку распознал сам YouTube (автосубтитры). Для автоперевода тоже `true`: исходник
//   всё равно машинный или перевод машинный — зрителю важно отличить от ручных субтитров автора;
// - подписи в адресе нет: фон допишет «English (auto-generated)» из языка и флага.

import { queryValueOf } from "./address-parts.ts";
import type { SubtitleNamingAdapter } from "./contract.ts";
import { languageOrNull } from "./language-tag.ts";

/** Корневые домены YouTube; поддомены (`www.`, `m.`, `music.`) тоже годятся. */
const YOUTUBE_ROOT_DOMAINS: readonly string[] = ["youtube.com", "youtube-nocookie.com"];

/** Путь, по которому плеер YouTube качает субтитры. */
const TIMEDTEXT_PATH = "/api/timedtext";

/** Значение `kind`, которым YouTube помечает автосубтитры (automatic speech recognition). */
const AUTO_GENERATED_KIND = "asr";

function isYoutubeHost(host: string): boolean {
  return YOUTUBE_ROOT_DOMAINS.some((root) => host === root || host.endsWith(`.${root}`));
}

export const youtubeTimedtextAdapter: SubtitleNamingAdapter = {
  name: "youtube-timedtext",
  nameByAddress(parts) {
    if (!isYoutubeHost(parts.host) || parts.path !== TIMEDTEXT_PATH) return null;
    const translated = languageOrNull(queryValueOf(parts, "tlang"));
    const language = translated ?? languageOrNull(queryValueOf(parts, "lang"));
    return { language, label: null, auto_generated: queryValueOf(parts, "kind") === AUTO_GENERATED_KIND };
  },
};
