// Record a real-interaction demo of `sessionwiki web` to a webm, by driving the
// live UI with Playwright (typing, clicking, results populating) and letting
// Playwright's recordVideo capture it. Run with the demo store served, then
// convert the webm to mp4/webp with ffmpeg (see scripts/make_web_demo.sh).
//
//   node scripts/record_web_demo.cjs <playwright-module-dir> <url> <out-dir>
//
// Why a standalone script (not the Playwright MCP): recordVideo can only be set
// at context creation, and the MCP hands you a pre-built context. So we make our
// own context here. Browsers resolve via PLAYWRIGHT_BROWSERS_PATH.

const PW = process.argv[2];
const URL = process.argv[3] || "http://127.0.0.1:8810";
const OUT = process.argv[4] || "/tmp/sw-rec";
const { chromium } = require(PW + "/playwright");

const W = 1280, H = 800;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: W, height: H },
    deviceScaleFactor: 1,
    recordVideo: { dir: OUT, size: { width: W, height: H } },
  });
  const page = await context.newPage();

  await page.goto(URL, { waitUntil: "networkidle" });
  await sleep(1300); // land on home

  // 1. Search - type for real so the box fills char by char and results stream in.
  await page.click("#q");
  await sleep(350);
  await page.type("#q", "token", { delay: 120 });
  await sleep(1500); // read the live results

  // 2. Open a session - its header carries tags, a note, and the resume command.
  await page.click("#list > *:first-child");
  await page.waitForSelector("#main .doc-head", { timeout: 4000 });
  await sleep(1700);

  // 3. See-also: scroll the related panel into view, then back up.
  try {
    await page.locator("#main .seealso").scrollIntoViewIfNeeded({ timeout: 2000 });
    await sleep(1500);
    await page.evaluate(() => { document.querySelector("#main").scrollTop = 0; });
    await sleep(700);
  } catch {}

  // 4. Provenance: click a touched-file chip -> sessions that touched that file.
  try {
    const chip = page.locator("#main .doc-head .filechip").first();
    if (await chip.count()) {
      await chip.click();
      await page.locator("#main .seealso").scrollIntoViewIfNeeded({ timeout: 2000 });
      await sleep(1800);
      await page.evaluate(() => { document.querySelector("#main").scrollTop = 0; });
      await sleep(500);
    }
  } catch {}

  // 5. Tag filter: click a tag chip in the header -> the sidebar narrows.
  try {
    const tag = page.locator("#main .doc-head .tagchip", { hasText: "perf" }).first();
    if (await tag.count()) {
      await tag.click();
      await sleep(1700);
    }
  } catch {}

  // 6. Dark theme.
  try {
    await page.click("#theme");
    await sleep(1500);
  } catch {}

  // 7. Language: open the picker and switch to Korean - the CJK moment.
  try {
    await page.click("#langbtn");
    await sleep(800);
    const ko = page.locator("#langmenu button", { hasText: "한국어" }).first();
    if (await ko.count()) {
      await ko.click();
      await sleep(1900);
    }
  } catch {}

  await sleep(700);
  await context.close(); // flush the webm
  await browser.close();

  const fs = require("fs");
  const file = fs.readdirSync(OUT).filter((f) => f.endsWith(".webm")).map((f) => OUT + "/" + f).sort().pop();
  console.log(file || "NO_WEBM");
})().catch((e) => {
  console.error("ERR", e.message);
  process.exit(1);
});
