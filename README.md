# GTA5 Web Port

Created and maintained by **SebasKitten**.

An experimental GTA V browser project using **WebAssembly memory64**, shared-memory Web Workers and **WebGPU**. Includes a web launcher, Story and Sandbox modes, graphics controls, separate saves and experimental multiplayer. Normal frame limits: 24, 30 and 60 FPS.

## Desktop PCs and Macs

Approximate starting points, not validated minimums or FPS guarantees:

- **CPU:** modern 4-core processor or better.
- **RAM:** 8 GB with the low-memory profile; 16 GB or more recommended.
- **GPU:** recent WebGPU-compatible integrated or dedicated graphics, current drivers and hardware acceleration enabled. No fixed VRAM minimum has been validated.
- **Browsers:** try **Google Chrome or Microsoft Edge** first. Brave, Opera, Opera GX and Vivaldi are other Chromium-based options; compatibility depends on the version and configuration.
- **Platforms:** Windows is the primary tested platform. macOS and Linux launchers are supported, but gameplay depends on browser/GPU compatibility. Use the launcher’s device check.

## Mobile phones and tablets

Mobile support is experimental. No reliable device list or minimum has been validated.

- **Hardware:** a recent high-end phone/tablet; 8 GB RAM or more is a rough starting point, not a guarantee. Start with low-memory graphics, 75% render scale and 24–30 FPS.
- **Android browsers:** try current **Chrome or Edge**, or another Chromium browser exposing the required features.
- **iOS/iPadOS:** [Blinker Fluid](https://github.com/Nodesclock/Blinker-fluid) is an experimental Blink/V8 browser option to investigate on compatible devices. Its installation and JIT requirements vary; follow its own documentation. This port has not been validated in Blinker Fluid. Installing Chrome or Edge on iOS alone does not guarantee desktop-Chromium compatibility.
- **Compatibility:** the browser needs WebGPU, shared WebAssembly memory64 and shader clip distances. Mobile GPU support for **BC-compressed textures** varies; devices missing it currently need a fallback that is not yet available in this release.

## Hosting

Use localhost or HTTPS with cross-origin isolation headers. The bundled local launcher requires Python 3.10+; mobile clients can connect to a desktop/LAN host without running Python on the phone.

**Self-hosted LAN HTTPS:** install and trust the launcher-supplied LAN root certificate on each client device before opening the game, including the host if it uses the HTTPS address. The launcher supplies the certificate download and installation instructions under LAN certificate/setup. Installation and trust are manual; the launcher does not silently install it. HTTP on localhost needs no certificate, and a public site with a publicly trusted HTTPS certificate needs no manual certificate installation.

Still in development. This repository contains documentation only; project code, binaries and game assets have not been uploaded.

