# ReqHarbor

Postman's job, a tenth of the weight. ReqHarbor is a fast desktop HTTP client: a three-pane request/response layout, cURL import and export, one-click OpenAPI import, projects, collections and environments, and the HTTP QUERY method, with optional sync and read-only share links.

## Download

Grab the latest build from **[Releases](https://github.com/mihajlopopara/reqharbor/releases/latest)**.

| Platform | File |
|---|---|
| macOS, Apple Silicon (M1 and later) | `ReqHarbor_<version>_aarch64.dmg` |
| macOS, Intel | `ReqHarbor_<version>_x64.dmg` |
| Windows 10/11 | `ReqHarbor_<version>_x64-setup.exe` (or the `.msi`) |
| Linux | `ReqHarbor_<version>_amd64.AppImage`, `.deb` (Debian/Ubuntu) or `.rpm` (Fedora/openSUSE) |

Every release ships a `SHA256SUMS.txt` to verify downloads.

## Install

The builds are not notarized or code-signed yet, so the OS asks you to confirm the first launch.

**macOS:** open the `.dmg` and drag ReqHarbor to Applications. If macOS says the app "is damaged" or "cannot be opened", run this once and open it again:

```sh
xattr -dr com.apple.quarantine /Applications/ReqHarbor.app
```

You can also allow it under System Settings → Privacy & Security → "Open Anyway".

**Windows:** run the installer. If SmartScreen appears, choose "More info" → "Run anyway". WebView2 is installed automatically if missing.

**Linux:** for the AppImage, `chmod +x ReqHarbor_*.AppImage` and run it. For packages, `sudo apt install ./ReqHarbor_*_amd64.deb` or `sudo dnf install ./ReqHarbor-*.x86_64.rpm`.

## Development

Everything builds and tests inside the dev container:

```sh
docker compose run --rm dev cargo test --workspace
docker compose run --rm -w /app/ui dev npm test
docker compose run --rm -w /app/worker dev npm test
```

Specs live in `docs/` (`PRODUCT.md`, `DESIGN.md`, `API_CONTRACT.md`). The sync/share backend is the Cloudflare Worker in `worker/`.

## Releasing

1. Bump `version` in `src-tauri/tauri.conf.json`, `Cargo.toml` (workspace) and `ui/package.json`.
2. Commit, then tag and push: `git tag v0.2.0 && git push origin v0.2.0`.
3. The **Desktop build** workflow builds macOS (arm64 + x64), Windows and Linux, and publishes the GitHub Release with all installers and checksums. Tags with a suffix (e.g. `v0.2.0-beta.1`) are published as pre-releases.

## License

MIT
