// Режим передачи страниц стенда (`mode=handoff` в адресе страницы СТЕНДА, не продукта).
//
// Идея. Обычный режим страницы: видео играет силами браузера, страница сама проверяет
// кадры. Под «тенью» браузер не декодирует, и проверки «кадры растут» стали бы ложью (тень
// подделывает счётчик страницы). Поэтому в режиме передачи страница:
//   1. играет как обычно до `handoff-ready` (это её доказательство, что она честная);
//   2. сообщает стенду `handoff-ready` и ждёт команд робота (он «отдаёт вкладку» плееру
//      через служебную страницу экстеншена; страница сама ничего экстеншену не говорит);
//   3. после передачи отчитывается ФАКТАМИ, видимыми странице: часы идут, `paused`,
//      буфер наперёд продолжает докачиваться, сторож не пересоздал MediaSource;
//   4. НАСТОЯЩИЙ счётчик декодированных кадров берёт мимо тени (`createRealFrameProbe`).
//
// Команды робота приходят опросом `GET /bench/commands/<run>/<сколько получено>`; после
// каждой страница пишет в журнал `ack <команда>`, и робот идёт дальше. Журнал и отчёт —
// единственный канал страницы к стенду.

import { totalFrames, waitFor } from './bench-checks.js';

/** Режим страницы из адреса. */
export const HANDOFF_MODE = 'handoff';

/** Период опроса команд робота, мс. */
const COMMAND_POLL_MS = 250;
/** Период пробы часов страницы, мс (живость страницы под тенью). */
const SAMPLE_PERIOD_MS = 250;
/** Сколько ждём загрузку чистого iframe, мс (потом берём что есть). */
const PROBE_FRAME_LOAD_MS = 1500;
/** Минимальный прирост часов страницы под тенью, с: «часы идут», а не «одно значение». */
const CLOCK_MIN_ADVANCE_S = 1;
/** Самый длинный допустимый провал между пробами, мс (дольше — страница под тенью «замерла»). */
const MAX_SAMPLE_GAP_MS = 3000;
/** Допуск «буфер у конца ролика», с. */
const END_OF_MEDIA_EPS_S = 0.5;
/** Сколько ждём реакцию страницы на команду плеера (пауза, play, перемотка), мс. */
const PLAYER_COMMAND_WAIT_MS = 8000;
/** Сколько секунд часы должны стоять после паузы плеера, с. */
const FROZEN_OBSERVE_MS = 1500;
/** Длина сегмента роликов стенда, с (`SEGMENT_SECONDS` в media/generate.sh). */
const SEGMENT_LENGTH_S = 2;
/** Допуск позиции при перемотке и возврате, с. */
const POSITION_TOLERANCE_S = 3;
/** Сколько ждём возврат страницы к самостоятельному воспроизведению, мс. */
const RETURN_WAIT_MS = 15000;
/** Сколько кадров браузер должен декодировать сам после возврата: «снова играет сам». */
const RETURN_MIN_REAL_FRAMES = 10;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Настоящий счётчик кадров мимо тени. Тень подделывает `getVideoPlaybackQuality` на
 * прототипе СВОЕГО окна; у свежего `about:blank`-iframe, созданного страницей при загрузке,
 * свой прототип со своим, нетронутым методом. Вызов `.call(video)` даёт настоящее значение.
 * Если получить метод нельзя, проба честно сообщает `available: false` (а не нули).
 */
export async function createRealFrameProbe() {
  const frame = document.createElement('iframe');
  frame.setAttribute('aria-hidden', 'true');
  frame.tabIndex = -1;
  frame.style.cssText = 'position:absolute;width:0;height:0;border:0;visibility:hidden';
  (document.body ?? document.documentElement).append(frame);
  await new Promise((resolve) => {
    const timer = setTimeout(resolve, PROBE_FRAME_LOAD_MS);
    frame.addEventListener('load', () => { clearTimeout(timer); resolve(); }, { once: true });
    if (frame.contentDocument?.readyState === 'complete') { clearTimeout(timer); resolve(); }
  });
  // Метод берём ОДИН раз и держим ссылку: сам iframe остаётся в документе.
  const method = frame.contentWindow?.HTMLVideoElement?.prototype?.getVideoPlaybackQuality;
  if (typeof method !== 'function') {
    return { available: false, framesOf: () => 0, reason: 'no getVideoPlaybackQuality in the probe frame' };
  }
  return {
    available: true,
    reason: '',
    framesOf: (video) => Reflect.apply(method, video, []).totalVideoFrames,
  };
}

/**
 * Сколько команд робота страница уже получила. Одно значение на всю страницу (страница = один
 * прогон): страница, у которой несколько фаз (`ad-preroll`: сначала реклама, потом фильм), создаёт
 * несколько каналов, и каждый обязан продолжить с того места, где остановился прошлый.
 */
const commandCursor = { received: 0 };

/** Опрос команд робота; `next()` отдаёт команды по одной в порядке отправки. */
class CommandChannel {
  constructor(context) {
    this.context = context;
    this.queue = [];
  }

  async next() {
    for (;;) {
      if (this.queue.length > 0) return this.queue.shift();
      const { run, reportOrigin } = this.context.params;
      try {
        const response = await fetch(`${reportOrigin}/bench/commands/${run}/${commandCursor.received}`);
        if (response.ok) {
          const fresh = await response.json();
          commandCursor.received += fresh.length;
          this.queue.push(...fresh);
        }
      } catch (error) {
        console.error('bench: command poll failed', error);
      }
      if (this.queue.length === 0) await sleep(COMMAND_POLL_MS);
    }
  }
}

/**
 * Ждёт одну именную команду робота (остальные подтверждает и пропускает). Для фаз страницы до
 * основной фазы передачи: страница сказала «готова» своей меткой и ждёт, пока робот сделает
 * свою проверку и скажет идти дальше.
 */
export async function waitForRobotCommand(context, wantedCommand) {
  const commands = new CommandChannel(context);
  for (;;) {
    const command = await commands.next();
    context.log(`ack ${command}`);
    if (command === wantedCommand) return;
  }
}

/** Конец буферизованного диапазона, содержащего текущую позицию (или 0). */
function bufferedEndAround(video) {
  const { buffered, currentTime } = video;
  for (let index = 0; index < buffered.length; index += 1) {
    if (currentTime >= buffered.start(index) - END_OF_MEDIA_EPS_S && currentTime <= buffered.end(index)) {
      return buffered.end(index);
    }
  }
  return 0;
}

/**
 * Сколько кадров браузер НА САМОМ ДЕЛЕ декодировал за окно — сумма приростов настоящего
 * счётчика между тиками сэмплера. Разница «конец минус начало» врёт на смене поколения: счётчик
 * браузера обнуляется вместе с новым источником, и кадры, декодированные после обнуления, тонули
 * в отрицательной разнице (09a2: −27…−32 у страниц смены поколения). Упавшее значение значит
 * «счётчик начался заново», и всё, что в нём уже есть, — новые декодированные кадры.
 */
class DecodedFrameTally {
  constructor(probe, video) {
    this.probe = probe;
    this.video = video;
    this.restart();
  }

  /** Начать счёт заново от текущего показания (момент передачи). */
  restart() {
    this.last = this.probe.available ? this.probe.framesOf(this.video) : 0;
    this.total = 0;
  }

  /** Учесть показание счётчика сейчас. */
  sample() {
    if (!this.probe.available) return;
    const current = this.probe.framesOf(this.video);
    this.total += current >= this.last ? current - this.last : current;
    this.last = current;
  }
}

/** Снимок фактов, видимых странице, и настоящий счётчик мимо тени. */
function snapshot({ video, player, probe }) {
  return {
    at: performance.now(),
    currentTime: video.currentTime,
    paused: video.paused,
    ended: video.ended,
    bufferedEnd: bufferedEndAround(video),
    visibleFrames: totalFrames(video),
    realFrames: probe.framesOf(video),
    generations: player?.generations ?? null,
    segmentRequests: player?.segmentRequestCount ?? null,
  };
}

/**
 * Фаза передачи: вызывается страницей ПОСЛЕ её собственных проверок «играет».
 * `hooks` — действия страницы по команде робота (`hook:<имя>`): смена качества, смена поколения.
 * `expectedExtraGenerations` — сколько новых MediaSource страница сама сделает за окно (хуки).
 *   Число либо функция `() => число`, вызываемая в конце окна: так страница объявляет и переходы,
 *   которые происходят САМИ по её собственной логике (автопереход серии по `ended`), не ослабляя
 *   проверку: каждый лишний MediaSource обязан быть объяснён страницей.
 * `readyMarker` — метка в журнале, по которой робот узнаёт «страница готова» (по умолчанию
 * `handoff-ready`; страница с двумя фазами называет вторую иначе, если первая уже писала эту).
 * `skipFinishChecks` — имена итоговых проверок, которые у этой страницы теряют смысл: она сама
 * забирает у плеера видео (снимает источник, ставит файл) или замораживает свой главный поток,
 * и «часы идут под тенью» там нечему доказывать. Пропуск явный и поимённый; метрики остаются.
 */
export async function runHandoffPhase(context, {
  video, player = null, hooks = {}, durationS = null, expectedExtraGenerations = 0,
  skipFinishChecks = [], readyMarker = 'handoff-ready',
}) {
  const probe = context.realFrames;
  const commands = new CommandChannel(context);
  const decoded = new DecodedFrameTally(probe, video);
  const state = { video, player, probe, decoded };
  const samples = [];
  const sampler = setInterval(() => {
    samples.push(performance.now());
    decoded.sample();
  }, SAMPLE_PERIOD_MS);
  if (player) player.followPlayerSeeks();
  let mark = null;
  // Счётчик обязан показывать кадры, пока браузер играет сам: ноль у играющего видео
  // означал бы сломанную пробу, и «браузер не декодирует» под тенью ничего бы не доказывало.
  const realFramesNow = probe.available ? probe.framesOf(video) : 0;
  context.check('real-frame-probe', probe.available && realFramesNow > 0,
    probe.available ? `the clean counter reads ${realFramesNow} frames while the browser plays` : probe.reason);
  context.log(readyMarker);
  try {
    for (;;) {
      const command = await commands.next();
      if (command === 'handed-off') {
        mark = snapshot(state);
        samples.length = 0;
        decoded.restart();
      } else if (command === 'finish') {
        finishPhase(context, state, mark, samples, { durationS, expectedExtraGenerations, skipFinishChecks });
        context.log('ack finish');
        return;
      } else {
        await runCommand(context, state, hooks, command);
      }
      context.log(`ack ${command}`);
    }
  } finally {
    clearInterval(sampler);
  }
}

/** Итог окна под тенью: метрики для порогов стенда и проверки фактов страницы. */
function finishPhase(context, state, mark, samples, { durationS, expectedExtraGenerations, skipFinishChecks }) {
  const { probe } = state;
  const skipped = new Set(skipFinishChecks);
  const check = (name, ok, detail) => {
    if (skipped.has(name)) {
      context.log(`finish check '${name}' skipped by the page: ${detail}`);
      return;
    }
    context.check(name, ok, detail);
  };
  if (!mark) {
    context.check('handed-off-before-finish', false, 'the robot never sent handed-off');
    return;
  }
  const now = snapshot(state);
  state.decoded.sample();
  const windowS = (now.at - mark.at) / 1000;
  const clockAdvance = now.currentTime - mark.currentTime;
  const bufferGain = now.bufferedEnd - mark.bufferedEnd;
  const maxGap = samples.reduce((worst, at, index) => (index === 0 ? worst : Math.max(worst, at - samples[index - 1])), 0);
  const extraGenerations = state.player ? now.generations - mark.generations : 0;
  context.metric('shadow_window_s', windowS);
  context.metric('real_frames_in_window', state.decoded.total);
  context.metric('visible_frames_in_window', now.visibleFrames - mark.visibleFrames);
  context.metric('real_probe_available', probe.available ? 1 : 0);
  context.metric('clock_advance_s', clockAdvance);
  context.metric('sample_ticks', samples.length);
  context.metric('extra_generations', extraGenerations);
  check('page-alive-under-shadow', samples.length >= windowS && maxGap <= MAX_SAMPLE_GAP_MS,
    `${samples.length} ticks in ${windowS.toFixed(1)} s, worst gap ${Math.round(maxGap)} ms`);
  // Новый MediaSource по команде робота начинает ролик с нуля: сравнение с отметкой до смены
  // тогда бессмысленно, а ход часов нового ролика проверил сам хук.
  const generationChanged = extraGenerations > 0;
  const clockOk = clockAdvance >= CLOCK_MIN_ADVANCE_S || generationChanged
    || (durationS !== null && now.currentTime >= durationS - END_OF_MEDIA_EPS_S);
  check('page-clock-advances', clockOk,
    `currentTime ${mark.currentTime.toFixed(2)} -> ${now.currentTime.toFixed(2)} (+${clockAdvance.toFixed(2)} s)`);
  const nearEnd = durationS !== null && now.bufferedEnd >= durationS - END_OF_MEDIA_EPS_S;
  check('page-buffer-keeps-filling', bufferGain > 0 || nearEnd || generationChanged,
    `bufferedEnd ${mark.bufferedEnd.toFixed(2)} -> ${now.bufferedEnd.toFixed(2)}`);
  check('page-sees-playback-not-pause', !now.paused || now.ended || state.returned === true,
    `paused=${now.paused} ended=${now.ended}`);
  const expectedGenerations = typeof expectedExtraGenerations === 'function'
    ? expectedExtraGenerations() : expectedExtraGenerations;
  check('no-unexpected-media-source-reload', extraGenerations === expectedGenerations,
    `generations ${mark.generations} -> ${now.generations}, expected +${expectedGenerations}`);
}

/** Команда робота, реагирующая на действие плеера или действие самой страницы. */
async function runCommand(context, state, hooks, command) {
  const { video } = state;
  const [name, argument] = command.split(':');
  if (name === 'hook') {
    const hook = hooks[argument];
    if (!hook) {
      context.check(`hook-${argument}`, false, 'the page has no such hook');
      return;
    }
    await hook(context, state);
  } else if (name === 'expect-paused') {
    const paused = await waitFor(() => video.paused, PLAYER_COMMAND_WAIT_MS);
    const frozenAt = video.currentTime;
    await sleep(FROZEN_OBSERVE_MS);
    const drift = Math.abs(video.currentTime - frozenAt);
    context.check('player-pause-reaches-page', paused && drift < 0.3,
      `paused=${video.paused}, clock drift while paused ${drift.toFixed(2)} s`);
  } else if (name === 'expect-playing') {
    const playing = await waitFor(() => !video.paused, PLAYER_COMMAND_WAIT_MS);
    const before = video.currentTime;
    const advanced = await waitFor(() => video.currentTime >= before + 0.5, PLAYER_COMMAND_WAIT_MS);
    context.check('player-play-reaches-page', playing && advanced,
      `paused=${video.paused}, currentTime ${before.toFixed(2)} -> ${video.currentTime.toFixed(2)}`);
  } else if (name === 'report-buffer') {
    // Робот выбирает цель перемотки ЗА буфером: ему нужен конец буфера страницы. Отметка
    // журнала запросов берётся здесь, ДО перемотки, чтобы чужие старые запросы не засчитались.
    state.requestMark = state.player ? state.player.requests.length : 0;
    context.log(`buffer-report end=${bufferedEndAround(video).toFixed(2)} duration=${video.duration.toFixed(2)}`);
  } else if (name === 'expect-seek-beyond') {
    await expectSeekBeyondBuffer(context, state, Number(argument));
  } else if (name === 'expect-seek') {
    await expectPlayerSeek(context, state, Number(argument));
  } else if (name === 'expect-returned') {
    await expectReturn(context, state, Number(argument));
  } else {
    context.check(`command-${name}`, false, `unknown command '${command}'`);
  }
}

/** Перемотка плеером: часы страницы у цели, страница докачала новое место и играет оттуда. */
async function expectPlayerSeek(context, state, targetS) {
  const { video, player } = state;
  const requestsBefore = player ? player.requests.length : 0;
  const arrived = await waitFor(() => Math.abs(video.currentTime - targetS) <= POSITION_TOLERANCE_S, PLAYER_COMMAND_WAIT_MS);
  context.check('player-seek-reaches-page', arrived,
    `target ${targetS.toFixed(2)} s, page clock ${video.currentTime.toFixed(2)} s`);
  if (!player) return;
  const fetched = await waitFor(
    () => player.requests.slice(requestsBefore).some((request) => request.kind === 'segment' && request.startS >= targetS - POSITION_TOLERANCE_S),
    PLAYER_COMMAND_WAIT_MS,
  );
  const starts = player.requests.slice(requestsBefore).filter((request) => request.kind === 'segment').map((request) => request.startS);
  // Перемотка назад в уже скачанное место честно обходится без запросов: тогда страница
  // обязана иметь данные впереди цели (иначе играть ей нечем).
  const coveredAhead = bufferedEndAround(video) >= targetS + 2;
  context.check('page-fetches-the-new-place', fetched || coveredAhead,
    `segment starts after the seek: ${starts.slice(0, 6).join(',')}; buffered end ${bufferedEndAround(video).toFixed(2)} s`);
}

/**
 * Перемотка плеером ЗА конец буфера страницы: страница обязана запросить сегмент, который
 * начинается у цели (±длина сегмента), и часы страницы стоят у цели. Это и есть
 * «страница докачала новое место».
 */
async function expectSeekBeyondBuffer(context, state, targetS) {
  const { video, player } = state;
  const arrived = await waitFor(() => Math.abs(video.currentTime - targetS) <= POSITION_TOLERANCE_S, PLAYER_COMMAND_WAIT_MS);
  context.check('beyond-buffer-seek-reaches-page', arrived,
    `target ${targetS.toFixed(2)} s, page clock ${video.currentTime.toFixed(2)} s`);
  if (!player) {
    context.check('page-fetches-segment-at-target', false, 'no player to read requests from');
    return;
  }
  const mark = state.requestMark ?? 0;
  const nearTarget = (request) => request.kind === 'segment' && Math.abs(request.startS - targetS) <= SEGMENT_LENGTH_S;
  const fetched = await waitFor(() => player.requests.slice(mark).some(nearTarget), PLAYER_COMMAND_WAIT_MS);
  const starts = player.requests.slice(mark).filter((request) => request.kind === 'segment').map((request) => request.startS);
  context.check('page-fetches-segment-at-target', fetched,
    `target ${targetS.toFixed(2)} s, segment starts requested since the buffer report: ${starts.slice(0, 8).join(',')}`);
}

/**
 * Возврат (`return_to_page{T, paused}` при закрытии окна плеера): тени нет, страница стоит
 * НА ПАУЗЕ в позиции ≈T (так её оставил плеер); затем страница САМА зовёт `play()`, как
 * человек, и браузер снова декодирует, а часы идут от ≈T.
 */
async function expectReturn(context, state, positionS) {
  const { video, probe } = state;
  state.returned = true;
  const nearT = () => Math.abs(video.currentTime - positionS) <= POSITION_TOLERANCE_S;
  const settled = await waitFor(() => video.paused && nearT(), RETURN_WAIT_MS);
  // Пауза держится: тень не «доигрывает» сама (проверяем реальный счётчик и часы).
  const timeAtPause = video.currentTime;
  // Первое окно — на декод перемотки: после возврата файла Chromium на паузе разово декодирует
  // кадры до T (замер 09b2: 0 -> 18, дальше счётчик стоит, позиция не меняется). Настоящая игра
  // видна во втором окне: счётчик растёт дальше, а часы едут.
  await sleep(FROZEN_OBSERVE_MS);
  const framesAfterSeekDecode = probe.framesOf(video);
  await sleep(FROZEN_OBSERVE_MS);
  const stayed = video.paused && Math.abs(video.currentTime - timeAtPause) < 0.3
    && probe.framesOf(video) - framesAfterSeekDecode <= 3;
  context.check('returned-paused-at-position', settled && stayed,
    `closed at ${positionS.toFixed(2)} s, page paused=${video.paused} at ${video.currentTime.toFixed(2)} s, real frames during hold ${probe.framesOf(video) - framesAfterSeekDecode}`);
  // Страница сама продолжает, как человек.
  const startTime = video.currentTime;
  const framesBefore = probe.framesOf(video);
  try {
    await video.play();
  } catch (error) {
    if (error?.name !== 'NotAllowedError') throw error;
    video.muted = true;
    await video.play();
  }
  const decoding = await waitFor(() => probe.framesOf(video) >= framesBefore + RETURN_MIN_REAL_FRAMES, RETURN_WAIT_MS);
  const advanced = await waitFor(() => video.currentTime >= startTime + 0.5, RETURN_WAIT_MS);
  context.check('page-plays-by-itself-from-position', decoding && advanced && !video.paused,
    `real frames ${framesBefore} -> ${probe.framesOf(video)}, currentTime ${startTime.toFixed(2)} -> ${video.currentTime.toFixed(2)}, paused=${video.paused}`);
}
