/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

// Tauri expects a fixed port (src-tauri/tauri.conf.json devUrl).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // tokens.css is imported as-is from ../docs/design
    fs: { allow: [repoRoot] },
  },
  preview: { port: 4173, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: ["es2022", "safari16", "chrome110"],
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    chunkSizeWarningLimit: 400,
  },
  // Unit tests run in node but must use Svelte's client runtime for runes stores.
  // One graphql-js instance in tests, as in the app bundle: the GraphQL packages' ESM builds
  // (Node would load graphql-language-service's CommonJS build with a second graphql-js).
  ...(process.env.VITEST
    ? {
        resolve: {
          conditions: ["browser"],
          alias: [
            { find: /^graphql$/, replacement: fileURLToPath(new URL("./node_modules/graphql/index.mjs", import.meta.url)) },
            { find: /^graphql-language-service$/, replacement: fileURLToPath(new URL("./node_modules/graphql-language-service/esm/index.js", import.meta.url)) },
          ],
        },
      }
    : {}),
  test: {
    include: ["tests/unit/**/*.test.ts"],
    environment: "node",
    server: { deps: { inline: ["graphql-language-service", "cm6-graphql"] } },
  },
});
