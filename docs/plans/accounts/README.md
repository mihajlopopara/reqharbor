# Accounts, login, sync and team projects — plan

Output of the PO → designer → dev → QA investigation (2 review rounds, 2026-11-27).

| File | What it is |
|---|---|
| [00-current-sync-map.md](00-current-sync-map.md) | How sync, shares and anonymous workspaces work today |
| [01-auth-research.md](01-auth-research.md) | Auth options for Tauri + Cloudflare Workers + D1, with sources and recommendation |
| [02-product-spec.md](02-product-spec.md) | Product spec: stories, acceptance criteria, roles, open decisions (§16) |
| [03-ux-design.md](03-ux-design.md) | Screens, flows, copy, states |
| [04-tech-design.md](04-tech-design.md) | D1 migrations, Worker API, client sync changes, phased plan (§12) |
| [05-qa-review.md](05-qa-review.md) | Final QA review: open issues, test plan, risks |

## Decisions (recommended defaults, accepted unless noted)

- **Auth**: first-party auth in the existing Worker — GitHub + Google OAuth with PKCE and a `127.0.0.1` loopback redirect from the desktop app; email codes later (M4). No third-party auth vendor.
- **Sessions**: 1 h access token + rotating refresh token with reuse detection; tokens in the OS keychain (0600 file on ad-hoc-signed macOS builds).
- **Unit of sharing**: a **project** (with its collections and environments) is personal, team or local-only.
- **Roles**: owner, admin, editor, viewer. Invite links. Viewers can edit and send but not save.
- **Accounts stay optional**: the app works fully offline; anonymous `rh_` token sync keeps working (under Advanced).
- **Data**: Cloudflare at-rest encryption only (no E2E in v1); secret variable values never sync.

## Release order

M0 client forward-compat → M1 Worker auth + personal spaces → M1 client sign-in + personal sync → M2 teams (Worker, then client) → M3 polish/QA → M4 email codes, account linking, paste-code.
