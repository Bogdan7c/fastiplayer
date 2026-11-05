// Общий каркас страниц «реклама → фильм» (сессия 09c2, решения 201 и 204).
//
// Что моделируют страницы: перед фильмом играет реклама (отдельный элемент, фрейм, тот же
// элемент, цепочка роликов), по её `ended` страница убирает ролик как живой сайт и запускает
// фильм. Плеер обязан доиграть рекламу, переехать на фильм В ТОМ ЖЕ окне (один сеанс) и показать
// фильм, а браузер при этом почти не декодирует.
//
// Роли:
//   * страница описывает ХОД событий (`build`): какие элементы, что делать по `ended`;
//   * `AdHopRecorder` считает то, что видно только странице: сколько раз сработал `ended` у каждого
//     ролика, исход `play()` фильма, часы фильма, НАСТОЯЩИЕ кадры, декодированные браузером
//     (счётчик мимо тени), и пишет метрики отчёта, которые проверяет судья стенда
//     (`src/scenarios/capture/ad_hop`);
//   * `runAdHopScenario` ведёт страницу: готовность → команды робота → итоговые проверки.
//
// Метки журнала страницы, по которым робот узнаёт этапы: `handoff-ready` (реклама играет, можно
// передавать), `film-started` (фильм запущен страницей), `ad-removed` (страница убрала рекламу).

import { pageParams, runScenario } from './bench-report.js';
import { checkPlaying, sleep } from './bench-checks.js';
import { BenchPlayer, standardRefs } from './bench-player.js';
import { mountVideo, requireMimeSupport } from './scenario-kit.js';
import { CommandChannel } from './handoff-mode.js';

/** Период сэмплера настоящих кадров, мс. */
const SAMPLE_PERIOD_MS = 250;
/** Бюджет страницы без робота (`pages`): цепочка из трёх роликов и фильм не влезают в 25 с. */
const PLAIN_PAGE_BUDGET_MS = 55000;
/** Сколько страница без робота ждёт запуск фильма после начала рекламы, мс. */
const PLAIN_FLOW_WAIT_MS = 45000;
/** Фильм под тенью обязан продвинуть часы страницы хотя бы на столько, с. */
const FILM_MIN_CLOCK_ADVANCE_S = 1;

/**
 * Сумма приростов НАСТОЯЩЕГО счётчика кадров одного элемента между тиками сэмплера. Падение значения
 * значит «счётчик начался заново» (новый источник): всё, что в нём уже есть, — новые кадры.
 */
export class ElementFrameTally {
  constructor(probe, video) {
    this.probe = probe;
    this.video = video;
    this.restart();
  }

  /** Начать счёт заново от текущего показания. */
  restart() {
    this.last = this.probe.available ? this.probe.framesOf(this.video) : 0;
    this.total = 0;
  }

  /** Учесть показание сейчас. */
  sample() {
    if (!this.probe.available) return;
    const current = this.probe.framesOf(this.video);
    this.total += current >= this.last ? current - this.last : current;
    this.last = current;
  }
}

/** Свидетель страницы: считает то, что видно только ей, и пишет метрики отчёта. */
export class AdHopRecorder {
  constructor(ctx) {
    this.ctx = ctx;
    /** Настоящий счётчик кадров (`null` без режима передачи). */
    this.probe = ctx.realFrames?.available ? ctx.realFrames : null;
    /** Сколько раз сработал `ended`, по меткам роликов. */
    this.endedByLabel = new Map();
    this.adTallies = [];
    /** Кадры рекламных элементов, чей счёт уже закрыт (фильм начался). */
    this.closedAdFrames = 0;
    this.externalAdFrames = 0;
    this.filmTally = null;
    this.film = null;
    this.filmPlayOutcome = null;
    this.filmBeganAt = null;
    this.filmStartedAt = null;
    this.filmClockAtStart = 0;
    this.handedOffAt = null;
    this.handedOffListeners = [];
    this.banner = null;
    this.bannerTally = null;
    this.flowDone = new Promise((resolve) => { this.announceFlowDone = resolve; });
    this.sampler = setInterval(() => this.sample(), SAMPLE_PERIOD_MS);
  }

  sample() {
    for (const tally of this.adTallies) tally.sample();
    this.filmTally?.sample();
    this.bannerTally?.sample();
  }

  stop() {
    clearInterval(this.sampler);
  }

  /**
   * Следить за рекламным роликом: считать его `ended` под меткой `labelOf` (строка или функция,
   * которая отвечает «какой ролик сейчас на элементе»). `ended` самого элемента фильма считается
   * под меткой `film-element`: фильм до конца за окно не доигрывает, лишнее `ended` — ошибка.
   */
  trackAd(video, labelOf) {
    video.addEventListener('ended', () => {
      const label = video === this.film ? 'film-element' : (typeof labelOf === 'function' ? labelOf() : labelOf);
      this.noteAdEnded(label);
    });
    if (this.probe) this.adTallies.push(new ElementFrameTally(this.probe, video));
  }

  /** Страница получила `ended` рекламного ролика `label`. */
  noteAdEnded(label) {
    const count = (this.endedByLabel.get(label) ?? 0) + 1;
    this.endedByLabel.set(label, count);
    this.ctx.log(`ad-ended ${label} count=${count}`);
  }

  /** Реклама в чужом фрейме: фрейм сам считает свои кадры и сообщает их вместе с `ended`. */
  noteExternalAdEnded(label, realFrames) {
    this.noteAdEnded(label);
    this.externalAdFrames += realFrames;
  }

  /** Подписка на команду робота «передано» (фрейму рекламы тоже нужно начать счёт заново). */
  onHandedOff(listener) {
    this.handedOffListeners.push(listener);
  }

  /** Робот передал вкладку: счёт настоящих кадров рекламы начинается заново с этого момента. */
  markHandedOff() {
    this.handedOffAt = performance.now();
    for (const tally of this.adTallies) tally.restart();
    for (const listener of this.handedOffListeners) listener();
  }

  /** Баннер рядом с фильмом: его настоящие кадры должны продолжать расти (баннер остаётся браузеру). */
  watchBanner(video) {
    this.banner = video;
    if (this.probe) this.bannerTally = new ElementFrameTally(this.probe, video);
  }

  /** Фильм начинается на `video`: счёт кадров рекламы закрыт, счёт кадров фильма открыт. */
  beginFilm(video) {
    this.sample();
    this.filmBeganAt = performance.now();
    this.closedAdFrames += this.adTallies.reduce((sum, tally) => sum + tally.total, 0);
    this.adTallies = [];
    this.film = video;
    if (this.probe) this.filmTally = new ElementFrameTally(this.probe, video);
  }

  /**
   * `play()` фильма, как делает сайт: обещание ждём и записываем исход. `NotAllowedError` — политика
   * автоплея, как у остальных страниц стенда повторяем без звука; любой другой отказ (в первую очередь
   * `AbortError`, когда что-то прервало запуск) — провал страницы.
   */
  async playFilm(video) {
    if (this.film !== video) this.beginFilm(video);
    let outcome = 'resolved';
    try {
      await video.play();
    } catch (error) {
      outcome = error?.name ?? 'error';
      if (outcome === 'NotAllowedError') {
        video.muted = true;
        try {
          await video.play();
          outcome = 'resolved';
        } catch (retryError) {
          outcome = retryError?.name ?? 'error';
        }
      }
    }
    this.filmPlayOutcome = outcome;
    this.filmStartedAt = performance.now();
    this.filmClockAtStart = video.currentTime;
    // Метка пишется и при отказе: робот не должен ждать фильм, который уже не придёт.
    this.ctx.log(`film-started play=${outcome}`);
    this.announceFlowDone();
  }

  /** Поток страницы оборвался ошибкой: записываем провал и снимаем ожидания робота. */
  failFlow(error) {
    this.ctx.check('ad-hop-flow', false, `${error?.name ?? 'Error'}: ${error?.message ?? error}`);
    this.filmPlayOutcome = `flow-error:${error?.name ?? 'Error'}`;
    this.ctx.log(`film-started play=${this.filmPlayOutcome}`);
    this.announceFlowDone();
  }

  /** Запустить асинхронный ход страницы; ошибка не теряется, а попадает в отчёт (результат тогда `null`). */
  guard(promise) {
    return promise.catch((error) => {
      this.failFlow(error);
      return null;
    });
  }

  /** Итоговые проверки и метрики. `expectedAdEnded` — метки роликов, у каждого `ended` ровно один. */
  finish({ expectedAdEnded, expectFilm }) {
    const { ctx } = this;
    this.sample();
    const labels = new Set([...expectedAdEnded, ...this.endedByLabel.keys()]);
    const wrong = [...labels].filter((label) => (this.endedByLabel.get(label) ?? 0) !== (expectedAdEnded.includes(label) ? 1 : 0));
    const total = [...this.endedByLabel.values()].reduce((sum, count) => sum + count, 0);
    ctx.metric('ad_ended_total', total);
    ctx.check('ad-ended-once-per-clip', wrong.length === 0,
      `ended counts ${JSON.stringify(Object.fromEntries(this.endedByLabel))}, expected exactly one for ${JSON.stringify(expectedAdEnded)}`);
    const adWindowEnd = this.filmBeganAt ?? performance.now();
    const adWindowS = this.handedOffAt === null ? 0 : Math.max(0, (adWindowEnd - this.handedOffAt) / 1000);
    ctx.metric('ad_shadow_window_s', adWindowS);
    ctx.metric('ad_real_frames', this.closedAdFrames + this.adTallies.reduce((sum, tally) => sum + tally.total, 0) + this.externalAdFrames);
    if (this.bannerTally) {
      ctx.metric('banner_real_frames', this.bannerTally.total);
      ctx.check('banner-keeps-playing', this.banner && !this.banner.paused,
        `banner paused=${this.banner?.paused}, real frames ${this.bannerTally.total}`);
    }
    if (!expectFilm) return;
    ctx.check('film-play-resolved', this.filmPlayOutcome === 'resolved', `play() outcome: ${this.filmPlayOutcome}`);
    if (!this.film || this.filmStartedAt === null) {
      ctx.check('film-started', false, 'the page never started the film');
      return;
    }
    const windowS = (performance.now() - this.filmStartedAt) / 1000;
    const clockAdvance = this.film.currentTime - this.filmClockAtStart;
    ctx.metric('film_window_s', windowS);
    ctx.metric('film_clock_advance_s', clockAdvance);
    ctx.metric('film_real_frames', this.filmTally?.total ?? 0);
    ctx.metric('film_paused', this.film.paused ? 1 : 0);
    ctx.metric('film_play_rejected', this.filmPlayOutcome === 'resolved' ? 0 : 1);
    ctx.check('film-not-paused-for-the-page', !this.film.paused, `paused=${this.film.paused}`);
    if (ctx.params.handoff) {
      ctx.check('film-clock-advances-under-shadow', clockAdvance >= FILM_MIN_CLOCK_ADVANCE_S,
        `film clock ${this.filmClockAtStart.toFixed(2)} -> ${this.film.currentTime.toFixed(2)} in ${windowS.toFixed(1)} s`);
    }
  }
}

/** Команды робота до итога: `handed-off`, действия страницы (`hook:<имя>`), `finish`. */
async function serveRobotCommands(ctx, recorder, hooks, onFinish) {
  const commands = new CommandChannel(ctx);
  ctx.log('handoff-ready');
  for (;;) {
    const command = await commands.next();
    if (command === 'handed-off') {
      recorder.markHandedOff();
    } else if (command === 'finish') {
      await onFinish();
      ctx.log('ack finish');
      return;
    } else if (command.startsWith('hook:')) {
      const hook = hooks[command.slice('hook:'.length)];
      if (hook) await hook(ctx);
      else ctx.check(`hook-${command}`, false, 'the page has no such hook');
    } else {
      ctx.check(`command-${command}`, false, `unknown command '${command}'`);
    }
    ctx.log(`ack ${command}`);
  }
}

/**
 * Страница «реклама → фильм». `build(ctx, recorder)` запускает рекламу, проверяет, что она играет, и
 * возвращает `{ expectedAdEnded, expectFilm, hooks?, cleanup? }`.
 */
export async function runAdHopScenario(scenario, build) {
  const params = pageParams();
  await runScenario(scenario, async (ctx) => {
    const recorder = new AdHopRecorder(ctx);
    try {
      const page = await build(ctx, recorder);
      const finish = async () => {
        recorder.finish(page);
        await page.cleanup?.();
      };
      if (params.handoff) {
        await serveRobotCommands(ctx, recorder, page.hooks ?? {}, finish);
        return;
      }
      // Без робота (`pages`): браузер играет сам, ход страницы доходит до конца, фильм играет.
      const reached = await Promise.race([recorder.flowDone.then(() => true), sleep(PLAIN_FLOW_WAIT_MS).then(() => false)]);
      ctx.check('ad-hop-flow-completed', reached, 'the page flow reached its end (film started or ad removed)');
      if (page.expectFilm && recorder.film) await checkPlaying(ctx, recorder.film, 'film-plays-by-itself');
      await finish();
    } finally {
      recorder.stop();
    }
  }, { budgetMs: params.handoff ? undefined : PLAIN_PAGE_BUDGET_MS });
}

/** Адрес файла из каталога стенда на этом сервере. */
export function mediaUrl(catalog, assetId) {
  return `/media/${catalog.assets[assetId].url}`;
}

/** Рекламный ролик: запустить и дождаться «играет» (без звука, если политика автоплея не дала). */
export async function playAd(ctx, ad, name = 'ad-playing') {
  await ad.play().catch(async () => { ad.muted = true; await ad.play(); });
  await checkPlaying(ctx, ad, name);
}

/**
 * Как живой сайт убирает ролик: пустой источник, перезагрузка, удаление из документа. После этого
 * страница не должна находить на экране ни одного `<video>` рекламы.
 */
export function removeAdLikeLiveSite(ad) {
  ad.src = '';
  ad.load();
  ad.remove();
}

/**
 * Фильм «сейчас»: элемент, MediaSource, `play()`. `play()` зовётся в той же задаче, что и привязка
 * источника (до `sourceopen`): так делают живые плееры, и сторож в странице обязан успеть поставить
 * тень раньше первого кадра. `video`/`player` можно передать готовые (тот же элемент, что играл рекламу).
 */
export async function startFilmNow(ctx, recorder, catalog, { video = null, player = null } = {}) {
  const refs = standardRefs(catalog, 'h264-aac');
  requireMimeSupport(catalog, refs);
  const filmVideo = video ?? mountVideo(document.body, 'film');
  const filmPlayer = player ?? new BenchPlayer(filmVideo, catalog, { log: (line) => ctx.log(line) });
  const attaching = filmPlayer.start({ refs, autoplay: false });
  recorder.beginFilm(filmVideo);
  const playing = recorder.playFilm(filmVideo);
  await Promise.all([attaching, playing]);
  filmPlayer.followPlayerSeeks();
  return { video: filmVideo, player: filmPlayer };
}
