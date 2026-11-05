// Общий фейковый мир сценариев передачи вкладки (tab-handoff, честные концы 09a2):
// фон целиком (`createTabHandoffs`) на фейковых часах, двери плеера, сокете и порте фрейма.
import assert from "node:assert/strict";
import type { FrameInjectionResult } from "../../src/background/handoff/frame-survey.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/handoff-session.ts";
import { NO_LOCAL_ADDRESS_CONSENT, type LocalAddressConsent } from "../../src/background/handoff/file-address-guard.ts";
import { createFileReferrerRules } from "../../src/background/handoff/file-referrer-rule.ts";
import { createLocalAddressBlock, localAddressBlockRules } from "../../src/background/handoff/local-address-block-rules.ts";
import { createSessionRules, FIRST_REQUEST_RULE_ID, type SessionRule, type SessionRulesApi } from "../../src/background/handoff/session-rules.ts";
import type { RangeFetch } from "../../src/background/handoff/range-fetcher.ts";
import { createMemoryTabModelStore, type TabModelStore } from "../../src/background/tab-model/session-store.ts";
import { createTabModel } from "../../src/background/tab-model/tab-model.ts";
import { createTabHandoffs } from "../../src/background/handoff/tab-handoffs.ts";
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

/** Хост страниц экстеншена в тестах (инициатор его запросов). */
export const EXTENSION_HOST = "extension-id";

/** Необязательные особенности мира сценария. */
export interface WorldOptions {
  readonly localAddressSetting?: LocalAddressSetting | Promise<LocalAddressSetting>;
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
  let tabAlive = true;
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
  const handoffs = createTabHandoffs({
    tabModel,
    door,
    scheduler: clock.scheduler,
    createWebSocket: sockets.create,
    connectFrame: framePorts.connect,
    executeInAllFrames: () => {
      surveyCount += 1;
      return Promise.resolve(frames);
    },
    describeTab: () => Promise.resolve({ title: "Серія 3\nОзвучка", url: "https://www.example.com/watch?v=1" }),
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
    sessionRules,
    settingFailures,
    handoffs,
    tabModel,
    store,
    storeFailures,
    surveys: () => surveyCount,
    tabExistsQuestions: () => tabExistsQuestions,
    closeTab: () => void (tabAlive = false),
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
