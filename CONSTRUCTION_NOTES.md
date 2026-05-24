# Base Camp Construction Notes

Generated from the v1.25.1 source in `src/main.rs`.

## Project Shape

- Native Windows desktop app written in Rust 2021.
- GUI framework is `eframe`/`egui` 0.31.1 with immediate-mode rendering.
- The application is intentionally single-file: nearly all behavior lives in `src/main.rs`.
- Build resource work is in `build.rs`, which creates a multi-size ICO from PNG assets and embeds it into the Windows executable.
- `build.bat` wraps `cargo build --release` and copies the binary to versioned filenames.

## Important Version Drift

- `Cargo.toml`, `build.bat`, source header, and `main()` title say v1.25.1.
- `DESIGN.md` says v1.11.3.
- `README.md` says v1.7.0.
- HTTP user-agent strings still say `BaseCamp/1.13`.
- Current source includes WWFF support, but the design document mostly describes POTA/SOTA/DX only.

## Main Data Model

- `PotaSpot` is the canonical internal spot shape. POTA API responses deserialize directly into it.
- `SotaSpot` converts into `PotaSpot` with `to_pota_spot()`.
- `WwffSpot` also converts into `PotaSpot`; its timestamps are normalized by replacing the first space with `T`.
- DX cluster parsed lines are also represented as `PotaSpot`.
- `SpotType` distinguishes `Pota`, `Sota`, `Dx`, and `Wwff`.
- `SpotEntry` wraps a `PotaSpot` with derived display state: band, country, hunted, QRT, not-heard, DXCC country, and ATNO status.
- Spot identity is string-keyed by `make_spot_key()`:
  - POTA/SOTA/WWFF: `activator-reference-band-mode`
  - DX: `DX-activator-band-mode`

## Persistent Settings

- Settings are serialized as JSON under `%APPDATA%\basecamp\settings.json`.
- Persisted fields include N3FJP host/port, DX callsign/nodes/auto-start, auto-refresh, station grid, dark mode, duplicate hiding, QRT hiding, and max spot age.
- Session-only state includes hunted spots, not-heard spots, selected row, last tuned spot, DXCC caches, and UI popup state.

## Threading And Shared State

- UI runs on the main egui event loop.
- Shared spot list state is `Arc<Mutex<AppState>>`.
- Long-running controls use `Arc<AtomicBool>` flags.
- `trigger_fetch()` starts a background thread that fetches POTA, SOTA, and WWFF spots, merges current DX cache entries, then swaps a new `Vec<SpotEntry>` into `AppState`.
- DX cluster runs in its own reconnecting thread while enabled.
- N3FJP log-event listening runs in its own thread while enabled.
- DXCC/ATNO lookups run in small batches on a background thread and feed results back through a mutex-protected vector.
- The app uses blocking IO throughout. `tokio` is present in `Cargo.toml` but not used by the current code.

## External Integrations

- POTA API: `https://api.pota.app/spot/activator`
- SOTA API: `https://api2.sota.org.uk/api/spots/-2/all`
- WWFF spots JSON: `https://spots.wwff.co/static/spots.json`
- DX cluster: telnet-style TCP nodes configured as newline-separated `host:port` entries.
- N3FJP AC Log: TCP API, default `127.0.0.1:1100`.

## Fetch And Merge Pipeline

- HTTP calls use `reqwest::blocking::Client` with a 15-second timeout.
- Each source is converted into `SpotEntry` with:
  - `freq_to_band()` derived band
  - `location_to_country()` derived country/entity
  - hunted/not-heard flags copied from session sets
  - QRT detected by searching comments for `QRT`
- Partial failures are tolerated. Errors from each source are appended into `fetch_error`.
- If any spots are successfully loaded, `fetch_error` is cleared.
- DX cache is pruned to 15 minutes before merge.

## Filtering And Sorting

- Filters are applied in `get_filtered_spots()`.
- Empty band/mode/type sets mean "show all".
- Callsign search also searches reference and display park/name text.
- Max age filtering treats `0` as no limit.
- Duplicate hiding keeps only the newest visible spot per activator callsign after sorting.
- Sorting supports type, activator, frequency, mode, band, reference, park, location, comment, distance, and spot time.
- Distance sorting currently compares grid strings as a proxy rather than numeric distance.

## UI Construction

- Top panel contains logo, title, refresh button/countdown, filter toggle, settings toggle, N3FJP toggle, DX Cluster toggle, ATNO alert count, and spot count.
- Settings are shown in an `egui::Window`.
- Bottom panel is a status bar for hunted count, N3FJP state, DX state, action status, and fetch errors.
- Optional left side panel contains filters and quick filter buttons.
- Central panel renders a pinned header grid and a scrollable data grid.
- Column widths are fixed constants so header and body columns stay aligned.
- Row interactions are handled by measuring row rectangles and checking pointer position, rather than inserting `ui.interact()` cells inside the grid.
- Context menu is rendered as a foreground `egui::Area` outside the grid.
- Keyboard shortcuts include arrow navigation, `R` refresh, `Enter` tune, `Shift+T` tune next visible unworked spot, `H` hunt, `L` log, `N` not-heard, `/` focus search, and `Esc` close/clear.

## N3FJP Behavior

- `N3fjpClient` opens a new TCP connection per command.
- Tune sends a frequency change and updates QSO-in-progress fields.
- Log sends `UPDATEANDLOG`.
- Listener subscribes to `CALLTABENTEREVENTS` and queues logged calls.
- The UI drains logged calls and marks matching activators as hunted.
- DXCC lookup uses `COUNTRYLISTLOOKUP`; ATNO status uses `ATNO`.
- Interesting ATNO statuses are `ATNO`, `OC`, and `OW`; these increment the alert count and get visual emphasis.

## DX Cluster Behavior

- DX nodes are parsed from the settings text area.
- The cluster thread cycles through configured nodes and reconnects while enabled.
- It logs in with the user's callsign, sends an initial `sh/dx 50`, then keeps reading live lines.
- Parsed spots are cached with receive timestamps.
- When an initial batch arrives, `DxClusterState.needs_refresh` prompts the UI loop to trigger a merge fetch.

## Construction Risks And Improvement Notes

- The single-file layout is easy to search but makes unrelated changes risky; likely future split points are `models`, `settings`, `n3fjp`, `dx_cluster`, `fetch`, and `ui`.
- Version strings are duplicated in several places and have drifted from docs and HTTP user agents.
- `make_spot_key()` intentionally includes band and mode so hunted/logged and not-heard state does not leak across band/mode changes.
- `get_dx_cache_spots()` is currently unused.
- Distance sort is not truly distance-based.
- `tokio` is unused and can be removed unless planned work needs async.
- Source/comments contain mojibake in several strings, likely from encoding drift. This does not stop compilation but can hurt UI/readme clarity.
- The fetch thread performs POTA, SOTA, and WWFF requests sequentially; source latency is additive.
- `queue_dxcc_lookups()` can run whenever a host string is present, even if N3FJP is not actually reachable. Failures are cached as `ERR`.
- There are no automated tests for parsing, keying, filtering, or frequency/band conversion; those functions are good low-risk test targets.
