// Общий читатель языка из параметров адреса: `?lang=en`, `?language=pt-BR`. Подходит многим сайтам
// и сервисам субтитров, у которых язык — открытый параметр запроса. Подписи в адресе не бывает.

import { queryValueOf } from "./address-parts.ts";
import type { SubtitleNamingAdapter } from "./contract.ts";
import { languageOrNull } from "./language-tag.ts";

/** Имена параметров, в которых сайты кладут язык, в порядке предпочтения. */
const LANGUAGE_PARAMETER_NAMES: readonly string[] = ["lang", "language"];

export const genericAddressAdapter: SubtitleNamingAdapter = {
  name: "generic-address",
  nameByAddress(parts) {
    for (const name of LANGUAGE_PARAMETER_NAMES) {
      const language = languageOrNull(queryValueOf(parts, name));
      if (language !== null) return { language, label: null, auto_generated: false };
    }
    return null;
  },
};
