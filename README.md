# AIPG Desktop

An all-in-one desktop app for the [AI Power Grid](https://aipowergrid.io): your
models, prices, worker status, earnings and **offline alerts** in one window.
Built with [Tauri](https://tauri.app) — a small, sandboxed native app (uses the
OS webview, no bundled browser).

> Unofficial, community-built. Not affiliated with AI Power Grid.

![icon](app-icon.png)

## Install

Grab the installer for your OS from the [Releases](https://github.com/speteai/aipg-desktop/releases):

- **Windows** — `.msi` or `.exe` (NSIS)
- **macOS** — `.dmg` (universal)
- **Linux** — `.AppImage` or `.deb`

The builds are **unsigned** (no paid code-signing cert), so on first launch your
OS may warn you ("unknown publisher" / Gatekeeper). Choose *run anyway* / right-click
→ *Open*. The source and the build pipeline are public here, so you can verify or
build it yourself.

Then open **Settings**, paste your `grid_…` API key, and you're connected.

## Features

- **Overview** — spendable balance, workers online, den earned, jobs — at a glance.
- **Models** — live text/vision models with prices, plus the image models.
- **Earnings** — recent jobs with den, units and duration.
- **Offline alerts** — native desktop notifications when a worker goes offline,
  enters maintenance, or recovers (while the app is open). Optionally also pushed
  to your phone via an [ntfy](https://ntfy.sh) topic.

## Security

- Your API key is stored **locally** in the app and sent only to
  `api.aipowergrid.io`.
- The app is sandboxed by Tauri's capability allowlist; its content-security
  policy only permits connections to the grid API, ntfy.sh, and a local inference
  backend.
- No telemetry, no servers of ours — it talks directly to the grid.

## Build it yourself

```bash
npm install
npm run tauri icon app-icon.png   # generate the icon set
npm run tauri dev                 # run locally
npm run tauri build               # produce an installer for your OS
```

Requires the [Tauri prerequisites](https://tauri.app/start/prerequisites/) (Rust +
your OS's webview dev libraries).

## License

MIT — see [LICENSE](LICENSE).
