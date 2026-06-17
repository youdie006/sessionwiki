// Record a real-interaction demo of `sessionwiki web` to a webm, driving the
// live UI with Playwright (typing, clicking, results populating). Logs a
// timeline of scene keyframes - each with the on-screen rect of the element the
// scene is about, measured from the live DOM - so scripts/zoom_web_demo.py can
// apply an ad-style focus zoom that follows the action when encoding to mp4.
//
//   node scripts/record_web_demo.cjs <playwright-module-dir> <url> <out-dir>
//
// Prints the webm path on the last line; writes <out-dir>/timeline.json.
// The tour stays in English (the UI's Korean switch is intentionally omitted).

const PW = process.argv[2];
const URL = process.argv[3] || "http://127.0.0.1:8810";
const OUT = process.argv[4] || "/tmp/sw-rec";
const { chromium } = require(PW + "/playwright");
const fs = require("fs");

const W = 1280, H = 800;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: W, height: H },
    deviceScaleFactor: 1,
    recordVideo: { dir: OUT, size: { width: W, height: H } },
  });
  const t0 = Date.now(); // ~ when recordVideo starts
  const page = await context.newPage();
  const keyframes = [];
  // A scene keyframe: the time it settled + the rect to focus (null = whole
  // frame) + a caption. The zoom eases to this focus and holds until the next.
  const kf = async (selector, caption, opts = {}) => {
    let focus = null;
    if (selector) {
      focus = await page.evaluate(({ sel, union }) => {
        const r = (e) => { const b = e.getBoundingClientRect(); return [b.left, b.top, b.width, b.height]; };
        const el = document.querySelector(sel);
        if (!el) return null;
        if (!union) return r(el);
        const u = document.querySelector(union);
        if (!u) return r(el);
        const a = el.getBoundingClientRect(), b = u.getBoundingClientRect();
        const x0 = Math.min(a.left, b.left), y0 = Math.min(a.top, b.top);
        const x1 = Math.max(a.right, b.right), y1 = Math.max(a.bottom, b.bottom);
        return [x0, y0, x1 - x0, y1 - y0];
      }, { sel: selector, union: opts.union || null });
    }
    keyframes.push({ at: Date.now() - t0, focus, caption });
  };

  await page.goto(URL, { waitUntil: "networkidle" });
  await sleep(1200);
  await kf(null, "One wiki for every AI coding session");

  // 1. Search - type for real; results stream in.
  await page.click("#q");
  await sleep(300);
  await page.type("#q", "token", { delay: 120 });
  await sleep(1500);
  await kf("#q", "Search across every tool - partial words and CJK", { union: "#list" });

  // 2. Open a session - tags, a note, the resume command.
  await page.click("#list > *:first-child");
  await page.waitForSelector("#main .doc-head", { timeout: 4000 });
  await sleep(1500);
  await kf("#main .doc-head", "Tags, a note, and a one-command resume");

  // 3. Provenance: click a touched-file chip -> sessions that touched it.
  try {
    const chip = page.locator("#main .doc-head .filechip").first();
    if (await chip.count()) {
      await chip.click();
      await page.locator("#main .seealso").scrollIntoViewIfNeeded({ timeout: 2000 });
      await sleep(1700);
      await kf("#main .seealso", "Trace a file back to the sessions that wrote it");
      await page.evaluate(() => { document.querySelector("#main").scrollTop = 0; });
      await sleep(500);
    }
  } catch {}

  // 4. Tag filter: click a tag chip in the header -> the sidebar narrows.
  try {
    const tag = page.locator("#main .doc-head .tagchip", { hasText: "perf" }).first();
    if (await tag.count()) {
      await tag.click();
      await sleep(1600);
      await kf("#list", "Filter by tag", { union: "#q" });
    }
  } catch {}

  // 5. Dark theme - whole frame, stays in English.
  try {
    await page.click("#theme");
    await sleep(1600);
    await kf(null, "Light and dark, 100% local");
  } catch {}

  await sleep(700);
  await context.close(); // flush the webm
  await browser.close();

  const webm = fs.readdirSync(OUT).filter((f) => f.endsWith(".webm")).map((f) => OUT + "/" + f).sort().pop();
  fs.writeFileSync(OUT + "/timeline.json", JSON.stringify({ size: [W, H], total: Date.now() - t0, keyframes }, null, 2));
  console.log(webm || "NO_WEBM");
})().catch((e) => {
  console.error("ERR", e.message);
  process.exit(1);
});
