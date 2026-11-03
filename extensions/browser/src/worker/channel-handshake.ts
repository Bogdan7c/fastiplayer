// Сторона воркера рукопожатия личного канала (src/shared/worker-channel-contract.ts).
//
// Слушатель `message` на глобале воркера ставится ПЕРВЫМ делом хука, в фазе захвата, раньше любого
// кода сайта. Смотрится ТОЛЬКО первое сообщение воркера: обёртка MAIN-мира шлёт рукопожатие сразу
// после конструктора, раньше, чем сайт успеет что-то послать. Если первое сообщение — наше (ровно
// поле-метка нужной версии и ровно один порт), оно гасится `stopImmediatePropagation`, и сайт его
// не видит. Иначе слушатель всё равно снимается: сообщение уходит сайту нетронутым, а хук живёт без
// канала (поток не откроется — честный след). Подделка метки позже первой просто идёт сайту.

import { isWorkerChannelHandshake } from "../shared/worker-channel-contract.ts";
import type { ChannelOriginals } from "../page/page-channel.ts";
import type { OddityReporter } from "../page/prototype-wrap.ts";

/** Поставить слушатель первого сообщения. `onPort` получает порт канала. */
export function listenForWorkerChannel(scope: object, originals: ChannelOriginals, onPort: (port: object) => void, reportOddity: OddityReporter): void {
  const onFirstMessage = (event: unknown): void => {
    try {
      originals.removeListener(scope, "message", onFirstMessage, true);
      const port = handshakePortOf(originals, event);
      if (port === null) {
        reportOddity("worker channel handshake is missing");
        return;
      }
      originals.stopImmediatePropagation(event);
      onPort(port);
    } catch {
      reportOddity("worker channel handshake failed");
    }
  };
  originals.addListener(scope, "message", onFirstMessage, true);
}

/** Порт из события, если это наше рукопожатие; иначе `null`. */
function handshakePortOf(originals: ChannelOriginals, event: unknown): object | null {
  if (!isWorkerChannelHandshake(originals.eventData(event))) return null;
  const ports = originals.eventPorts(event) as { readonly length: number; readonly [index: number]: unknown } | null;
  if (ports === null || ports.length !== 1) return null;
  const port = ports[0];
  return typeof port === "object" && port !== null ? port : null;
}
