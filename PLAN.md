# BDO Optimizer — Implementation Plan

Open-source Rust GUI app that detects the user's hardware, applies the optimal Black Desert Online
CPU-affinity configuration from ACanadianDude's performance guide, and benchmarks FPS locally to
find the best mask for the user's specific machine.

## Research conclusions that shape the design

1. **Affinity must be applied via launch inheritance.** BDO's anti-cheat blocks setting affinity on
   the running `BlackDesert64.exe` (access denied) and resets priority tweaks. The safe, working
   method — and what the guide itself prescribes — is to launch `BlackDesertLauncher.exe` with the
   mask already applied (`start /affinity <hex>` or CreateProcess-suspended → SetProcessAffinityMask
   → resume) so the game inherits it. We never touch the running game process — no injection, no
   handles into the protected process. This is the core of the "optimized shortcut" feature.
2. **FPS capture: Intel PresentMon 2.x CLI** (MIT license, bundled with our releases). ETW-based,
   fully out-of-process, anti-cheat safe. We spawn it targeting the game PID, parse the per-frame
   CSV (`MsBetweenPresents` by header name, not column index). Requires admin elevation (UAC prompt
   at capture start) — unavoidable ETW constraint.
3. **BDO is effectively Windows-only.** Its anti-cheat blocks Proton on Linux; no macOS build.
   The app still builds and runs on all three OSes (hardware detection + viewing saved benchmark
   data everywhere), but Apply-Optimization and Benchmark are Windows features. Linux gets generic
   taskset/.desktop + MangoHud support as a stretch goal; macOS has no affinity API at all
   (detection/analytics only, by OS design).
4. **X3D CCD detection should be done live, not by lookup table.** Read L3 cache topology
   (`GetLogicalProcessorInformationEx(RelationCache)` on Windows, sysfs on Linux): the core group
   sharing the ~96 MB L3 is the V-Cache CCD. The guide's static CPU→mask table is the fallback and
   the sanity check. (The 9000X3D generation broke the "V-Cache is always CCD0" assumption, so
   topology-first is the robust choice.)
5. **"1% low" is ambiguous** — we compute both standard variants from raw frame times and label
   them: P1 (1000 / 99th-percentile frame time) and the CapFrameX-style time-weighted "1% low
   integral" (best at surfacing stutter, which is exactly what affinity changes affect).
   Average FPS = total frames / total time, never a mean of instantaneous FPS.

## Stack

| Concern | Choice |
|---|---|
| GUI | `eframe`/`egui` + `egui_plot` (single self-contained .exe, native charts; pin versions together) |
| CPU detection | `sysinfo` + `raw-cpuid`; Windows cache topology via `windows` crate; Linux via sysfs |
| GPU detection | `wgpu` adapter enumeration (one API for NVIDIA/AMD/Intel on all OSes); PCI vendor ID distinguishes vendors |
| Shortcut creation | `IShellLinkW` COM via `windows` crate (the pure-Rust `mslnk` crate is stale — verify or skip) |
| FPS capture | Bundled PresentMon CLI, spawned per session; `csv` + `serde` for parsing |
| Process detection | `sysinfo` polling for `BlackDesert64.exe` → auto start/stop capture |
| Storage | One JSON file per benchmark session (raw frame-time array + metadata) in `directories` data dir; stats recomputed on load. SQLite later only if querying demands it |

## Data model (per benchmark session)

```json
{
  "timestamp": "...",
  "label": "affinity 555",
  "config": { "affinity_mask": "555", "cores": [0,2,4,6,8,10] },
  "hardware": { "cpu": "Ryzen 9 7900X3D", "gpu": "..." },
  "frames_ms": [16.6, 16.9, ...],
  "presentmon_version": "2.5.x"
}
```

Raw frame times are the source of truth; avg/max/min/P1/1%-low-integral are derived at display time.

## App screens (single window, 3 tabs)

1. **Hardware** — detected CPU (model, cores, CCD/V-Cache layout), GPU (vendor/model), and the
   recommended affinity mask with an explanation of which cores it selects and why.
2. **Optimize** — recommended mask (editable/overridable, e.g. 555 vs 554), game path picker
   (auto-detect Steam/Pearl Abyss install), Steam checkbox (`-steam` arg), one button:
   **Create Optimized Shortcut** → writes a desktop .lnk that launches the launcher with the mask,
   flagged run-as-administrator. Plus a "Verify" helper that reads the running game's actual
   affinity (read-only) to confirm the mask took.
3. **Benchmark** — start/stop capture (auto-detects game launch/exit), live frame-time sparkline,
   and a session table + bar chart comparing avg / max / P1 / 1%-low-integral across saved
   sessions, grouped by affinity mask. Warn when a session has too few frames for percentile
   metrics (< ~1000).

## Phases

**Phase 1 — MVP (Windows)**
- Cargo workspace scaffold, eframe app shell, CI (GitHub Actions: build on win/linux/mac, clippy, fmt).
- Hardware detection + recommendation engine: guide's Ryzen/Intel mask tables as embedded data,
  V-Cache CCD topology detection overriding/confirming the table, mask → core-list explanation.
- Optimized shortcut creation (COM .lnk) + launch-with-affinity, affinity verification readback.

**Phase 2 — Benchmarking**
- PresentMon bundling + capture pipeline (elevation flow, `--terminate_on_proc_exit`,
  `--stop_existing_session`), CSV parsing, session JSON storage.
- Metrics engine (with unit tests against known frame-time fixtures) + comparison UI (egui_plot).

**Phase 3 — Polish / stretch**
- Linux generic support (taskset shortcuts, MangoHud log import), macOS graceful degradation.
- Additional guide tweaks as optional toggles: `GameOption.txt`/`gameVariable.xml` PostFilter=0
  edit, memory-compression PowerShell toggle, links/instructions for driver-level settings
  (NVIDIA Profile Inspector / AMD Enhanced Sync — instructions, not automation, in v1).
- Import PresentMon/CapFrameX files for interop.

## Repo layout

```
bdo-optimizer/
├─ Cargo.toml            # workspace
├─ crates/
│  ├─ app/               # eframe GUI
│  ├─ hw/                # CPU/GPU detection + recommendation engine (pure, testable)
│  ├─ launch/            # affinity launch, shortcut creation (per-OS impls)
│  └─ bench/             # PresentMon driver, CSV parsing, metrics math, session store
├─ vendor/presentmon/    # bundled PresentMon.exe + MIT LICENSE + pinned version note
├─ .github/workflows/ci.yml
├─ README.md  LICENSE  PLAN.md
```

## Non-goals / explicitly skipped

- Registry `CpuPriorityClass` tweak — guide confirms anti-cheat resets it; dead.
- Any interaction with the running game process beyond read-only affinity verification.
- Automated driver-settings writing (NVIDIA Profile Inspector automation) in v1 — instructions only.
- Windows debloat scripts — out of scope for a game optimizer (system-wide risk).

## Open questions for the user

1. **App/repo name** — `bdo-optimizer` (matching the folder), or something brandable?
2. **License** — MIT recommended (matches PresentMon bundling); OK?
3. **Cross-platform scope** — given BDO can't run on Linux/macOS, is "app builds everywhere,
   optimize/benchmark on Windows" acceptable? (Full Linux support only makes sense as a
   generic-game tool.)
4. **GitHub** — create the repo under your account via `gh` once building starts?
