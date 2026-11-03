// Точка входа файлов-курьеров. Один и тот же код собирается 15 раз; esbuild подставляет вместо
// `__CARRIER_REFUSED_FAMILIES__` строку отказываемых семейств конкретного файла (см. scripts/build.ts).
import { decodeRefusedFamilies } from "../shared/codec-policy.ts";
import { startCodecCarrier } from "./codec-carrier.ts";

declare const __CARRIER_REFUSED_FAMILIES__: string;

const families = decodeRefusedFamilies(__CARRIER_REFUSED_FAMILIES__);
if (families !== undefined) {
  // Отменяемое: MAIN-мир, приняв политику, отменяет «tell» — курьер перестаёт слушать «ask».
  startCodecCarrier(document, families, (type, payload) => new CustomEvent(type, { detail: payload, cancelable: true }));
}
