// Ждущие команды тени для ещё не названного плееру источника (16a3): что и в каком порядке уходит плееру.

import assert from "node:assert/strict";
import { test } from "node:test";

import { ComingSourceCommands, isSourceCommand, type SourceCommand } from "../../src/page/coming-source-commands.ts";

const play: SourceCommand = { kind: "page_playback", request: { kind: "play" } };
const pause: SourceCommand = { kind: "page_playback", request: { kind: "pause" } };
const seekTo = (targetUs: number): SourceCommand => ({ kind: "page_seek", target_us: targetUs });
const rate = (permille: number): SourceCommand => ({ kind: "page_playback", request: { kind: "set_rate", rate: permille } });

test("named source: a command goes out at once and nothing stays waiting", () => {
  const commands = new ComingSourceCommands();
  assert.deepEqual(commands.submit(play, "named"), [play]);
  assert.equal(commands.hasWaiting, false);
});

test("source not yet named: commands wait and leave in the page's order once it is named", () => {
  const commands = new ComingSourceCommands();
  assert.deepEqual(commands.submit(play, "not_yet_named"), []);
  assert.deepEqual(commands.submit(seekTo(0), "not_yet_named"), []);
  assert.equal(commands.hasWaiting, true);
  assert.deepEqual(commands.release("not_yet_named"), [], "still unknown: nothing leaves");
  assert.deepEqual(commands.release("named"), [play, seekTo(0)]);
  assert.equal(commands.hasWaiting, false);
  assert.deepEqual(commands.release("named"), [], "a released command is not sent twice");
});

test("one slot per kind: the last seek, the last play/pause and the last rate win, in the order of their last submission", () => {
  const commands = new ComingSourceCommands();
  for (const command of [seekTo(5_000_000), play, rate(1500), pause, seekTo(0), play, rate(1000)]) commands.submit(command, "not_yet_named");
  assert.deepEqual(commands.release("named"), [seekTo(0), play, rate(1000)], "three commands at most, whatever the page sends");
});

test("a command submitted when the source becomes named carries the waiting ones out first", () => {
  const commands = new ComingSourceCommands();
  commands.submit(seekTo(0), "not_yet_named");
  assert.deepEqual(commands.submit(play, "named"), [seekTo(0), play]);
});

test("only seeks and playback requests are commands for the source", () => {
  assert.equal(isSourceCommand(play), true);
  assert.equal(isSourceCommand(seekTo(1)), true);
  assert.equal(isSourceCommand({ kind: "shadow_alive" }), false);
  assert.equal(isSourceCommand({ kind: "next_episode_wait_began" }), false);
});
