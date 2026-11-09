// Чтение тела ответа с потолком: копим куски, на превышении отменяем поток и бросаем всё.

import { viewOfAppendSource, type BinaryIntrinsics } from "../binary-view.ts";
import type { ByteTools } from "./byte-tools.ts";
import type { BodyReader } from "./response-readers.ts";

/**
 * Прочитать тело целиком, но не больше `maxBytes`. `null` — тело длиннее потолка (поток отменён),
 * не байты в потоке или чтение сорвалось: документа нет, странице ничего не сообщается.
 */
export async function readBodyWithLimit(reader: BodyReader, binary: BinaryIntrinsics, tools: ByteTools, maxBytes: number): Promise<Uint8Array | null> {
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const step = await reader.read();
      if (step.done) break;
      const chunk = viewOfAppendSource(binary, step.value);
      if (chunk === null) {
        reader.cancel();
        return null;
      }
      total += tools.lengthOf(chunk);
      if (total > maxBytes) {
        // Потолок: дальше не читаем и не копим — отменяем поток, чтобы сеть и память отпустили остаток.
        reader.cancel();
        return null;
      }
      chunks.push(chunk);
    }
  } catch {
    return null;
  }
  return total === 0 ? null : tools.join(chunks, total);
}
