// Крошечный локальный сервер статуса: расширение шлёт POST со статусом, скрипт запуска читает файлы.
// node status-server.mjs <workdir>  -> пишет порт в <workdir>/status.port
import http from "node:http";
import { appendFileSync, writeFileSync } from "node:fs";
import path from "node:path";
const dir = process.argv[2];
const server = http.createServer((req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Headers", "*");
  if (req.method !== "POST") { res.writeHead(204); res.end(); return; }
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    res.writeHead(204); res.end();
    let parsed;
    try { parsed = JSON.parse(body); } catch { return; }
    if (parsed.logLine) appendFileSync(path.join(dir, "extension-console.log"), parsed.logLine + "\n");
    if (parsed.stats) writeFileSync(path.join(dir, "last-stats.json"), JSON.stringify(parsed.stats));
    if (parsed.final) writeFileSync(path.join(dir, "final-stats.json"), JSON.stringify(parsed.stats));
  });
});
server.listen(0, "127.0.0.1", () => writeFileSync(path.join(dir, "status.port"), String(server.address().port)));
