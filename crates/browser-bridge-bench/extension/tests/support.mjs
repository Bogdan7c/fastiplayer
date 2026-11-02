// Общие помощники тестов стендового перехватчика. Эталон формы сообщений — НАСТОЯЩИЙ строгий
// разборщик протокола из extensions/browser (TypeScript, node снимает типы сам): лишнее поле,
// неверный вариант или значение вне правил = отказ, как у Rust-плеера.
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const extensionDir = path.resolve(here, "..");
const repoRoot = path.resolve(extensionDir, "../../..");
export const fixturesDir = path.join(repoRoot, "crates/browser-bridge-protocol/fixtures");
const require = createRequire(import.meta.url);

// Библиотеки — обычные скрипты (UMD-подобные), в node приходят через module.exports.
export const frameLib = require(path.join(extensionDir, "lib/frame.js"));
export const base64Lib = require(path.join(extensionDir, "lib/base64.js"));
export const activationLib = require(path.join(extensionDir, "lib/activation.js"));
export const trackerLib = require(path.join(extensionDir, "lib/generation-tracker.js"));
export const driverLib = require(path.join(extensionDir, "lib/session-driver.js"));

const protocolDir = path.join(repoRoot, "extensions/browser/src/protocol");
export const protocol = await import(path.join(protocolDir, "index.ts"));
export const { decodeDataFrame } = await import(path.join(protocolDir, "data-frame.ts"));

export const readFixture = (relative) => readFileSync(path.join(fixturesDir, relative));

// Разбор сообщения страницы строгим разборщиком; провал теста с причиной, если отвергнуто.
export function expectValidPageMessage(text) {
  const result = protocol.decodePageSession(text);
  if (!result.ok) throw new Error("protocol rejected " + text + " :: " + JSON.stringify(result.error));
  return result.value;
}
