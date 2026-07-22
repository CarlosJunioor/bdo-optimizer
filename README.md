# BDO Optimizer

Open-source desktop app that optimizes Black Desert Online performance for **your** hardware
and proves it with local FPS benchmarks.

Based on the community research in
[ACanadianDude's Ultimate BDO Performance Guide](https://docs.google.com/document/d/1cyLaDiPL_B6nOZw_qPE_wOGuoeRT-qddTjevTFoFBkg).

## What it does

- **Detects your hardware** — CPU model, core/CCD topology (including which CCD carries
  3D V-Cache on AMD X3D chips), and GPU (NVIDIA / AMD / Intel).
- **Recommends the optimal CPU affinity mask** for BDO (the game performs best confined to
  ≤6 physical cores on one cache domain, SMT disabled) and creates an **optimized desktop
  shortcut** that launches the game with that mask applied.
- **Benchmarks FPS locally** using Intel [PresentMon](https://github.com/GameTechDev/PresentMon)
  (out-of-process ETW capture — no injection, no overlay). Records average, max, P1, and
  time-weighted 1% low FPS per session so you can A/B test masks (e.g. `555` vs `554`) and
  keep whichever is genuinely faster on your machine. All data stays in a local folder.

## Anti-cheat safety

The app **never touches the running game process**. The affinity mask is applied to
`BlackDesertLauncher.exe` at launch so `BlackDesert64.exe` inherits it — the same technique the
performance guide prescribes manually. FPS capture is ETW-based (OS event tracing), fully
outside the game. The only read of the game process is a read-only affinity verification.

## Platform support

| | Windows | Linux | macOS |
|---|---|---|---|
| Hardware detection | ✅ | ✅ | partial |
| Apply optimization / shortcut | ✅ | generic games only¹ | ❌ (no OS affinity API) |
| FPS benchmarking | ✅ (PresentMon) | planned (MangoHud import) | ❌ |

¹ BDO itself does not run on Linux (anti-cheat blocks Proton) or macOS.

## Building

```
cargo build --release
```

The binary is `target/release/bdo-optimizer`. On Windows, benchmarking requires
administrator elevation (an ETW constraint of PresentMon).

## Repository layout

- `crates/app` — egui GUI
- `crates/hw` — hardware detection + recommendation engine
- `crates/launch` — affinity launch, shortcut creation, verification
- `crates/bench` — PresentMon driver, metrics, session storage
- `vendor/presentmon` — bundled PresentMon CLI (MIT, Intel)

## License

MIT — see [LICENSE](LICENSE). Bundled PresentMon is MIT-licensed by Intel
(`vendor/presentmon/LICENSE.txt`).
