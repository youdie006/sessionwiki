// Render the README banner + social-preview PNGs from docs/banner.html.
// Driven by env: PW_DIR (playwright module dir), CHROME (chromium binary),
// BASE (served banner.html URL), OUT (output dir). See scripts/make_banner.sh.
const PW = process.env.PW_DIR;
const { chromium } = require(PW + "/playwright");
const CHROME = process.env.CHROME;
const BASE = process.env.BASE;
const OUT = process.env.OUT;

const jobs = [
  { name: "banner.png",          theme: "light", lang: "en", size: "banner", w: 1280, h: 400, dsf: 2 },
  { name: "banner-dark.png",     theme: "dark",  lang: "en", size: "banner", w: 1280, h: 400, dsf: 2 },
  { name: "banner-ko.png",       theme: "light", lang: "ko", size: "banner", w: 1280, h: 400, dsf: 2 },
  { name: "banner-ko-dark.png",  theme: "dark",  lang: "ko", size: "banner", w: 1280, h: 400, dsf: 2 },
  { name: "social-preview.png",  theme: "light", lang: "en", size: "social", w: 1280, h: 640, dsf: 1 },
];

(async () => {
  const browser = await chromium.launch({ executablePath: CHROME });
  for (const j of jobs) {
    const page = await browser.newPage({ viewport: { width: j.w, height: j.h }, deviceScaleFactor: j.dsf });
    await page.goto(`${BASE}?theme=${j.theme}&lang=${j.lang}&size=${j.size}`, { waitUntil: "networkidle" });
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(250);
    await page.screenshot({ path: `${OUT}/${j.name}` });
    console.log("rendered", j.name);
    await page.close();
  }
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });
