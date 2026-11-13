// Что страница видит у эфира HLS под тенью вместо настоящего источника (сессия 10a3, решение 232, п. 2–3).
//
// - `duration` = `+Infinity`: так эфир выглядит у настоящего элемента (MDN `HTMLMediaElement.duration`), и по
//   нему сайты рисуют «LIVE»;
// - `seekable` — окно `live_window` плеера во времени страницы; пока окна или сдвига часов нет, или у эфира нет
//   DVR, — пусто (как у браузера; Chromium 156 у нативного эфира `seekable` не заполняет вовсе);
// - `buffered` — от позиции страницы до края эфира. Почему не «всё окно»: `buffered` отвечает плееру сайта на
//   вопрос «есть ли данные дальше, можно ли играть», а прошлое окна плеер Fastiplayer не держит. До края эфира
//   данные честно будут (плеер качает их сам), дальше края их нет ни у кого. Края нет — пусто;
// - `networkState` = NETWORK_LOADING: эфир качается всё время (у записи-файла — «загружено», NETWORK_IDLE);
// - адрес, `srcObject` и размер кадра — из снимка элемента при передаче, как у тени файла.
//
// Эфир кончился (`recording_window`, 16d1): `duration` — конечный край записи во времени страницы (как у
// настоящего элемента после `ENDLIST`: зонд Chromium 156 — `durationchange` и конечная длительность),
// `buffered` — до края записи, `networkState` = NETWORK_IDLE (качать больше нечего, плеер дочитывает сам).

import type { MediaTimeMicros } from "../../../protocol/time.ts";
import type { ShadowSourceView } from "../file-source-mode.ts";
import type { ElementOriginals } from "../originals.ts";
import { createTimeRanges, type TimeRangesShape } from "../whole-file-ranges.ts";
import type { LiveHlsTimeSource, PageSpanUs } from "./live-hls-time-source.ts";

/** `HTMLMediaElement.NETWORK_LOADING`: браузер сейчас качает данные. */
const NETWORK_LOADING = 2;
/** `HTMLMediaElement.NETWORK_IDLE`: источник выбран, сейчас ничего не качается. */
const NETWORK_IDLE = 1;
const MICROS_PER_SECOND = 1_000_000;

/** Что виду нужно снаружи. */
export interface LiveViewInputs {
  readonly originals: ElementOriginals;
  /** Позиция страницы сейчас (модель тени), мкс. */
  readonly pagePositionUs: () => MediaTimeMicros;
  /** Отправить странице `durationchange`: эфир стал записью или край записи сменился. */
  readonly announceDurationChange: () => void;
}

export class LiveHlsSourceView implements ShadowSourceView {
  readonly #source: LiveHlsTimeSource;
  readonly #snapshot: ShadowSourceView;
  readonly #inputs: LiveViewInputs;

  constructor(source: LiveHlsTimeSource, snapshot: ShadowSourceView, inputs: LiveViewInputs) {
    this.#source = source;
    this.#snapshot = snapshot;
    this.#inputs = inputs;
  }

  duration(): number {
    const durationUs = this.#source.pageDurationUs();
    return durationUs === undefined ? Number.POSITIVE_INFINITY : durationUs / MICROS_PER_SECOND;
  }

  seekable(): TimeRangesShape {
    return this.#ranges(this.#source.pageSeekableSpanUs());
  }

  buffered(): TimeRangesShape {
    const edgeUs = this.#source.pageEdgeUs();
    if (edgeUs === undefined) return this.#ranges(undefined);
    const positionUs = this.#inputs.pagePositionUs();
    // Позиция бывает чуть дальше последнего известного края (интерполяция между окнами): промежуток
    // всё равно содержит позицию и не выворачивается.
    return this.#ranges([Math.min(positionUs, edgeUs), Math.max(positionUs, edgeUs)]);
  }

  networkState(): number {
    return this.#source.pageTimelineKind() === "recording" ? NETWORK_IDLE : NETWORK_LOADING;
  }

  currentSrc(): string {
    return this.#snapshot.currentSrc();
  }

  srcObject(): unknown {
    return this.#snapshot.srcObject();
  }

  videoWidth(): number {
    return this.#snapshot.videoWidth();
  }

  videoHeight(): number {
    return this.#snapshot.videoHeight();
  }

  /** TimeRanges страницы из промежутка в мкс (секунды — как у браузера). */
  #ranges(spanUs: PageSpanUs | undefined): TimeRangesShape {
    const span = spanUs === undefined ? undefined : ([spanUs[0] / MICROS_PER_SECOND, spanUs[1] / MICROS_PER_SECOND] as const);
    return createTimeRanges(span, this.#inputs.originals);
  }
}
