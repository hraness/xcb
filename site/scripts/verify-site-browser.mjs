import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { localVerificationOrigin, browserOwner, ownedChromiumLaunchOptions, pinnedBrowserExecutable, pinnedChromiumDefinition, verifyOwnedChromium } from "./owned-browser.mjs";
import { chromium } from "playwright-core";

const root = fileURLToPath(new URL("../", import.meta.url));
const production = process.argv.includes("--production");
assert.ok(process.argv.slice(2).every(argument => argument === "--production" || argument.startsWith("--local-origin=")), "Unknown argument");
const localOriginArguments = process.argv.slice(2).filter(argument => argument.startsWith("--local-origin="));
assert.ok(localOriginArguments.length <= 1, "Use at most one local verification origin.");
const localOrigin = localVerificationOrigin(localOriginArguments[0]?.slice("--local-origin=".length), production);
const routes = ["/", "/docs", "/install", "/download", "/compare", "/reflexes", "/blog"];
const artifacts = resolve(process.env.SITE_BROWSER_ARTIFACTS ?? "/tmp/xcb-site-browser");
await mkdir(artifacts, { recursive: true });
let origin = localOrigin ?? "https://xcb.sh";
let server;
let exited;
let output = "";
let browser;
const results = [];
let launchOptions;
let browserIdentity;
let interruption;
const owner = browserOwner({
  launch: () => chromium.launch(launchOptions),
  close: acquired => acquired.close(),
  stopServer: async () => {
    if (!server) return;
    if (server.exitCode === null && server.signalCode === null) server.kill("SIGTERM");
    const timer = setTimeout(() => { if (server.exitCode === null && server.signalCode === null) server.kill("SIGKILL"); }, 5_000);
    try { await exited; } finally { clearTimeout(timer); }
  },
});
const interrupted = signal => {
  interruption ??= new Error(`Browser verification interrupted by ${signal}.`);
  process.exitCode = signal === "SIGINT" ? 130 : signal === "SIGHUP" ? 129 : 143;
  void owner.stop().catch(error => { console.error(error); process.exitCode = 1; });
};
const onSIGINT = () => interrupted("SIGINT");
const onSIGTERM = () => interrupted("SIGTERM");
const onSIGHUP = () => interrupted("SIGHUP");
process.once("SIGINT", onSIGINT);
process.once("SIGTERM", onSIGTERM);
process.once("SIGHUP", onSIGHUP);
try {
  const definition = pinnedChromiumDefinition();
  const executablePath = await pinnedBrowserExecutable(chromium.executablePath(), process.env.XCB_BROWSER_EXECUTABLE);
  launchOptions = { ...ownedChromiumLaunchOptions(executablePath, definition.defaultArgs), timeout: 15_000,
    handleSIGHUP: false, handleSIGINT: false, handleSIGTERM: false };
  if (interruption) throw interruption;
  if (!production && localOrigin === undefined) {
    const reservation = createServer();
    reservation.listen(0, "127.0.0.1");
    await once(reservation, "listening");
    const port = reservation.address().port;
    await new Promise((resolveClose, reject) => reservation.close(error => error ? reject(error) : resolveClose()));
    if (interruption) throw interruption;
    origin = `http://127.0.0.1:${port}`;
    server = spawn(process.execPath, [resolve(root, "node_modules/next/dist/bin/next"), "start", "--hostname", "127.0.0.1", "--port", String(port)], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
    exited = new Promise((resolveExit, reject) => { server.once("exit", resolveExit); server.once("error", reject); });
    void exited.catch(() => undefined);
    // Keep diagnostics bounded while draining both pipes.
    for (const stream of [server.stdout, server.stderr]) stream.on("data", chunk => { output = (output + chunk).slice(-32_768); });
    const deadline = Date.now() + 45_000;
    let ready = false;
    while (Date.now() < deadline) {
      if (interruption) throw interruption;
      if (server.exitCode !== null || server.signalCode !== null) throw new Error(`Next exited: ${output}`);
      try { if ((await fetch(origin, { signal: AbortSignal.timeout(2_000) })).ok) { ready = true; break; } } catch { /* Wait for our server to bind. */ }
      await new Promise(resolveWait => setTimeout(resolveWait, 100));
    }
    assert.ok(ready, `Next did not become ready: ${output}`);
  }
  browser = await owner.start();
  browserIdentity = await verifyOwnedChromium(browser, executablePath, definition.expectedVersion);
  console.log(`Verification browser: ${browserIdentity.browserVersion}; executable: ${browserIdentity.executable}; source: pinned Playwright`);
  for (const width of [360, 390, 1440]) for (const theme of ["light", "dark"]) {
    const context = await browser.newContext({ viewport: { width, height: width === 360 ? 740 : width === 390 ? 844 : 900 }, colorScheme: theme, isMobile: width <= 600, hasTouch: width <= 600 });
    try {
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", error => errors.push(error.message));
      page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
      for (const route of routes) {
        const response = await page.goto(origin + route);
        assert.equal(response?.status(), 200, route);
        assert.equal(await page.evaluate(() => matchMedia("(pointer: coarse)").matches), width <= 600, `${route}: pointer matches viewport fixture`);
        await page.locator("main").waitFor();
        await page.evaluate(() => document.fonts.ready);
        const state = await page.evaluate(() => {
          const footer = document.querySelector("#hraness-site-footer");
          return {
            overflow: Math.max(document.documentElement.scrollWidth, document.body.scrollWidth) > innerWidth,
            heading: document.querySelector("h1")?.textContent?.trim(),
            theme: document.documentElement.dataset.theme,
            footerPositions: [footer, footer?.querySelector(".hraness-site-footer__inner")].map(element => element ? getComputedStyle(element).position : null),
            smallHeaderTargets: innerWidth > 600 ? [] : [...document.querySelectorAll("header a, header button, header summary")].filter(element => { const box = element.getBoundingClientRect(); return box.width > 0 && box.height > 0 && (box.width < 43.5 || box.height < 43.5); }).map(element => ({ label: element.textContent?.trim() || element.getAttribute("aria-label"), width: element.getBoundingClientRect().width, height: element.getBoundingClientRect().height })),
          };
        });
        const name = `${width}-${theme}-${route === "/" ? "home" : route.slice(1).replaceAll("/", "_")}`;
        const screenshot = await page.screenshot({ path: resolve(artifacts, `${name}.png`), fullPage: true, animations: "disabled" });
        state.screenshotWidth = screenshot.readUInt32BE(16);
        await writeFile(resolve(artifacts, `${name}.json`), JSON.stringify({ route, state, errors }, null, 2));
        assert.ok(!state.overflow, `${route}: horizontal overflow at ${width}`);
        assert.equal(state.screenshotWidth, width, `${route}: full-page capture exceeds viewport`);
        assert.ok(state.heading, `${route}: missing heading`);
        assert.equal(state.theme, theme, `${route}: system appearance`);
        assert.ok(state.footerPositions.every(position => position === "static" || position === "relative"), `${route}: footer not in normal flow`);
        assert.deepEqual(state.smallHeaderTargets, [], `${route}: phone targets below 44px`);
        assert.deepEqual(errors, [], `${route}: browser errors`);
        results.push({ route, width, theme });
      }
      await page.goto(origin);
      const themeMenu = page.locator(".hraness-design-palette-menu");
      await page.waitForFunction(() => document.querySelector(".hraness-design-palette-menu")?.dataset.ready === "true");
      await themeMenu.locator(":scope > summary").click();
      const targetTheme = theme === "light" ? "dark" : "light";
      await page.getByRole("radio", { name: new RegExp(`^${targetTheme}$`, "iu") }).check();
      await page.waitForFunction(expected => document.documentElement.dataset.theme === expected, targetTheme);
      await page.reload();
      await page.waitForFunction(expected => document.documentElement.dataset.theme === expected, targetTheme);
      await page.locator("header").getByRole("link", { name: "Docs", exact: true }).click();
      await page.waitForURL(url => url.pathname === "/docs");
      assert.deepEqual(errors, [], "Browser errors after appearance and navigation");
    } finally { await context.close(); }
  }
} finally {
  try { await owner.stop(); }
  finally {
    process.removeListener("SIGINT", onSIGINT);
   process.removeListener("SIGTERM", onSIGTERM);
    process.removeListener("SIGHUP", onSIGHUP);
  }
}
if (interruption) throw interruption;
await writeFile(resolve(artifacts, "results.json"), JSON.stringify({ origin, production, browserIdentity, source: process.env.GITHUB_SHA ?? null, capturedAt: new Date().toISOString(), cleanup: "browser and owned server closed", results }, null, 2) + "\n");
console.log(`Verified ${results.length} route/viewport/theme combinations at ${origin}.`);
