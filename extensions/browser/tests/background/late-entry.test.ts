// Поздний вход (сессия 10d) целиком на фейковом браузере: порядок шагов, старый документ, секунда,
// подсказка, отказы и молчаливые концы. Судья — что увидел человек и что случилось со вкладкой.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { EntryResult } from "../../src/background/entries/entry-report-text.ts";
import type { EntryReporter } from "../../src/background/entries/entry-reporter.ts";
import { LATE_FILM_WAIT_MS, LATE_SEEK_SETTLE_MS } from "../../src/background/entries/late-entry-step.ts";
import { createLateEntry, LATE_PLAY_WAIT_MS, LATE_POLL_INTERVAL_MS, LATE_PRESS_PLAY_HINT_MS, type LateEntry } from "../../src/background/entries/late-entry.ts";
import type { LateSeekTarget, LateVideoFact } from "../../src/background/entries/late-video-reader.ts";
import type { TabVideoRecord } from "../../src/background/tab-model/tab-snapshot.ts";
import type { HandOffTabOutcome, VideoRef } from "../../src/shared/handoff-contract.ts";
import type { SurveyTabReply } from "../../src/shared/survey-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock, flushPromises, type FakeClock } from "./support.ts";

const TAB = 7;
const ORIGIN = "https://films.example";
const NEW_DOCUMENT = "doc-new";
const OLD_DOCUMENT = "doc-old";
const STARTED: HandOffTabOutcome = { kind: "started", player_window: "new" };

function record(document: string, playing: boolean, durationS: number | null): TabVideoRecord {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document,
    element: 1,
    frame_id: 0,
    media: { kind: "media_source", in_worker: false },
    playing,
    visible_area_px: 1000,
    duration_s: durationS,
    verdict: "main",
  };
}

function surveyOf(video: TabVideoRecord | null): SurveyTabReply {
  const main: VideoRef | null = video === null ? null : { frame_id: 0, document: video.document, element: 1 };
  return { kind: "surveyed", videos: video === null ? [] : [video], main, in_player: { kind: "no_session" } };
}

const fact = (position: number, duration: number | null, playing = true): LateVideoFact => ({ position_s: position, duration_s: duration, playing, visible_area_px: 1000 });

/** Фейковая вкладка: то, что сейчас на странице, меняет сам тест. */
class FakeTab {
  origin: string | undefined = ORIGIN;
  survey: SurveyTabReply = surveyOf(null);
  videos: LateVideoFact[] = [];
  reloadFails = false;
  outcomes: HandOffTabOutcome[] = [STARTED];
  /** Что сделает сайт в ответ на постановку секунды. */
  onSeek: (target: LateSeekTarget) => void = () => {};
  readonly order: string[] = [];
  readonly seeks: LateSeekTarget[] = [];
  readonly handOffs: number[] = [];
  readonly reports: EntryResult[] = [];
  readonly notes: string[] = [];
}

function setup(page: FakeTab): { clock: FakeClock; entry: LateEntry } {
  const clock = createFakeClock();
  const reporter: EntryReporter = {
    report: (_tabId, result) => {
      page.reports.push(result);
      return Promise.resolve();
    },
    forgetProblem: () => Promise.resolve(),
  };
  const entry = createLateEntry({
    tabs: {
      reload: () => {
        page.order.push("reload");
        return page.reloadFails ? Promise.reject(new Error("no such tab")) : Promise.resolve();
      },
      originOf: () => Promise.resolve(page.origin),
    },
    videos: {
      read: () => {
        page.order.push("read");
        return Promise.resolve(page.videos);
      },
      seek: (_tabId, target) => {
        page.seeks.push(target);
        page.onSeek(target);
        return Promise.resolve();
      },
    },
    handoffs: {
      surveyTab: () => Promise.resolve(page.survey),
      handOffTab: (tabId) => {
        page.handOffs.push(tabId);
        return Promise.resolve(page.outcomes.shift() ?? STARTED);
      },
    },
    scheduler: clock.scheduler,
    reporter,
    note: (message) => void page.notes.push(message),
  });
  return { clock, entry };
}

async function tick(clock: FakeClock, ms = LATE_POLL_INTERVAL_MS): Promise<void> {
  clock.advance(ms);
  await flushPromises();
}

/** Запустить вход на странице, где человек стоял на 600 с фильма на 3000 с, и дойти до перезагрузки. */
async function startOnFilm(page: FakeTab): Promise<{ clock: FakeClock; entry: LateEntry }> {
  page.videos = [fact(600, 3_000)];
  const world = setup(page);
  assert.equal(world.entry.start(TAB), true);
  await flushPromises();
  return world;
}

/** После перезагрузки сайт поднял фильм с нуля. */
function pageReloaded(page: FakeTab, playing = true): void {
  page.survey = surveyOf(record(NEW_DOCUMENT, playing, 3_000));
  page.videos = [fact(0, 3_000, playing)];
}

describe("поздний вход: главный путь", () => {
  it("секунду запоминает ДО перезагрузки, потом ставит её и отдаёт видео", async () => {
    const page = new FakeTab();
    page.onSeek = (target) => void (page.videos = [fact(target.position_s, 3_000)]);
    const { clock, entry } = await startOnFilm(page);
    assert.deepEqual(page.order.slice(0, 2), ["read", "reload"], "после перезагрузки читать было бы уже нечего");
    pageReloaded(page);
    await tick(clock);
    assert.deepEqual(page.seeks, [{ duration_s: 3_000, position_s: 600 }]);
    assert.deepEqual(page.handOffs, [], "сначала секунда, потом передача");
    await tick(clock);
    assert.deepEqual(page.handOffs, [TAB]);
    assert.deepEqual(page.reports, [{ kind: "outcome", outcome: STARTED }]);
    assert.equal(entry.isRunning(TAB), false);
  });

  it("второе нажатие не вызывает вторую перезагрузку", async () => {
    const page = new FakeTab();
    const { entry } = await startOnFilm(page);
    assert.equal(entry.isRunning(TAB), true);
    assert.equal(entry.start(TAB), false);
    await flushPromises();
    assert.equal(page.order.filter((step) => step === "reload").length, 1);
  });

  it("видео из старого документа за новое не принимается (перезагрузка ещё не случилась)", async () => {
    const page = new FakeTab();
    page.survey = surveyOf(record(OLD_DOCUMENT, true, 3_000));
    const { clock } = await startOnFilm(page);
    await tick(clock);
    await tick(clock);
    assert.deepEqual(page.handOffs, [], "старый документ: передавать рано");
    assert.deepEqual(page.seeks, []);
  });

  it("эфир: секунды нет, передаём первое заигравшее без перемотки", async () => {
    const page = new FakeTab();
    page.videos = [fact(600, null)];
    const { clock, entry } = setup(page);
    entry.start(TAB);
    await flushPromises();
    page.survey = surveyOf(record(NEW_DOCUMENT, true, null));
    await tick(clock);
    assert.deepEqual(page.seeks, []);
    assert.deepEqual(page.handOffs, [TAB]);
  });
});

describe("поздний вход: страница сама не запустила видео", () => {
  it("подсказка один раз; секунда уже стоит; передача после нажатия play", async () => {
    const page = new FakeTab();
    page.onSeek = (target) => void (page.videos = [fact(target.position_s, 3_000, false)]);
    const { clock } = await startOnFilm(page);
    pageReloaded(page, false);
    await tick(clock, LATE_PRESS_PLAY_HINT_MS);
    await tick(clock);
    await tick(clock);
    assert.deepEqual(page.reports, [{ kind: "late_press_play" }]);
    assert.deepEqual(page.handOffs, [], "видео стоит: отдавать нечего");
    assert.equal(page.seeks.length, 1, "секунда поставлена заранее");
    page.survey = surveyOf(record(NEW_DOCUMENT, true, 3_000));
    page.videos = [fact(600, 3_000)];
    await tick(clock);
    assert.deepEqual(page.handOffs, [TAB]);
  });

  it("играющая реклама: подсказывать «нажми play» незачем", async () => {
    const page = new FakeTab();
    const { clock } = await startOnFilm(page);
    page.survey = surveyOf(record(NEW_DOCUMENT, true, 30));
    page.videos = [fact(1, 30)];
    await tick(clock, LATE_PRESS_PLAY_HINT_MS);
    await tick(clock);
    assert.deepEqual(page.reports, []);
  });

  it("срок вышел: плашка, страница остаётся как есть, работа снята", async () => {
    const page = new FakeTab();
    const { clock, entry } = await startOnFilm(page);
    for (let elapsed = 0; elapsed <= LATE_PLAY_WAIT_MS; elapsed += LATE_POLL_INTERVAL_MS) await tick(clock);
    assert.deepEqual(page.reports.at(-1), { kind: "late_no_video_started" });
    assert.deepEqual(page.handOffs, []);
    assert.equal(entry.isRunning(TAB), false);
  });
});

describe("поздний вход: реклама и упрямый сайт", () => {
  it("реклама после показавшегося фильма ждёт свой полный срок, а не уходит в плеер сразу", async () => {
    const page = new FakeTab();
    const { clock } = await startOnFilm(page);
    const adPlays = (): void => {
      page.survey = surveyOf(record(NEW_DOCUMENT, true, 30));
      page.videos = [fact(1, 30)];
    };
    const secondsOfPolling = async (seconds: number): Promise<void> => {
      for (let second = 0; second < seconds; second += 1) await tick(clock);
    };
    adPlays();
    await secondsOfPolling(50);
    pageReloaded(page);
    await tick(clock);
    adPlays();
    await secondsOfPolling(20);
    assert.deepEqual(page.handOffs, [], "от начала первой рекламы прошло больше срока, но фильм его сбросил");
    await secondsOfPolling(LATE_FILM_WAIT_MS / 1_000 - 20 + 1);
    assert.deepEqual(page.handOffs, [TAB], "свой полный срок реклама отстояла");
  });

  it("реклама доигрывает в браузере, фильм уходит в плеер с той же секунды", async () => {
    const page = new FakeTab();
    page.onSeek = (target) => void (page.videos = [fact(target.position_s, 3_000)]);
    const { clock } = await startOnFilm(page);
    page.survey = surveyOf(record(NEW_DOCUMENT, true, 30));
    page.videos = [fact(1, 30)];
    await tick(clock);
    await tick(clock);
    assert.deepEqual(page.handOffs, [], "рекламу не отдаём: секунда относится к фильму");
    assert.deepEqual(page.seeks, [], "рекламу не перематываем");
    pageReloaded(page);
    await tick(clock);
    await tick(clock);
    assert.deepEqual(page.handOffs, [TAB]);
    assert.deepEqual(page.reports, [{ kind: "outcome", outcome: STARTED }]);
  });

  it("фильм не вышел за минуту: отдаём что играет и говорим, что секунда не сохранена", async () => {
    const page = new FakeTab();
    const { clock } = await startOnFilm(page);
    page.survey = surveyOf(record(NEW_DOCUMENT, true, 30));
    page.videos = [fact(1, 30)];
    await tick(clock);
    await tick(clock, LATE_FILM_WAIT_MS);
    await tick(clock);
    assert.deepEqual(page.handOffs, [TAB]);
    assert.deepEqual(page.reports, [{ kind: "late_started_from_site_position" }]);
  });

  it("сайт отвергает перемотку: две попытки, затем передача и честная плашка", async () => {
    const page = new FakeTab();
    const { clock } = await startOnFilm(page);
    pageReloaded(page);
    await tick(clock);
    await tick(clock, LATE_SEEK_SETTLE_MS);
    await tick(clock, LATE_SEEK_SETTLE_MS);
    assert.equal(page.seeks.length, 2);
    assert.deepEqual(page.handOffs, [TAB]);
    assert.deepEqual(page.reports, [{ kind: "late_started_from_site_position" }]);
  });

  it("окно плеера уже открыто: остаётся совет переключиться, а не новая плашка про секунду", async () => {
    const page = new FakeTab();
    const alreadyOpen: HandOffTabOutcome = { kind: "started", player_window: "already_open" };
    page.outcomes = [alreadyOpen];
    const { clock } = await startOnFilm(page);
    pageReloaded(page);
    await tick(clock);
    await tick(clock, LATE_SEEK_SETTLE_MS);
    await tick(clock, LATE_SEEK_SETTLE_MS);
    assert.deepEqual(page.reports, [{ kind: "outcome", outcome: alreadyOpen }]);
  });
});

describe("поздний вход: концы и сбои", () => {
  it("адрес вкладки не прочитался: перезагрузки нет, человек видит отказ, а не тишину", async () => {
    const page = new FakeTab();
    page.origin = undefined;
    const { entry } = setup(page);
    assert.equal(entry.start(TAB), true);
    await flushPromises();
    assert.deepEqual(page.order, [], "ни чтения видео, ни перезагрузки");
    assert.deepEqual(page.reports, [{ kind: "late_reload_failed" }]);
    assert.deepEqual(page.notes, ["late entry could not read the tab address"]);
    assert.equal(entry.isRunning(TAB), false, "работа снята, вкладку можно запускать снова");
  });

  it("вкладку закрыли: работа снята молча, ни плашки, ни передачи", async () => {
    const page = new FakeTab();
    const { clock, entry } = await startOnFilm(page);
    page.origin = undefined;
    await tick(clock);
    assert.equal(entry.isRunning(TAB), false);
    assert.deepEqual(page.reports, []);
    assert.deepEqual(page.handOffs, []);
  });

  it("человек ушёл на другой сайт: видео того сайта мы не трогаем", async () => {
    const page = new FakeTab();
    const { clock, entry } = await startOnFilm(page);
    page.origin = "https://other.example";
    pageReloaded(page);
    await tick(clock);
    assert.equal(entry.isRunning(TAB), false);
    assert.deepEqual(page.handOffs, []);
    assert.deepEqual(page.seeks, []);
  });

  it("вкладка не перезагрузилась: плашка, работа снята", async () => {
    const page = new FakeTab();
    page.reloadFails = true;
    const { entry } = await startOnFilm(page);
    await flushPromises();
    assert.deepEqual(page.reports, [{ kind: "late_reload_failed" }]);
    assert.equal(entry.isRunning(TAB), false);
  });

  it("человек успел отдать видео другим входом: вход не вмешивается и не шлёт плашку", async () => {
    const page = new FakeTab();
    const { clock, entry } = await startOnFilm(page);
    page.survey = { kind: "surveyed", videos: [], main: null, in_player: { kind: "in_player", video: null } };
    await tick(clock);
    assert.equal(entry.isRunning(TAB), false);
    assert.deepEqual(page.reports, []);
  });

  it("временный отказ передачи: ждём дальше, окончательный показываем", async () => {
    const page = new FakeTab();
    page.onSeek = (target) => void (page.videos = [fact(target.position_s, 3_000)]);
    page.outcomes = [{ kind: "no_video" }, { kind: "protected" }];
    const { clock } = await startOnFilm(page);
    pageReloaded(page);
    await tick(clock);
    await tick(clock);
    assert.deepEqual(page.reports, []);
    await tick(clock);
    assert.deepEqual(page.reports, [{ kind: "outcome", outcome: { kind: "protected" } }]);
  });

  it("сбой шага не срывает работу и не молчит: виден разработчику, следующий шаг идёт", async () => {
    const page = new FakeTab();
    const { clock } = await startOnFilm(page);
    pageReloaded(page);
    page.onSeek = () => {
      throw new Error("frame is gone");
    };
    await tick(clock);
    assert.ok(page.notes.includes("late entry step failed, waiting on"));
    page.onSeek = (target) => void (page.videos = [fact(target.position_s, 3_000)]);
    await tick(clock);
    assert.equal(page.seeks.length, 2);
  });
});
