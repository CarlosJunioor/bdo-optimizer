# BDO Optimizer

Inspect your PC, safely apply BDO CPU affinity, and prove the gain locally.

![Rust](https://img.shields.io/badge/Rust-2021-orange)
![Platform](https://img.shields.io/badge/BDO_workflow-Windows-0078D4)
![License](https://img.shields.io/badge/license-MIT-blue)

BDO Optimizer is an open-source desktop app for Black Desert Online players who
want a hardware-aware CPU-affinity recommendation and a repeatable way to check
whether it improves frame rate and frame pacing on their own PC.

`Inspect hardware → Apply safely → Verify read-only → Measure baseline and optimized → Compare`

The affinity recommendations are based on
[ACanadianDude's Ultimate BDO Performance Guide](https://docs.google.com/document/d/1cyLaDiPL_B6nOZw_qPE_wOGuoeRT-qddTjevTFoFBkg).

## What it does

- **Explains the relevant hardware:** CPU model and topology, L1/L2/L3 cache,
  AMD X3D V-Cache CCDs, RAM, GPU and driver details, and local storage.
- **Recommends a supported affinity mask:** the recommendation is pre-filled,
  while alternate masks remain available under the advanced control.
- **Applies affinity without touching the running game:** BDO inherits the mask
  from `BlackDesertLauncher.exe`.
- **Verifies before measuring:** the app reads the running process affinity and
  blocks an optimized capture until it matches the selected mask.
- **Compares against a real baseline:** saved runs contain raw frame times and
  locally calculated average FPS, P1 low, and time-weighted low metrics.
- **Applies the guide's NVIDIA driver-profile tweaks (optional):** merge-imports
  the per-game "Black Desert" profile through the bundled NVIDIA Profile
  Inspector and verifies the result against the driver database. Nothing global
  changes, and one click restores driver defaults.
- **Applies the guide's config-file tweaks (optional):** sets PostFilter and
  Tessellation off in `GameOption.txt` and every UserCache `gamevariable.xml`,
  backing each file up first, with a one-click restore.
- **Applies the safe settings in one click:** the config-file tweaks, Windows'
  "Optimizations for windowed games", and — when the app runs as administrator on
  an NVIDIA GPU — the driver profile, each reported as its own step and reversed
  by one "Undo all of it". Memory compression and HAGS stay on their own buttons:
  one needs a reboot, the other affects every application.
- **Shows its version:** the current package version appears in the app header.

## Download

Grab the latest `bdo-optimizer-*-windows-x64.zip` from
[Releases](https://github.com/CarlosJunioor/bdo-optimizer/releases), extract the
folder anywhere, and run `bdo-optimizer.exe`. Keep the bundled files together —
FPS capture and the NVIDIA tweaks look for their tools beside the exe. Windows
SmartScreen will warn on first run because the build is unsigned: *More info* →
*Run anyway* (each release ships a `.sha256` you can check first).

## Requirements

### To use the BDO workflow

- 64-bit Windows supported by BDO and PresentMon.
- An installed copy of Black Desert Online.
- `PresentMon.exe` beside `bdo-optimizer.exe` for FPS capture. Official packaged
  builds should ship both files together.
- `nvidiaProfileInspector.exe` (plus its `.exe.config`) beside
  `bdo-optimizer.exe` for the optional NVIDIA driver-profile tweaks. Packaged
  builds ship it; the feature is hidden on machines without an NVIDIA GPU.
- Administrator permission for benchmarking. Windows ETW will not start the
  PresentMon trace from a non-elevated process.

Hardware inspection and saved-session viewing can run on other supported desktop
platforms, but applying and measuring BDO affinity are Windows-only.

### To build from source

- Stable Rust with Cargo.
- On Windows, the `x86_64-pc-windows-msvc` Rust toolchain and Microsoft C++
  build tools.
- Git.

Run the development build:

```powershell
git clone https://github.com/CarlosJunioor/bdo-optimizer.git
cd bdo-optimizer
cargo run -p bdo-optimizer
```

Debug builds find the checked-in `vendor/presentmon/PresentMon.exe`.

Create a runnable release layout:

```powershell
cargo build --release
Copy-Item vendor\presentmon\PresentMon.exe target\release\PresentMon.exe
.\target\release\bdo-optimizer.exe
```

The standalone release executable does not search arbitrary parent folders for
PresentMon; keep the two executables together.

## Safe first run

### 1. Inspect

Open **1 Inspect** and review the detected CPU, caches, memory, GPU, storage, and
recommended affinity. Select **Continue to Apply** when a recommendation exists.

### 2. Apply

In **2 Apply**:

1. Confirm the detected BDO install, or browse to
   `BlackDesertLauncher.exe`.
2. Enable the Steam option only for a Steam installation.
3. Keep the recommended mask unless you intentionally want to test an alternate.
4. Choose **Launch Now with Affinity** or **Create Optimized Shortcut**.
5. Start BDO and wait for the status to change from **PENDING** to **VERIFIED**.

The app may show a UAC prompt because the BDO launcher itself requires
elevation. Verification is read-only and runs once per second.

### 3. Restore the normal launch

Affinity is applied only to that launch chain. To restore the default:

1. Close BDO and its launcher.
2. Start BDO from its normal launcher or Steam entry.
3. Delete the optional optimized desktop shortcut if you no longer want it.

There is no persistent service to disable. BDO Optimizer never writes global
driver settings, global Windows settings, registry priority tweaks, or the
running game process. The only driver change it can make is the optional,
user-initiated NVIDIA per-game "Black Desert" profile described below — and
that has its own **Restore driver defaults** button.

## NVIDIA driver profile (optional)

On Windows machines with an NVIDIA GPU, **2 Apply** shows an extra section that
applies the performance guide's driver-profile recommendations to the game's
own "Black Desert" profile:

- **Threaded optimization** — On for 6+ physical cores, Off for older
  quad-cores (the guide's rule).
- **Ansel** — Off.
- **Power management mode** — Adaptive.
- **Ultra Low Latency** — opt-in checkbox; the guide advises testing whether
  your CPU handles the overhead.

How it works and why it is safe:

- The change is a **merge** import through the bundled, hash-pinned
  [NVIDIA Profile Inspector](https://github.com/Orbmu2k/nvidiaProfileInspector)
  CLI. Only the listed settings are written; anything else on the profile
  (G-Sync preferences, for example) and all global driver settings are left
  alone.
- Every apply is **verified**: the app exports the driver database afterwards
  and confirms the profile really carries the expected values.
- **Restore driver defaults** writes the documented default values back to the
  same settings.
- Profile Inspector requires administrator rights, so the section offers the
  same **Restart as administrator** flow the Benchmark tab uses.
- AMD users: the equivalent tweaks (Radeon Enhanced Sync on, Wait for Vertical
  Refresh off) have no safe automation API — set them in AMD Software manually.

## Game config files (optional)

**2 Apply** also offers the guide's config-file edits, which disable the forced
post-process sharpening and (on High presets and above) tessellation:

- `Documents\Black Desert\GameOption.txt` — `postFilter = 0` and
  `Tessellation = 0`.
- Every UserCache `gamevariable.xml` — each `<PostFilter Value="0"/>` and
  `<Tessellation Value="false"/>` entry. The XML files use `true`/`false`
  where the text file uses `1`/`0`; each is written in its own format.

Notes:

- The first time a file is changed, its original is copied to
  `<name>.bdo-optimizer.bak` next to it; **Restore from backup** copies the
  originals back. Later applies never overwrite the backup, so the restore
  point is always the pre-optimizer state.
- Only those specific values change — every other byte of the files is
  preserved.
- The buttons refuse to run while BDO is open, because the game rewrites these
  files on exit and would undo (or race) the edit. Close the game, apply, then
  launch.
- **Turning on the in-game Display Filter setting undoes these edits.** The
  guide is explicit about this. If sharpening comes back, check that setting
  before re-applying.

## Repeatable benchmark method

Use the same graphics settings, resolution, scene, camera path, character, and
capture length for every run. Close unnecessary background work and let the
system reach a similar temperature before each pass.

### Capture the baseline

1. Close any optimized BDO launch.
2. In **3 Measure**, select **Baseline / normal launch**.
3. Launch BDO normally.
4. Wait for **Full CPU affinity detected**. The baseline capture remains disabled
   while a restricted mask is detected.
5. Enter the test scene, select **Start baseline capture**, and perform the route.
6. Select **Stop**, or exit the game to end and save the run.

### Capture the optimized run

1. Close BDO completely.
2. Launch it from **2 Apply** or the optimized shortcut.
3. In **3 Measure**, select **Optimized / affinity launch**.
4. Wait for **Verified running mask**. Unverified or mismatched runs cannot start.
5. Repeat the same route and duration, then stop or exit.

### Compare

Select exactly one **Baseline** and one optimized session. The comparison always
uses Baseline as A and Optimized as B, so the displayed delta is `B − A`.

Aim for at least 1,000 captured frames; shorter runs are marked low confidence.
For a result you can trust, repeat each mode at least three times and compare
representative pairs rather than relying on one unusually good run.

> **Known limitation:** baseline readiness currently derives the full affinity
> mask from the detected logical-processor count. Unusual Windows processor-group
> or offline-CPU layouts may not unlock baseline capture correctly.

## Permissions and anti-cheat behavior

| Action | Permission | Game-process behavior |
|---|---|---|
| Inspect hardware | Standard user | No game access |
| Create/launch optimized shortcut | UAC may appear | Sets affinity on the launcher before it runs |
| Verify affinity | Read-only process query | Reads the current mask; never sets it |
| Capture FPS | Administrator | PresentMon uses external Windows ETW tracing |
| Apply/restore NVIDIA profile | Administrator | Profile Inspector writes only the per-game driver profile; no game access |
| Apply/restore game config files | Standard user | Edits `Documents\Black Desert` files with backups; refuses while BDO runs |
| Show overlay | Same app session | Separate always-on-top native window; no injection or graphics hook |

The FPS overlay can appear over windowed and borderless-windowed BDO. It cannot
appear over exclusive fullscreen because it is an external desktop window. The
overlay polls affinity read-only and shows live capture statistics; it does not
modify or inject into the game.

## Saved data

Each completed run is saved locally as one JSON file containing:

- timestamp and label;
- CPU and GPU model;
- baseline/optimized role plus expected and read-only observed affinity masks;
- PresentMon version;
- raw frame times.

Metrics are recalculated from the raw frame times when loaded, so formula fixes
can improve existing sessions. The per-user session directory is shown at the
bottom of **3 Measure** and is resolved with the operating system's standard
application-data location. Runs are newest-first and can be deleted from the
session table.

During capture, PresentMon CSV output is written to the operating system's
temporary directory and deleted when the capture worker finishes, on every exit
path including errors. The saved session JSON keeps the raw frame times. No
benchmark data is uploaded.

## Platform support

| Capability | Windows | Linux | macOS |
|---|---:|---:|---:|
| Hardware inspection | Yes | Yes | Partial |
| View saved sessions | Yes | Yes | Yes |
| Apply/verify BDO affinity in the app | Yes | No | No |
| Live BDO capture with PresentMon | Yes | No | No |
| External FPS overlay during capture | Yes | No BDO workflow | No BDO workflow |

BDO itself is not supported through this app on Linux or macOS. The launch
library contains generic non-Windows helpers, but the desktop BDO Apply and
Measure flow intentionally exposes only supported Windows behavior.

## Troubleshooting

### `PresentMon.exe not found`

Place `PresentMon.exe` next to the running `bdo-optimizer.exe`. For a debug build,
the checked-in `vendor/presentmon/PresentMon.exe` is also accepted.

### Benchmarking requires administrator rights

Use **Restart as administrator** in **3 Measure** and accept the UAC prompt.
Hardware inspection does not require elevation.

### Baseline capture stays disabled

Close BDO and its launcher, then start BDO normally. Wait for the read-only
affinity check to report full CPU affinity. See the processor-group limitation
in the benchmark section if this PC has an unusual CPU layout.

### Optimized capture stays pending or reports a mismatch

Close BDO completely, confirm the selected mask, and relaunch from **2 Apply** or
the optimized shortcut. Do not switch to a normal launcher between verification
and capture.

### The NVIDIA profile apply fails or the section is missing

The section only appears on Windows with a detected NVIDIA GPU. Applying needs
`nvidiaProfileInspector.exe` (and its `.exe.config`) next to `bdo-optimizer.exe`
and administrator rights. If verification reports that the driver database does
not show the expected values, open NVIDIA Profile Inspector manually — a driver
update may have renamed the "Black Desert" profile.

### The BDO install was not detected

Use **Browse** and select `BlackDesertLauncher.exe`. Confirm the Steam option
matches the installation.

### The overlay is not visible

Use borderless-windowed or windowed mode, enable **Show overlay**, and begin a
capture. Exclusive fullscreen cannot display a separate desktop window above the
game.

### The capture saved no usable frames

Confirm BDO reached the 3D scene, run long enough to produce frames, and stop the
capture normally. If the app reports ETW or access-denied errors, restart it as
administrator.

## Contributing

Keep changes focused and preserve the core safety rule: never write to
`BlackDesert64.exe`.

```powershell
cargo fmt --all --check
cargo test --workspace
cargo build --workspace
```

The workspace contains:

- `crates/app` — egui desktop application;
- `crates/hw` — hardware detection and recommendation engine;
- `crates/launch` — safe launch inheritance, shortcuts, and read-only verification;
- `crates/bench` — PresentMon capture, metrics, and local session storage;
- `vendor/presentmon` — pinned PresentMon executable and MIT license;
- `vendor/nvidiaProfileInspector` — pinned NVIDIA Profile Inspector executable
  and MIT license.

## License

BDO Optimizer is licensed under the [MIT License](LICENSE). Bundled PresentMon is
licensed by Intel under MIT; see
[`vendor/presentmon/LICENSE.txt`](vendor/presentmon/LICENSE.txt). Bundled NVIDIA
Profile Inspector is licensed by Orbmu2k under MIT; see
[`vendor/nvidiaProfileInspector/LICENSE.txt`](vendor/nvidiaProfileInspector/LICENSE.txt).
