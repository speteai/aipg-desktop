<p align="center">
  <img src="app-icon.png" alt="AIPG Desktop" width="120">
</p>

<h1 align="center">AIPG Desktop</h1>

<p align="center">
  An all-in-one desktop app for the <a href="https://aipowergrid.io">AI Power Grid</a> —
  chat, image generation, running your own worker, model prices, earnings and offline
  alerts, in one window.
</p>

<p align="center">
  <em>Unofficial, community-built. Not affiliated with AI Power Grid.</em>
</p>

Built with [Tauri](https://tauri.app): a small, sandboxed native app that uses the OS
webview instead of bundling a browser. Installers are a few MB, not a few hundred.

## Install

Grab the installer for your OS from the [Releases page](https://github.com/speteai/aipg-desktop/releases/latest):

| OS | File |
|----|------|
| **Windows** | `…_x64-setup.exe` (NSIS) or `…_x64_en-US.msi` |
| **macOS** | `…_universal.dmg` (Intel + Apple Silicon) |
| **Linux** | `…_amd64.AppImage`, `…_amd64.deb`, or `…x86_64.rpm` |

The builds are **unsigned** (no paid code-signing certificate), so on first launch your
OS may warn you ("unknown publisher" / Gatekeeper). Choose *Run anyway* / right-click →
*Open*. The full source and build pipeline are public here, so you can verify or build it
yourself.

On first run, paste your `grid_…` API key (from the AIPG Console). Chat works right away —
running a worker is optional.

## Features

- **Chat** — talk to the grid (`Auto` lets the grid pick the best model, or choose a
  specific one) or to a **local model** on your own Ollama. Streaming replies, multiple
  conversations, local history with a context cap. Nothing leaves your machine except the
  prompt you send to the grid.
- **Images** — text-to-image and image-to-image on the grid's image models
  (FLUX.2 Klein, Krea Turbo).
- **Run a worker** — one click turns your machine into a grid worker and starts earning:
  - downloads the **official** `grid-inference-worker` binary (SHA-256 verified),
  - downloads and runs a **sandboxed, managed Ollama** — no system install required,
  - detects your hardware (RAM / CPU / GPU) and **suggests a fitting model**, with a
    fallback to grid chat when the machine is too small,
  - shows live download progress (GB and %), and lets you pick **where models are stored**.
- **Overview / Models / Earnings** — spendable balance, workers online, den earned and
  jobs; live model list with prices; recent jobs plus a **local earnings-history graph**.
- **Offline alerts** — native notifications when a worker goes offline, enters
  maintenance, or recovers (while the app is open), optionally also pushed to your phone
  via an [ntfy](https://ntfy.sh) topic.
- **Runs in the background** — close to the system tray and keep the worker earning;
  optionally auto-start the worker when the app opens.

## Security & privacy

- Your API key is stored **locally** and sent only to `api.aipowergrid.io`.
- The app is sandboxed by Tauri's capability allowlist; its content-security policy only
  permits connections to the grid API, `ntfy.sh`, and the local inference backend.
- The managed Ollama and worker run as sandboxed child processes under the app's data
  directory — no global install, and they're stopped when you quit.
- No telemetry and no servers of ours: it talks directly to the grid.

## Build it yourself

```bash
npm install
npm run tauri icon app-icon.png   # generate the icon set from the logo
npm run tauri dev                 # run locally
npm run tauri build               # produce an installer for your OS
```

Requires the [Tauri prerequisites](https://tauri.app/start/prerequisites/) (Rust + your
OS's webview dev libraries). Releases are built automatically for all three OSes by GitHub
Actions on every `v*` tag.

## License

MIT — see [LICENSE](LICENSE).
