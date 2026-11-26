import { expect, test } from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/**
 * QA round 2 — run the shared contract probe (qa/r2/contract_probe.js) against the browser mock
 * and save the results; qa/r2/shape_diff.mjs compares them with the same probe run through the
 * real Tauri IPC (qa/r2/run_real_app.sh → qa/r2/out/contract_real.json).
 */
const repo = fileURLToPath(new URL("../../", import.meta.url));

test("QA-R2 contract probe against the browser mock", async ({ page }) => {
  await page.goto("/?seed=empty");
  await expect(page.getByTestId("app")).toBeVisible();
  const probe = readFileSync(`${repo}qa/r2/contract_probe.js`, "utf8").replace(/^\/\/.*$/gm, "");
  const spec = readFileSync(`${repo}qa/fixtures/openapi/webhooks_v31.yaml`, "utf8");
  const result = await page.evaluate(
    async ({ probe, spec }) => {
      const api = await import("/src/lib/api/index.ts" as string);
      const b = await api.getBackend();
      // eslint-disable-next-line no-eval
      const contractProbe = (0, eval)(`(${probe.trim().replace(/^async function contractProbe/, "async function")})`);
      return contractProbe((c: string, a: Record<string, unknown>) => b.invoke(c, a), {
        echo: "http://127.0.0.1:8099",
        worker: "http://127.0.0.1:8787",
        tmp: "/tmp/qa-body.bin",
        spec,
      });
    },
    { probe, spec },
  );
  mkdirSync(`${repo}qa/r2/out`, { recursive: true });
  writeFileSync(`${repo}qa/r2/out/contract_mock.json`, JSON.stringify(result, null, 2));
  expect(Object.keys(result).length).toBeGreaterThan(80);
});
