// Opens the app in Chromium against the preview backend and walks through every view.
// Fails on any console error or broken interaction. Run from the repo root:
//   cargo run --release -p culpritcore --example export_demo -- ui-dev/sessions
//   npx http-server -p 8765 -s . &
//   node ui-dev/check.js [screenshot folder]
const { chromium } = require("playwright");

const URL = "http://127.0.0.1:8765/ui/index.html";
const shots = process.argv[2];
const fail = (msg) => {
  throw new Error(msg);
};

(async () => {
  const browser = await chromium.launch();
  const errors = [];
  const watch = (page, name) => {
    page.on("pageerror", (e) => errors.push(`${name}: ${e.message}`));
    page.on("console", (m) => m.type() === "error" && errors.push(`${name}: ${m.text()}`));
  };
  const shot = async (page, name) => shots && page.screenshot({ path: `${shots}/${name}.png` });

  // Report view, newest session first.
  const page = await browser.newPage({ viewport: { width: 1120, height: 740 } });
  watch(page, "report");
  await page.goto(URL);
  await page.waitForSelector(".headline");
  if ((await page.locator(".item").count()) !== 5) fail("expected 5 sessions in the list");
  if (await page.locator("#btn-record").isVisible()) fail("record button shows in a build that can't record");
  if (!(await page.locator(".headline").innerText()).includes("Microsoft Defender")) fail("wrong first report");
  await shot(page, "report");

  // Hovering the timeline shows a tooltip; clicking a stutter tick opens that moment.
  const box = await page.locator(".tl-body .over").boundingBox();
  await page.mouse.move(box.x + box.width * 0.5, box.y + 50);
  await page.waitForSelector(".tl-tip.show");
  const ticks = await page.evaluate(() => {
    const inc = [...document.querySelectorAll(".inc")].length;
    return inc;
  });
  if (ticks < 10) fail("moments list is missing rows");
  await page.locator(".inc-row").nth(3).click();
  if (!(await page.locator(".inc.open .inc-body").isVisible())) fail("moment didn't open");

  // Keyboard moves through sessions.
  for (const expect of ["Wi-Fi", "game server", "No stutters", "GPU throttled"]) {
    await page.keyboard.press("ArrowDown");
    await page.waitForFunction((t) => document.querySelector(".headline")?.textContent.includes(t), expect);
  }
  await shot(page, "report-last");

  // Filter chips on the Wi-Fi session.
  await page.locator(".item").nth(1).click();
  await page.waitForFunction(() => document.querySelector(".headline")?.textContent.includes("Wi-Fi"));
  await page.locator('[data-filter="lag"]').click();
  const kinds = await page.locator(".inc .kind").evaluateAll((els) => els.map((e) => e.className));
  if (!kinds.length || kinds.some((k) => !k.includes("lag"))) fail("lag filter shows other moments");

  // Delete asks first, then removes the session.
  await page.locator(".item").nth(3).click({ button: "right" });
  await page.waitForSelector(".dialog h3");
  await shot(page, "delete");
  await page.keyboard.press("Escape");
  if (await page.locator(".dialog h3").count()) fail("Escape didn't close the dialog");
  await page.locator(".item").nth(3).click({ button: "right" });
  await page.locator('[data-act="delete"]').click();
  await page.waitForFunction(() => document.querySelectorAll(".item").length === 4);

  // The example report is labelled as one.
  await page.locator("#btn-example").click();
  await page.waitForSelector(".tag");

  // Live view, with the preview backend pretending it can record.
  const live = await browser.newPage({ viewport: { width: 1120, height: 740 } });
  watch(live, "live");
  await live.goto(URL + "?record");
  await live.locator("#btn-record").click();
  await live.waitForFunction(() => document.getElementById("lv-game")?.textContent === "Counter-Strike 2", null, { timeout: 8000 });
  await live.waitForTimeout(1200);
  if ((await live.locator(".ping-row").count()) < 3) fail("live view is missing ping rows");
  await shot(live, "live");
  await live.locator('[data-act="stop"]').click();
  await live.waitForSelector(".headline");

  // Empty state.
  const empty = await browser.newPage({ viewport: { width: 1120, height: 740 } });
  watch(empty, "empty");
  await empty.route("**/ui-dev/sessions/index.json", (r) => r.fulfill({ body: "[]", contentType: "application/json" }));
  await empty.goto(URL);
  await empty.waitForSelector(".empty h1");
  await shot(empty, "empty");

  // Smallest window size.
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForTimeout(150);
  const overflow = await page.evaluate(() => document.getElementById("main").scrollWidth > document.getElementById("main").clientWidth);
  if (overflow) fail("report scrolls sideways at the minimum window size");
  await shot(page, "narrow");

  await browser.close();
  if (errors.length) fail("console errors:\n" + errors.join("\n"));
  console.log("ui check passed");
})().catch((e) => {
  console.error(e.message || e);
  process.exit(1);
});
