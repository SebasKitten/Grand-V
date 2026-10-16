# GrandV Back end launcher

Source for the native desktop host is in this directory. Packaged executables use the name `$GrandV back-end host` (`.exe` on Windows, `.app` on macOS). The launcher includes asset download/setup instructions; game assets remain separate. See the repository Releases page for packaged downloads.

Native Rust replacement for the Python/Tk launcher, HTTP server and multiplayer relay. The Python source and its release builds remain available for comparison and rollback.

The native window, product metadata and package name use **GrandV Back end launcher**. `assets/app.png` is the supplied GrandV logo, embedded in the window/taskbar icon and the Windows executable's multi-resolution icon resource. macOS packaging generates an `.icns` from the same image. Linux includes the PNG and a desktop-entry template; installation must place the executable/icon on the appropriate search paths. Existing game directories and protocol IDs remain compatible.

Icon generation trims the logo's transparent export padding and centers the artwork in a square with a small border, so shortcut/taskbar icons fill the available space. The original supplied PNG remains unchanged.

## Players

Use the executable built for your OS and CPU architecture. No Rust, Python, C++ build tools or separate relay worker is required. Windows builds statically link the Microsoft C runtime. macOS and Linux use their normal system libraries and graphics drivers.

Place the launcher beside `mirror` in `playgta5-offline`, or select the game folder with **Browse**. Game assets remain external. macOS resolves the default game folder beside the `.app` bundle. Select **This PC**, **LAN / school network**, or **Public website**, optionally enable multiplayer, and click **Start & open game**.

Stop or closing the window cancels this launcher's listener, discovery socket and relay sessions. It does not stop another application's server. Join works without a local game folder.

LAN hosting creates a private CA and server certificate when needed. Share only `lan-root-ca.cer`; clients install trust manually. Existing Python RSA roots are preserved, including their original DER fingerprint and private key. Explicit certificate renewal refreshes the server leaf while retaining the root. A supplied certificate/key pair is validated and used without automatic renewal. No system trust changes are made.

Public hosting supports an automatic Cloudflare Quick Tunnel or a manually configured trusted HTTPS origin. With a reverse proxy and direct HTTPS disabled, the internal listener remains on loopback. Router, firewall and reverse proxy configuration for manual hosting remain external.

## Hosting for friends with free HTTPS

1. Install [cloudflared](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/downloads/). Windows: use **Copy install command (PowerShell / CMD)**, open PowerShell or Command Prompt, paste `winget install --id Cloudflare.cloudflared --exact` and press Enter. Restart the launcher after installation so it picks up PATH changes. Alternatively place `cloudflared.exe` beside the launcher (`cloudflared` on macOS/Linux).
2. Choose **Public website**, enable **Cloudflare free tunnel (temporary HTTPS link)**, select the game folder and optionally enable multiplayer with a generated code.
3. Press **Start server** or **Start & open game**. GrandV binds its backend to loopback HTTP, starts its owned cloudflared process, captures the HTTPS URL and configures game links and the `/relay` WebSocket origin. Direct HTTPS is disabled automatically. No account, domain, port forwarding or client certificate import is required.
4. Use **Share game link** for the link/QR. Send the multiplayer code separately. Friends open the link in Chrome/Edge; they need neither local game files nor cloudflared.
5. Keep the host awake and launcher open. **Stop server** or closing the launcher stops its tunnel/backend. An unexpected tunnel exit stops hosting; start again for a new URL. Other cloudflared processes/services are preserved.

The UI automates startup and URL setup after one-time dependency installation. It does not install software or a background service. The choice is remembered in `launcher-settings.json`; every Start creates a fresh URL. Headless mode does not manage tunnels; use the manual procedure below.

The URL is public, including game files and existing save APIs. The multiplayer code protects only the relay. For access control or a stable hostname, configure a named tunnel and Cloudflare Access separately, then use the manual **Public HTTPS origin** field.

[Quick Tunnel limits](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/): no uptime guarantee, 200 in-flight requests, no SSE, new hostname each start. Browser saves/cache are scoped to each origin, so each URL has separate browser storage. Host disk backups remain, but do not assume automatic browser-profile recovery. Asset loads consume host upload bandwidth. All proxied visitors appear as loopback, so backend per-IP admission/bandwidth limits apply collectively; forwarded IP headers are not trusted.

For failures, inspect **Console output**. URL allocation times out after 45 seconds and stops the attempted tunnel/backend. Existing `.cloudflared/config.yaml` / `config.yml` can interfere; inspect them without deleting unrelated setups. Public DNS/connection readiness can lag URL creation by a few seconds. See [Cloudflare's official guide](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/).

Manual alternative, including the older Python launcher: run `cloudflared tunnel --url http://127.0.0.1:8000`, paste its printed `https://....trycloudflare.com` origin into **Public website**, leave direct HTTPS off and start GrandV on port 8000. Share the `/launcher.html` link, keep both programs running, and stop cloudflared separately. Substitute your actual game port.

## Speed-limit guidance

Advanced server settings also provide **Maximum concurrent connections / streams**. Direct HTTP/HTTPS and manually configured proxy hosting have no fixed application ceiling: choose a positive limit, or **0 = unlimited** (the default, including existing settings without this field). This counts open connections, including idle connections and relay sessions, rather than players. Automatic Cloudflare tunnel mode always uses **200**, disables this control, and retains your direct-hosting value for switching back. Cloudflare's own 200 in-flight request limit remains separate. Stop and restart hosting to apply changes.

Advanced server settings show a tuning starting point of **10 MiB/s (about 84 Mbit/s) per direct client** when capacity allows, or **0/unlimited on a fast LAN** to avoid artificial asset throttling. This is a practical estimate, not a measured game minimum or a stall-free guarantee. Existing limits/defaults remain unchanged.

For Internet hosting, budget about 80% of measured host **upload**: per-player MiB/s = upload Mbit/s × 0.8 / 8.388608 / active players. A 100 Mbit/s upload supports a budget of about 9.5 MiB/s total, or 4.8 MiB/s each for two equally active players. Through Cloudflare, the field is labeled as a **shared tunnel limit** because all visitors appear as loopback and share the cap. Direct clients sharing a source IP, and other reverse proxies, can also share a cap. Limits are not reservations of bandwidth or fairness guarantees. Raise the cap if capacity allows and asset loads are late; otherwise reduce simultaneous players. Initial uncached loads, client performance and network latency also affect results.

## Compatibility

**Console output** opens a separate native, movable/resizable desktop window with live launcher status, HTTP request results, browser `/log` messages, TLS failures and relay events. It includes Follow output, Copy output and Clear controls. Closing the console leaves hosting running; closing the launcher closes both windows and its owned server. Output history is bounded to 1500 lines / 512 KiB, and timestamps are labeled UTC. Request query strings, launcher tokens and relay access codes are not added by the server's diagnostic logging.

**Start server** starts hosting without opening a browser. Once running, URLs appear beneath the buttons for localhost, `127.0.0.1`, and each network IPv4 address when the selected audience enables LAN/network access. Local-only and public-proxy modes explain their listening scope. **Start & open game** retains its existing browser-opening behavior.

- Settings: existing `launcher-settings.json`, preserving unknown fields.
- Access code: existing `multiplayer-access-code.txt`, outside the served directory.
- Disk saves: existing browser profile directories under `userdata`, base64 restore data, millisecond mtimes, legacy tombstone lists and timestamp maps.
- Game HTTP APIs: `GET`/`HEAD`, single byte ranges, `/data/batch?gz=1`, `X-Run-Lengths`, `/log`, userdata PUT/DELETE/restore, and authenticated loopback `/__local/port` changes.
- Isolation headers: COOP, COEP and CORP on responses; correct WASM/JavaScript MIME types.
- Per-source-IP client admission and aggregate streaming bandwidth; 128 HTTP/WebSocket connection slots and a 64-session relay limit.
- Relay protocol 1: `check`, `join`, `welcome`, `joined`, `pose`, `left`; case-sensitive access codes, build/map agreement, two players per room, server-assigned IDs, bounded/coalesced queues and message limits.
- UDP discovery: `GTA5LAN1` on port 8014 with request nonce, private peers, fingerprint and canonical browser link.
- Public certificate download/info, including plaintext download on the LAN HTTPS listener; other plaintext requests redirect or reject.
- QR sharing, manual Join, offline troubleshooting, four optional donation wallets. Solana is first and expands automatically whenever the donation menu opens; opening another coin closes the previously expanded coin.
- **Updates and info** opens a movable small popup with **Telegram** (`https://t.me/reGRANDTA`), **Discord** (`https://discord.gg/Td8Jdjw3Wf`) and **GitHub** (`https://github.com/SebasKitten/Grand-V`) buttons. GitHub links to the backend launcher distribution repository. Each opens its link in the default browser.

The relay now runs inside the host process at `/relay`. The old `relay_port` setting is retained for configuration compatibility but is not opened or used. This build does not proxy a separately launched Python relay. Share links never contain the local launcher capability or access code.

Deliberate hardening: directory listings are disabled, reserved save metadata names cannot be written through the save API, malformed tombstones fail closed, paths cannot escape through existing symlinks/junctions, and request parsing/body collection has time/size limits. Directory indexes still work.

## Build and test

Run in this directory with Rust stable and the native platform build prerequisites installed:

```text
cargo test --locked --all-targets -- --test-threads=1
cargo build --locked --release
target/release/gta-browser-launcher --ui-smoke --no-open --ui-screenshot ui-smoke.png
```

On Windows the executable has `.exe`. The GUI smoke run closes itself and creates no browser tab. Tests use temporary game/save fixtures; they do not read or modify player saves.

Automated HTTP/TLS fixtures bind only to loopback and disable LAN advertising, including fixtures configured with LAN settings. The UDP protocol test also binds only to loopback. Do not grant firewall access to hashed `compatibility-*.exe` or `legacy_ca-*.exe` test binaries. Only deliberate LAN/public hosting by the finished launcher needs external network access.

Headless hosting is also supported:

```text
gta-browser-launcher --headless --folder PATH_TO_PLAYGTA5_OFFLINE --no-open
```

It loads the folder's existing settings. `--port` overrides the remembered port. Ctrl+C stops the owned host. A normal GUI launch accepts `--folder`, `--port` and `--no-open` too.

`../../.github/workflows/rust-build.yml` builds Windows x64, macOS ARM64/Intel, and Linux x64, alongside the existing Python workflow. The workflow runs from `LauncherApp/rust` within the Grand-V repository. Keep `troubleshooting.json` and `donations/wallets.json` beside `rust`; they are embedded at compile time. Do not upload game assets, saves, generated certificates, access codes, private keys or `target`/`dist` directories.

The separately ignored `legacy_ca` test requires `GTA_LEGACY_CA_FIXTURE` pointing to a temporary game folder whose CA was generated by the original Python `lan_tls.py`; run it with `cargo test --test legacy_ca -- --ignored` to verify RSA CA migration independently.

macOS packages are unsigned/unnotarized. Builds on this Windows PC do not establish macOS/Linux runtime or full game compatibility; use the native CI jobs and real-device testing before replacing their Python releases.

Latest packaging update 2026-10-15: `dist/GrandV Back end launcher-bandwidth.exe` adds speed-limit guidance and a shared-limit label for Cloudflare, and includes the GitHub distribution button and Cloudflare setup changes below. The standard Windows ZIP now contains this build. Formatting, embedded JSON parsing and release build passed; the archived executable hash matches the standalone build. Earlier executable variants are preserved. Bandwidth values are tuning estimates; no new gameplay throughput benchmark was performed.

## Local validation — 2026-10-13

Update 2026-10-15: the latest Cloudflare-enabled Windows build is `dist/GrandV Back end launcher-cloudflare-setup.exe` (also in `dist/GrandV-Launcher-windows-x64.zip`). This version explicitly says to paste the install command into PowerShell or Command Prompt, then use Start server for automatic backend/tunnel/link setup. The earlier executable was open and remains unchanged. Focused tunnel parser/process-lifecycle tests, public-origin/WebSocket integration, and GUI/controller tests pass. Process tests use local fixtures and do not create public tunnels. Live Cloudflare connectivity, full-game remote loading and macOS/Linux runtime behavior remain unverified. The UI smoke mode does not start public Quick Tunnels.

Windows x64 release built using Rust 1.99.0. Two GUI/controller tests, ten backend compatibility tests and the separately enabled Python RSA CA migration test passed. The standalone packaged executable passed native GUI startup/render/close testing; its DLL import list contains Windows system libraries only. `cargo fmt --check` passed. No full game or multi-device LAN run was performed, and remote macOS/Linux CI has not been run.

Try `dist/GrandV Back end launcher-color.exe`, or extract `dist/GrandV-Launcher-windows-x64.zip`. The updated filename preserves the original executable while it is running. `dist/SHA256SUMS.txt` records the updated executable hash. The ZIP's executable was checked against that verified binary. The project validation/checkpoint record is `../../_reports/rust-launcher-validation-2026-10-13.json`. No test processes, native smoke windows or agent-created browser tabs remain; user-opened launcher windows are preserved.
