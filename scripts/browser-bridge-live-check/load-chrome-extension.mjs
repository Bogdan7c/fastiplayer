// Загрузка распакованного расширения в запущенный Chrome через CDP `Extensions.loadUnpacked`
// (нужны флаги браузера --remote-debugging-port и --enable-unsafe-extension-debugging).
// node load-chrome-extension.mjs <порт отладки> <папка расширения (путь, видимый БРАУЗЕРУ)> <ожидаемый ID>
// Код выхода: 0 - загружено и ID совпал; 1 - иначе.
const [port, extensionPath, expectedId] = process.argv.slice(2);

// Адрес браузерного WebSocket берём из /json/version; ждём, пока Chrome поднимется.
async function waitForBrowserSocketUrl() {
  for (let attempt = 0; attempt < 120; attempt++) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (response.ok) return (await response.json()).webSocketDebuggerUrl;
    } catch { /* браузер ещё стартует */ }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  return null;
}

const socketUrl = await waitForBrowserSocketUrl();
if (!socketUrl) { console.error("Chrome DevTools endpoint did not come up"); process.exit(1); }
const socket = await new Promise((resolve, reject) => {
  const opened = new WebSocket(socketUrl);
  opened.onopen = () => resolve(opened);
  opened.onerror = () => reject(new Error("cannot connect to " + socketUrl));
});
const reply = await new Promise((resolve) => {
  socket.onmessage = (event) => { const message = JSON.parse(event.data); if (message.id === 1) resolve(message); };
  socket.send(JSON.stringify({ id: 1, method: "Extensions.loadUnpacked", params: { path: extensionPath } }));
});
console.log("Extensions.loadUnpacked", JSON.stringify(reply));
const loadedId = reply.result?.id;
if (!loadedId) { console.error("loadUnpacked failed (is --enable-unsafe-extension-debugging set?)"); process.exit(1); }
if (expectedId && loadedId !== expectedId) { console.error(`extension id ${loadedId} != expected ${expectedId}`); process.exit(1); }
process.exit(0);
