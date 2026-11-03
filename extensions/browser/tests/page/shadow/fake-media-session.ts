// Фейковый MediaSession для стража и тени: что «браузер» отдал бы системе медиакнопок.

const KNOWN_ACTIONS = ["play", "pause", "seekto", "seekbackward", "seekforward", "nexttrack", "previoustrack", "stop"];

/** Фейковый MediaSession: поля `system*` — то, что браузер отдал бы системе медиакнопок. */
export function createFakeMediaSessionWindow() {
  class FakeMediaMetadata {
    readonly title: string;
    constructor(init: { title: string }) {
      this.title = init.title;
    }
  }
  class FakeMediaSession {
    systemMetadata: unknown = null;
    systemPlaybackState = "none";
    readonly systemHandlers = new Map<string, unknown>();
    systemPositionState: unknown = undefined;
    /** Каждый вызов, дошедший до «браузера» (даже если тут же отменён). */
    readonly systemPositionCalls: unknown[] = [];
    get metadata(): unknown {
      return this.systemMetadata;
    }
    set metadata(value: unknown) {
      if (value !== null && !(value instanceof FakeMediaMetadata)) throw new TypeError("not MediaMetadata");
      this.systemMetadata = value;
    }
    get playbackState(): string {
      return this.systemPlaybackState;
    }
    set playbackState(value: string) {
      if (["none", "paused", "playing"].includes(value)) this.systemPlaybackState = value;
    }
    setActionHandler(action: string, handler: unknown): void {
      if (!KNOWN_ACTIONS.includes(action)) throw new TypeError(`unknown action ${action}`);
      if (handler === null) this.systemHandlers.delete(action);
      else this.systemHandlers.set(action, handler);
    }
    setPositionState(state?: { duration: number; position: number }): void {
      this.systemPositionCalls.push(state);
      if (state === undefined) {
        this.systemPositionState = undefined;
        return;
      }
      if (state.position > state.duration) throw new TypeError("position after duration");
      this.systemPositionState = state;
    }
  }
  const mediaSession = new FakeMediaSession();
  const win = { navigator: { mediaSession }, MediaMetadata: FakeMediaMetadata, TypeError };
  return { win: win as unknown as Window, mediaSession, FakeMediaMetadata };
}
