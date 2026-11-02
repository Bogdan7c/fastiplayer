// Версия протокола и договорённость о ней (решение 86). Зеркало version.rs.
//
// Правило «договорились - значит строго»: версия - пара major.minor; общая версия -
// тот же major и меньшая из двух minor; разные major - отказ «обновите» с
// указанием, кого обновлять. Форма `hello` и отказа по версии не меняется никогда.

import { plainInteger } from "./numbers.ts";
import { type Codec, stringEnum, struct } from "./schema.ts";

/** Версия протокола: пара `major.minor`. */
export interface ProtocolVersion {
  readonly major: number;
  readonly minor: number;
}

/** Версия протокола, которую реализует этот экстеншен. */
export const CURRENT_PROTOCOL_VERSION: ProtocolVersion = Object.freeze({ major: 1, minor: 0 });

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
