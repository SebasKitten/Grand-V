# GTA5 Web Port

Created and maintained by **SebasKitten**.

An experimental GTA V browser project using **WebAssembly memory64**, shared-memory Web Workers and **WebGPU**. Includes a web launcher, Story and Sandbox modes, graphics controls, separate saves and experimental multiplayer. Current normal frame limits: 24, 30 and 60 FPS.

## Approximate requirements

These are starting-point estimates, not a validated minimum or FPS guarantee.

- CPU: modern 4-core processor or better.
- RAM: aim for 8 GB with the low-memory profile; 16 GB or more recommended.
- Graphics: WebGPU-capable GPU with BC texture compression and shader clip distances; hardware acceleration enabled. No fixed VRAM minimum has been validated.
- Browser: up-to-date desktop Chromium with shared WebAssembly memory64 support.
- Hosting: localhost or HTTPS with cross-origin isolation headers; the bundled local launcher needs Python 3.10+.

## Platforms

Windows is the primary tested platform. macOS and Linux local launchers are supported; gameplay compatibility depends on the browser, GPU and required features. macOS support is a target, not a guarantee for every Intel or Apple Silicon Mac. Mobile support remains experimental, especially on GPUs without BC texture support.

Still in development. This repository contains documentation only; project code, binaries and game assets have not been uploaded.
