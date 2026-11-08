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

/** Версия протокола, которую реализует этот экстеншен. */
export const CURRENT_PROTOCOL_VERSION: ProtocolVersion = PROTOCOL_VERSION_1_4;

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
