import { chromium } from "@playwright/test";
const b = await chromium.launch();
for (const [w,h,z] of [[800,600,1],[1280,800,1.5]]) {
  const p = await (await b.newContext({ viewport: { width: Math.round(w/z), height: Math.round(h/z) }, colorScheme: "dark" })).newPage();
  await p.goto("http://127.0.0.1:5173/");
  await p.getByTestId("app").waitFor();
  const send = await p.getByTestId("send-button").boundingBox();
  console.log(`viewport ${Math.round(w/z)}x${Math.round(h/z)} send.x+w=`, send.x + send.width);
  await p.addStyleTag({ content: "[data-testid=app]{min-width:0 !important}" });
  await p.waitForTimeout(300);
  await p.getByTestId("url-input").fill("https://api.example.com/v1/users");
  await p.getByTestId("send-button").click();
  await p.getByTestId("response-status").waitFor();
  await p.waitForTimeout(300);
  await p.screenshot({ path: `/app/docs/qa/design/screens/23-${Math.round(w/z)}-minwidth-removed-dark.png` });
}
await b.close();
