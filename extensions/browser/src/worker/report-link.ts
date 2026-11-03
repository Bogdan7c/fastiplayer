// Отправка донесений хука воркера в MAIN-мир по личному порту.
//
// Порт приходит не сразу: сообщение-рукопожатие обёртки встаёт в очередь воркера раньше любого
// сообщения сайта, но браузер раздаёт сообщения только ПОСЛЕ того, как скрипт воркера отработал.
// А сайт мог уже в первой строке скрипта отдать handle своего MediaSource (`postMessage`). Поэтому
// донесения до порта копятся в короткой очереди и уходят первыми, как только порт принят.
// Элементы потоков в очередь не попадают: поток открывает только MAIN-мир, то есть уже после порта.

import type { WorkerReport } from "../shared/worker-channel-contract.ts";
import type { ChannelOriginals } from "../page/page-channel.ts";
import type { OddityReporter } from "../page/prototype-wrap.ts";

/**
 * Сколько донесений держим до прихода порта. Хук до порта шлёт лишь донесения о handle и начале
 * MediaSource — единицы; предел только не даёт враждебному коду сайта раздуть память воркера.
 */
const MAX_REPORTS_BEFORE_PORT = 256;

/** Отправка донесения; `transfer` — буферы, которые передаются, а не копируются. */
export interface ReportSink {
  send(report: WorkerReport, transfer: readonly Transferable[]): void;
}

export class WorkerReportLink implements ReportSink {
  readonly #originals: ChannelOriginals;
  readonly #reportOddity: OddityReporter;
  #port: object | null = null;
  readonly #pending: WorkerReport[] = [];

  constructor(originals: ChannelOriginals, reportOddity: OddityReporter) {
    this.#originals = originals;
    this.#reportOddity = reportOddity;
  }

  /** Порт есть (рукопожатие принято). */
  get isConnected(): boolean {
    return this.#port !== null;
  }

  /** Принять порт: первым уходит `hook_ready`, затем накопленные донесения по порядку. */
  adopt(port: object): void {
    this.#port = port;
    this.#post({ kind: "hook_ready" }, []);
    for (const report of this.#pending.splice(0)) this.#post(report, []);
  }

  /** Отправить или (до порта) отложить. Сбой отправки бросается вызывающему. */
  send(report: WorkerReport, transfer: readonly Transferable[]): void {
    if (this.#port !== null) {
      this.#post(report, transfer);
      return;
    }
    if (this.#pending.length >= MAX_REPORTS_BEFORE_PORT) {
      this.#reportOddity("too many worker reports before the channel port");
      return;
    }
    this.#pending.push(report);
  }

  #post(report: WorkerReport, transfer: readonly Transferable[]): void {
    if (this.#port === null) return;
    this.#originals.postToPort(this.#port, report, transfer);
  }
}
