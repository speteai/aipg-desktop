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

Built with [Tauri](https://tauri.app): a small native app that uses the OS
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

On first run, sign in with your browser, or paste a `grid_…` API key from the AIPG Console.
Chat works right away — running a worker is optional and needs an API key.

## Features

- **Chat** — talk to the grid (`Auto` lets the grid pick the best model, or choose a
  specific one) or to a **local model** on your own Ollama. Streaming replies, multiple
  conversations, local history with a context cap. Nothing leaves your machine except the
  prompt you send to the grid.
  - **Attachments** — drop, paste or pick text, code and PDF files; their text is sent
    along as context. Images can be attached for a local vision model (the grid has none).
  - **Quick prompt** — a global shortcut (default `Ctrl+Shift+Space`, changeable or off in
    Settings) opens a one-line prompt above all windows and sends it to a new chat.
- **Images** — text-to-image and image-to-image on the grid's image models, with format
  and count options, live generation progress, and a **local gallery** (save, reuse a
  prompt, use a result as the next source image). You choose the **folder the images
  are saved in**; the prompt box is full-width and remembers its height.
- **Run a worker** — one click turns your machine into a grid worker and starts earning:
  - downloads the **official** `grid-inference-worker` binary and checks it against a
    SHA-256 that is fixed in this app's source code,
  - downloads and runs its **own copy of Ollama** from the app's data folder, on its own
    port (11511) and with its own model store — no system install, and separate from an
    Ollama you already have (using yours instead is an explicit option),
  - loads models with a **context length you choose** (default 8192) and reports the
    same number to the grid,
  - detects your hardware (RAM / CPU / GPU) and **suggests a fitting model**, with a
    fallback to grid chat when the machine is too small,
  - shows live download progress (GB and %), lets you pick **where models are stored**,
    and lists installed models with their size so you can delete what you no longer need,
  - restarts the worker automatically if it crashes.
- **Worker log and warm start** — the worker's output is kept in a log file, also on Windows where the worker has no
  console (the app then reads it from the worker's local dashboard). Every start and stop is logged with time and
  reason. Right after a start the model is loaded into memory, so the first grid job does not run into a timeout.
- **Worker activity** — Overview shows a history of the worker on this computer: per hour (last 24 hours) or per day
  (last 7 days) how long it was online and how long it was actually serving grid jobs, with jobs and den. Recorded
  while the worker runs in the app and stored on this computer only.
- **Overview / Models / Earnings** — spendable balance with today's free allowance (and what holding AIPG adds to it), workers online (retired workers
  can be hidden), den earned and jobs; live model list with prices; recent jobs plus a **local earnings-history graph**.
- **Network** — a live diagram of the grid: which workers are online, which models they
  serve, who handled the latest jobs. (The grid publishes no worker locations, so it is a
  structure view, not a geographic map.)
- **Connect** — a local **MCP connector** lets assistants on the same computer (Claude
  Desktop, Claude Code, Cursor, …) offload work to the grid: run a text task, generate
  images, list models, check your credit. One click registers it with Claude Desktop.
  It runs on your machine (`aipg-desktop --mcp`), so hosted clients such as ChatGPT or
  claude.ai in the browser cannot use it. After an app update, restart the assistant
  program so it loads new or changed tools.
  Results state the estimated cost and your remaining credit; a quote tool estimates a
  cost beforehand, a worker-status tool reports your workers and recent log errors, and
  you can set a **daily spending limit** for assistants. Tool descriptions warn that
  prompts are processed in plain text on third-party machines.
- **Offline alerts** — native notifications when a worker goes offline, enters
  maintenance, or recovers (while the app is open), optionally also pushed to your phone
  via an [ntfy](https://ntfy.sh) topic.
- **Runs in the background** — close to the system tray and keep the worker earning;
  optionally launch at login and auto-start the worker.
- **Self-updating** — the app checks GitHub Releases for a newer version and installs it
  in one click. Updates are signature-verified against a key baked into the app.
  (On Linux this works for the AppImage; `.deb`/`.rpm` users update via a new package.)

## Signing in

**Sign in with your browser** opens the AIPG Console, where you approve the app; there is
no key to copy. The app receives a token that lasts 15 minutes and renews it by itself. The
grid ends a sign-in after 30 days without use or 180 days at the latest, and you can end it
yourself under Settings → Sign out. This covers chat, images, your credit and the connector.
Running a worker needs an API key, because the worker program only accepts a key.

## Getting an API key

On first start the app walks you through it: open the
[AIPG Console key page](https://console.aipowergrid.io/dashboard/api-key), sign in or create an account, create a key, paste it.
The app checks the key with the grid before saving it. Without a key you can still browse
the network and the model list and chat with a model on your own computer.

## Protection against other grid participants

Grid requests are answered on computers run by independent people. The app limits what
such a computer can learn or do:

- **Private data is masked.** Before a chat message or a connector request goes to the
  grid, keys, passwords, e-mail addresses, IBANs, card and phone numbers are found and
  (after asking, or always, as you choose) replaced by placeholders; the real values are
  put back into the answer on your computer. This catches clearly shaped values only.
- **“Local only” chats** can use local models only and never reach the grid.
- **Answers are untrusted.** The connector hands every grid answer to the assistant
  marked as third-party content whose instructions must not be followed. The app shows
  answers as plain text and never runs anything from them.

- **Cross-check.** Under every grid reply in the chat, *Cross-check* asks a different
  model the same question without showing it the first answer, has the two answers
  compared and shows whether they agree, where they differ and, as far as the grid shows
  it, which workers served them. *Cross-check* in the chat bar does this for every reply.
  It costs two extra requests. Agreement is not proof (two models can share a mistake,
  and the comparison is itself made by a model), but a single worker returning a wrong
  or manipulated answer no longer passes unnoticed.

The grid offers no encryption and no choice of worker, so a worker always sees the
(masked) prompt and can return a wrong answer.

## Security & privacy

- Your API key is stored **locally** and sent only to `api.aipowergrid.io`.
- A browser sign-in is stored in a file in the app's data folder (owner-readable only on
  Linux/macOS). During sign-in the app listens on `127.0.0.1` only, for at most five minutes.
- If you enable the connector, the key is also written to a file in the app's data folder
  (owner-readable only on Linux/macOS) so the connector process can use it; disabling the
  connector removes the key from that file.
- The worker gets the key through its environment, not its command line, and API keys
  are blanked in the worker log.
- The web page inside the app can only call the commands on Tauri's capability
  allowlist. Its content-security policy permits connections to the grid API, `ntfy.sh`
  and the two local Ollama ports (11511 for the app's own, 11434 for yours) only.
- The app's Ollama and the worker are ordinary child processes (not an OS sandbox) that
  live in the app's data directory — no global install. They are stopped when you quit;
  on Windows they are also ended if the app is killed or crashes, and on every start the
  app ends any of its own processes that a previous run left behind.
- Downloads are verified against SHA-256 values fixed in the source code. That detects
  a corrupted or swapped file; it does not make the upstream projects themselves trusted.
- Only one instance of the app runs at a time.
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

## Support

AIPG Desktop is free, unofficial and built in spare time. If it is useful to you, a
voluntary tip on the **Base** network (ETH or USDC) is welcome:

`0x1C0356d4ff32aEe3DF28Acf941335A1823dB50A4`

Send on Base only. Nothing in the app depends on it.
