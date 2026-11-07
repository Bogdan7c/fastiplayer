// Общее тело страницы «нативный HLS LIVE в <video src>» (сессия 10a3).
//
// Эфир делает маршрут стенда `/cdn/hls-live/<прогон>/stream.m3u8` (src/site/hls_live.rs): скользящее
// окно из 4 сегментов ролика `hls-ts-live`, без `EXT-X-ENDLIST`, `MEDIA-SEQUENCE` растёт по часам.
// Часы эфира стартуют с первого запроса плейлиста этого прогона.
//
// Что видит и проверяет страница:
//  * `video.duration === Infinity` (Infinity нельзя класть в метрики — `bench-report.js` их
//    отвергает, поэтому факт идёт в `context.check` и в текст `String(video.duration)`);
//  * `video.seekable`: страница ведёт выборки start/end. ФАКТ разведки (Chromium 156): у НАТИВНОГО
//    элемента на скользящем плейлисте `seekable` ПУСТ (length 0) всё время, поэтому до передачи
//    страница его только фиксирует (метрика `native_seekable_ranges`), а не требует;
//  * под тенью «в эфире» (команды робота `hook:*`): `duration` всё ещё Infinity, а вот `seekable`
//    обязана стать непустым окном (его подставляет тень по сообщению плеера) и конец окна
//    растёт вместе с эфиром, часы страницы идут;
//  * после возврата (закрыли окно плеера): настоящий элемент снова играет. «Стоит у края эфира»
//    для нативного элемента доказывает судья по журналу сервера (он грузит окно, актуальное
//    на момент возврата, а не сегменты с «старой секунды»): свой `currentTime` нативный элемент
//    отсчитывает от начала окна загрузки, и `seekable` у него пуст.
//
// Команды робота (в режиме передачи):
//   hook:mark-live          сразу после передачи: запомнить конец seekable и часы страницы
//   hook:expect-live-window спустя время под тенью: эфир идёт, окно едет
//   hook:mark-before-return перед закрытием окна плеера: запомнить «старую секунду» страницы
//   hook:expect-returned-live после закрытия: элемент у края эфира и играет сам, без play страницы (решение 234)

import { playNativeHlsAsset } from './native-hls-page.js';
import { waitFor } from './bench-checks.js';

/** Период выборок seekable, мс. */
const SAMPLE_PERIOD_MS = 1000;
/** Конец seekable под тенью обязан вырасти не меньше этой доли времени наблюдения… */
const MIN_SHADOW_ADVANCE_RATIO = 0.4;
/** …но не требуем больше этого, с (ролик 120 с; после его конца эфир замирает). */
const MAX_REQUIRED_SHADOW_ADVANCE_S = 8;
/** Сколько ждём, пока вернувшийся элемент загрузится и заиграет, мс (меньше ожидания ack робота). */
const RETURN_WAIT_MS = 15000;
/** Сколько секунд часов после возврата считаем «играет», с. */
const RETURN_MIN_ADVANCE_S = 1;
/** Сколько выборок хранить (час эфира). */
const MAX_SAMPLES = 3600;
/** Раз в сколько выборок писать строку в журнал. */
const LOG_EVERY_SAMPLES = 5;
/** События элемента, которые пишем в журнал со значениями (порядок событий — факт для экстеншена). */
const LOGGED_EVENTS = [
  'loadstart', 'durationchange', 'loadedmetadata', 'canplay', 'play', 'playing', 'pause', 'waiting', 'stalled',
  'error', 'ended', 'emptied', 'seeking', 'seeked',
];

/** Окно seekable как число-пара (или null, если окна нет). */
function seekableWindow(video) {
  const { seekable } = video;
  if (seekable.length === 0) return null;
  return { start: seekable.start(0), end: seekable.end(seekable.length - 1) };
}

/** Строка «что видит страница» для журнала. */
function describe(video) {
  const window = seekableWindow(video);
  const range = window ? `${window.start.toFixed(2)}..${window.end.toFixed(2)}` : 'none';
  return `duration=${String(video.duration)} currentTime=${video.currentTime.toFixed(2)} seekable=${range} paused=${video.paused}${video.error ? ` error=${video.error.code} '${video.error.message}'` : ''}`;
}

/** Выборки seekable по таймеру и журнал событий элемента. */
class LiveObserver {
  constructor(context, video) {
    this.context = context;
    this.video = video;
    this.samples = [];
    this.markLive = null;
    this.markBeforeReturn = null;
  }

  start() {
    for (const name of LOGGED_EVENTS) {
      this.video.addEventListener(name, () => this.context.log(`event ${name}: ${describe(this.video)}`));
    }
    // Видимость вкладки: окно плеера поверх браузера может её «скрыть», а скрытому видео Chromium
    // отключает видеодорожку — это важно для разбора возврата.
    document.addEventListener('visibilitychange', () => this.context.log(`visibility ${document.visibilityState}: ${describe(this.video)}`));
    setInterval(() => this.#sample(), SAMPLE_PERIOD_MS);
    this.#sample();
  }

  #sample() {
    const window = seekableWindow(this.video);
    if (!window) return;
    if (this.samples.length >= MAX_SAMPLES) this.samples.shift();
    this.samples.push({ at: performance.now(), currentTime: this.video.currentTime, ...window });
    if (this.samples.length % LOG_EVERY_SAMPLES === 1) this.context.log(`sample ${describe(this.video)}`);
  }

  /** Текущий снимок (со временем стены) — точка отсчёта для сравнений. */
  snapshot() {
    return { at: performance.now(), currentTime: this.video.currentTime, window: seekableWindow(this.video) };
  }
}

/** Проверки на обычной странице: эфир (`duration = Infinity`) и факт про `seekable` нативного элемента. */
function checkLiveBeforeHandoff(context, video) {
  context.check('live-duration-is-infinity', video.duration === Infinity, `duration=${String(video.duration)}`);
  // Нативный Chromium отдаёт пустой seekable у скользящего окна: фиксируем, не требуем.
  context.metric('native_seekable_ranges', video.seekable.length);
  context.log(`native element facts: ${describe(video)} buffered-ranges=${video.buffered.length}`);
}

/** Команды робота. */
function createHooks(context, observer, video) {
  return {
    'mark-live': () => {
      observer.markLive = observer.snapshot();
      context.log(`mark-live ${describe(video)}`);
    },
    'expect-live-window': () => {
      const mark = observer.markLive;
      const now = observer.snapshot();
      // Окно под тенью может появиться не сразу (его присылает плеер): точка отсчёта — первая
      // выборка непустого окна после передачи, а не мгновение передачи.
      const first = observer.samples.find((sample) => mark && sample.at >= mark.at) ?? null;
      const last = observer.samples.length > 0 ? observer.samples[observer.samples.length - 1] : null;
      context.check('shadow-duration-is-infinity', video.duration === Infinity, `duration=${String(video.duration)}`);
      context.check('shadow-seekable-is-not-empty', first !== null && now.window !== null,
        `seekable ranges now: ${video.seekable.length}; samples of a non-empty window since the hand-off: ${observer.samples.filter((sample) => mark && sample.at >= mark.at).length}`);
      if (first === null || last === null || mark === null) return;
      const spanS = (last.at - first.at) / 1000;
      const needed = Math.min(MAX_REQUIRED_SHADOW_ADVANCE_S, spanS * MIN_SHADOW_ADVANCE_RATIO);
      const endAdvance = last.end - first.end;
      const clockAdvance = now.currentTime - mark.currentTime;
      context.check('shadow-seekable-end-grows', spanS >= 1 && endAdvance >= needed,
        `seekable end ${first.end.toFixed(2)} -> ${last.end.toFixed(2)} (+${endAdvance.toFixed(2)} s, need ${needed.toFixed(2)}) in ${spanS.toFixed(1)} s; start ${first.start.toFixed(2)} -> ${last.start.toFixed(2)}`);
      context.check('shadow-seekable-covers-the-page-clock', last.start - 0.5 <= now.currentTime && now.currentTime <= last.end + 0.5,
        `currentTime ${now.currentTime.toFixed(2)} s, seekable ${last.start.toFixed(2)}..${last.end.toFixed(2)}`);
      context.check('shadow-page-clock-advances', clockAdvance >= needed,
        `currentTime ${mark.currentTime.toFixed(2)} -> ${now.currentTime.toFixed(2)} (+${clockAdvance.toFixed(2)} s, need ${needed.toFixed(2)})`);
      context.metric('seekable_end_advance_s', endAdvance);
      context.metric('seekable_start_s', last.start);
      context.metric('seekable_end_s', last.end);
      context.metric('shadow_clock_advance_s', clockAdvance);
      context.metric('seekable_samples', observer.samples.length);
    },
    'mark-before-return': () => {
      observer.markBeforeReturn = observer.snapshot();
      context.log(`mark-before-return ${describe(video)} visibility=${document.visibilityState}`);
    },
    'expect-returned-live': async (_context, state) => {
      state.returned = true;
      const old = observer.markBeforeReturn;
      // Возврат перезагружает `src`: элемент начинает эфир заново и играет САМ (решение 234: эфир HLS
      // возвращается с игрой, `play()` — вместе с перезагрузкой). Страница play не жмёт: загруженный на
      // паузе эфир встроенный HLS Chromium 156 потом не запускает (DEMUXER_ERROR_COULD_NOT_PARSE).
      await waitFor(() => video.readyState >= 1, RETURN_WAIT_MS);
      context.log(`return hook sees ${describe(video)} readyState=${video.readyState} visibility=${document.visibilityState}`);
      const startedAt = video.currentTime;
      const advanced = await waitFor(() => video.currentTime >= startedAt + RETURN_MIN_ADVANCE_S, RETURN_WAIT_MS);
      context.log(`returned ${describe(video)} old-second=${old?.currentTime.toFixed(2)} buffered-ranges=${video.buffered.length}`);
      context.check('returned-element-plays-live', advanced && !video.paused && video.duration === Infinity,
        `currentTime ${startedAt.toFixed(2)} -> ${video.currentTime.toFixed(2)}, paused=${video.paused}, duration=${String(video.duration)}`);
      context.metric('returned_native_seekable_ranges', video.seekable.length);
    },
  };
}

/** Страница: играет эфир через `<video src>`, проверяет его и при передаче отвечает на команды робота. */
export function playNativeHlsLive(context) {
  let observer = null;
  const hooks = {};
  return playNativeHlsAsset(context, 'hls-ts-live', {
    // Эфир строит маршрут стенда; `<прогон>` в пути обнуляет часы эфира для каждого прогона.
    playlistAddress: () => `/cdn/hls-live/${context.params.run}/stream.m3u8`,
    afterPlaying: async (video) => {
      observer = new LiveObserver(context, video);
      observer.start();
      checkLiveBeforeHandoff(context, video);
      Object.assign(hooks, createHooks(context, observer, video));
    },
    hooks,
    handoffOptions: {
      durationS: null,
      // Эти две итоговые проверки сравнивают часы «до/после» в одном домене времени; после возврата
      // настоящий элемент начинает эфир заново, а ход часов и окна под тенью проверяют хуки выше.
      skipFinishChecks: ['page-clock-advances', 'page-buffer-keeps-filling'],
    },
  });
}
