// Решение «отказать ли в этой MIME-строке» — чистая функция от политики и строки.
import type { CodecPolicy } from "../../shared/codec-policy.ts";
import { isHlsMimeType, refusedFamilies } from "./mime-classifier.ts";

/**
 * Надо ли ответить «не поддерживается».
 *  - HLS — всегда, при любой политике (решение 64);
 *  - при `refuse` — если среди заказанных видео-кодеков есть названный в политике;
 *  - иначе — нет, и ответ остаётся за браузером.
 */
export function shouldRefuseMime(policy: CodecPolicy, mime: string): boolean {
  if (isHlsMimeType(mime)) return true;
  if (policy.kind === "transparent") return false;
  return refusedFamilies(mime, policy.families).length > 0;
}
