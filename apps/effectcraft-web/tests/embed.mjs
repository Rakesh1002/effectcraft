// Browser test of `?embed` over the Chrome DevTools Protocol (no npm dependencies, Node >= 22):
//
//   cargo xtask web --serve 8765 &
//   node apps/effectcraft-web/tests/embed.mjs --url http://127.0.0.1:8765/
//
// Memory storage and an empty start; putFile stores without importing; exportProject returns
// the File > Save text and the revision; markSaved only marks the revision it was given;
// `effectcraft:revision`, `effectcraft:save-request`, `effectcraft:output` and
// `effectcraft:import-request` fire, and nothing downloads. With the tab hidden (another tab in
// front: no animation frames) `execute` and `renderFrame` still resolve. Prints a JSON report; exits non-zero
// on failure.
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const arg = (k, d) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : d;
};
const base = arg("url", "http://127.0.0.1:8765/");
const chrome = arg("chrome", process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");
const port = Number(arg("port", "9335"));
const out = mkdtempSync(join(tmpdir(), "ec-embed-"));
const downloads = join(out, "downloads");
mkdirSync(downloads, { recursive: true });
const proc = spawn(chrome, [
  "--headless=new",
  `--remote-debugging-port=${port}`,
  `--user-data-dir=${join(out, "profile")}`,
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-component-update",
  "--enable-unsafe-webgpu",
  "--window-size=1400,900",
  "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let ws;
let pageId;
let nextId = 1;
const waiting = new Map();
const chooser = [];
const started = [];

async function connect() {
  for (let i = 0; i < 100; i++) {
    try {
      const page = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page");
      if (page) {
        pageId = page.id;
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
        ws.onmessage = (m) => {
          const msg = JSON.parse(m.data);
          if (msg.id && waiting.has(msg.id)) {
            waiting.get(msg.id)(msg);
            waiting.delete(msg.id);
          } else if (msg.method === "Browser.downloadWillBegin") {
            started.push(msg.params.url);
          } else if (msg.method === "Page.fileChooserOpened") {
            chooser.push(msg.params);
          }
        };
        return;
      }
    } catch {}
    await sleep(100);
  }
  throw new Error("Chrome did not start");
}

function send(method, params = {}) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => waiting.set(id, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result))));
}

async function js(expr) {
  const wrapped = `(async () => { try { return await (${expr}); } catch (e) { throw new Error(String(e && e.message || e)); } })()`;
  const r = await send("Runtime.evaluate", { expression: wrapped, awaitPromise: true, returnByValue: true, timeout: 300000 });
  if (r.exceptionDetails) throw new Error(`${expr}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
  return r.result.value;
}

async function until(expr, ms = 60000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await js(expr)) return;
    await sleep(100);
  }
  throw new Error(`timeout waiting for ${expr}`);
}

const check = (cond, msg) => {
  if (!cond) throw new Error(`check failed: ${msg}`);
};
const events = (name) => js(`window.__ec.filter((e) => e.name === ${JSON.stringify(name)})`);

const report = { url: base, steps: {} };
let failed = false;
try {
  await connect();
  await send("Page.enable");
  await send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: downloads, eventsEnabled: true });
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: `window.__ec = [];
      for (const name of ["effectcraft:revision", "effectcraft:save-request", "effectcraft:output", "effectcraft:import-request"]) {
        window.addEventListener(name, (e) => {
          const d = e.detail || {};
          window.__ec.push({ name, detail: name === "effectcraft:import-request"
            ? { kind: d.kind, files: (d.files || []).map((f) => ({ name: f.name, size: f.size, isFile: f instanceof File })) }
            : d });
        });
      }`,
  });
  await send("Page.navigate", { url: new URL("?embed", base).href });
  await until("!!(window.effectcraftLoad && (window.effectcraftLoad.readyMs || window.effectcraftLoad.error))", 180000);
  const load = await js("window.effectcraftLoad");
  if (load.error) throw new Error(load.error);

  const info = await js("effectcraft.info()");
  report.steps.boot = { readyMs: Math.round(load.readyMs), storage: info.storage, restored: info.restored, serviceWorker: info.serviceWorker };
  check(info.storage.backend === "memory", `storage backend ${info.storage.backend}`);
  check(info.restored === false, "no session restore");
  check(!info.serviceWorker, "no service worker");
  const api = await js(`["putFile", "exportProject", "markSaved", "revision"].map((k) => typeof effectcraft[k])`);
  check(api.every((t) => t === "function"), `api ${api}`);
  report.steps.api = { api, itemsAtStart: (await js(`effectcraft.execute("project.summary", {})`)).items.length };

  await until(`window.__ec.some((e) => e.name === "effectcraft:revision")`);
  const r0 = await js("effectcraft.revision()");
  await js(`effectcraft.execute("comp.new", {name: "E", width: 64, height: 36, duration: 0.2})`);
  await until(`window.__ec.some((e) => e.name === "effectcraft:revision" && e.detail.revision > ${r0} && e.detail.dirty)`);
  report.steps.revision = { before: r0, after: await js("effectcraft.revision()") };

  // A 2x2 red PNG.
  const png = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGP4z8DwHwyBNAMDAE7KB/m0B6w8AAAAAElFTkSuQmCC";
  const itemsBeforePut = (await js(`effectcraft.execute("project.summary", {})`)).items.length;
  const stored = await js(`effectcraft.putFile("/assets/a1/red.png", Uint8Array.from(atob("${png}"), (c) => c.charCodeAt(0)))`);
  const listed = await js(`effectcraft.files().some((f) => f.path === "/assets/a1/red.png")`);
  await sleep(500);
  const itemsAfterPut = (await js(`effectcraft.execute("project.summary", {})`)).items.length;
  check(stored === "/assets/a1/red.png" && listed, "putFile stores the file");
  check(itemsAfterPut === itemsBeforePut, "putFile imports nothing");
  await js(`effectcraft.execute("file.import", {paths: ["/assets/a1/red.png"]})`);
  const exported = await js("effectcraft.exportProject()");
  const doc = JSON.parse(exported.text);
  const footage = JSON.stringify(doc).includes("/assets/a1/red.png");
  check(typeof exported.revision === "number" && footage, "exportProject has the footage");
  writeFileSync(join(out, "export.ecproj"), exported.text);
  report.steps.putFile = { stored, footageInExport: footage, export: join(out, "export.ecproj"), revision: exported.revision };

  const stale = await js(`effectcraft.markSaved(${exported.revision - 1})`);
  const marked = await js(`effectcraft.markSaved(${exported.revision})`);
  check(!stale && marked, `markSaved stale=${stale} current=${marked}`);
  await until(`window.__ec.some((e) => e.name === "effectcraft:revision" && e.detail.revision === ${exported.revision} && !e.detail.dirty)`);
  report.steps.markSaved = { stale, marked };

  await js(`effectcraft.request("ui.menu.invoke", {command: "file.save"})`);
  await until(`window.__ec.some((e) => e.name === "effectcraft:save-request")`);
  report.steps.saveRequest = (await events("effectcraft:save-request"))[0].detail;

  await js(`effectcraft.request("ui.menu.invoke", {command: "comp.saveFrameAs"})`);
  await until(`window.__ec.some((e) => e.name === "effectcraft:output")`);
  const frameOut = (await events("effectcraft:output"))[0].detail;
  report.steps.saveFrame = frameOut;

  await js(`effectcraft.execute("renderQueue.add", {output: "/embed-render.gif"})`);
  await js(`effectcraft.request("ui.menu.invoke", {command: "renderQueue.render"})`);
  await until(`window.__ec.some((e) => e.name === "effectcraft:output" && e.detail.paths.some((p) => p.endsWith(".gif")))`, 180000);
  const gif = (await events("effectcraft:output")).find((e) => e.detail.paths.some((p) => p.endsWith(".gif"))).detail.paths[0];
  const gifBytes = await js(`effectcraft.readFile(${JSON.stringify(gif)})?.length ?? 0`);
  check(gifBytes > 0, "render output stays in the file table");
  report.steps.render = { path: gif, bytes: gifBytes };

  await send("Page.setInterceptFileChooserDialog", { enabled: true });
  await js(`effectcraft.request("ui.menu.invoke", {command: "file.import"})`);
  await sleep(1000);
  if (chooser.length) {
    const file = join(out, "pick.png");
    writeFileSync(file, Buffer.from(png, "base64"));
    await send("DOM.setFileInputFiles", { files: [file], backendNodeId: chooser[0].backendNodeId });
    await until(`window.__ec.some((e) => e.name === "effectcraft:import-request")`);
    report.steps.importRequest = (await events("effectcraft:import-request"))[0].detail;
    check(report.steps.importRequest.files[0]?.isFile, "import-request carries File objects");
  } else {
    report.steps.importRequest = "skipped: the headless file chooser did not open without a user gesture";
  }

  const other = await send("Target.createTarget", { url: "about:blank" });
  await send("Target.activateTarget", { targetId: other.targetId });
  await until("document.hidden", 10000).catch(() => {});
  const hidden = await js("document.hidden");
  if (hidden) {
    const timed = async (expr) => {
      const t0 = Date.now();
      const v = await js(`Promise.race([${expr}, new Promise((_, rej) => setTimeout(() => rej(new Error("no reply in 10 s")), 10000))])`);
      return [Date.now() - t0, v];
    };
    const rafWhileHidden = await js(`new Promise((res) => { let n = 0; const f = () => { n++; requestAnimationFrame(f); }; requestAnimationFrame(f); setTimeout(() => res(n), 1000); })`);
    const [executeMs] = await timed(`effectcraft.execute("comp.new", {name: "Hidden", width: 32, height: 32, duration: 0.1})`);
    const [renderFrameMs, frame] = await timed(`effectcraft.renderFrame({max_side: 64}).then((r) => JSON.stringify(r).length)`);
    const [revisionMs, rev] = await timed(`new Promise((res) => { const r = effectcraft.revision(); const t = setInterval(() => { const e = window.__ec.filter((x) => x.name === "effectcraft:revision").pop(); if (e && e.detail.revision === r) { clearInterval(t); res(r); } }, 20); })`);
    report.steps.hiddenTab = { hidden, rafWhileHidden, executeMs, renderFrameMs, frameJsonBytes: frame, revisionEventMs: revisionMs, revision: rev };
    check(frame > 0, "renderFrame in a hidden tab");
  } else {
    report.steps.hiddenTab = "skipped: this Chrome did not hide the page behind another tab";
  }
  await send("Target.activateTarget", { targetId: pageId });
  await until("!document.hidden", 10000);
  const t0 = Date.now();
  await js(`effectcraft.execute("comp.new", {name: "Visible", width: 32, height: 32, duration: 0.1})`);
  report.steps.visibleAgain = { executeMs: Date.now() - t0 };

  await sleep(1000);
  const origin = new URL(base).origin;
  const ours = started.filter((u) => u.startsWith("blob:") || u.startsWith(origin));
  check(ours.length === 0, `downloads: ${ours}`);
  report.steps.downloads = { fromThePage: ours.length, files: readdirSync(downloads).length };
} catch (e) {
  failed = true;
  report.error = String(e && e.stack || e);
}
console.log(JSON.stringify(report, null, 2));
proc.kill();
process.exit(failed ? 1 : 0);
