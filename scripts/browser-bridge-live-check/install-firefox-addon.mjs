// Временная установка распакованного расширения в запущенный Firefox через WebDriver BiDi
// (webExtension.install). node install-firefox-addon.mjs <порт remote-debugging> <папка расширения>
const [port, extPath] = process.argv.slice(2);
let id = 0;
const pending = new Map();
let socket;
for (let attempt = 0; attempt < 60; attempt++) {
  try {
    socket = await new Promise((resolve, reject) => {
      const s = new WebSocket(`ws://127.0.0.1:${port}/session`);
      s.onopen = () => resolve(s);
      s.onerror = () => reject(new Error("connect"));
    });
    break;
  } catch { await new Promise((r) => setTimeout(r, 500)); }
}
if (!socket) { console.error("Firefox BiDi did not come up"); process.exit(1); }
socket.onmessage = (e) => { const m = JSON.parse(e.data); pending.get(m.id)?.(m); };
const call = (method, params = {}) => new Promise((resolve) => {
  const n = ++id; pending.set(n, resolve); socket.send(JSON.stringify({ id: n, method, params }));
});
console.log("session.new", JSON.stringify(await call("session.new", { capabilities: {} })).slice(0, 120));
const result = await call("webExtension.install", { extensionData: { type: "path", path: extPath } });
console.log("install", JSON.stringify(result));
process.exit(result.type === "success" ? 0 : 1);
