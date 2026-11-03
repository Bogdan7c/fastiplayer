// Общий фейковый мир сценариев передачи вкладки (tab-handoff, честные концы 09a2):
// фон целиком (`createTabHandoffs`) на фейковых часах, двери плеера, сокете и порте фрейма.
import assert from "node:assert/strict";
import type { FrameInjectionResult } from "../../src/background/handoff/frame-survey.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/handoff-session.ts";
import { createTabHandoffs } from "../../src/background/handoff/tab-handoffs.ts";
import type { HandOffTabOutcome, PageEvent, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { createFakeClock, createFakeDoor, flushPromises, recordFramePorts, recordSockets } from "./support.ts";

export const TAB = 12;
export const MP4 = 'video/mp4; codecs="avc1.42c00a"';
export const PLAYING_VIDEO: VideoCandidate = { document: "doc-7f3a", element: 4, has_live_media: true, playing: true, visible_area_px: 200_000 };

export function setup(frames: readonly FrameInjectionResult[] = [{ frameId: 0, result: [PLAYING_VIDEO] }]) {
  const clock = createFakeClock();
  const door = createFakeDoor();
  const sockets = recordSockets();
  const framePorts = recordFramePorts();
  const ended: HandoffSessionEnd[] = [];
  let tabAlive = true;
  let surveyCount = 0;
  let tabExistsQuestions = 0;
  const handoffs = createTabHandoffs({
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
    onSessionEnded: (_tabId, end) => void ended.push(end),
  });
  return {
    clock,
    door,
    sockets,
    framePorts,
    ended,
    handoffs,
    surveys: () => surveyCount,
    tabExistsQuestions: () => tabExistsQuestions,
    closeTab: () => void (tabAlive = false),
  };
}

export type World = ReturnType<typeof setup>;

/** Довести передачу до «started»: плеер принял сеанс, страница начала тень. */
export async function startHandoff(world: World): Promise<HandOffTabOutcome> {
  const outcome = world.handoffs.handOffTab(TAB);
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
