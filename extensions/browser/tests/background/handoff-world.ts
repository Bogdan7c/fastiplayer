// Общий фейковый мир сценариев передачи вкладки (tab-handoff, честные концы 09a2):
// фон целиком (`createTabHandoffs`) на фейковых часах, двери плеера, сокете и порте фрейма.
import assert from "node:assert/strict";
import type { FrameInjectionResult } from "../../src/background/handoff/frame-survey.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/handoff-session.ts";
import { NO_LOCAL_ADDRESS_CONSENT, type LocalAddressConsent } from "../../src/background/handoff/file-address-guard.ts";
import type { AdMirrorBrowser } from "../../src/background/handoff/ad-skip-mirror.ts";
import type { VariantMirrorBrowser } from "../../src/background/handoff/variant-mirror.ts";
import type { VariantActionOutcome, VariantCommand } from "../../src/shared/variant-watch-contract.ts";
import type { PressSkipOutcome } from "../../src/shared/ad-watch-contract.ts";
import { createFileReferrerRules } from "../../src/background/handoff/file-referrer-rule.ts";
import { createLocalAddressBlock, localAddressBlockRules } from "../../src/background/handoff/local-address-block-rules.ts";
import { createSessionRules, FIRST_REQUEST_RULE_ID, type SessionRule, type SessionRulesApi } from "../../src/background/handoff/session-rules.ts";
import type { RangeFetch } from "../../src/background/handoff/range-fetcher.ts";
import type { PageReopenBrowser } from "../../src/background/handoff/page-return/page-reopen-browser.ts";
import type { PageTabRevealApi } from "../../src/background/handoff/page-tab-reveal.ts";
import { createMemoryTabModelStore, type TabModelStore } from "../../src/background/tab-model/session-store.ts";
import { createTabModel } from "../../src/background/tab-model/tab-model.ts";
import { createTabHandoffs } from "../../src/background/handoff/tab-handoffs.ts";
import { createChoiceMemoryStore } from "../../src/background/choice-memory/choice-memory-store.ts";
import { memoryStore } from "./auto-handoff-lists-fake-dom.ts";
import type { HandOffTabOutcome, PageEvent, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import type { LocalAddressSetting } from "../../src/shared/local-address-setting.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock, createFakeDoor, flushPromises, recordFramePorts, recordSockets } from "./support.ts";

export const TAB = 12;
export const MP4 = 'video/mp4; codecs="avc1.42c00a"';
export const PLAYING_VIDEO: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "media_source", in_worker: false }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS };

/** Сеть фона по умолчанию: ни один сценарий без файла её звать не должен. */
const NO_NETWORK: RangeFetch = () => Promise.reject(new Error("test: unexpected background fetch"));

/** Фейк сессионных правил браузера: хранит правила и журнал вызовов. */
export function createFakeSessionRules(initialRuleIds: readonly number[] = []) {
  const rules = new Map<number, SessionRule | undefined>(initialRuleIds.map((id) => [id, undefined]));
  /** Сколько раз правило ставили (снятые из `rules` исчезают, а здесь остаются). */
  const added: SessionRule[] = [];
  const api: SessionRulesApi = {
    updateSessionRules: async ({ removeRuleIds = [], addRules = [] }) => {
      for (const id of removeRuleIds) rules.delete(id);
      for (const rule of addRules) {
        rules.set(rule.id, rule);
        added.push(rule);
      }
    },
    getSessionRules: async () => [...rules.keys()].map((id) => ({ id })),
  };
  /** Правила на запрос: постоянные правила заслона стоят всегда и в счёт не идут. */
  const requestRuleCount = (): number => [...rules.keys()].filter((id) => id >= FIRST_REQUEST_RULE_ID).length;
  const addedRequestRules = (): SessionRule[] => added.filter((rule) => rule.id >= FIRST_REQUEST_RULE_ID);
  return { api, rules, added, requestRuleCount, addedRequestRules };
}

/** Фейк браузерной стороны дублёра кнопки пропуска (09d): журнал аренд, нажатий и выводов вперёд. */
export function createFakeAdMirrorBrowser() {
  const armings: { readonly tabId: number; readonly leaseMs: number }[] = [];
  const pressed: { readonly tabId: number; readonly frameId: number }[] = [];
  const notes: string[] = [];
  const behavior: { pressOutcome: PressSkipOutcome } = { pressOutcome: "pressed" };
  const browser: AdMirrorBrowser = {
    armAdWatch: (tabId, leaseMs) => void armings.push({ tabId, leaseMs }),
    pressSkipInFrame: (tabId, frameId) => {
      pressed.push({ tabId, frameId });
      return Promise.resolve(behavior.pressOutcome);
    },
  };
  return { browser, armings, pressed, notes, behavior };
}

/** Фейк браузерной стороны меню вариантов (14b): журнал аренд и команд, исход команды задаёт сценарий. */
export function createFakeVariantMirrorBrowser() {
  const armings: { readonly tabId: number; readonly leaseMs: number }[] = [];
  const commands: { readonly tabId: number; readonly frameId: number; readonly command: VariantCommand }[] = [];
  const behavior: { outcome: VariantActionOutcome | Error } = { outcome: "done" };
  const browser: VariantMirrorBrowser = {
    armVariantWatch: (tabId, leaseMs) => void armings.push({ tabId, leaseMs }),
    sendVariantCommand: (tabId, frameId, command) => {
      commands.push({ tabId, frameId, command });
      return behavior.outcome instanceof Error ? Promise.reject(behavior.outcome) : Promise.resolve(behavior.outcome);
    },
  };
  return { browser, armings, commands, behavior };
}

/** Хост страниц экстеншена в тестах (инициатор его запросов). */
export const EXTENSION_HOST = "extension-id";

/** Необязательные особенности мира сценария. */
export interface WorldOptions {
  readonly localAddressSetting?: LocalAddressSetting | Promise<LocalAddressSetting>;
  /**
   * Окно вкладки (11b2). Есть — фон знает, где жила страница, и уход страницы может стать «сиротой»; нет —
   * как до 11b2: браузер окна не называет, уход кончается `tab_gone`.
   */
  readonly tabWindowId?: number;
  /** Браузерная сторона возврата страницы (11b2); по умолчанию — «нет окна». */
  readonly pageReopen?: PageReopenBrowser;
  /** Браузерная сторона «показать страницу» (13c); по умолчанию — вкладка на месте, всё получается. */
  readonly pageTabReveal?: PageTabRevealApi;
  /** Вкладка в приватном окне (14b2); по умолчанию — обычное окно. */
  readonly incognito?: boolean;
}

/** «Показать страницу» по умолчанию: вкладка в окне 1, окно слушается (сценарии до 13c его не зовут). */
const NO_PAGE_TAB_REVEAL: PageTabRevealApi = {
  activateTab: async () => ({ windowId: 1 }),
  focusWindow: async () => {},
  drawAttention: async () => {},
};

/** Возврат по умолчанию: окна нет, ничего не открывается (сценарии до 11b2 его не зовут). */
const NO_PAGE_REOPEN: PageReopenBrowser = {
  findWindow: async () => undefined,
  findOpenTab: async () => undefined,
  openTab: () => Promise.reject(new Error("test: unexpected page reopen")),
  bringForwardMuted: () => Promise.reject(new Error("test: unexpected page reopen")),
  setMuted: async () => {},
  closeTab: async () => {},
  armPageReturn: async () => "no_script",
  describeTab: async () => undefined,
  readVideos: async () => [],
};

/** Версия экстеншена в этом мире. */
export const WORLD_EXTENSION_VERSION = "1.2.3";

/** Массив кандидатов в описании фрейма — ответ нашего скрипта этой же версии; другой ответ идёт как есть. */
function scriptedFrameResult(frame: FrameInjectionResult): FrameInjectionResult {
  return Array.isArray(frame.result) ? { ...frame, result: { script_version: WORLD_EXTENSION_VERSION, candidates: frame.result } } : frame;
}

export function setup(
  frames: readonly FrameInjectionResult[] | Promise<readonly FrameInjectionResult[]> = [{ frameId: 0, result: [PLAYING_VIDEO] }],
  fetchRange: RangeFetch = NO_NETWORK,
  store: TabModelStore = createMemoryTabModelStore(),
  options: WorldOptions = {},
) {
  const clock = createFakeClock();
  const door = createFakeDoor();
  const sockets = recordSockets();
  const framePorts = recordFramePorts();
  const ended: HandoffSessionEnd[] = [];
  const adMirrorFake = createFakeAdMirrorBrowser();
  const variantMirrorFake = createFakeVariantMirrorBrowser();
  const armings: { readonly tabId: number; readonly leaseMs: number }[] = [];
  // Ответ фреймов на опрос; сценарий может его сменить (страница убрала видео, поставила другое).
  let currentFrames = frames;
  // Опрос фреймов отклоняется (служебная страница браузера: внедрение запрещено).
  let surveyError: Error | undefined;
  let tabAlive = true;
  let tabUrl = "https://www.example.com/watch?v=1";
  let surveyCount = 0;
  let tabExistsQuestions = 0;
  const storeFailures: unknown[] = [];
  const settingFailures: unknown[] = [];
  const sessionRules = createFakeSessionRules();
  const requestRules = createSessionRules({
    api: sessionRules.api,
    scheduler: clock.scheduler,
    permanentRules: localAddressBlockRules(EXTENSION_HOST),
    reportFailureOnce: () => {},
  });
  const tabModel = createTabModel({ store, now: () => clock.scheduler.now(), reportStoreFailure: (error) => void storeFailures.push(error) });
  // Память выбора (14b2) на своём хранилище в памяти: тесты читают, что в неё записано.
  const choiceStorage = memoryStore();
  const choiceMemory = createChoiceMemoryStore({ store: choiceStorage.store, now: () => clock.scheduler.now() });
  const handoffs = createTabHandoffs({
    tabModel,
    choiceMemory,
    door,
    scheduler: clock.scheduler,
    createWebSocket: sockets.create,
    connectFrame: framePorts.connect,
    armNextVideo: (tabId, leaseMs) => void armings.push({ tabId, leaseMs }),
    adMirrorBrowser: adMirrorFake.browser,
    variantMirrorBrowser: variantMirrorFake.browser,
    noteForDeveloper: (message) => void adMirrorFake.notes.push(message),
    executeInAllFrames: () => {
      surveyCount += 1;
      return surveyError === undefined ? Promise.resolve(currentFrames).then((frames) => frames.map(scriptedFrameResult)) : Promise.reject(surveyError);
    },
    extensionVersion: WORLD_EXTENSION_VERSION,
    describeTab: () =>
      Promise.resolve(tabAlive ? { title: "Серія 3\nОзвучка", url: tabUrl, windowId: options.tabWindowId, incognito: options.incognito ?? false } : undefined),
    pageReopen: options.pageReopen ?? NO_PAGE_REOPEN,
    pageTabReveal: options.pageTabReveal ?? NO_PAGE_TAB_REVEAL,
    tabExists: () => {
      tabExistsQuestions += 1;
      return Promise.resolve(tabAlive);
    },
    sessionKeyFor: (tabId, attempt) => `t${tabId}-${attempt}-test`,
    fetchRange,
    fileReferrer: createFileReferrerRules(requestRules),
    localAddressBlock: createLocalAddressBlock(requestRules, EXTENSION_HOST),
    readLocalAddressSetting: () => Promise.resolve(options.localAddressSetting ?? "block"),
    reportSettingFailure: (error) => void settingFailures.push(error),
    onSessionEnded: (_tabId, end) => void ended.push(end),
  });
  return {
    clock,
    door,
    sockets,
    framePorts,
    ended,
    armings,
    adMirror: adMirrorFake,
    variantMirror: variantMirrorFake,
    choiceStorage,
    sessionRules,
    settingFailures,
    handoffs,
    tabModel,
    store,
    storeFailures,
    surveys: () => surveyCount,
    tabExistsQuestions: () => tabExistsQuestions,
    setFrames: (next: readonly FrameInjectionResult[]) => void (currentFrames = next),
    forbidSurveys: (error: Error) => void (surveyError = error),
    closeTab: () => void (tabAlive = false),
    navigateTab: (url: string) => void (tabUrl = url),
  };
}

export type World = ReturnType<typeof setup>;

/** Довести передачу до «started»: плеер принял сеанс, страница начала тень. */
export async function startHandoff(world: World, consent: LocalAddressConsent = NO_LOCAL_ADDRESS_CONSENT): Promise<HandOffTabOutcome> {
  const outcome = world.handoffs.handOffTab(TAB, consent);
  await flushPromises();
  const socket = world.sockets.last();
  socket.open();
  socket.receive({ type: "attached" });
  const port = world.framePorts.last();
  const begin = port.posted[0];
  assert.ok(begin?.kind === "begin_handoff");
  port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" } satisfies PageEvent);
  return outcome;
}

/** Страница передала одно поколение: SourceBuffer и один кусок. */
export function deliverStream(world: World): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
  port.deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([1, 2, 3]) });
}
