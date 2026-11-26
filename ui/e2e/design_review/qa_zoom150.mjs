// QA round 3 — DR-001: 1280×800 window at 150 % zoom (= 853×533 CSS px, DPR 1.5), dark + light, with a response.
import { chromium } from "@playwright/test";
const b = await chromium.launch();
for (const theme of ["dark", "light"]) {
  const ctx = await b.newContext({ viewport: { width: 853, height: 533 }, deviceScaleFactor: 1.5, colorScheme: theme });
  const p = await ctx.newPage();
  await p.goto("http://127.0.0.1:5173/");
  await p.getByTestId("app").waitFor();
  await p.getByTestId("url-input").fill("https://api.example.com/v1/users");
  await p.getByTestId("send-button").click();
  await p.getByTestId("response-status").waitFor();
  await p.waitForTimeout(300);
  await p.screenshot({ path: `/app/docs/qa/design/screens_r3/30-zoom150-${theme}.png` });
  const r = await p.evaluate(() => { const s = document.querySelector('[data-testid="send-button"]').getBoundingClientRect(); return { sendRight: Math.round(s.right), vw: innerWidth, scrollW: document.documentElement.scrollWidth }; });
  console.log(theme, JSON.stringify(r));
  await ctx.close();
}
await b.close();
