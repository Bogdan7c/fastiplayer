// Версия протокола и договорённость о ней (решение 86). Зеркало version.rs.
//
// Правило «договорились - значит строго»: версия - пара major.minor; общая версия -
// тот же major и меньшая из двух minor; разные major - отказ «обновите» с
// указанием, кого обновлять. Форма `hello` и отказа по версии не меняется никогда.
//
// Minor только добавляет. Каждое сообщение знает, в какой версии появилось
// (`session/introduced-in.ts`): сторона не шлёт того, чего нет в общей версии сеанса.
// 1.0 - MSE, файл, часы, команды, реклама; 1.1 (сессия 10a1, решение 228) - нативный
// HLS по номеркам: `hls_source_added`, `fetch_resource`, `resource_fetch_finished`;
// 1.2 (сессия 10a3, решение 232) - HLS live: `live_window` (окно эфира от плеера).
// VOD HLS по-прежнему хватает 1.1.
// 1.3 (сессия 11a, решение 237) - новых сообщений нет, новое ЗНАЧЕНИЕ: причина
// `tab_gone{page_player_failed}` (плеер сайта сломался во время передачи). Значение новее
// общей версии сторона не шлёт: экстеншен со старым плеером говорит `navigated_away`.
// 1.4 (сессия 11b1, решения 240-241) - «возврат вкладки»: страница -> плеер `page_left`,
// `page_returned`, `page_reopen_failed`; плеер -> страница `reopen_page` (договор -
// `session/page-reopen.ts`). `tab_gone` не менялся, коды закрытия не менялись.
// 1.5 (сессия 13b1, решения 270-274) - субтитры страницы: страница -> плеер
// `subtitle_track_offered` и байты документа двоичной рамкой вида 3 (договор -
// `session/page-subtitles.ts`). При общей версии ниже 1.5 экстеншен субтитры не шлёт.
// 1.6 (сессия 13c, решение 278) - «показать страницу»: плеер -> страница `show_page` (вкладка сеанса
// становится активной, окно просят выйти вперёд и подсвечивают). Шлёт его плеер, и только при общей версии 1.6.
// 1.7 (сессия 14a, решения 279-283) - варианты страницы в окне плеера: страница -> плеер `page_navigation`
// (есть ли следующая серия) и `subtitle_preference` (запомненный выбор субтитров); плеер -> страница
// `subtitle_choice` (выбор зрителя). Сообщения 1.0 `variant_menu_changed`, `select_variant`, `next_episode`
// не менялись. Договор - `session/page-navigation.ts` и `session/page-subtitles.ts`.
// 1.8 (сессия 15c2, решение 301) - исход выбора на странице: страница -> плеер `variant_outcome` (чем кончился
// выбор зрителя: готово, у озвучки нет серии, только человеком, сбой). Договор - `session/variant-outcome.ts`.
// 1.9 (сессия 16a, решения 303, 306-308) - смена серии на той же странице: страница -> плеер
// `awaiting_next_episode` (ждём новый источник после конца серии) и `page_title_changed` (заголовок вкладки
// сменился), код закрытия 4011 `next_episode_not_started` (новый источник не пришёл за срок; со старым плеером -
// прежний 4009). Договор - `session/episode-change.ts`.

import { plainInteger } from "./numbers.ts";
import { type Codec, stringEnum, struct } from "./schema.ts";

/** Версия протокола: пара `major.minor`. */
export interface ProtocolVersion {
  readonly major: number;
  readonly minor: number;
}

/** Первая версия (сессия 01): MSE, файл `<video src>`, часы, команды, реклама. */
export const PROTOCOL_VERSION_1_0: ProtocolVersion = Object.freeze({ major: 1, minor: 0 });

/** Нативный HLS по номеркам ресурсов (сессия 10a1, решение 228). */
export const PROTOCOL_VERSION_1_1: ProtocolVersion = Object.freeze({ major: 1, minor: 1 });

/** HLS live: окно эфира `live_window` (сессия 10a3, решение 232). */
export const PROTOCOL_VERSION_1_2: ProtocolVersion = Object.freeze({ major: 1, minor: 2 });

/** Причина `tab_gone{page_player_failed}` (сессия 11a, решение 237). */
export const PROTOCOL_VERSION_1_3: ProtocolVersion = Object.freeze({ major: 1, minor: 3 });

/** Возврат вкладки: `page_left`, `page_returned`, `page_reopen_failed`, `reopen_page` (сессия 11b1, решения 240-241). */
export const PROTOCOL_VERSION_1_4: ProtocolVersion = Object.freeze({ major: 1, minor: 4 });

/** Субтитры страницы: `subtitle_track_offered` и рамка вида 3 (сессия 13b1, решения 270-274). */
export const PROTOCOL_VERSION_1_5: ProtocolVersion = Object.freeze({ major: 1, minor: 5 });

/** «Показать страницу»: `show_page` (сессия 13c, решение 278). */
export const PROTOCOL_VERSION_1_6: ProtocolVersion = Object.freeze({ major: 1, minor: 6 });

/** Варианты страницы в окне плеера: `page_navigation`, `subtitle_preference`, `subtitle_choice` (сессия 14a). */
export const PROTOCOL_VERSION_1_7: ProtocolVersion = Object.freeze({ major: 1, minor: 7 });

/** Исход выбора на странице: `variant_outcome` (сессия 15c2, решение 301). */
export const PROTOCOL_VERSION_1_8: ProtocolVersion = Object.freeze({ major: 1, minor: 8 });

/** Смена серии на той же странице: `awaiting_next_episode`, `page_title_changed`, код 4011 (сессия 16a). */
export const PROTOCOL_VERSION_1_9: ProtocolVersion = Object.freeze({ major: 1, minor: 9 });

/** Версия протокола, которую реализует этот экстеншен. */
export const CURRENT_PROTOCOL_VERSION: ProtocolVersion = PROTOCOL_VERSION_1_9;

/**
 * Входит ли в версию `agreed` (обычно - общую версию сеанса) то, что появилось в
 * `introducedIn`: тот же major и не меньшая minor. Зеркало `ProtocolVersion::includes`.
 */
export function versionIncludes(agreed: ProtocolVersion, introducedIn: ProtocolVersion): boolean {
  return agreed.major === introducedIn.major && agreed.minor >= introducedIn.minor;
}

/** Какую из сторон моста нужно обновить. */
export type BridgeSoftware = "extension" | "player";

export const bridgeSoftwareCodec: Codec<BridgeSoftware> = stringEnum<BridgeSoftware>(["extension", "player"]);

export const protocolVersionCodec: Codec<ProtocolVersion> = struct<ProtocolVersion>({
  major: plainInteger("u16"),
  minor: plainInteger("u16"),
});

/** Итог договорённости о версии. */
export type VersionNegotiation =
  | { readonly ok: true; readonly version: ProtocolVersion }
  | { readonly ok: false; readonly update: BridgeSoftware };

/**
 * Договаривается об общей версии. Отстал `major` у плеера - обновить плеер,
 * отстал у экстеншена - обновить экстеншен. Один `major` - меньшая из `minor`.
 */
export function negotiateProtocolVersion(player: ProtocolVersion, extension: ProtocolVersion): VersionNegotiation {
  if (player.major < extension.major) {
    return { ok: false, update: "player" };
  }
  if (player.major > extension.major) {
    return { ok: false, update: "extension" };
  }
  return { ok: true, version: { major: player.major, minor: Math.min(player.minor, extension.minor) } };
}
