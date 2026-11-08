// Режим 2 целиком на подделках браузера (12a): донесение сторожа -> решение фона -> передача вкладки /
// отпускание / кредит клика. Проверяем то, что видит страница и человек: ответ фону сторожа, команду передачи
// с точной ссылкой, просьбы фреймам, плашку.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createAutoHandoff, type AutoHandoff, type AutoHandoffDependencies } from "../../src/background/auto-handoff/auto-handoff.ts";
import { noticeOfAutoHandoffOutcome } from "../../src/background/auto-handoff/outcome-notice.ts";
import { PLAY_CREDIT_GRACE_MS } from "../../src/background/auto-handoff/viewer-play-decision.ts";
import type { EntryResult } from "../../src/background/entries/entry-report-text.ts";
import { createHeldVideoListener, type FrameSender } from "../../src/background/handoff/hand-off-command.ts";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import type { VideoFilterSettings } from "../../src/shared/video-filter-settings.ts";
import { HELD_VIDEO_REPORT_KIND, type HandOffTabOutcome, type HeldVideoReply, type HeldVideoReport, type HeldVideoTrigger, type VideoCandidate, type VideoRef } from "../../src/shared/handoff-contract.ts";
import { VIEWER_OFFER_CHOICE_KIND, type ViewerOfferChoice, type ViewerOfferChoiceMessage } from "../../src/shared/viewer-offer-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { flushPromises } from "./support.ts";

const TAB = 12;
const FRAME = 3;
const FILM: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "media_source", in_worker: false }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS };
const SHORT_CLIP: VideoCandidate = { ...FILM, media: { kind: "file" }, duration_s: 20, file_host: "site.example" };
/** Все фильтры видео включены везде: так режим 2 вёл себя в 12a, на этом построены старые ряды ниже. */
const ALL_FILTERS_ON: VideoFilterSettings = {
  sound: { enabled: true, sites: [] },
  duration: { enabled: true, sites: [] },
  loop: { enabled: true, sites: [] },
};
const ON: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true, video_filters: ALL_FILTERS_ON };
/** Режим включён, фильтры как по умолчанию — все выключены (решение 261). */
const ON_DEFAULT_FILTERS: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true };

const report = (candidate: VideoCandidate, trigger: HeldVideoTrigger = "viewer_play"): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger });
const ref = (candidate: VideoCandidate): VideoRef => ({ frame_id: FRAME, document: candidate.document, element: candidate.element });

interface WorldOptions {
  readonly settings?: AutoHandoffSettings | "read_fails";
  readonly url?: string | undefined;
  readonly probeResult?: "ad_on_screen" | "no_ad";
  /** Исход передачи; можно отдать обещание, чтобы держать передачу «в пути». */
  readonly handOffOutcome?: Promise<HandOffTabOutcome>;
  readonly activeTabs?: number[];
  readonly heldVideoReply?: HeldVideoReply;
  /** Запись настройки отказывает (правило с предложения «Ask» не сохраняется). */
  readonly writeFails?: boolean;
}

function createWorld(options: WorldOptions = {}) {
  const handOffCalls: { tabId: number; consent: unknown; video: VideoRef | undefined }[] = [];
  const heldVideoCalls: HeldVideoReport[] = [];
  const credits: { tabId: number; leaseMs: number }[] = [];
  const releases: { tabId: number; frameId: number; element: number }[] = [];
  const reports: { tabId: number; result: EntryResult }[] = [];
  const notes: string[] = [];
  const probes: number[] = [];
  const activeTabs = options.activeTabs ?? [];
  const withdrawals: { tabId: number; frameId: number; element: number }[] = [];
  /** Хранилище подделки: запись предложения «Ask» видна следующему чтению, как в настоящем `storage.local`. */
  let stored: AutoHandoffSettings | "read_fails" = options.settings ?? ON;
  const dependencies: AutoHandoffDependencies = {
    handoffs: {
      heldVideo: (_tabId, _frameId, held) => {
        heldVideoCalls.push(held);
        return options.heldVideoReply ?? "release";
      },
      handOffTab: (tabId, consent, video) => {
        handOffCalls.push({ tabId, consent, video });
        return options.handOffOutcome ?? Promise.resolve({ kind: "started", player_window: "new" });
      },
      activeTabIds: () => activeTabs,
    },
    readSettings: () => (stored === "read_fails" ? Promise.reject(new Error("storage")) : Promise.resolve(stored)),
    writeSettings: async (settings) => {
      if (options.writeFails === true) throw new Error("disk full");
      stored = settings;
    },
    describeTab: async () => ("url" in options ? { url: options.url } : { url: "https://site.example/watch" }),
    adProbe: { probe: async (tabId) => (probes.push(tabId), options.probeResult ?? "no_ad") },
    grantPlayCredit: (tabId, leaseMs) => void credits.push({ tabId, leaseMs }),
    releaseHeldVideo: (tabId, frameId, element) => void releases.push({ tabId, frameId, element }),
    withdrawViewerOffer: (tabId, frameId, element) => void withdrawals.push({ tabId, frameId, element }),
    reporter: { report: async (tabId, result) => void reports.push({ tabId, result }) },
    note: (message) => void notes.push(message),
  };
  const autoHandoff: AutoHandoff = createAutoHandoff(dependencies);
  return { autoHandoff, handOffCalls, heldVideoCalls, credits, releases, reports, notes, probes, activeTabs, withdrawals, stored: () => stored };
}

describe("режим 2: передача по нажатию play", () => {
  it("принято: ответ adopt_new_session, передача вкладки с точной ссылкой на видео, плашки нет (новое окно)", async () => {
    const world = createWorld();
    const reply = await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    assert.equal(reply, "adopt_new_session");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
    await flushPromises();
    assert.deepEqual(world.releases, []);
    assert.deepEqual(world.reports, []);
  });

  it("режим выключен (настройка по умолчанию): release, передачи нет", async () => {
    const world = createWorld({ settings: DEFAULT_AUTO_HANDOFF_SETTINGS });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release");
    assert.deepEqual(world.handOffCalls, []);
  });

  it("сбой чтения настройки: режим считается выключенным, release, причина в журнале", async () => {
    const world = createWorld({ settings: "read_fails" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release");
    assert.deepEqual(world.handOffCalls, []);
    assert.equal(world.notes.length, 1);
  });

  it("стоп-лист (встроенный и свой), вкладка без адреса и служебная страница: release без передачи", async () => {
    const cases: readonly { name: string; url: string | undefined; settings: AutoHandoffSettings }[] = [
      { name: "встроенный", url: "https://meet.google.com/abc", settings: ON },
      { name: "свой", url: "https://news.example/a", settings: { ...ON, own_stop_list: ["news.example"] } },
      { name: "без адреса", url: undefined, settings: ON },
      { name: "служебная страница", url: "chrome://settings", settings: ON },
    ];
    for (const row of cases) {
      const world = createWorld({ url: row.url, settings: row.settings });
      assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", row.name);
      assert.deepEqual(world.handOffCalls, [], row.name);
    }
  });

  it("без звука и короче порога без рекламы: не передаём", async () => {
    const world = createWorld();
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report({ ...FILM, muted: true })), "release");
    assert.deepEqual(world.handOffCalls, []);
    assert.deepEqual(world.credits, []);
  });

  it("фильтры по умолчанию (решение 261, все выключены): короткий беззвучный зацикленный ролик уходит в плеер без пробы", async () => {
    const world = createWorld({ settings: ON_DEFAULT_FILTERS });
    // 20 с, без звука, цикл: при включённых фильтрах общий классификатор назвал бы его «беззвучным украшением».
    const reel: VideoCandidate = { ...SHORT_CLIP, muted: true, loop: true };
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(reel)), "adopt_new_session");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(reel) }]);
    assert.deepEqual(world.probes, [], "проба рекламы не нужна: длина не проверяется");
    assert.deepEqual(world.credits, []);
  });

  it("снят только фильтр звука: беззвучный длинный фильм уходит, беззвучный короткий ролик идёт веткой рекламы", async () => {
    const soundOff: AutoHandoffSettings = { ...ON, video_filters: { ...ALL_FILTERS_ON, sound: { enabled: false, sites: [] } } };
    const world = createWorld({ settings: soundOff });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report({ ...FILM, muted: true })), "adopt_new_session");
    const short = createWorld({ settings: soundOff });
    assert.equal(await short.autoHandoff.heldVideo(TAB, FRAME, report({ ...SHORT_CLIP, muted: true })), "release");
    assert.deepEqual(short.probes, [TAB], "короткий ролик — проба рекламы, как со звуком");
    assert.equal(short.credits.length, 1, "рекламы нет: кредит клика, как со звуком");
    assert.deepEqual(short.handOffCalls, []);
  });

  it("список сайтов фильтра (решение 261): фильтр выключен везде, но сайт вкладки в списке — там он действует", async () => {
    const listed: AutoHandoffSettings = {
      ...ON_DEFAULT_FILTERS,
      video_filters: { ...ON_DEFAULT_FILTERS.video_filters, sound: { enabled: false, sites: ["site.example"] }, loop: { enabled: false, sites: ["site.example"] } },
    };
    // Вкладка подделки — https://site.example/watch: беззвучное и зацикленное отпускаются.
    for (const video of [{ ...FILM, muted: true }, { ...FILM, loop: true }]) {
      const world = createWorld({ settings: listed });
      assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(video)), "release", JSON.stringify(video));
      assert.deepEqual(world.handOffCalls, []);
    }
    // На другом сайте те же видео уходят: список действует только на свои сайты.
    for (const video of [{ ...FILM, muted: true }, { ...FILM, loop: true }]) {
      const world = createWorld({ settings: listed, url: "https://other.example/v" });
      assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(video)), "adopt_new_session", JSON.stringify(video));
    }
  });

  it("передача отказала (protected): фрейму уходит просьба отпустить именно это видео, человеку — тишина", async () => {
    const world = createWorld({ handOffOutcome: Promise.resolve({ kind: "protected" }) });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session");
    await flushPromises();
    assert.deepEqual(world.releases, [{ tabId: TAB, frameId: FRAME, element: FILM.element }]);
    assert.deepEqual(world.reports, []);
  });

  it("плеер недоступен: просьба отпустить и плашка отказа", async () => {
    const outcome: HandOffTabOutcome = { kind: "player_unavailable", reason: "player_not_responding" };
    const world = createWorld({ handOffOutcome: Promise.resolve(outcome) });
    await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    await flushPromises();
    assert.equal(world.releases.length, 1);
    assert.deepEqual(world.reports, [{ tabId: TAB, result: { kind: "outcome", outcome } }]);
  });

  it("окно плеера уже открыто: видео не отпускается, человеку плашка «переключись на окно»", async () => {
    const outcome: HandOffTabOutcome = { kind: "started", player_window: "already_open" };
    const world = createWorld({ handOffOutcome: Promise.resolve(outcome) });
    await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    await flushPromises();
    assert.deepEqual(world.releases, []);
    assert.deepEqual(world.reports, [{ tabId: TAB, result: { kind: "outcome", outcome } }]);
  });

  it("короткий ролик без рекламы: кредит клика всем фреймам на длину + запас, ответ release, передачи нет", async () => {
    const world = createWorld({ probeResult: "no_ad" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(SHORT_CLIP)), "release");
    assert.deepEqual(world.probes, [TAB]);
    assert.deepEqual(world.credits, [{ tabId: TAB, leaseMs: 20_000 + PLAY_CREDIT_GRACE_MS }]);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("короткий ролик, на вкладке видна реклама: передаём, кредита нет", async () => {
    const world = createWorld({ probeResult: "ad_on_screen" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(SHORT_CLIP)), "adopt_new_session");
    assert.deepEqual(world.handOffCalls.map((call) => call.video), [ref(SHORT_CLIP)]);
    assert.deepEqual(world.credits, []);
  });

  it("известная реклама (рекламный хост) передаётся без пробы", async () => {
    const world = createWorld();
    const ad = { ...SHORT_CLIP, file_host: "doubleclick.net" };
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(ad)), "adopt_new_session");
    assert.deepEqual(world.probes, []);
  });
});

describe("режим 2: маршрутизация и одно решение на вкладку", () => {
  it("next_video идёт в передачу вкладки без изменений и синхронно", () => {
    const world = createWorld({ heldVideoReply: "adopt" });
    const held = report(FILM, "next_video");
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, held), "adopt");
    assert.deepEqual(world.heldVideoCalls, [held]);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("у вкладки живая передача и её сторож взял видео (переезд 09c2): adopt, новой передачи нет", () => {
    const world = createWorld({ activeTabs: [TAB], heldVideoReply: "adopt" });
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt");
    assert.equal(world.heldVideoCalls.length, 1);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("у вкладки живая передача, а человек запустил другое видео: оно заменяет прежнее через handOffTab с выбранным видео", async () => {
    const world = createWorld({ activeTabs: [TAB], heldVideoReply: "release" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session");
    assert.equal(world.heldVideoCalls.length, 1, "сначала спросили сеанс вкладки");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
  });

  it("второе донесение, пока первое решается или стартует: release; после исхода вкладка свободна", async () => {
    let finish: (outcome: HandOffTabOutcome) => void = () => {};
    const world = createWorld({ handOffOutcome: new Promise((resolve) => (finish = resolve)) });
    const first = world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", "пока решается");
    assert.equal(await first, "adopt_new_session");
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", "пока стартует");
    assert.equal(world.handOffCalls.length, 1);
    finish({ kind: "protected" });
    await flushPromises();
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session", "после исхода");
    assert.equal(world.handOffCalls.length, 2);
  });

  it("донесения разных вкладок не мешают друг другу", async () => {
    const world = createWorld({ handOffOutcome: new Promise(() => {}) });
    assert.equal(await world.autoHandoff.heldVideo(1, FRAME, report(FILM)), "adopt_new_session");
    assert.equal(await world.autoHandoff.heldVideo(2, FRAME, report(FILM)), "adopt_new_session");
    assert.deepEqual(world.handOffCalls.map((call) => call.tabId), [1, 2]);
  });

  it("другой вход начал передачу этой вкладки, пока мы судили: решает handOffTab (то же видео или замена)", async () => {
    const world = createWorld();
    const answer = world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    world.activeTabs.push(TAB);
    assert.equal(await answer, "adopt_new_session");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
  });
});

describe("слушатель донесений: ответ позже", () => {
  const sender: FrameSender = { id: "ext", tab: { id: TAB }, frameId: FRAME };

  it("готовый ответ уходит сразу и канал не держится; обещание держит канал и отвечает позже", async () => {
    const world = createWorld();
    const replies: HeldVideoReply[] = [];
    const listener = createHeldVideoListener("ext", (tabId, frameId, held) => world.autoHandoff.heldVideo(tabId, frameId, held));
    assert.equal(listener(report(FILM, "next_video"), sender, (reply) => replies.push(reply)), false);
    assert.deepEqual(replies, ["release"]);
    assert.equal(listener(report(FILM), sender, (reply) => replies.push(reply)), true);
    assert.deepEqual(replies, ["release"]);
    await flushPromises();
    assert.deepEqual(replies, ["release", "adopt_new_session"]);
  });

  it("сорвавшееся обещание — release и отметка в журнале", async () => {
    const notes: string[] = [];
    const replies: HeldVideoReply[] = [];
    const listener = createHeldVideoListener("ext", () => Promise.reject(new Error("boom")), (message) => notes.push(message));
    assert.equal(listener(report(FILM), sender, (reply) => replies.push(reply)), true);
    await flushPromises();
    assert.deepEqual(replies, ["release"]);
    assert.equal(notes.length, 1);
  });
});

describe("что сказать человеку об исходе", () => {
  it("таблица: молчим, кроме окна «уже открыто» и отказов плеера", () => {
    const outcomes: readonly { outcome: HandOffTabOutcome; speaks: boolean }[] = [
      { outcome: { kind: "started", player_window: "new" }, speaks: false },
      { outcome: { kind: "started", player_window: "already_open" }, speaks: true },
      { outcome: { kind: "player_unavailable", reason: "x" }, speaks: true },
      { outcome: { kind: "failed", reason: "x" }, speaks: true },
      { outcome: { kind: "no_video" }, speaks: false },
      { outcome: { kind: "protected" }, speaks: false },
      { outcome: { kind: "not_supported_yet", form: "native_hls" }, speaks: false },
      { outcome: { kind: "reload_needed", reason: "script_missing" }, speaks: false },
      { outcome: { kind: "video_gone" }, speaks: false },
      { outcome: { kind: "local_address_refused" }, speaks: false },
      { outcome: { kind: "local_address_needs_consent", host: "nas.local" }, speaks: false },
    ];
    for (const row of outcomes) {
      const notice = noticeOfAutoHandoffOutcome(row.outcome);
      assert.deepEqual(notice, row.speaks ? { kind: "outcome", outcome: row.outcome } : undefined, row.outcome.kind);
    }
  });
});

describe("режим 2: предложение «Ask» (12b)", () => {
  const ASK_SITE: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "site.example": "ask" } };
  const DOC = "0123456789abcdef0123456789abcdef";
  const ASKED_FILM: VideoCandidate = { ...FILM, document: DOC };
  const choiceOf = (choice: ViewerOfferChoice, candidate: VideoCandidate = ASKED_FILM): ViewerOfferChoiceMessage => ({
    kind: VIEWER_OFFER_CHOICE_KIND,
    document: candidate.document,
    element: candidate.element,
    choice,
  });

  /** Человек нажал play на сайте `ask`: ответ сторожу и висящее предложение. */
  async function askedWorld(options: WorldOptions = {}) {
    const world = createWorld({ settings: ASK_SITE, ...options });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(ASKED_FILM)), "ask_viewer");
    return world;
  }

  it("сайт ask (даже при выключенном режиме): ответ ask_viewer, передачи и отпускания нет", async () => {
    const world = await askedWorld();
    await flushPromises();
    assert.deepEqual(world.handOffCalls, []);
    assert.deepEqual(world.releases, []);
  });

  it("новый сайт при unknown_site_rule ask — тоже вопрос; без звука вопроса нет", async () => {
    const world = createWorld({ settings: { ...ON, unknown_site_rule: "ask" } });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(ASKED_FILM)), "ask_viewer");
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report({ ...ASKED_FILM, element: 9, muted: true })), "release");
  });

  it("«Play in Fastiplayer»: передача с точной ссылкой, правило не тронуто", async () => {
    const world = await askedWorld();
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("fastiplayer")), "hand_off");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(ASKED_FILM) }]);
    assert.deepEqual(world.stored(), ASK_SITE);
  });

  it("«Play here»: отпустить, передачи нет, правило не тронуто", async () => {
    const world = await askedWorld();
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("here")), "play_here");
    assert.deepEqual(world.handOffCalls, []);
    assert.deepEqual(world.stored(), ASK_SITE);
  });

  it("«Always»: правило сайта always и сразу передача; «Never»: правило never и играть здесь", async () => {
    const always = await askedWorld();
    assert.equal(await always.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("always")), "hand_off");
    assert.deepEqual((always.stored() as AutoHandoffSettings).site_rules, { "site.example": "always" });
    assert.equal(always.handOffCalls.length, 1);

    const never = await askedWorld();
    assert.equal(await never.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("never")), "play_here");
    assert.deepEqual((never.stored() as AutoHandoffSettings).site_rules, { "site.example": "never" });
    assert.deepEqual(never.handOffCalls, []);
  });

  it("правило родителя меняется там, где записано; новый сайт без правила получает своё", async () => {
    const parent = await askedWorld({ settings: { ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "example.com": "ask" } }, url: "https://www.video.example.com/w" });
    await parent.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("never"));
    assert.deepEqual((parent.stored() as AutoHandoffSettings).site_rules, { "example.com": "never" });

    const fresh = await askedWorld({ settings: { ...ON, unknown_site_rule: "ask" }, url: "https://www.video.example.com/w" });
    await fresh.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("always"));
    assert.deepEqual((fresh.stored() as AutoHandoffSettings).site_rules, { "video.example.com": "always" });
  });

  it("сбой записи правила действие не отменяет, а пишет причину в журнал", async () => {
    const world = await askedWorld({ writeFails: true });
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("always")), "hand_off");
    assert.equal(world.handOffCalls.length, 1);
    assert.deepEqual(world.stored(), ASK_SITE);
    assert.ok(world.notes.some((note) => note.includes("could not save")));
  });

  it("выбор не того видео или не того фрейма — no_offer: ничего не пишется и не передаётся", async () => {
    const world = await askedWorld({ settings: { ...ON, unknown_site_rule: "ask" } });
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("always", { ...ASKED_FILM, element: 77 })), "no_offer");
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME + 1, choiceOf("always")), "no_offer");
    assert.deepEqual(world.handOffCalls, []);
    assert.deepEqual((world.stored() as AutoHandoffSettings).site_rules, {});
    // Само предложение от чужих выборов не пропало: настоящий выбор проходит.
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("here")), "play_here");
  });

  it("после сна фона (памяти нет) выбор принимается, только пока сайт вкладки и правда ask", async () => {
    const asleep = createWorld({ settings: ASK_SITE });
    assert.equal(await asleep.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("fastiplayer")), "hand_off");
    const notAsk = createWorld({ settings: ON });
    assert.equal(await notAsk.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("always")), "no_offer");
    assert.deepEqual((notAsk.stored() as AutoHandoffSettings).site_rules, {});
    assert.deepEqual(notAsk.handOffCalls, []);
  });

  it("новое предложение на той же вкладке снимает прежнее: его видео остаётся на паузе, выбор по нему уже не принят", async () => {
    const world = await askedWorld();
    const second: VideoCandidate = { ...ASKED_FILM, element: 8 };
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(second)), "ask_viewer");
    assert.deepEqual(world.withdrawals, [{ tabId: TAB, frameId: FRAME, element: ASKED_FILM.element }]);
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("fastiplayer")), "no_offer");
    assert.equal(await world.autoHandoff.viewerOfferChoice(TAB, FRAME, choiceOf("fastiplayer", second)), "hand_off");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(second) }]);
  });
});
