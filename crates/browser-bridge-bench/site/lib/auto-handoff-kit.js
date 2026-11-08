// Общие куски страниц режима 2 «автозапуск» (сессия 12a, команда стенда `auto-handoff`).
//
// Идея режима. Человек САМ нажимает play на видео со звуком, и экстеншен без кнопки «Отдать» уводит
// видео в плеер. Поэтому в отличие от страниц режима 1 (`handoff-mode.js`) эти страницы:
//   * НЕ играют сами: видео стоит, пока настоящий клик робота не нажмёт кнопку страницы `Play`
//     (автовоспроизведение в стенде разрешено, и именно это доказывает, что экстеншен игнорирует
//     запуск без клика: стоило бы странице начать сама — и «нет передачи» ничего бы не значило);
//   * не вызывают `hand_off_tab`: передачу делает сам экстеншен, а страница лишь свидетель;
//   * отдают роботу ФАКТЫ окнами: «отметить» → время идёт → «доложить». Судит робот
//     (`src/scenarios/auto_handoff/judge.rs`), страница не прячет провал за своим `check`.
//
// Команды робота (приходят опросом, как в `handoff-mode.js`; после каждой страница пишет `ack <команда>`):
//   hook:mark-window    начать окно наблюдения: настоящие кадры, секунда страницы, момент;
//   hook:report-window  закрыть окно и написать одну строку фактов:
//       auto-window seq=<n> ok=1 probe=<0|1> window_s=<с> real_frames=<n> clock_advance_s=<с>
//         start_s=<с> end_s=<с> paused=<0|1> muted=<0|1> ended=<0|1> looping=<0|1> duration=<с|inf|nan>
//     `seq` растёт с каждым докладом: строки журнала приходят вразнобой, робот ищет нужную по номеру;
//   hook:report-state   мгновенный снимок видео (для вкладок, которые должны стоять на паузе):
//       auto-state seq=<n> t=<с> paused=<0|1> ended=<0|1>
//   handed-off          страница ничего не делает (отметка для общих скриптов), только `ack`;
//   finish              итог страницы (отчёт) и выход.
// Метрики окна дублируются в отчёт под именами `capture` (`real_probe_available`, `shadow_window_s`,
// `real_frames_in_window`): так «браузер не декодирует» судит тот же `browser_does_not_decode`.
//
// Строки журнала страницы про действия человека (по ним робот отличает «клик дошёл» от «стенд мёртв»):
//   play-clicked active=<true|false>   кнопка Play получила настоящий клик (`userActivation.isActive`);
//   play-resolved | play-rejected <имя> исход `video.play()`;
//   pause-clicked                      кнопка Pause получила клик;
//   video-event <play|playing|pause> t=<с>   события элемента (для разбора провала).

import { ElementFrameTally } from './ad-hop-kit.js';
import { checkPlaying } from './bench-checks.js';
import { CommandChannel } from './handoff-mode.js';
import { installEntryProbe, registerEntryTarget } from './entry-probe.js';

/** Период выборок настоящих кадров, мс. */
const SAMPLE_PERIOD_MS = 100;
/** Знаков после запятой в секундах строк фактов. */
const SECONDS_DIGITS = 3;

/** Секунды для строки фактов; `Infinity` и `NaN` пишутся словами (в число их не обратить). */
function describeDuration(duration) {
  if (duration === Infinity) return 'inf';
  if (Number.isNaN(duration)) return 'nan';
  return duration.toFixed(SECONDS_DIGITS);
}

/** События элемента, которые идут в журнал: порядок `play`/`pause` — главная улика при разборе. */
const LOGGED_VIDEO_EVENTS = ['play', 'playing', 'pause'];

/** Пишет в журнал страницы события элемента. */
export function logVideoEvents(ctx, video) {
  for (const name of LOGGED_VIDEO_EVENTS) {
    video.addEventListener(name, () => ctx.log(`video-event ${name} t=${video.currentTime.toFixed(2)}`));
  }
}

/**
 * Кнопки страницы `Play` и `Pause`. Робот кликает по ним настоящей мышью (цели зонда `play` и `pause`),
 * поэтому `video.play()` вызывается из обработчика настоящего клика — с прозрачной активацией
 * пользователя, как у живых сайтов. Отказ `play()` не маскируем: он в журнале.
 *
 * `Play` лежит ПОВЕРХ видео, как у X, YouTube и большинства плееров: режим 2 засчитывает только клик по
 * самому видео (решение 262), и кнопка под видео (лайк, «ещё») запуском человека не считается. Для этого
 * видео переносится в бокс `position: relative` на том же месте (оно ещё стоит, перенос его не трогает).
 * `Pause` — под видео: пауза не запуск.
 */
export function addPlayControls(ctx, video, parent = document.body) {
  const box = document.createElement('div');
  box.className = 'player-box';
  Object.assign(box.style, { position: 'relative', display: 'inline-block' });
  video.parentNode?.insertBefore(box, video);
  box.append(video);
  const play = document.createElement('button');
  play.id = 'play';
  play.type = 'button';
  play.textContent = 'Play';
  Object.assign(play.style, { position: 'absolute', left: '50%', top: '50%', transform: 'translate(-50%, -50%)', margin: '0', zIndex: '1' });
  box.append(play);
  const pause = document.createElement('button');
  pause.id = 'pause';
  pause.type = 'button';
  pause.textContent = 'Pause';
  parent.append(pause);
  play.addEventListener('click', async () => {
    ctx.log(`play-clicked active=${navigator.userActivation?.isActive ?? 'unknown'}`);
    try {
      await video.play();
      ctx.log('play-resolved');
    } catch (error) {
      ctx.log(`play-rejected ${error?.name ?? 'Error'}`);
    }
  });
  pause.addEventListener('click', () => {
    ctx.log('pause-clicked');
    video.pause();
  });
  registerEntryTarget('play', () => play);
  registerEntryTarget('pause', () => pause);
  return { play, pause };
}

/** Окно наблюдения: настоящие кадры (мимо тени), секунда страницы и момент от отметки. */
class WindowWatch {
  constructor(ctx, video) {
    this.ctx = ctx;
    this.video = video;
    // Счётчик создан ДО плеера страницы (`runScenario`), поэтому тень его не подделывает.
    this.probe = ctx.realFrames?.available ? ctx.realFrames : null;
    this.tally = null;
    this.mark = null;
    this.reportSeq = 0;
    this.stateSeq = 0;
    this.sampler = setInterval(() => this.tally?.sample(), SAMPLE_PERIOD_MS);
  }

  stop() {
    clearInterval(this.sampler);
  }

  /** Начало окна: кадры считаются с нуля. */
  begin() {
    this.tally = this.probe ? new ElementFrameTally(this.probe, this.video) : null;
    this.mark = { at: performance.now(), timeS: this.video.currentTime };
    this.ctx.log(`window-marked t=${this.mark.timeS.toFixed(2)} paused=${this.video.paused ? 1 : 0}`);
  }

  /** Конец окна: строка фактов и метрики отчёта. */
  async report() {
    this.reportSeq += 1;
    const seq = this.reportSeq;
    if (this.mark === null) {
      await this.ctx.log(`auto-window seq=${seq} ok=0 reason=no-mark`);
      return;
    }
    this.tally?.sample();
    const windowS = (performance.now() - this.mark.at) / 1000;
    const realFrames = this.tally?.total ?? 0;
    const clockAdvance = this.video.currentTime - this.mark.timeS;
    const facts = {
      seq,
      ok: 1,
      probe: this.probe ? 1 : 0,
      window_s: windowS.toFixed(SECONDS_DIGITS),
      real_frames: realFrames,
      clock_advance_s: clockAdvance.toFixed(SECONDS_DIGITS),
      start_s: this.mark.timeS.toFixed(SECONDS_DIGITS),
      end_s: this.video.currentTime.toFixed(SECONDS_DIGITS),
      paused: this.video.paused ? 1 : 0,
      muted: this.video.muted ? 1 : 0,
      ended: this.video.ended ? 1 : 0,
      // Зацикленное превью за окно переходит через конец ролика: его часы по модулю длины ничего не доказывают.
      looping: this.video.loop ? 1 : 0,
      duration: describeDuration(this.video.duration),
    };
    this.ctx.metric('real_probe_available', this.probe ? 1 : 0);
    this.ctx.metric('shadow_window_s', windowS);
    this.ctx.metric('real_frames_in_window', realFrames);
    await this.ctx.log(`auto-window ${Object.entries(facts).map(([name, value]) => `${name}=${value}`).join(' ')}`);
    this.mark = null;
  }

  /** Мгновенный снимок видео. */
  async reportState() {
    this.stateSeq += 1;
    const { video } = this;
    await this.ctx.log(`auto-state seq=${this.stateSeq} t=${video.currentTime.toFixed(SECONDS_DIGITS)} paused=${video.paused ? 1 : 0} ended=${video.ended ? 1 : 0}`);
  }
}

/**
 * Страница сыграла все свои проверки и ждёт робота: зонд входов включён (робот видит, где на экране
 * кнопки), готовность объявлена, дальше страница служит командам и по `finish` выходит.
 * `hooks` — свои действия страницы (`hook:<имя>`), если нужны сверх общих.
 */
export async function serveAutoHandoff(ctx, { video, hooks = {} }) {
  installEntryProbe(ctx);
  const watch = new WindowWatch(ctx, video);
  const commands = new CommandChannel(ctx);
  ctx.log('handoff-ready');
  try {
    for (;;) {
      const command = await commands.next();
      if (command === 'finish') {
        ctx.log('ack finish');
        return;
      }
      if (command === 'hook:mark-window') watch.begin();
      else if (command === 'hook:report-window') await watch.report();
      else if (command === 'hook:report-state') await watch.reportState();
      else if (command.startsWith('hook:') && hooks[command.slice('hook:'.length)]) await hooks[command.slice('hook:'.length)]();
      else if (command !== 'handed-off') ctx.check(`command-${command}`, false, 'the page does not know this command');
      await ctx.log(`ack ${command}`);
    }
  } finally {
    watch.stop();
  }
}

/**
 * Обычный режим страницы (команда `pages`, без робота): кнопку нажимает сама страница, а браузер
 * обязан сыграть видео. Это проверка самой страницы, не режима 2 (экстеншена здесь нет).
 */
export async function playPlainWithoutRobot(ctx, video, play) {
  play.click();
  await checkPlaying(ctx, video, 'plays-after-the-page-clicks-play');
}
