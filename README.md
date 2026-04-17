# KM5E's Base Camp v1.7.0

A Windows desktop application for browsing POTA, SOTA, and DX cluster spots with
N3FJP AC Log integration. Tune your radio with one click and track your progress.

## Features

- **POTA, SOTA & DX Spots** — Fetches POTA spots from `api.pota.app`, SOTA spots
  from `api2.sota.org.uk`, and DX cluster spots via telnet. All displayed in a
  unified sortable table with a Type column and quick-select type filters.
- **DX Cluster Telnet** — Connects to configurable DX cluster nodes (defaults:
  ve7cc.net, dxc.nc7j.com, gb7mbc.net). Spots stream in real-time and are cached
  for 15 minutes with automatic pruning.
- **N3FJP AC Log Integration** — Tunes your radio via the AC Log TCP API (port 1100).
  Clicking "Tune" sets the frequency, populates the callsign, mode, and reference
  in AC Log's entry fields.
- **Auto-Hunt on Log** — Background listener watches N3FJP for logged QSOs and
  automatically marks matching spots as hunted.
- **Hunt Tracking** — Mark spots as hunted so you can track your session progress.
  Hunted spots are shown with strikethrough text and a green tint.
- **Not Heard** — Mark spots you tuned to but couldn't hear. Greys them out.
- **QRT Detection** — Spots with "QRT" in comments are automatically greyed out.
- **Filtering** — Filter by band, mode, country/entity, and free-text search
  (searches callsign, park reference, and park name). Quick-select buttons for
  common bands and modes.
- **Sortable Columns** — Click any column header to sort ascending/descending.
- **Hide Hunted** — Toggle to remove already-hunted spots from view.

## Screenshot Layout

```
┌─────────────────────────────────────────────────────────────────────┐
│ 🏕 KM5E's Base Camp v1.7.0  │ 🔄 Refresh │ ⚙ Settings │  45 spots │ 32s ago  │
├──────────┬──────────────────────────────────────────────────────────┤
│ Filters  │ Band │ Mode │ Freq │ Activator │ Ref │ Park │ Loc │ Time │ Actions │
│          │──────│──────│──────│───────────│─────│──────│─────│──────│─────────│
│ Search:  │ 20m  │ SSB  │14250 │ W1AW      │K-01 │ Acad │US-ME│12:34 │📻 🎯    │
│ [______] │ 40m  │ CW   │ 7030 │ K5ABC     │K-45 │ Ozar │US-AR│12:33 │📻 ✅    │
│          │ ...  │ ...  │ ...  │ ...       │ ... │ ...  │ ... │ ...  │ ...     │
│ Band: All│      │      │      │           │     │      │     │      │         │
│ Mode: All│      │      │      │           │     │      │     │      │         │
│ Ctry: All│      │      │      │           │     │      │     │      │         │
│ □ Hide   │      │      │      │           │     │      │     │      │         │
│  hunted  │      │      │      │           │     │      │     │      │         │
├──────────┴──────────────────────────────────────────────────────────┤
│ Hunted this session: 3 │ N3FJP: 127.0.0.1:1100 │ ✅ Tuned to ...  │
└─────────────────────────────────────────────────────────────────────┘
```

## Prerequisites

1. **Rust toolchain** — Install from [rustup.rs](https://rustup.rs/).
   Make sure you have the Windows MSVC target:
   ```
   rustup default stable-msvc
   ```

2. **N3FJP AC Log** — Running with the TCP API enabled:
   - Open AC Log → Settings → Application Program Interface
   - Check **TCP API Enabled (Server)**
   - Note the port (default 1100)
   - AC Log must have rig control configured for the Tune button to
     change your radio's frequency

## Building

```bash
# Clone or download this project, then:
cd basecamp
cargo build --release
```

The binary will be at `target\release\basecamp.exe`.

### Cross-compiling from Linux (optional)

```bash
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

## Running

```bash
# Just run the exe — no installation needed
.\target\release\basecamp.exe
```

Or double-click `basecamp.exe` from Explorer.

## Configuration

Click **⚙ Settings** in the top bar to configure:

| Setting             | Default       | Description                                   |
|---------------------|---------------|-----------------------------------------------|
| N3FJP Host          | `127.0.0.1`   | IP address of the machine running AC Log      |
| N3FJP Port          | `1100`        | TCP port for the AC Log API                   |
| Auto-refresh        | Enabled       | Automatically re-fetch spots on a timer       |
| Refresh interval    | 60 seconds    | How often to poll the POTA API (min 15s)      |

Use **Test Connection** to verify AC Log is reachable before tuning.

## Usage Tips

- **Tune** sends three commands to AC Log: `CHANGEFREQ` (tunes the radio),
  then sets the callsign, mode, and park reference in AC Log's entry fields.
  After making the QSO, press Enter in AC Log to log it as usual.

- **Hunt / Hunted** is a local toggle for your session. It does not interact
  with the POTA website. Hunted status is keyed on `callsign + park reference`,
  so the same activator at a different park is a separate entry.

- The **country/entity** filter is derived from the `locationDesc` field
  (e.g., `US-FL` → `US`, `VE-ON` → `VE`, `JA-TK` → `JA`).

- **Frequency normalization**: The POTA API sometimes returns frequency in kHz
  (e.g., `14074.0`) and sometimes in MHz (e.g., `14.074`). The app handles both
  formats for band detection and N3FJP tuning.

## How It Works

1. **Spots** are fetched from `https://api.pota.app/spot/activator` which returns
   a JSON array of current activator spots.

2. **N3FJP communication** uses the AC Log TCP API (documented at
   [n3fjp.com/help/api.html](http://www.n3fjp.com/help/api.html)).
   Commands are XML-like strings sent over a TCP socket:
   - `<CMD><CHANGEFREQ><VALUE>14.250</VALUE></CMD>` — tunes the radio
   - `<CMD><UPDATE><FIELDNAME>txtEntryCall</FIELDNAME><VALUE>W1AW</VALUE></CMD>` — sets fields

3. **GUI** is built with [egui](https://github.com/emilk/egui) / eframe,
   a pure-Rust immediate-mode GUI library that compiles natively on Windows
   with no external dependencies.

## Dependencies

| Crate     | Version | Purpose                              |
|-----------|---------|--------------------------------------|
| eframe    | 0.31.1  | Native GUI framework (egui backend)  |
| egui      | 0.31.1  | Immediate-mode GUI                   |
| reqwest   | 0.12    | HTTP client for POTA API             |
| serde     | 1.0     | JSON deserialization                 |
| serde_json| 1.0     | JSON parsing                         |
| chrono    | 0.4     | Date/time utilities                  |
| tokio     | 1       | Async runtime (reqwest dependency)   |

## License

This project is provided as-is for personal amateur radio use. 73!
