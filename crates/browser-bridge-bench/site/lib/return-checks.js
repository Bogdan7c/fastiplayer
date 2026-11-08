// Возврат видео странице (сессия 11a, решения 236–238): что видит страница, когда тень снята.
//
// Страница здесь — свидетель, а не судья. Она пишет в журнал ФАКТЫ одной строкой, а решает по ним
// робот (`src/scenarios/page_return/judge.rs`): так допуски живут в одном месте и проверяются
// юнит-тестами, а страница не прячет провал за своим `check`.
//
// Команды робота (их разбирает `runHandoffPhase` и передаёт сюда всё незнакомое):
//   mark-return          запомнить секунду страницы и момент «прямо перед действием» (клик «Return»,
//                        закрытие окна) и начать следить за часами, паузой и событиями элемента;
//   observe-return       дождаться поворота (браузер снова декодирует сам ИЛИ элемент встал на паузу),
//                        понаблюдать `SETTLE_OBSERVE_MS` и написать строку `return-facts …`;
//   count-video-clicks   начать считать клики и нажатия, дошедшие до самого `<video>`;
//   report-video-clicks  написать `video-clicks n=<число>`.
//
// Следом идёт `return-timeline <мс от отметки>:<что>@<секунда> …` — лента событий элемента и смен
// `paused` для разбора провала (судья её не читает).
//
// Формат строки фактов (разбирает `page_return/facts.rs`, поля через пробел, порядок не важен):
//   return-facts turned=<0|1> mark_s=<с> turn_s=<с> elapsed_s=<с> final_s=<с> final_paused=<0|1>
//     settle_s=<с> settle_frames=<число> paused_flips=<число> max_jump_s=<с> play_events=<число>
//     pause_events=<число> seeking_events=<число>
// `elapsed_s` — от отметки до поворота; `max_jump_s` — самый большой скачок часов против хода
// настенных часов (при игре часы идут вместе со стенными, на паузе стоят); `paused_flips` — сколько
// раз `paused` сменился с отметки до конца наблюдения.

import { sleep, waitFor } from './bench-checks.js';

/** Период выборки часов и паузы, мс: чаще шага опроса команд, реже, чем стоит нагрузка. */
const WATCH_SAMPLE_MS = 100;
/** Сколько ждём поворот после отметки, мс (меньше ожидания подтверждения команды роботом, 45 с). */
const TURN_WAIT_MS = 20000;
/** Сколько настоящих кадров должен декодировать браузер, чтобы считать «играет сам». */
const TURN_MIN_REAL_FRAMES = 10;
/** Сколько наблюдаем после поворота, мс: и игра, и пауза обязаны продержаться всё это время. */
const SETTLE_OBSERVE_MS = 3000;
/** События элемента, которые считаем: переключения игры и перемотки возврата. */
const COUNTED_EVENTS = ['play', 'pause', 'seeking'];
/** События, которые только пишем в ленту времени (для разбора провала, судья их не читает). */
const TIMELINE_ONLY_EVENTS = ['playing', 'waiting', 'seeked'];
/** Нажатия мыши, которые пишем в ленту времени. */
const POINTER_EVENTS = ['pointerdown', 'pointerup', 'click'];

/** Короткое имя цели события для ленты (`video#v`, `div`, `html`). */
function describeTarget(target) {
  if (!target || !target.tagName) return 'none';
  return `${target.tagName.toLowerCase()}${target.id ? `#${target.id}` : ''}`;
}

/** Сколько записей ленты времени держим (с запасом на шквал событий у сломанной страницы). */
const TIMELINE_LIMIT = 40;

/** Счётчик настоящих кадров между выборками; падение значения = счётчик начался заново (новый источник). */
class RealFrameTally {
  constructor(readFrames) {
    this.readFrames = readFrames;
    this.last = readFrames();
    this.total = 0;
  }

  sample() {
    const current = this.readFrames();
    this.total += current >= this.last ? current - this.last : current;
    this.last = current;
  }
}

/** Наблюдатель от отметки до конца наблюдения: часы, пауза, события, настоящие кадры. */
class ReturnWatch {
  constructor(video, readFrames) {
    this.video = video;
    this.markS = video.currentTime;
    this.markAt = performance.now();
    this.events = Object.fromEntries(COUNTED_EVENTS.map((name) => [name, 0]));
    /** Лента «что и когда» от отметки, мс: события элемента и смены `paused` по выборкам. */
    this.timeline = [];
    this.listeners = [...COUNTED_EVENTS, ...TIMELINE_ONLY_EVENTS].map((name) => {
      const listener = () => {
        if (name in this.events) this.events[name] += 1;
        this.note(name);
      };
      video.addEventListener(name, listener);
      return [name, listener];
    });
    // Нажатия мыши по всей странице — в ленту (кто принял нажатие и отпускание): так видно, дошёл ли
    // опоздавший клик «Return» до самого `<video>` или его съела заглушка.
    this.pointerListeners = POINTER_EVENTS.map((name) => {
      const listener = (event) => this.note(`${name}>${describeTarget(event.target)}`);
      window.addEventListener(name, listener, { capture: true, passive: true });
      return [name, listener];
    });
    this.tally = new RealFrameTally(readFrames);
    this.last = { at: this.markAt, timeS: this.markS, paused: video.paused };
    this.pausedFlips = 0;
    this.maxJumpS = 0;
    this.timer = setInterval(() => this.sample(), WATCH_SAMPLE_MS);
  }

  /** Запись ленты: мс от отметки, что случилось, секунда страницы. */
  note(what) {
    if (this.timeline.length >= TIMELINE_LIMIT) return;
    const sinceMarkMs = Math.round(performance.now() - this.markAt);
    this.timeline.push(`${sinceMarkMs}:${what}@${this.video.currentTime.toFixed(2)}`);
  }

  /** Лента одной строкой (`return-timeline …`). */
  describeTimeline() {
    return this.timeline.join(' ');
  }

  /** Одна выборка: скачок часов против стенных и смена паузы. */
  sample() {
    const now = performance.now();
    const timeS = this.video.currentTime;
    const { paused } = this.video;
    const wallS = (now - this.last.at) / 1000;
    // При игре часы обязаны уйти на столько же, сколько стенные; на паузе — стоять. Если пауза
    // сменилась внутри шага, точный миг неизвестен: в допуск идёт весь шаг.
    const expectedS = this.last.paused ? 0 : wallS;
    const allowanceS = paused !== this.last.paused ? wallS : 0;
    const jumpS = Math.max(0, Math.abs(timeS - this.last.timeS - expectedS) - allowanceS);
    this.maxJumpS = Math.max(this.maxJumpS, jumpS);
    if (paused !== this.last.paused) {
      this.pausedFlips += 1;
      this.note(paused ? 'became-paused' : 'became-playing');
    }
    this.tally.sample();
    this.last = { at: now, timeS, paused };
  }

  stop() {
    clearInterval(this.timer);
    for (const [name, listener] of this.listeners) this.video.removeEventListener(name, listener);
    for (const [name, listener] of this.pointerListeners) window.removeEventListener(name, listener, { capture: true });
  }

  /** Поворот → наблюдение → факты одной строкой. */
  async observe() {
    const turned = await waitFor(
      () => this.tally.total >= TURN_MIN_REAL_FRAMES || this.video.paused,
      TURN_WAIT_MS,
      WATCH_SAMPLE_MS,
    );
    this.note(turned ? 'turn' : 'no-turn');
    const turnAt = performance.now();
    const turnS = this.video.currentTime;
    const framesAtTurn = this.tally.total;
    await sleep(SETTLE_OBSERVE_MS);
    this.sample();
    const settleS = (performance.now() - turnAt) / 1000;
    const facts = {
      turned: turned ? 1 : 0,
      mark_s: this.markS.toFixed(3),
      turn_s: turnS.toFixed(3),
      elapsed_s: ((turnAt - this.markAt) / 1000).toFixed(3),
      final_s: this.video.currentTime.toFixed(3),
      final_paused: this.video.paused ? 1 : 0,
      settle_s: settleS.toFixed(3),
      settle_frames: this.tally.total - framesAtTurn,
      paused_flips: this.pausedFlips,
      max_jump_s: this.maxJumpS.toFixed(3),
      play_events: this.events.play,
      pause_events: this.events.pause,
      seeking_events: this.events.seeking,
    };
    this.stop();
    return Object.entries(facts).map(([name, value]) => `${name}=${value}`).join(' ');
  }
}

/** Считает клики и нажатия, дошедшие до самого элемента (заглушка обязана их глотать). */
class VideoClickCounter {
  constructor(video) {
    this.count = 0;
    for (const name of ['click', 'pointerdown', 'mousedown', 'pointerup', 'mouseup']) {
      video.addEventListener(name, () => { this.count += 1; }, { capture: true });
    }
  }
}

/**
 * Команды возврата. Возвращает `true`, если команда своя (её выполнили), `false` — чужая.
 * `state` — состояние фазы передачи (`handoff-mode.js`): `video`, `probe`, сюда же кладётся наблюдатель.
 */
export async function runReturnCommand(context, state, name) {
  const { video, probe } = state;
  if (name === 'mark-return') {
    state.returnWatch?.stop();
    state.returnWatch = new ReturnWatch(video, () => (probe.available ? probe.framesOf(video) : 0));
    context.log(`return-mark t=${state.returnWatch.markS.toFixed(3)} paused=${video.paused ? 1 : 0}`);
    return true;
  }
  if (name === 'observe-return') {
    if (!state.returnWatch) {
      context.check('observe-return-after-mark', false, 'observe-return came without mark-return');
      return true;
    }
    const watch = state.returnWatch;
    const facts = await watch.observe();
    state.returnWatch = null;
    context.log(`return-timeline ${watch.describeTimeline()}`);
    // Тень снята: итоговая проверка «страница видит игру» после этого теряет смысл (пауза честна).
    state.returned = true;
    context.log(`return-facts ${facts}`);
    return true;
  }
  if (name === 'count-video-clicks') {
    state.videoClicks = new VideoClickCounter(video);
    return true;
  }
  if (name === 'report-video-clicks') {
    context.log(`video-clicks n=${state.videoClicks?.count ?? -1}`);
    return true;
  }
  return false;
}
