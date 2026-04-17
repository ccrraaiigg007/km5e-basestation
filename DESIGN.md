# KM5E's Base Camp — Design Document

**Version:** 1.11.3
**Author:** Craig (KM5E), Charleston SC
**Platform:** Windows desktop (Rust + egui)
**Purpose:** Unified amateur radio spot browser for POTA, SOTA, and DX cluster activity with N3FJP AC Log integration.

---

## 1. Overview

Base Camp is a native Windows desktop application for amateur radio hunting and DXing. It aggregates real-time activity spots from three sources:

- **Parks On The Air (POTA)** — via `https://api.pota.app/spot/activator`
- **Summits On The Air (SOTA)** — via `https://api2.sota.org.uk/api/spots/-2/all`
- **DX Cluster nodes** — via telnet, with automatic failover across a configurable priority list

Spots are displayed in a unified table with rich filtering, sorting, and one-click integration with N3FJP AC Log for tuning and QSO logging. DXCC and ATNO (All-Time New One) status is resolved in the background for every spot so new entities are visually highlighted.

The app is designed for a single user running one instance on their operating desk, alongside their logger and radio control software.

---

## 2. Architecture

### 2.1 Single-file layout

The entire application lives in `src/main.rs` (~3500 lines). This is intentional — the author is learning Rust and a single file is easier to navigate while the codebase is small. Top-level items are ordered: data models → helper utilities → N3FJP client → DX cluster → `SpotEntry` → `PotaHunterApp` struct → `eframe::App` impl → entry point.

### 2.2 Dependencies

```toml
eframe = "0.31.1"       # Window management + native rendering
egui = "0.31.1"         # Immediate-mode GUI
image = "0.25"          # Icon loading
reqwest = { version = "0.12", features = ["blocking", "json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = { version = "0.4", features = ["serde"] }
tokio = { version = "1", features = ["full"] }   # currently unused, reserved
dirs = "5"              # %APPDATA% resolution
open = "5"              # Opens URLs in default browser
```

### 2.3 Threading model

- **Main (UI) thread:** Runs the egui event loop. All rendering and state mutation happens here.
- **POTA/SOTA/DX-merge fetch thread:** Spawned per refresh. Calls the HTTP APIs, harvests DX cluster cache, constructs `SpotEntry` values, and swaps them into `AppState` behind a mutex.
- **DX cluster thread:** Long-lived when DX is enabled. Maintains a telnet connection with failover, parses spots, and appends to `DxClusterState.cached_spots`.
- **N3FJP listener thread:** Long-lived when enabled. Opens a TCP connection to AC Log to receive `CALLTABENTEREVENTS` so manual QSO logging in AC Log can auto-mark spots as hunted.
- **DXCC lookup thread:** Spawned on demand. Batches up to 20 unknown `(call, band, mode)` combos and resolves them via N3FJP commands. Results stream back via a `Mutex<Vec<...>>`.

Synchronization is `Arc<Mutex<T>>` and `Arc<AtomicBool>`. No async/await is used despite `tokio` being in `Cargo.toml` — it's left in as a dependency in case future features need it.

### 2.4 Platform

Windows-only in practice. Font loading reads `C:\Windows\Fonts\seguiemj.ttf` at startup for emoji support. The build target is `x86_64-pc-windows-msvc`. The app ships as a single `basecamp.exe` with the logo icon embedded via `include_bytes!`.

---

## 3. Data Model

### 3.1 `PotaSpot` (canonical format)

This struct is the shared internal format. POTA spots are deserialized directly from the POTA API JSON. SOTA spots and DX cluster spots are converted to this format before use.

```rust
struct PotaSpot {
    spot_id: Option<u64>,
    activator: String,        // callsign
    frequency: String,        // in kHz as text (e.g. "7058.5")
    mode: String,             // "CW", "SSB", "FT8", etc.
    reference: String,        // park/summit ref, or empty for DX
    park_name: Option<String>,
    spot_time: Option<String>,// ISO 8601 from API
    spotter: Option<String>,
    comments: Option<String>,
    source: Option<String>,
    name: Option<String>,
    location_desc: Option<String>,  // "US-SC", "JP-TK", etc.
    grid4: Option<String>,
    grid6: Option<String>,
}
```

### 3.2 `SotaSpot` — SOTA API format

Deserialized from SOTA, then converted via `SotaSpot::to_pota_spot()`.

### 3.3 `SpotType` enum

```rust
enum SpotType { Pota, Sota, Dx }
```

Drives type-specific behavior (reference link URLs, type badge color, right-click menu options).

### 3.4 `SpotEntry` — display wrapper

The actual struct rendered in the table. Wraps a `PotaSpot` with derived and flag data:

```rust
struct SpotEntry {
    spot: PotaSpot,
    spot_type: SpotType,
    band: String,           // derived from freq via freq_to_band()
    country: String,        // derived from location_desc
    hunted: bool,
    is_qrt: bool,           // parsed from comments
    not_heard: bool,
    dxcc_country: Option<String>,  // from N3FJP COUNTRYLISTLOOKUP
    atno_status: Option<String>,   // ATNO/OW/OC/OWBMW/OCBMW/BMC
}
```

### 3.5 Spot keys

The app uses stable string keys to track identity across list mutations. Spot indices go stale the moment a new DX cluster spot arrives.

```rust
fn make_spot_key(spot_type: SpotType, activator: &str, reference: &str) -> String {
    match spot_type {
        SpotType::Pota | SpotType::Sota => format!("{}-{}", activator, reference),
        SpotType::Dx => format!("DX-{}", activator),
    }
}
```

Everything that tracks a specific spot — selection, last-tuned, hunted set, not-heard set, context menu state — uses this key, never an index.

### 3.6 `AppState`

The shared state behind `Arc<Mutex>`:

```rust
struct AppState {
    spots: Vec<SpotEntry>,
    last_fetch: Option<Instant>,
    fetch_error: Option<String>,
    is_fetching: bool,
}
```

### 3.7 `Settings` — persisted configuration

Serialized to JSON at `%APPDATA%\basecamp\settings.json`.

```rust
struct Settings {
    n3fjp_host: String,       // default "127.0.0.1"
    n3fjp_port: String,       // default "1100"
    dx_callsign: String,
    dx_nodes: String,         // newline-separated "host:port"
    dx_auto_start: bool,
    auto_refresh: bool,
    refresh_interval_secs: u32,  // default 120
    my_grid: String,
    dark_mode: bool,
    hide_dupes: bool,
    hide_qrt: bool,
    max_age_mins: i64,
}
```

---

## 4. External Integrations

### 4.1 POTA API

- Endpoint: `https://api.pota.app/spot/activator`
- Format: JSON array matching `PotaSpot`
- Timeout: 15 seconds
- User-Agent: `BaseCamp/1.11.3`
- No authentication
- Called on refresh (manual or auto)

### 4.2 SOTA API

- Endpoint: `https://api2.sota.org.uk/api/spots/-2/all`
- Format: JSON array matching `SotaSpot`
- Converted to `PotaSpot` format via `SotaSpot::to_pota_spot()`

### 4.3 DX Cluster (telnet)

- Defaults: `ve7cc.net:23`, `dxc.nc7j.com:7300`, `gb7mbc.net:8000`
- Protocol: plain text over TCP
- Login: user's callsign followed by `\r\n`
- Initial query: `sh/dx 50\r\n` to fetch the last 50 spots
- Parses two line formats:
  - **Live:** `DX de SPOTTER:    14215.0  N1ABC        ...`
  - **History:** `14215.0  N1ABC       2026-01-15 1830Z  ...`
- Failover: tries nodes in order, cycles to next on connect/read failure with 2-3s delay, reconnects indefinitely while DX is enabled
- Cache retention: 15 minutes, pruned continuously
- Status updates are written to `DxClusterState.status` and displayed in the bottom status bar
- `DxClusterState.needs_refresh` is set when the initial batch has arrived, triggering a spot table refresh

### 4.4 N3FJP AC Log

Base Camp communicates with N3FJP AC Log via its TCP API (default port 1100). The `N3fjpClient` struct wraps all command sends.

**Commands sent by Base Camp:**

| Command | Purpose |
|---------|---------|
| `CHANGEFREQ` | Tune radio to spot frequency |
| `QSOINPROGRESS` | Fill call/band/mode/freq fields in AC Log |
| `UPDATEANDLOG` | Log a complete QSO |
| `CALLTABENTEREVENTS` | Subscribe to log events (used by listener thread) |
| `COUNTRYLISTLOOKUP` | Resolve a callsign to DXCC country name |
| `ATNO` | Check worked status for (country, band, mode) |
| `READBMF` | Test connection |

**ATNO response values:**

- `ATNO` — All-Time New One (never worked on any band/mode)
- `OW` — Worked overall but not confirmed
- `OC` — Confirmed on another band/mode
- `OWBMW` — Worked on this band/mode, not confirmed
- `OCBMW` — Confirmed elsewhere, worked on this band/mode
- `BMC` — Confirmed on this band AND mode

The UI highlights `ATNO`, `OW`, and `OC` (the "interesting" cases) with a soft red row tint and a `NEW!` badge on the callsign.

---

## 5. UI Layout

### 5.1 Top panel (toolbar)

Left to right:
- Logo (28×28 px) + title `KM5E's Base Camp v1.11.3`
- `🔄 Refresh (Ns)` button — countdown to next auto-refresh, or `⏳ Refreshing...` when in flight
- `☰ Filters` / `☰ Filters ▸` — toggles sidebar visibility
- `Settings` button — toggles settings window
- `N3FJP: ON/.../OFF` toggle — starts/stops the AC Log listener
- `DX: ON/.../OFF` toggle — starts/stops the DX cluster connection
- `NEW: N` — only shown when ATNO alerts exist
- Right-aligned: `{count} spots | {elapsed}s ago`

### 5.2 Left sidebar (Filters) — collapsible

Populated only if `show_filters` is true. Contains:
- Search box (callsign / reference / park name) with autofocus via `/` key
- Band multi-select (dropdown for All + quick-select row for common bands)
- Mode multi-select (dropdown + quick-select row)
- Country/Entity dropdown (All + populated from visible spots)
- `Hide hunted spots` / `Hide QRT spots` checkboxes
- Max spot age picker (5m / 15m / 30m / 1h / 2h / All)
- `Clear All Filters` button (red-tinted)
- Quick Band Filters — tappable pills for each band present in current spots
- Quick Mode Filters — tappable pills for each mode
- Quick Type Filters — POTA / SOTA / DX pills
- `🗑 Clear Hunted List` button

### 5.3 Central panel (spot table)

Uses `egui::Grid` with `striped(true)` inside a `ScrollArea::both()` with `id_salt("spot_scroll")` for scroll persistence.

Columns:
1. **Type** — POTA / SOTA / DX badge, always full color
2. **Band** — subtle pastel color coding
3. **Mode**
4. **Freq (kHz)** — monospace, right-aligned
5. **Activator** — bold; strikethrough if hunted; red bold + `NEW!` if ATNO; blue if not-heard; `[QRT]` appended if QRT
6. **Reference** — park/summit ref
7. **Name** — park/summit name, truncated to ~25 chars with `…`, full text on hover
8. **Location** — country/state (e.g. `US-SC`)
9. **Comment** — truncated to ~20 chars with `…`, full on hover
10. **Dist** — distance/bearing from user's grid (if set)
11. **Age** — monospace, right-aligned, color-coded (green <15m, amber 15-30m, red >30m)
12. **Actions** — icon-only buttons at 36px each: 📻 Tune, 🎯/✅ Hunt toggle, 📝 Log, 🚫/👂 Not-heard toggle

Empty states:
- First load + no data + fetching: spinner with "Fetching spots..."
- No data and not fetching: "No spots yet" with hint
- Filters hide all spots: "No spots match current filters" + Clear All button

Row interaction (no `ui.interact()` inside Grid — that breaks layout):
- Row click detection is done via `ui.input()` pointer position checked against a row rect
- Primary click selects
- Secondary click captures spot data and opens the context menu as a separate `egui::Area` popup outside the grid
- Double click tunes
- Keyboard: Up/Down nav, Enter = tune, H = hunt, L = log, R = refresh, / = focus search, Esc = close menu/settings/clear search

### 5.4 Bottom status bar

Shows: `Hunted: N | N3FJP: Connected/off | DX: Connected/Off | {action status} | {fetch error if any}`

### 5.5 Row highlighting

Background tint precedence (first match wins):
1. Last-tuned spot — gold
2. ATNO — soft red
3. Selected — light blue
4. Hunted — light green
5. Not-heard — light blue, darker blue text
6. QRT — warm grey

Row height is measured dynamically — saved `row_top` Y at render start, subtracted from `available_rect_before_wrap().top()` after cells render. This matters because hardcoded heights don't match actual font-driven row heights.

---

## 6. Key Behaviors & Fixes

### 6.1 Selection survives list reordering

`selected_spot_key: Option<String>` stores the key, not the row index. When DX spots insert above the selected row, the key still matches the correct entry.

### 6.2 DXCC lookups survive list churn

The DXCC pipeline is fully cache-based, not index-based:
- `queue_dxcc_lookups()` sends `(call, band, mode)` tuples to the worker thread
- Worker returns `(cache_key, call, country, status)` — no indices
- `apply_dxcc_results()` populates `dxcc_cache` and `atno_cache`, then sweeps all spots every frame and fills in cached data
- New spots pick up cached lookups automatically on the next frame

### 6.3 DX cluster initial batch population

When the DX thread signals `needs_refresh = true` after the initial `sh/dx 50` arrives, the flag is only cleared if a fetch can actually start (not while `is_fetching`). Otherwise the signal would be lost and the spots wouldn't appear until the next auto-refresh.

### 6.4 egui Grid interaction caveat

**Never use `ui.interact()` inside a `Grid`.** It creates a phantom cell that steals clicks from in-row buttons and produces visible layout artifacts. Use raw pointer position detection with `ui.input()` against a measured row rect instead. Context menus are rendered as standalone `egui::Area` popups outside the grid.

### 6.5 Dark mode on first launch

`egui::Visuals::dark()` or `light()` must be applied to the context **before** cloning the style. Otherwise the cloned style carries the default theme and the visuals switch doesn't propagate fully.

### 6.6 Emoji font loading

Segoe UI Emoji is loaded at runtime from `C:\Windows\Fonts\seguiemj.ttf` and added as a fallback font to both `Proportional` and `Monospace` families. `FontData` is wrapped in `std::sync::Arc` as egui 0.31 requires.

---

## 7. File Layout

```
basecamp/
├── Cargo.toml
├── build.bat              # Windows build script that renames output with version
├── README.md
├── DESIGN.md              # this file
├── assets/
│   ├── icon_32.png        # title bar
│   ├── icon_64.png        # embedded via include_bytes!
│   ├── icon_256.png       # for installers
│   └── logo.svg           # source vector
└── src/
    └── main.rs            # the entire app
```

Settings file: `%APPDATA%\basecamp\settings.json` (on Windows).
Build output: `target\release\basecamp.exe`, copied by `build.bat` to `basecamp-v{version}.exe`.

---

## 8. User Preferences (applies when working with Claude Code)

- **Version every change.** Bump `Cargo.toml`, the title bar heading string (`KM5E's Base Camp v1.x.y`), the main() title, and `build.bat`'s filename suffix for every release, including tiny fixes. Use the `-v1.x.y` suffix in filenames.
- **Explain the code.** When making changes, explain what was done and why. The author is learning Rust.
- **Prefer larger fonts** for readability (currently 18px body, 26px heading).
- **Persist everything user-facing** to `settings.json` when adding new options.

---

## 9. Known Patterns to Follow

**Adding a new persisted setting:**

1. Add field to `Settings` struct and `Default for Settings`
2. Add matching field to `PotaHunterApp` struct
3. Load it in `Default for PotaHunterApp` via `s.field_name`
4. Save it in `save_current_settings()` via `self.field_name.clone()` if String
5. Add UI in the Settings window
6. Call `save_current_settings()` when value changes

**Adding a new column:**

1. Add variant to `SortColumn` enum
2. Add header entry to the `headers` vec in central panel
3. Add cell rendering in the data row loop
4. Add sort branch in `get_filtered_spots()` or equivalent
5. Decide truncation if the data is variable-length

**Adding a new context menu item:**

Add to the `egui::Area` popup in `update()` near the bottom — NOT inside the grid. The menu has access to `self.ctx_menu_spot`, `self.ctx_menu_spot_type`, `self.ctx_menu_spot_key`, and `self.ctx_menu_original_idx`.

**Adding a new keyboard shortcut:**

1. Add a key check in the `ctx.input(|i| ...)` block near the top of `update()`
2. Add handler code below, respecting the `typing` check (don't fire when a text field has focus) for letter keys
3. Document in the "Showing N spots" header label

---

## 10. Current Release — v1.11.3

- All QoL improvements: zebra striping, subtler band colors, right-aligned monospace frequencies, icon-only action buttons, collapsible filter sidebar, scroll position persistence, loading/empty states, double-click to tune, / and Esc shortcuts, refresh countdown timer
- Built on eframe/egui 0.31.1

See `README.md` for user-facing release notes.
