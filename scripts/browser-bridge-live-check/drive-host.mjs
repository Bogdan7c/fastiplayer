// Без браузера: сам играет роль браузера для хоста (argv Chromium) и проигрывает тот же план.
// Использование: node drive-host.mjs <host-binary> <папка-расширения> <id-расширения-Chromium>
// (DBUS_SESSION_BUS_ADDRESS — частная шина; папка — готовая extension-build с plan.json и media/)
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const { playStream } = require(path.join(here, "extension/runner.js"));
const [hostBinary, extensionDir, chromiumExtensionId] = process.argv.slice(2);
if (!hostBinary || !extensionDir || !chromiumExtensionId) {
  console.error("usage: node drive-host.mjs <host-binary> <extension-dir> <chromium-extension-id>");
  process.exit(2);
}
const plan = JSON.parse(readFileSync(path.join(extensionDir, "plan.json"), "utf8"));
const media = Object.fromEntries(plan.files.map((f) => [f, new Uint8Array(readFileSync(path.join(extensionDir, "media", f)))]));
const origin = `chrome-extension://${chromiumExtensionId}/`;
const t0 = Date.now();
const log = (s) => console.log(`[drive +${((Date.now() - t0) / 1000).toFixed(2)}s] ${s}`);

const host = spawn(hostBinary, [origin], { stdio: ["pipe", "pipe", "inherit"] });
let buffer = Buffer.alloc(0);
const waiting = [];
host.stdout.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32LE(0)) {
    const len = buffer.readUInt32LE(0);
    const message = JSON.parse(buffer.subarray(4, 4 + len).toString());
    buffer = buffer.subarray(4 + len);
    log("host -> " + message.type);
    waiting.shift()?.(message);
  }
});
const hostExit = new Promise((r) => host.on("exit", (code, sig) => r({ code, sig })));
function request(message) {
  const body = Buffer.from(JSON.stringify(message));
  const head = Buffer.alloc(4); head.writeUInt32LE(body.length);
  host.stdin.write(Buffer.concat([head, body]));
  log("-> host " + message.type);
  return new Promise((r) => waiting.push(r));
}

const hello = await request({ type: "hello", protocol: { major: 1, minor: 0 }, software_version: "0.1.0", capabilities: [] });
if (hello.type !== "hello_accepted") throw new Error(JSON.stringify(hello));
const granted = await request({
  type: "open_session", request_id: 1, session_key: "tab-1-video-" + (Date.now() % 100000),
  page: { title: "Bridge live check", domain: "localhost" }, intent: "window",
  hints: { duration: { kind: "finite", duration_us: plan.duration_us }, position: { kind: "unknown" } },
});
if (granted.type !== "session_granted") throw new Error(JSON.stringify(granted));
const stats = await playStream({
  url: `ws://127.0.0.1:${granted.port}/`, attach: { session_key: granted.session_key, token: granted.token },
  plan, media, log, report: () => {},
  lingerAfterEndMs: Number(process.env.LINGER_MS ?? 1000),
});
console.log("STATS " + JSON.stringify(stats));
host.stdin.end(); // EOF, как закрытый браузер
const exit = await Promise.race([hostExit, new Promise((r) => setTimeout(() => r("host did not exit in 10s"), 10000))]);
console.log("HOST EXIT " + JSON.stringify(exit));
process.exit(stats.phase === "done" ? 0 : 1);
