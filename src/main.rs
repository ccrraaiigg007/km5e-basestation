// KM5E's Base Camp v1.19.0
// POTA, SOTA & DX Spot Browser with N3FJP AC Log integration
// Displays POTA, SOTA and DX cluster spots with radio tuning via N3FJP API

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// POTA API data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PotaSpot {
    #[serde(default)]
    spot_id: Option<u64>,
    #[serde(default)]
    activator: String,
    #[serde(default)]
    frequency: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    reference: String,
    #[serde(default)]
    park_name: Option<String>,
    #[serde(default)]
    spot_time: Option<String>,
    #[serde(default)]
    spotter: Option<String>,
    #[serde(default)]
    comments: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    location_desc: Option<String>,
    #[serde(default)]
    grid4: Option<String>,
    #[serde(default)]
    grid6: Option<String>,
}

impl PotaSpot {
    /// Get the best available park name, trying parkName first, then name,
    /// then falling back to the park reference.
    fn display_park_name(&self) -> &str {
        // Try parkName first (non-empty)
        if let Some(ref pn) = self.park_name {
            let trimmed = pn.trim();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        // Try name field (may be park name in some API responses)
        if let Some(ref n) = self.name {
            let trimmed = n.trim();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        // Fallback to reference
        if !self.reference.is_empty() {
            return &self.reference;
        }
        "-"
    }
}

// ---------------------------------------------------------------------------
// SOTA API data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SotaSpot {
    #[serde(default)]
    activator_callsign: String,
    #[serde(default)]
    frequency: String,
    #[serde(default)]
    mode: String,
    #[serde(default, alias = "summitCode")]
    summit_code: String,
    #[serde(default, alias = "summitDetails")]
    summit_details: Option<String>,
    #[serde(default, alias = "timeStamp")]
    time_stamp: Option<String>,
    #[serde(default)]
    comments: Option<String>,
    #[serde(default)]
    callsign: Option<String>,
}

impl SotaSpot {
    /// Convert a SOTA spot into the unified PotaSpot format for display
    fn to_pota_spot(&self) -> PotaSpot {
        // SOTA frequency is in MHz (e.g. "14.062"), convert to kHz for consistency
        let freq_khz = match self.frequency.parse::<f64>() {
            Ok(f) if f < 1000.0 => format!("{:.1}", f * 1000.0),
            _ => self.frequency.clone(),
        };

        // Extract association/country from summit code (e.g. "W7I/SI-153" -> "W7I")
        let loc = if let Some(idx) = self.summit_code.find('/') {
            self.summit_code[..idx].to_string()
        } else {
            self.summit_code.clone()
        };

        PotaSpot {
            spot_id: None,
            activator: self.activator_callsign.clone(),
            frequency: freq_khz,
            mode: self.mode.clone(),
            reference: self.summit_code.clone(),
            park_name: self.summit_details.clone(),
            spot_time: self.time_stamp.clone(),
            spotter: self.callsign.clone(),
            comments: self.comments.clone(),
            source: Some("SOTA".to_string()),
            name: self.summit_details.clone(),
            location_desc: Some(loc),
            grid4: None,
            grid6: None,
        }
    }
}

// ---------------------------------------------------------------------------
// WWFF API data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct WwffSpot {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    activator: String,
    /// Already in kHz (float) — e.g. 7059.5
    #[serde(default)]
    frequency_khz: f64,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    reference: String,
    #[serde(default)]
    reference_name: String,
    #[serde(default)]
    remarks: String,
    #[serde(default)]
    spotter: String,
    #[serde(default)]
    spot_time_formatted: Option<String>,
}

impl WwffSpot {
    fn to_pota_spot(&self) -> PotaSpot {
        // frequency_khz is already kHz — format to one decimal to match PotaSpot convention
        let frequency = format!("{:.1}", self.frequency_khz);

        // Normalise timestamp: WWFF uses "2026-05-01 00:46:50" (space separator);
        // spot_age_minutes() expects ISO 8601 with a T separator.
        let spot_time = self.spot_time_formatted.as_ref().map(|s| s.replacen(' ', "T", 1));

        // Extract programme prefix for location_desc (e.g. "VKFF-1925" -> "VKFF")
        let loc = self.reference
            .find('-')
            .map(|i| self.reference[..i].to_string())
            .unwrap_or_else(|| self.reference.clone());

        PotaSpot {
            spot_id: self.id,
            activator: self.activator.clone(),
            frequency,
            mode: self.mode.clone(),
            reference: self.reference.clone(),
            park_name: Some(self.reference_name.clone()),
            spot_time,
            spotter: if self.spotter.is_empty() { None } else { Some(self.spotter.clone()) },
            comments: if self.remarks.is_empty() { None } else { Some(self.remarks.clone()) },
            source: Some("WWFF".to_string()),
            name: Some(self.reference_name.clone()),
            location_desc: Some(loc),
            grid4: None,
            grid6: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Spot type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SpotType {
    Pota,
    Sota,
    Dx,
    Wwff,
}

impl SpotType {
    fn label(&self) -> &'static str {
        match self {
            SpotType::Pota => "POTA",
            SpotType::Sota => "SOTA",
            SpotType::Dx => "DX",
            SpotType::Wwff => "WWFF",
        }
    }
}

// ---------------------------------------------------------------------------
// Persistent settings
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    n3fjp_host: String,
    n3fjp_port: String,
    dx_callsign: String,
    dx_nodes: String,
    dx_auto_start: bool,
    auto_refresh: bool,
    refresh_interval_secs: u32,
    my_grid: String,
    dark_mode: bool,
    hide_dupes: bool,
    hide_qrt: bool,
    max_age_mins: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            n3fjp_host: "127.0.0.1".to_string(),
            n3fjp_port: "1100".to_string(),
            dx_callsign: String::new(),
            dx_nodes: "ve7cc.net:23\ndxc.nc7j.com:7300\ngb7mbc.net:8000".to_string(),
            dx_auto_start: false,
            auto_refresh: true,
            refresh_interval_secs: 120,
            my_grid: String::new(),
            dark_mode: true,
            hide_dupes: false,
            hide_qrt: false,
            max_age_mins: 15,
        }
    }
}

fn settings_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("basecamp");
    std::fs::create_dir_all(&dir).ok();
    dir.join("settings.json")
}

fn load_settings() -> Settings {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

fn save_settings(settings: &Settings) {
    let path = settings_path();
    if let Ok(data) = serde_json::to_string_pretty(settings) {
        std::fs::write(&path, data).ok();
    }
}

// ---------------------------------------------------------------------------
// Grid square / distance helpers
// ---------------------------------------------------------------------------

/// Convert a Maidenhead grid square (4 or 6 char) to (lat, lon) in degrees
fn grid_to_latlon(grid: &str) -> Option<(f64, f64)> {
    let g: Vec<char> = grid.to_uppercase().chars().collect();
    if g.len() < 4 {
        return None;
    }
    if g[0] < 'A' || g[0] > 'R' || g[1] < 'A' || g[1] > 'R' {
        return None;
    }
    let lon = (g[0] as i32 - 'A' as i32) as f64 * 20.0 - 180.0
        + (g[2] as i32 - '0' as i32) as f64 * 2.0;
    let lat = (g[1] as i32 - 'A' as i32) as f64 * 10.0 - 90.0
        + (g[3] as i32 - '0' as i32) as f64 * 1.0;

    let (lon, lat) = if g.len() >= 6 && g[4].is_ascii_alphabetic() && g[5].is_ascii_alphabetic() {
        (
            lon + (g[4] as i32 - 'A' as i32) as f64 * (2.0 / 24.0) + (1.0 / 24.0),
            lat + (g[5] as i32 - 'A' as i32) as f64 * (1.0 / 24.0) + (0.5 / 24.0),
        )
    } else {
        (lon + 1.0, lat + 0.5)
    };

    Some((lat, lon))
}

/// Haversine distance in miles between two (lat, lon) points
fn haversine_miles(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 3958.8; // Earth radius in miles
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
}

/// Bearing in degrees from point 1 to point 2
fn bearing_degrees(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let lat1 = lat1.to_radians();
    let lat2 = lat2.to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Calculate distance and bearing string from my grid to a spot's grid
fn distance_bearing(my_grid: &str, spot_grid: &str) -> Option<String> {
    let (lat1, lon1) = grid_to_latlon(my_grid)?;
    let (lat2, lon2) = grid_to_latlon(spot_grid)?;
    let dist = haversine_miles(lat1, lon1, lat2, lon2);
    let brg = bearing_degrees(lat1, lon1, lat2, lon2);
    Some(format!("{:.0}mi {:.0}°", dist, brg))
}

/// Parse a spot time string and return age in minutes.
/// Handles formats: "2025-03-16T14:23:00", "14:23", etc.
fn spot_age_minutes(time_str: &str) -> i64 {
    let time_str = time_str.trim();
    if time_str.is_empty() || time_str == "-" {
        return 999;
    }

    let now = chrono::Utc::now();

    // Try full ISO datetime "2025-03-16T14:23:00"
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S") {
        let utc_dt = dt.and_utc();
        return (now - utc_dt).num_minutes();
    }

    // Try with fractional seconds
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(
        &time_str[..19.min(time_str.len())],
        "%Y-%m-%dT%H:%M:%S",
    ) {
        let utc_dt = dt.and_utc();
        return (now - utc_dt).num_minutes();
    }

    // Try HH:MM format (assume today UTC)
    if time_str.len() >= 4 && time_str.len() <= 5 {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(time_str, "%H:%M") {
            let today = now.date_naive();
            let dt = today.and_time(t).and_utc();
            let mins = (now - dt).num_minutes();
            // If negative (future), assume yesterday
            return if mins < -10 { mins + 1440 } else { mins };
        }
    }

    999
}

/// Format spot age as a human-readable string
fn spot_age_str(time_str: &str) -> String {
    let mins = spot_age_minutes(time_str);
    if mins >= 999 || mins < 0 {
        return "??".to_string();
    }
    if mins < 1 {
        "now".to_string()
    } else if mins < 60 {
        format!("{}m", mins)
    } else if mins < 1440 {
        format!("{}h{}m", mins / 60, mins % 60)
    } else {
        format!("{}d", mins / 1440)
    }
}

// ---------------------------------------------------------------------------
// Helpers: derive band from frequency string
// ---------------------------------------------------------------------------

fn freq_to_band(freq_str: &str) -> String {
    let freq_khz: f64 = match freq_str.parse::<f64>() {
        Ok(f) => f,
        Err(_) => return "??".to_string(),
    };

    // POTA API returns frequency in kHz (e.g. "14074.0")
    // but some spots use MHz (e.g. "14.074") – normalise
    let freq_khz = if freq_khz < 1000.0 {
        freq_khz * 1000.0
    } else {
        freq_khz
    };

    match freq_khz as u64 {
        1800..=2000 => "160m".to_string(),
        3500..=4000 => "80m".to_string(),
        5330..=5410 => "60m".to_string(),
        7000..=7300 => "40m".to_string(),
        10100..=10150 => "30m".to_string(),
        14000..=14350 => "20m".to_string(),
        18068..=18168 => "17m".to_string(),
        21000..=21450 => "15m".to_string(),
        24890..=24990 => "12m".to_string(),
        28000..=29700 => "10m".to_string(),
        50000..=54000 => "6m".to_string(),
        144000..=148000 => "2m".to_string(),
        420000..=450000 => "70cm".to_string(),
        _ => "??".to_string(),
    }
}

/// Extract the country/entity prefix from locationDesc (e.g. "US-FL" -> "US")
fn location_to_country(loc: &str) -> String {
    if let Some(idx) = loc.find('-') {
        loc[..idx].to_string()
    } else {
        loc.to_string()
    }
}

/// Convert POTA frequency string to MHz for N3FJP
fn freq_to_mhz(freq_str: &str) -> String {
    let val: f64 = match freq_str.parse::<f64>() {
        Ok(f) => f,
        Err(_) => return freq_str.to_string(),
    };
    if val > 1000.0 {
        // Already in kHz, convert to MHz
        format!("{:.4}", val / 1000.0)
    } else {
        // Already MHz
        format!("{:.4}", val)
    }
}

// ---------------------------------------------------------------------------
// N3FJP AC Log TCP client
// ---------------------------------------------------------------------------

struct N3fjpClient {
    host: String,
    port: u16,
}

impl N3fjpClient {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
        }
    }

    fn send_command(&self, cmd: &str) -> Result<String, String> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut stream = TcpStream::connect_timeout(
            &addr.parse().map_err(|e| format!("Invalid address: {}", e))?,
            Duration::from_secs(3),
        )
        .map_err(|e| format!("Connection failed: {}", e))?;

        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok();

        stream
            .write_all(cmd.as_bytes())
            .map_err(|e| format!("Write failed: {}", e))?;
        stream
            .flush()
            .map_err(|e| format!("Flush failed: {}", e))?;

        // Brief pause then read response
        std::thread::sleep(Duration::from_millis(200));

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let response = String::from_utf8_lossy(&buf[..n]).to_string();
        Ok(response)
    }

    /// Change the radio frequency via AC Log rig control
    fn change_freq(&self, freq_mhz: &str) -> Result<String, String> {
        let cmd = format!(
            "<CMD><CHANGEFREQ><VALUE>{}</VALUE></CMD>",
            freq_mhz
        );
        self.send_command(&cmd)
    }

    /// Populate call, freq, mode in AC Log and tune the radio.
    ///
    /// Uses two N3FJP API commands:
    ///  1. QSOINPROGRESS (API v1.7+) — fills in call, band, mode, freq fields
    ///  2. CHANGEFREQ — actually tunes the connected radio via rig control
    fn tune_to_spot(&self, spot: &PotaSpot) -> Result<String, String> {
        let freq_mhz = freq_to_mhz(&spot.frequency);
        let band = freq_to_band(&spot.frequency);

        // Map band string to N3FJP band number (just digits, no "m")
        let band_num = band.trim_end_matches('m');

        // Map mode to N3FJP contest mode
        let mode_upper = spot.mode.to_uppercase();
        let mode_val = match mode_upper.as_str() {
            "SSB" | "USB" | "LSB" | "AM" | "FM" => "SSB",
            "CW" => "CW",
            "FT8" | "FT4" | "RTTY" | "PSK31" | "PSK63" | "JS8"
            | "DIGITALVOICE" | "OLIVIA" | "MFSK" => "FT8",
            other => other,
        };

        // Step 1: Fill in QSO fields via QSOINPROGRESS (API v1.7+)
        let qso_cmd = format!(
            "<CMD><QSOINPROGRESS><CALL>{call}</CALL><BAND>{band}</BAND>\
             <MODE>{mode}</MODE><FREQ>{freq}</FREQ></CMD>",
            call = spot.activator,
            band = band_num,
            mode = mode_val,
            freq = freq_mhz,
        );
        self.send_command(&qso_cmd)?;

        // Delay between commands per API docs (minimum 5ms)
        std::thread::sleep(Duration::from_millis(50));

        // Step 2: Tune the radio via rig control
        let result = self.change_freq(&freq_mhz)?;

        Ok(result)
    }

    /// Log the current QSO via UPDATEANDLOG (API v1.7+).
    /// This tells AC Log to save the QSO with the data currently in the entry fields.
    fn log_qso(&self, spot: &PotaSpot) -> Result<String, String> {
        let freq_mhz = freq_to_mhz(&spot.frequency);
        let band = freq_to_band(&spot.frequency);
        let band_num = band.trim_end_matches('m');

        let mode_upper = spot.mode.to_uppercase();
        let mode_val = match mode_upper.as_str() {
            "SSB" | "USB" | "LSB" | "AM" | "FM" => "SSB",
            "CW" => "CW",
            "FT8" | "FT4" | "RTTY" | "PSK31" | "PSK63" | "JS8"
            | "DIGITALVOICE" | "OLIVIA" | "MFSK" => "FT8",
            other => other,
        };

        let now = chrono::Utc::now();
        let date = now.format("%Y/%m/%d").to_string();
        let time_on = now.format("%H:%M").to_string();

        let _comment = if !spot.reference.is_empty() {
            format!("{} {}", spot.reference, spot.display_park_name())
        } else {
            String::new()
        };

        let cmd = format!(
            "<CMD><UPDATEANDLOG><CALL>{call}</CALL><BAND>{band}</BAND>\
             <MODE>{mode}</MODE><FREQ>{freq}</FREQ>\
             <RSTR>599</RSTR><RSTS>599</RSTS>\
             <DATE>{date}</DATE><TIMEON>{time}</TIMEON></CMD>",
            call = spot.activator,
            band = band_num,
            mode = mode_val,
            freq = freq_mhz,
            date = date,
            time = time_on,
        );
        self.send_command(&cmd)
    }

    /// Look up DXCC country for a callsign.
    /// Returns (country_name, dxcc_number) or error.
    fn country_lookup(&self, call: &str) -> Result<(String, String), String> {
        let cmd = format!(
            "<CMD><COUNTRYLISTLOOKUP><CALL>{}</CALL></CMD>",
            call
        );
        let resp = self.send_command(&cmd)?;
        let country = parse_xml_tag(&resp, "COUNTRY").unwrap_or_default();
        let dxcc = parse_xml_tag(&resp, "DXCC").unwrap_or_default();
        if country.is_empty() {
            Err("Country not found".to_string())
        } else {
            Ok((country, dxcc))
        }
    }

    /// Check ATNO (All Time New One) status for a country on a given band/mode.
    /// Returns one of: "ATNO", "OW", "OC", "OWBMW", "OCBMW", "BMC"
    fn check_atno(&self, country: &str, band: &str, mode: &str) -> Result<String, String> {
        let mode_contest = match mode.to_uppercase().as_str() {
            "SSB" | "USB" | "LSB" | "AM" | "FM" => "PH",
            "CW" => "CW",
            _ => "DIG",
        };
        let cmd = format!(
            "<CMD><ATNO><BAND>{}</BAND><MODE>{}</MODE><COUNTRYWORKED>{}</COUNTRYWORKED></CMD>",
            band.trim_end_matches('m'), mode_contest, country
        );
        let resp = self.send_command(&cmd)?;
        let value = parse_xml_tag(&resp, "VALUE").unwrap_or_default();
        if value.is_empty() {
            Err("No ATNO response".to_string())
        } else {
            Ok(value)
        }
    }
}

// ---------------------------------------------------------------------------
// N3FJP event listener (background thread)
// ---------------------------------------------------------------------------

/// Parse a single XML-style tag value from an N3FJP record string.
/// e.g. parse_xml_tag("<CALL>W1AW</CALL><BAND>20</BAND>", "CALL") -> Some("W1AW")
fn parse_xml_tag(record: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag.to_uppercase());
    let close = format!("</{}>", tag.to_uppercase());
    let upper = record.to_uppercase();
    let start = upper.find(&open)? + open.len();
    let end = upper[start..].find(&close)? + start;
    Some(record[start..end].to_string())
}

/// Background listener that maintains a persistent TCP connection to N3FJP
/// AC Log and watches for ENTEREVENT notifications (fired when user logs a QSO).
/// Logged callsigns are pushed into the shared queue for the UI thread to process.
fn n3fjp_listener_thread(
    host: String,
    port: u16,
    running: Arc<AtomicBool>,
    connected: Arc<Mutex<bool>>,
    logged_calls: Arc<Mutex<Vec<String>>>,
) {
    let addr = format!("{}:{}", host, port);
    let sock_addr = match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => a,
        Err(_) => {
            *connected.lock().unwrap() = false;
            running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Connect
    let mut stream = match TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(_) => {
            *connected.lock().unwrap() = false;
            running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Use a short read timeout so we can check the running flag periodically
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();

    // Subscribe to enter/tab events from N3FJP
    let subscribe_cmd = "<CMD><CALLTABENTEREVENTS><VALUE>TRUE</VALUE></CMD>";
    if stream.write_all(subscribe_cmd.as_bytes()).is_err() {
        *connected.lock().unwrap() = false;
        running.store(false, Ordering::SeqCst);
        return;
    }
    let _ = stream.flush();

    *connected.lock().unwrap() = true;

    let mut buffer = String::new();
    let mut read_buf = [0u8; 4096];

    while running.load(Ordering::SeqCst) {
        match stream.read(&mut read_buf) {
            Ok(0) => {
                // Connection closed by server
                break;
            }
            Ok(n) => {
                buffer.push_str(&String::from_utf8_lossy(&read_buf[..n]));

                // Process complete <CMD>...</CMD> messages from the buffer
                loop {
                    let upper_buf = buffer.to_uppercase();
                    let start = match upper_buf.find("<CMD>") {
                        Some(s) => s,
                        None => {
                            buffer.clear();
                            break;
                        }
                    };
                    let end = match upper_buf[start + 5..].find("</CMD>") {
                        Some(e) => start + 5 + e,
                        None => break, // Incomplete message, wait for more data
                    };

                    let record = buffer[start + 5..end].to_string();
                    buffer = buffer[end + 6..].to_string();

                    // Check if this is an ENTEREVENT (user logged a QSO)
                    if record.to_uppercase().contains("<ENTEREVENT>") {
                        if let Some(call) = parse_xml_tag(&record, "CALL") {
                            let call = call.trim().to_uppercase();
                            if !call.is_empty() {
                                logged_calls.lock().unwrap().push(call);
                            }
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // Normal timeout — just loop and check running flag
                continue;
            }
            Err(_) => {
                // Connection error
                break;
            }
        }
    }

    *connected.lock().unwrap() = false;
    running.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// DX Cluster telnet client (background thread)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DxCachedSpot {
    spot: PotaSpot,
    received_at: Instant,
}

struct DxClusterState {
    cached_spots: Vec<DxCachedSpot>,
    connected: bool,
    status: String,
    needs_refresh: bool,
}

/// Parse a DX Spider format spot line into a PotaSpot.
/// Handles two formats:
/// Live:    "DX de W3LPL:     14195.0  5B4AHL       CW 15 dB 26 WPM CQ          1523Z"
/// History: "14195.0  5B4AHL       15-Mar-2025 1523Z  CW 15 dB 26 WPM CQ        <W3LPL>"
fn parse_dx_spot_line(line: &str) -> Option<PotaSpot> {
    let line = line.trim();

    // Try live format first: "DX de SPOTTER: FREQ CALL ..."
    if line.to_uppercase().starts_with("DX DE ") {
        return parse_dx_live_format(line);
    }

    // Try history format: "FREQ CALL DATE TIMEZ COMMENT <SPOTTER>"
    parse_dx_history_format(line)
}

fn parse_dx_live_format(line: &str) -> Option<PotaSpot> {
    let rest = &line[6..];
    let colon_pos = rest.find(':')?;
    let spotter = rest[..colon_pos].trim().to_string();
    let after_colon = rest[colon_pos + 1..].trim_start();

    let tokens: Vec<&str> = after_colon.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }

    let freq_str = tokens[0];
    let dx_call = tokens[1].to_string();
    let _freq_test: f64 = freq_str.parse().ok()?;

    let mut time_str = String::new();
    let mut comment_tokens = &tokens[2..];
    if let Some(last) = tokens.last() {
        if last.len() == 5 && last.ends_with('Z') && last[..4].chars().all(|c| c.is_ascii_digit()) {
            time_str = format!("{}:{}", &last[..2], &last[2..4]);
            comment_tokens = &tokens[2..tokens.len() - 1];
        }
    }

    let comment = comment_tokens.join(" ");
    let mode = guess_mode_from_comment_and_freq(&comment, freq_str);

    Some(PotaSpot {
        spot_id: None,
        activator: dx_call,
        frequency: freq_str.to_string(),
        mode,
        reference: String::new(),
        park_name: None,
        spot_time: if time_str.is_empty() { None } else { Some(time_str) },
        spotter: Some(spotter),
        comments: if comment.is_empty() { None } else { Some(comment) },
        source: Some("DXCluster".to_string()),
        name: None,
        location_desc: None,
        grid4: None,
        grid6: None,
    })
}

fn parse_dx_history_format(line: &str) -> Option<PotaSpot> {
    // Format: "14195.0  5B4AHL  15-Mar-2025 1523Z  CW 15 dB 26 WPM CQ  <W3LPL>"
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    let freq_str = tokens[0];
    let _freq_test: f64 = freq_str.parse().ok()?;
    let dx_call = tokens[1].to_string();

    // Look for spotter in angle brackets at end
    let spotter = if let Some(last) = tokens.last() {
        if last.starts_with('<') && last.ends_with('>') {
            Some(last[1..last.len() - 1].to_string())
        } else {
            None
        }
    } else {
        None
    };

    // Find time: look for a token matching HHMMz pattern
    let mut time_str = String::new();
    for token in &tokens[2..] {
        if token.len() == 5 && token.ends_with('Z') && token[..4].chars().all(|c| c.is_ascii_digit()) {
            time_str = format!("{}:{}", &token[..2], &token[2..4]);
            break;
        }
    }

    // Collect middle tokens as comment, excluding freq, call, date-like, time, spotter
    let comment: String = tokens[2..]
        .iter()
        .filter(|t| {
            // Skip date tokens (e.g. "15-Mar-2025"), time tokens, and spotter
            !(t.starts_with('<') && t.ends_with('>'))
                && !(t.len() == 5 && t.ends_with('Z') && t[..4].chars().all(|c| c.is_ascii_digit()))
                && !(t.contains('-') && t.len() > 8 && t.chars().any(|c| c.is_ascii_alphabetic()))
        })
        .copied()
        .collect::<Vec<&str>>()
        .join(" ");

    let mode = guess_mode_from_comment_and_freq(&comment, freq_str);

    Some(PotaSpot {
        spot_id: None,
        activator: dx_call,
        frequency: freq_str.to_string(),
        mode,
        reference: String::new(),
        park_name: None,
        spot_time: if time_str.is_empty() { None } else { Some(time_str) },
        spotter,
        comments: if comment.is_empty() { None } else { Some(comment) },
        source: Some("DXCluster".to_string()),
        name: None,
        location_desc: None,
        grid4: None,
        grid6: None,
    })
}

fn guess_mode_from_comment_and_freq(comment: &str, freq_str: &str) -> String {
    let cu = comment.to_uppercase();
    if cu.contains("FT8") {
        "FT8".to_string()
    } else if cu.contains("FT4") {
        "FT4".to_string()
    } else if cu.contains("RTTY") {
        "RTTY".to_string()
    } else if cu.contains("PSK") {
        "PSK".to_string()
    } else if cu.contains("CW") || cu.contains("WPM") {
        "CW".to_string()
    } else if cu.contains("SSB") {
        "SSB".to_string()
    } else {
        let fkhz: f64 = freq_str.parse().unwrap_or(0.0);
        let band_lower = [1800.0, 3500.0, 7000.0, 10100.0, 14000.0, 18068.0, 21000.0, 24890.0, 28000.0];
        let cw_upper = [1840.0, 3600.0, 7040.0, 10130.0, 14070.0, 18095.0, 21070.0, 24915.0, 28070.0];
        let mut guessed = "SSB";
        for (lo, cu_f) in band_lower.iter().zip(cw_upper.iter()) {
            if fkhz >= *lo && fkhz < *cu_f {
                guessed = "CW";
                break;
            }
        }
        guessed.to_string()
    }
}

/// Background thread: connects to DX cluster nodes, logs in, and streams spots
fn dx_cluster_thread(
    nodes: Vec<(String, u16)>,
    callsign: String,
    running: Arc<AtomicBool>,
    dx_state: Arc<Mutex<DxClusterState>>,
) {
    if callsign.trim().is_empty() {
        let mut st = dx_state.lock().unwrap();
        st.status = "No callsign configured".to_string();
        st.connected = false;
        running.store(false, Ordering::SeqCst);
        return;
    }

    if nodes.is_empty() {
        let mut st = dx_state.lock().unwrap();
        st.status = "No nodes configured".to_string();
        st.connected = false;
        running.store(false, Ordering::SeqCst);
        return;
    }

    let mut node_idx = 0;

    // Outer loop: cycle through nodes on disconnect/failure
    while running.load(Ordering::SeqCst) {
        let (ref host, port) = nodes[node_idx];
        let addr = format!("{}:{}", host, port);

        {
            let mut st = dx_state.lock().unwrap();
            st.status = format!("Connecting to {} ({}/{})...", addr, node_idx + 1, nodes.len());
            st.connected = false;
        }

        // Try to connect to this node
        let resolved: Vec<std::net::SocketAddr> = match addr.as_str().to_socket_addrs() {
            Ok(addrs) => addrs.collect(),
            Err(e) => {
                let mut st = dx_state.lock().unwrap();
                st.status = format!("DNS failed {}: {} - trying next node...", addr, e);
                node_idx = (node_idx + 1) % nodes.len();
                drop(st);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        let mut stream: Option<TcpStream> = None;
        for sock_addr in resolved {
            match TcpStream::connect_timeout(&sock_addr, Duration::from_secs(8)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    let mut st = dx_state.lock().unwrap();
                    st.status = format!("Failed {}: {} - trying next...", addr, e);
                }
            }
        }

        let mut stream = match stream {
            Some(s) => s,
            None => {
                let mut st = dx_state.lock().unwrap();
                st.status = format!("Failed {} - moving to next node...", addr);
                node_idx = (node_idx + 1) % nodes.len();
                drop(st);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(3))).ok();

        // Login
        std::thread::sleep(Duration::from_millis(500));
        let login = format!("{}\r\n", callsign.trim());
        if stream.write_all(login.as_bytes()).is_err() {
            let mut st = dx_state.lock().unwrap();
            st.status = format!("Login failed on {} - trying next...", addr);
            node_idx = (node_idx + 1) % nodes.len();
            drop(st);
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let _ = stream.flush();

        // Request recent spots
        std::thread::sleep(Duration::from_millis(1500));
        let _ = stream.write_all(b"sh/dx 50\r\n");
        let _ = stream.flush();

        {
            let mut st = dx_state.lock().unwrap();
            st.connected = true;
            st.status = format!("Connected to {} (fetching recent spots)", addr);
        }

        // Read loop for this node
        let mut buf = [0u8; 4096];
        let mut line_buf = String::new();
        let mut initial_batch_done = false;
        let connect_time = Instant::now();

        loop {
            if !running.load(Ordering::SeqCst) {
                break;
            }

            match stream.read(&mut buf) {
                Ok(0) => {
                    // Connection closed by server
                    let mut st = dx_state.lock().unwrap();
                    st.connected = false;
                    st.status = format!("Disconnected from {} - trying next node...", addr);
                    break;
                }
                Ok(n) => {
                    line_buf.push_str(&String::from_utf8_lossy(&buf[..n]));

                    while let Some(nl_pos) = line_buf.find('\n') {
                        let line = line_buf[..nl_pos].trim_end_matches('\r').to_string();
                        line_buf = line_buf[nl_pos + 1..].to_string();

                        if let Some(spot) = parse_dx_spot_line(&line) {
                            let cached = DxCachedSpot {
                                spot,
                                received_at: Instant::now(),
                            };
                            let mut st = dx_state.lock().unwrap();
                            st.cached_spots.push(cached);
                            if let Some(cutoff) = Instant::now().checked_sub(Duration::from_secs(15 * 60)) {
                                st.cached_spots.retain(|s| s.received_at > cutoff);
                            }
                        }
                    }

                    if !initial_batch_done && connect_time.elapsed() > Duration::from_secs(4) {
                        initial_batch_done = true;
                        let mut st = dx_state.lock().unwrap();
                        st.needs_refresh = true;
                        st.status = format!("Connected to {}", addr);
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    continue;
                }
                Err(e) => {
                    let mut st = dx_state.lock().unwrap();
                    st.connected = false;
                    st.status = format!("Connection lost to {}: {} - trying next node...", addr, e);
                    break;
                }
            }
        }

        // If we broke out of the read loop but are still running, try next node
        if running.load(Ordering::SeqCst) {
            node_idx = (node_idx + 1) % nodes.len();
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    {
        let mut st = dx_state.lock().unwrap();
        st.connected = false;
        st.status = "Stopped".to_string();
    }
    running.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SpotEntry {
    spot: PotaSpot,
    spot_type: SpotType,
    band: String,
    country: String,
    hunted: bool,
    is_qrt: bool,
    not_heard: bool,
    dxcc_country: Option<String>,
    /// ATNO status from N3FJP: "ATNO", "OW", "OC", "OWBMW", "OCBMW", "BMC"
    atno_status: Option<String>,
}

impl SpotEntry {
    /// Generate a consistent unique key for this spot entry.
    fn spot_key(&self) -> String {
        make_spot_key(self.spot_type, &self.spot.activator, &self.spot.reference)
    }
}

/// Standalone key function usable before SpotEntry is constructed
fn make_spot_key(spot_type: SpotType, activator: &str, reference: &str) -> String {
    match spot_type {
        SpotType::Pota | SpotType::Sota | SpotType::Wwff => format!("{}-{}", activator, reference),
        SpotType::Dx => format!("DX-{}", activator),
    }
}

struct AppState {
    spots: Vec<SpotEntry>,
    last_fetch: Option<Instant>,
    fetch_error: Option<String>,
    is_fetching: bool,
}

// ---------------------------------------------------------------------------
// Main application
// ---------------------------------------------------------------------------

struct PotaHunterApp {
    // State shared with background fetch thread
    state: Arc<Mutex<AppState>>,

    // N3FJP settings
    n3fjp_host: String,
    n3fjp_port: String,
    n3fjp_status: String,
    n3fjp_status_snapshot: String,        // previous value for change detection
    n3fjp_status_changed_at: Option<Instant>, // when status last changed (for timeout)

    // N3FJP event listener (auto-hunt on log)
    n3fjp_listener_running: Arc<AtomicBool>,
    n3fjp_listener_connected: Arc<Mutex<bool>>,
    n3fjp_logged_calls: Arc<Mutex<Vec<String>>>,

    // DX Cluster settings
    dx_callsign: String,
    dx_nodes: String,  // newline-separated "host:port" list
    dx_auto_start: bool,
    dx_running: Arc<AtomicBool>,
    dx_state: Arc<Mutex<DxClusterState>>,

    // Filters (multi-select for band, mode, and type)
    filter_bands: HashSet<String>,
    filter_modes: HashSet<String>,
    filter_types: HashSet<SpotType>,
    filter_country: String,
    filter_callsign: String,
    hide_hunted: bool,
    hide_dupes: bool,
    hide_qrt: bool,
    max_age_mins: i64,  // 0 = no limit

    // Available filter options (populated from spots)
    available_bands: Vec<String>,
    available_modes: Vec<String>,
    available_countries: Vec<String>,

    // Auto-refresh
    auto_refresh: bool,
    refresh_interval_secs: u32,

    // Hunted callsigns (persisted in memory for session)
    hunted_set: HashSet<String>,

    // Not-heard spots (keyed on activator-reference)
    not_heard_set: HashSet<String>,

    // Last tuned spot key (activator-reference)
    last_tuned_key: Option<String>,

    // DXCC / ATNO lookup caches
    dxcc_cache: HashMap<String, (String, String)>,   // callsign -> (country, dxcc_num)
    atno_cache: HashMap<String, String>,             // "country|band|mode" -> ATNO status
    dxcc_lookup_running: Arc<AtomicBool>,
    dxcc_pending: Arc<Mutex<Vec<(String, String, String)>>>,         // (call, band, mode)
    dxcc_results: Arc<Mutex<Vec<(String, String, String, String)>>>,  // (cache_key, call, country, status)
    atno_alert_count: usize,  // count of ATNO/new band-mode spots

    // Grid square for distance/bearing
    my_grid: String,

    // Dark mode
    dark_mode: bool,

    // Sort state
    sort_column: SortColumn,
    sort_ascending: bool,

    // UI state
    first_frame: bool,  // re-apply saved theme on frame 1 to override system theme
    show_settings: bool,
    show_filters: bool,
    search_focus_requested: bool,
    selected_spot_key: Option<String>,
    selected_row_idx: Option<usize>, // exact row in filtered list — unique even for duplicate keys
    scroll_to_selected: bool,
    // Filtered spot keys in display order (for keyboard navigation)
    filtered_keys: Vec<String>,
    // Context menu state (rendered outside grid to avoid extra cells)
    ctx_menu_row: Option<usize>,           // filtered row index
    ctx_menu_original_idx: Option<usize>,  // original spots index
    ctx_menu_spot: Option<PotaSpot>,       // cloned spot data
    ctx_menu_spot_type: Option<SpotType>,
    ctx_menu_spot_key: Option<String>,
    ctx_menu_pos: Option<egui::Pos2>,
    logo_texture: Option<egui::TextureHandle>,
}

#[derive(Clone, Copy, PartialEq)]
enum SortColumn {
    Type,
    Activator,
    Frequency,
    Mode,
    Band,
    Reference,
    Park,
    Location,
    Comment,
    Distance,
    SpotTime,
}

impl Default for PotaHunterApp {
    fn default() -> Self {
        let s = load_settings();
        Self {
            state: Arc::new(Mutex::new(AppState {
                spots: Vec::new(),
                last_fetch: None,
                fetch_error: None,
                is_fetching: false,
            })),
            n3fjp_host: s.n3fjp_host,
            n3fjp_port: s.n3fjp_port,
            n3fjp_status: String::new(),
            n3fjp_status_snapshot: String::new(),
            n3fjp_status_changed_at: None,
            n3fjp_listener_running: Arc::new(AtomicBool::new(false)),
            n3fjp_listener_connected: Arc::new(Mutex::new(false)),
            n3fjp_logged_calls: Arc::new(Mutex::new(Vec::new())),
            dx_callsign: s.dx_callsign,
            dx_nodes: s.dx_nodes,
            dx_auto_start: s.dx_auto_start,
            dx_running: Arc::new(AtomicBool::new(false)),
            dx_state: Arc::new(Mutex::new(DxClusterState {
                cached_spots: Vec::new(),
                connected: false,
                status: "Not started".to_string(),
                needs_refresh: false,
            })),
            filter_bands: HashSet::new(),
            filter_modes: HashSet::new(),
            filter_types: HashSet::new(),
            filter_country: "All".to_string(),
            filter_callsign: String::new(),
            hide_hunted: false,
            hide_dupes: s.hide_dupes,
            hide_qrt: s.hide_qrt,
            max_age_mins: s.max_age_mins,
            available_bands: Vec::new(),
            available_modes: Vec::new(),
            available_countries: vec!["All".to_string()],
            auto_refresh: s.auto_refresh,
            refresh_interval_secs: s.refresh_interval_secs,
            hunted_set: HashSet::new(),
            not_heard_set: HashSet::new(),
            last_tuned_key: None,
            dxcc_cache: HashMap::new(),
            atno_cache: HashMap::new(),
            dxcc_lookup_running: Arc::new(AtomicBool::new(false)),
            dxcc_pending: Arc::new(Mutex::new(Vec::new())),
            dxcc_results: Arc::new(Mutex::new(Vec::new())),
            atno_alert_count: 0,
            my_grid: s.my_grid,
            dark_mode: s.dark_mode,
            sort_column: SortColumn::SpotTime,
            sort_ascending: false,
            first_frame: true,
            show_settings: false,
            show_filters: true,
            search_focus_requested: false,
            selected_spot_key: None,
            selected_row_idx: None,
            scroll_to_selected: false,
            filtered_keys: Vec::new(),
            ctx_menu_row: None,
            ctx_menu_original_idx: None,
            ctx_menu_spot: None,
            ctx_menu_spot_type: None,
            ctx_menu_spot_key: None,
            ctx_menu_pos: None,
            logo_texture: None,
        }
    }
}

impl PotaHunterApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Load settings early to know dark mode preference
        let saved_settings = load_settings();

        // Apply dark/light mode FIRST
        cc.egui_ctx.set_visuals(if saved_settings.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        // Clone style AFTER visuals are set so we inherit the correct theme
        let mut style = (*cc.egui_ctx.style()).clone();
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(16.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(16.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(15.0, egui::FontFamily::Monospace),
        );
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.interaction.tooltip_delay = 0.5;
        cc.egui_ctx.set_style(style);

        // Load Open Sans SemiBold as the primary UI font
        let mut fonts = egui::FontDefinitions::default();
        let semibold_bytes = include_bytes!("../Open Sans font/static/OpenSans-SemiBold.ttf");
        fonts.font_data.insert(
            "opensans_semibold".to_string(),
            std::sync::Arc::new(egui::FontData::from_static(semibold_bytes)),
        );
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "opensans_semibold".to_string());
        }

        // Load Consolas as the primary monospace font (frequency readouts, age column)
        let consolas_paths = [
            "C:\\Windows\\Fonts\\consola.ttf",   // Consolas Regular
            "C:\\Windows\\Fonts\\lucon.ttf",      // Lucida Console fallback
        ];
        for path in &consolas_paths {
            if let Ok(font_data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "consolas".to_string(),
                    std::sync::Arc::new(egui::FontData::from_owned(font_data)),
                );
                if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                    family.insert(0, "consolas".to_string());
                }
                break;
            }
        }

        // Load emoji font (Segoe UI Emoji on Windows) as fallback
        let emoji_paths = [
            "C:\\Windows\\Fonts\\seguiemj.ttf",  // Windows Segoe UI Emoji
            "C:\\Windows\\Fonts\\segoeui.ttf",    // Segoe UI (fallback)
        ];
        for path in &emoji_paths {
            if let Ok(font_data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "emoji".to_string(),
                    std::sync::Arc::new(egui::FontData::from_owned(font_data)),
                );
                if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                    family.push("emoji".to_string());
                }
                if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                    family.push("emoji".to_string());
                }
                break;
            }
        }
        // Load a CJK font (Microsoft YaHei) as fallback for Chinese/Japanese/Korean
        // characters. Open Sans and the emoji font have no CJK glyphs, so without
        // this park names like "南京" render as squares.
        let cjk_paths = [
            "C:\\Windows\\Fonts\\msyh.ttc",   // Microsoft YaHei (Simplified Chinese)
            "C:\\Windows\\Fonts\\msyhbd.ttc",  // Microsoft YaHei Bold
            "C:\\Windows\\Fonts\\simsun.ttc",  // SimSun (older Windows fallback)
        ];
        for path in &cjk_paths {
            if let Ok(font_data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "cjk".to_string(),
                    std::sync::Arc::new(egui::FontData::from_owned(font_data)),
                );
                if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                    family.push("cjk".to_string());
                }
                if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                    family.push("cjk".to_string());
                }
                break;
            }
        }

        cc.egui_ctx.set_fonts(fonts);

        let mut app = Self::default();

        // Load logo as texture for display in title bar
        let png_bytes = include_bytes!("../assets/icon_64.png");
        if let Ok(img) = image::load_from_memory(png_bytes) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let pixels: Vec<egui::Color32> = rgba
                .pixels()
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            let color_image = egui::ColorImage {
                size: [w as usize, h as usize],
                pixels,
            };
            app.logo_texture = Some(cc.egui_ctx.load_texture(
                "logo",
                color_image,
                egui::TextureOptions::LINEAR,
            ));
        }

        app.trigger_fetch();

        // Auto-start DX cluster if setting is enabled
        if app.dx_auto_start && !app.dx_callsign.trim().is_empty() {
            app.start_dx_cluster();
        }

        app
    }

    /// Gather current settings and save to disk
    fn save_current_settings(&self) {
        let s = Settings {
            n3fjp_host: self.n3fjp_host.clone(),
            n3fjp_port: self.n3fjp_port.clone(),
            dx_callsign: self.dx_callsign.clone(),
            dx_nodes: self.dx_nodes.clone(),
            dx_auto_start: self.dx_auto_start,
            auto_refresh: self.auto_refresh,
            refresh_interval_secs: self.refresh_interval_secs,
            my_grid: self.my_grid.clone(),
            dark_mode: self.dark_mode,
            hide_dupes: self.hide_dupes,
            hide_qrt: self.hide_qrt,
            max_age_mins: self.max_age_mins,
        };
        save_settings(&s);
    }

    /// Kick off a background thread to fetch spots from POTA API
    fn trigger_fetch(&self) {
        let state = Arc::clone(&self.state);

        {
            let mut s = state.lock().unwrap();
            if s.is_fetching {
                return;
            }
            s.is_fetching = true;
            s.fetch_error = None;
        }

        let hunted = self.hunted_set.clone();
        let not_heard = self.not_heard_set.clone();
        let dx_state = Arc::clone(&self.dx_state);

        std::thread::spawn(move || {
            let pota_result = fetch_pota_spots();
            let sota_result = fetch_sota_spots();
            let wwff_result = fetch_wwff_spots();

            // Grab DX cached spots (already pruned to 15 min by the DX thread)
            let dx_spots: Vec<PotaSpot> = {
                let mut st = dx_state.lock().unwrap();
                if let Some(cutoff) = Instant::now().checked_sub(Duration::from_secs(15 * 60)) {
                    st.cached_spots.retain(|s| s.received_at > cutoff);
                }
                st.cached_spots.iter().map(|s| s.spot.clone()).collect()
            };

            let mut s = state.lock().unwrap();
            s.is_fetching = false;
            s.last_fetch = Some(Instant::now());

            let mut all_entries: Vec<SpotEntry> = Vec::new();

            // Process POTA spots
            match pota_result {
                Ok(spots) => {
                    for spot in spots {
                        let band = freq_to_band(&spot.frequency);
                        let country = location_to_country(
                            spot.location_desc.as_deref().unwrap_or(""),
                        );
                        let key = make_spot_key(SpotType::Pota, &spot.activator, &spot.reference);
                        let hunted_flag = hunted.contains(&key);
                        let not_heard_flag = not_heard.contains(&key);
                        let is_qrt = spot
                            .comments
                            .as_deref()
                            .unwrap_or("")
                            .to_uppercase()
                            .contains("QRT");
                        all_entries.push(SpotEntry {
                            spot,
                            spot_type: SpotType::Pota,
                            band,
                            country,
                            hunted: hunted_flag,
                            is_qrt,
                            not_heard: not_heard_flag,
                            dxcc_country: None,
                            atno_status: None,
                        });
                    }
                }
                Err(ref e) => {
                    s.fetch_error = Some(format!("POTA: {}", e));
                }
            }

            // Process SOTA spots
            match sota_result {
                Ok(spots) => {
                    for sota_spot in spots {
                        let spot = sota_spot.to_pota_spot();
                        let band = freq_to_band(&spot.frequency);
                        let country = location_to_country(
                            spot.location_desc.as_deref().unwrap_or(""),
                        );
                        let key = make_spot_key(SpotType::Sota, &spot.activator, &spot.reference);
                        let hunted_flag = hunted.contains(&key);
                        let not_heard_flag = not_heard.contains(&key);
                        let is_qrt = spot
                            .comments
                            .as_deref()
                            .unwrap_or("")
                            .to_uppercase()
                            .contains("QRT");
                        all_entries.push(SpotEntry {
                            spot,
                            spot_type: SpotType::Sota,
                            band,
                            country,
                            hunted: hunted_flag,
                            is_qrt,
                            not_heard: not_heard_flag,
                            dxcc_country: None,
                            atno_status: None,
                        });
                    }
                }
                Err(ref e) => {
                    if let Some(ref existing) = s.fetch_error {
                        s.fetch_error = Some(format!("{} | SOTA: {}", existing, e));
                    } else {
                        s.fetch_error = Some(format!("SOTA: {}", e));
                    }
                }
            }

            // Process WWFF spots
            match wwff_result {
                Ok(spots) => {
                    for wwff_spot in spots {
                        let spot = wwff_spot.to_pota_spot();
                        let band = freq_to_band(&spot.frequency);
                        let country = location_to_country(
                            spot.location_desc.as_deref().unwrap_or(""),
                        );
                        let key = make_spot_key(SpotType::Wwff, &spot.activator, &spot.reference);
                        let hunted_flag = hunted.contains(&key);
                        let not_heard_flag = not_heard.contains(&key);
                        let is_qrt = spot
                            .comments
                            .as_deref()
                            .unwrap_or("")
                            .to_uppercase()
                            .contains("QRT");
                        all_entries.push(SpotEntry {
                            spot,
                            spot_type: SpotType::Wwff,
                            band,
                            country,
                            hunted: hunted_flag,
                            is_qrt,
                            not_heard: not_heard_flag,
                            dxcc_country: None,
                            atno_status: None,
                        });
                    }
                }
                Err(ref e) => {
                    if let Some(ref existing) = s.fetch_error {
                        s.fetch_error = Some(format!("{} | WWFF: {}", existing, e));
                    } else {
                        s.fetch_error = Some(format!("WWFF: {}", e));
                    }
                }
            }

            // Process DX cluster cached spots
            for spot in dx_spots {
                let band = freq_to_band(&spot.frequency);
                let key = make_spot_key(SpotType::Dx, &spot.activator, &spot.reference);
                let hunted_flag = hunted.contains(&key);
                let not_heard_flag = not_heard.contains(&key);
                let is_qrt = false;
                all_entries.push(SpotEntry {
                    spot,
                    spot_type: SpotType::Dx,
                    band,
                    country: String::new(),
                    hunted: hunted_flag,
                    is_qrt,
                    not_heard: not_heard_flag,
                            dxcc_country: None,
                            atno_status: None,
                });
            }

            s.spots = all_entries;
            if !s.spots.is_empty() {
                s.fetch_error = None;
            }
        });
    }

    /// Rebuild filter option lists from current spots
    fn rebuild_filter_options(&mut self) {
        let state = self.state.lock().unwrap();
        let mut bands: HashSet<String> = HashSet::new();
        let mut modes: HashSet<String> = HashSet::new();
        let mut countries: HashSet<String> = HashSet::new();

        for entry in &state.spots {
            if !entry.band.is_empty() && entry.band != "??" {
                bands.insert(entry.band.clone());
            }
            if !entry.spot.mode.is_empty() {
                modes.insert(entry.spot.mode.to_uppercase());
            }
            if !entry.country.is_empty() {
                countries.insert(entry.country.clone());
            }
        }

        let mut bands: Vec<String> = bands.into_iter().collect();
        bands.sort();
        self.available_bands = bands;

        let mut modes: Vec<String> = modes.into_iter().collect();
        modes.sort();
        self.available_modes = modes;

        let mut countries: Vec<String> = countries.into_iter().collect();
        countries.sort();
        countries.insert(0, "All".to_string());
        self.available_countries = countries;
    }

    /// Get filtered and sorted spots
    fn get_filtered_spots(&self) -> Vec<(usize, SpotEntry)> {
        let state = self.state.lock().unwrap();
        let mut result: Vec<(usize, SpotEntry)> = state
            .spots
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                // Type filter (empty set = show all)
                if !self.filter_types.is_empty() && !self.filter_types.contains(&entry.spot_type) {
                    return false;
                }
                // Band filter (empty set = show all)
                if !self.filter_bands.is_empty() && !self.filter_bands.contains(&entry.band) {
                    return false;
                }
                // Mode filter (empty set = show all)
                if !self.filter_modes.is_empty()
                    && !self.filter_modes.contains(&entry.spot.mode.to_uppercase())
                {
                    return false;
                }
                // Country filter
                if self.filter_country != "All" && entry.country != self.filter_country {
                    return false;
                }
                // Callsign search
                if !self.filter_callsign.is_empty() {
                    let search = self.filter_callsign.to_uppercase();
                    if !entry.spot.activator.to_uppercase().contains(&search)
                        && !entry.spot.reference.to_uppercase().contains(&search)
                        && !entry
                            .spot
                            .display_park_name()
                            .to_uppercase()
                            .contains(&search)
                    {
                        return false;
                    }
                }
                // Hide hunted
                if self.hide_hunted && entry.hunted {
                    return false;
                }
                // Hide QRT
                if self.hide_qrt && entry.is_qrt {
                    return false;
                }
                // Max age filter (0 = no limit)
                if self.max_age_mins > 0 {
                    let age = spot_age_minutes(
                        entry.spot.spot_time.as_deref().unwrap_or(""),
                    );
                    if age > self.max_age_mins && age < 999 {
                        return false;
                    }
                }
                true
            })
            .map(|(i, e)| (i, e.clone()))
            .collect();

        // Sort
        let col = self.sort_column;
        let asc = self.sort_ascending;
        result.sort_by(|(_, a), (_, b)| {
            let ordering = match col {
                SortColumn::Type => {
                    a.spot_type.label().cmp(b.spot_type.label())
                }
                SortColumn::Activator => a.spot.activator.cmp(&b.spot.activator),
                SortColumn::Frequency => {
                    let fa: f64 = a.spot.frequency.parse().unwrap_or(0.0);
                    let fb: f64 = b.spot.frequency.parse().unwrap_or(0.0);
                    fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                }
                SortColumn::Mode => a.spot.mode.cmp(&b.spot.mode),
                SortColumn::Band => a.band.cmp(&b.band),
                SortColumn::Reference => a.spot.reference.cmp(&b.spot.reference),
                SortColumn::Park => {
                    let pa = a.spot.display_park_name();
                    let pb = b.spot.display_park_name();
                    pa.cmp(pb)
                }
                SortColumn::Location => a.country.cmp(&b.country),
                SortColumn::Comment => {
                    let ca = a.spot.comments.as_deref().unwrap_or("");
                    let cb = b.spot.comments.as_deref().unwrap_or("");
                    ca.cmp(cb)
                }
                SortColumn::Distance => {
                    // Sort by distance numerically - can't easily compute here
                    // so just compare grid strings as a proxy
                    let ga = a.spot.grid6.as_deref().or(a.spot.grid4.as_deref()).unwrap_or("");
                    let gb = b.spot.grid6.as_deref().or(b.spot.grid4.as_deref()).unwrap_or("");
                    ga.cmp(gb)
                }
                SortColumn::SpotTime => {
                    let ta = a.spot.spot_time.as_deref().unwrap_or("");
                    let tb = b.spot.spot_time.as_deref().unwrap_or("");
                    ta.cmp(tb)
                }
            };
            let ordering = if asc { ordering } else { ordering.reverse() };
            // Tie-break: newest spot_time first, then activator A→Z.
            // This makes the visible order deterministic even when the
            // underlying state.spots Vec is reshuffled by a data refresh.
            ordering
                .then_with(|| {
                    let ta = a.spot.spot_time.as_deref().unwrap_or("");
                    let tb = b.spot.spot_time.as_deref().unwrap_or("");
                    tb.cmp(ta) // descending — newer spots first on ties
                })
                .then_with(|| a.spot.activator.cmp(&b.spot.activator))
        });

        // Duplicate collapsing: keep only the newest spot per callsign
        if self.hide_dupes {
            let mut seen: HashSet<String> = HashSet::new();
            result.retain(|(_, entry)| {
                let key = entry.spot.activator.to_uppercase();
                seen.insert(key)
            });
        }

        result
    }

    fn toggle_sort(&mut self, col: SortColumn) {
        if self.sort_column == col {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = col;
            self.sort_ascending = true;
        }
    }

    fn sort_indicator(&self, col: SortColumn) -> &str {
        if self.sort_column == col {
            if self.sort_ascending {
                " ▲"
            } else {
                " ▼"
            }
        } else {
            ""
        }
    }

    /// Start the N3FJP event listener in a background thread
    fn start_n3fjp_listener(&mut self) {
        if self.n3fjp_listener_running.load(Ordering::SeqCst) {
            return; // Already running
        }

        let host = self.n3fjp_host.clone();
        let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
        let running = Arc::clone(&self.n3fjp_listener_running);
        let connected = Arc::clone(&self.n3fjp_listener_connected);
        let logged_calls = Arc::clone(&self.n3fjp_logged_calls);

        running.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            n3fjp_listener_thread(host, port, running, connected, logged_calls);
        });
    }

    /// Stop the N3FJP event listener
    fn stop_n3fjp_listener(&mut self) {
        self.n3fjp_listener_running.store(false, Ordering::SeqCst);
    }

    /// Check the logged calls queue and auto-mark matching spots as hunted
    fn process_logged_calls(&mut self) {
        let calls: Vec<String> = {
            let mut queue = self.n3fjp_logged_calls.lock().unwrap();
            if queue.is_empty() {
                return;
            }
            queue.drain(..).collect()
        };

        let mut state = self.state.lock().unwrap();
        for logged_call in &calls {
            let logged_upper = logged_call.to_uppercase();
            // Find all spots matching this callsign and mark them hunted
            for entry in &mut state.spots {
                if entry.spot.activator.to_uppercase() == logged_upper && !entry.hunted {
                    entry.hunted = true;
                    let key = format!("{}-{}", entry.spot.activator, entry.spot.reference);
                    self.hunted_set.insert(key);
                    self.n3fjp_status = format!(
                        "✅ Auto-hunted {} ({})",
                        entry.spot.activator, entry.spot.reference
                    );
                }
            }
        }
    }

    /// Parse the node list string into (host, port) tuples
    fn parse_dx_nodes(&self) -> Vec<(String, u16)> {
        self.dx_nodes
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                let parts: Vec<&str> = line.rsplitn(2, ':').collect();
                if parts.len() == 2 {
                    let port: u16 = parts[0].parse().unwrap_or(23);
                    let host = parts[1].to_string();
                    Some((host, port))
                } else {
                    Some((line.to_string(), 23))
                }
            })
            .collect()
    }

    /// Start the DX cluster listener in a background thread
    fn start_dx_cluster(&mut self) {
        if self.dx_running.load(Ordering::SeqCst) {
            return;
        }

        let nodes = self.parse_dx_nodes();
        if nodes.is_empty() {
            let mut st = self.dx_state.lock().unwrap();
            st.status = "No nodes configured".to_string();
            return;
        }

        let callsign = self.dx_callsign.clone();
        let running = Arc::clone(&self.dx_running);
        let dx_state = Arc::clone(&self.dx_state);

        running.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            dx_cluster_thread(nodes, callsign, running, dx_state);
        });
    }

    /// Stop the DX cluster listener
    fn stop_dx_cluster(&mut self) {
        self.dx_running.store(false, Ordering::SeqCst);
    }

    /// Get current DX spots from cache (pruned to 15 minutes)
    fn get_dx_cache_spots(&self) -> Vec<PotaSpot> {
        let mut st = self.dx_state.lock().unwrap();
        if let Some(cutoff) = Instant::now().checked_sub(Duration::from_secs(15 * 60)) {
            st.cached_spots.retain(|s| s.received_at > cutoff);
        }
        st.cached_spots.iter().map(|s| s.spot.clone()).collect()
    }

    /// Queue unchecked spots for DXCC/ATNO lookup and kick off background thread
    fn queue_dxcc_lookups(&mut self) {
        if self.dxcc_lookup_running.load(Ordering::SeqCst) {
            return;
        }

        let state = self.state.lock().unwrap();
        let mut pending: Vec<(String, String, String)> = Vec::new();
        let mut seen = HashSet::new();

        for entry in state.spots.iter() {
            if entry.atno_status.is_none() && !entry.spot.activator.is_empty() {
                let call = entry.spot.activator.clone();
                let band = entry.band.clone();
                let mode = entry.spot.mode.to_uppercase();
                let cache_key = format!("{}|{}|{}", call, band, mode);
                if !self.atno_cache.contains_key(&cache_key) && seen.insert(cache_key) {
                    pending.push((call, band, mode));
                }
            }
        }
        drop(state);

        if pending.is_empty() {
            return;
        }

        pending.truncate(20);

        *self.dxcc_pending.lock().unwrap() = pending;

        let host = self.n3fjp_host.clone();
        let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
        let running = Arc::clone(&self.dxcc_lookup_running);
        let pending_arc = Arc::clone(&self.dxcc_pending);
        let results_arc = Arc::clone(&self.dxcc_results);

        running.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let client = N3fjpClient::new(&host, port);
            let items: Vec<(String, String, String)> = {
                pending_arc.lock().unwrap().drain(..).collect()
            };

            let mut results: Vec<(String, String, String, String)> = Vec::new();
            let mut call_country: HashMap<String, (String, String)> = HashMap::new();

            for (call, band, mode) in &items {
                let cache_key = format!("{}|{}|{}", call, band, mode);

                let (country, _dxcc) = if let Some(cached) = call_country.get(call) {
                    cached.clone()
                } else {
                    match client.country_lookup(call) {
                        Ok(result) => {
                            call_country.insert(call.clone(), result.clone());
                            result
                        }
                        Err(_) => {
                            results.push((cache_key, call.clone(), String::new(), "ERR".to_string()));
                            continue;
                        }
                    }
                };

                if country.is_empty() {
                    results.push((cache_key, call.clone(), String::new(), "ERR".to_string()));
                    continue;
                }

                match client.check_atno(&country, &band, &mode) {
                    Ok(status) => {
                        results.push((cache_key, call.clone(), country.clone(), status));
                    }
                    Err(_) => {
                        results.push((cache_key, call.clone(), country.clone(), "ERR".to_string()));
                    }
                }

                std::thread::sleep(Duration::from_millis(30));
            }

            *results_arc.lock().unwrap() = results;
            running.store(false, Ordering::SeqCst);
        });
    }

    /// Apply DXCC lookup results from the background thread
    fn apply_dxcc_results(&mut self) {
        // First, consume any new results from the background thread into caches
        {
            let mut r = self.dxcc_results.lock().unwrap();
            if !r.is_empty() {
                for (cache_key, call, country, status) in r.drain(..) {
                    if !country.is_empty() {
                        self.dxcc_cache.insert(call, (country, String::new()));
                    }
                    self.atno_cache.insert(cache_key, status);
                }
            }
        }

        // Now sweep all spots and apply cached results
        let mut state = self.state.lock().unwrap();
        let mut alert_count = 0;

        for entry in &mut state.spots {
            let cache_key = format!(
                "{}|{}|{}",
                entry.spot.activator, entry.band, entry.spot.mode.to_uppercase()
            );

            if let Some(status) = self.atno_cache.get(&cache_key) {
                entry.atno_status = Some(status.clone());
                if let Some((country, _)) = self.dxcc_cache.get(&entry.spot.activator) {
                    entry.dxcc_country = Some(country.clone());
                }
                if status == "ATNO" || status == "OC" || status == "OW" {
                    alert_count += 1;
                }
            }
        }

        self.atno_alert_count = alert_count;
    }
}

/// Fetch spots from the POTA API (blocking)
fn fetch_pota_spots() -> Result<Vec<PotaSpot>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("BaseCamp/1.13")
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let response = client
        .get("https://api.pota.app/spot/activator")
        .send()
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let spots: Vec<PotaSpot> = response
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    Ok(spots)
}

/// Fetch spots from the SOTA API (blocking)
fn fetch_sota_spots() -> Result<Vec<SotaSpot>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("BaseCamp/1.13")
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    // Fetch last 2 hours of spots (negative = hours, max -72)
    let response = client
        .get("https://api2.sota.org.uk/api/spots/-2/all")
        .send()
        .map_err(|e| format!("SOTA request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("SOTA HTTP {}", response.status()));
    }

    let spots: Vec<SotaSpot> = response
        .json()
        .map_err(|e| format!("SOTA JSON parse error: {}", e))?;

    Ok(spots)
}

fn fetch_wwff_spots() -> Result<Vec<WwffSpot>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("BaseCamp/1.13")
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let response = client
        .get("https://spots.wwff.co/static/spots.json")
        .send()
        .map_err(|e| format!("WWFF request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("WWFF HTTP {}", response.status()));
    }

    let spots: Vec<WwffSpot> = response
        .json()
        .map_err(|e| format!("WWFF JSON parse error: {}", e))?;

    Ok(spots)
}

// ---------------------------------------------------------------------------
// egui app implementation
// ---------------------------------------------------------------------------

impl eframe::App for PotaHunterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Re-apply saved dark/light theme on the first frame.
        // eframe 0.31 applies the system theme after new() returns, so set_visuals()
        // in the constructor gets overridden.  Calling it here wins every time.
        if self.first_frame {
            ctx.set_visuals(if self.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            self.first_frame = false;
        }

        // Status message timeout — clear after 5 seconds without touching call sites
        if self.n3fjp_status != self.n3fjp_status_snapshot {
            self.n3fjp_status_snapshot = self.n3fjp_status.clone();
            if !self.n3fjp_status.is_empty() {
                self.n3fjp_status_changed_at = Some(Instant::now());
            }
        }
        if let Some(t) = self.n3fjp_status_changed_at {
            if !self.n3fjp_status.is_empty() && t.elapsed().as_secs() >= 5 {
                self.n3fjp_status.clear();
                self.n3fjp_status_snapshot.clear();
                self.n3fjp_status_changed_at = None;
            }
        }

        // Auto-refresh logic
        if self.auto_refresh {
            let should_refresh = {
                let state = self.state.lock().unwrap();
                match state.last_fetch {
                    Some(t) => t.elapsed() > Duration::from_secs(self.refresh_interval_secs as u64),
                    None => true,
                }
            };
            if should_refresh {
                self.trigger_fetch();
            }
        }

        // Rebuild filter options periodically
        self.rebuild_filter_options();

        // Process any calls logged via N3FJP listener
        self.process_logged_calls();

        // Apply any completed DXCC lookup results
        self.apply_dxcc_results();

        // Queue new DXCC lookups (N3FJP must be running with API enabled)
        if !self.n3fjp_host.is_empty() {
            self.queue_dxcc_lookups();
        }

        // Check if DX cluster has new initial batch ready
        {
            let mut dx_st = self.dx_state.lock().unwrap();
            if dx_st.needs_refresh {
                // Only clear the flag if a fetch can actually start
                let is_fetching = self.state.lock().unwrap().is_fetching;
                if !is_fetching {
                    dx_st.needs_refresh = false;
                    drop(dx_st);
                    self.trigger_fetch();
                }
            }
        }

        // Request repaint for auto-refresh timer
        ctx.request_repaint_after(Duration::from_secs(1));

        // Keyboard shortcuts — capture key states first
        let (key_down, key_up, key_r, key_enter, key_h, key_l, key_n, key_slash, key_esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::R) && !i.modifiers.any(),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::H) && !i.modifiers.any(),
                i.key_pressed(egui::Key::L) && !i.modifiers.any(),
                i.key_pressed(egui::Key::N) && !i.modifiers.any(),
                i.key_pressed(egui::Key::Slash),
                i.key_pressed(egui::Key::Escape),
            )
        });
        // Consume arrow keys so egui's ScrollArea doesn't also scroll in response.
        let typing = ctx.memory(|m| m.focused().is_some());
        if (key_down || key_up) && !typing {
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            });
        }
        // Rebuild filtered_keys so they reflect the current display order.
        self.filtered_keys = self.get_filtered_spots()
            .iter()
            .map(|(_, e)| e.spot_key())
            .collect();

        // Re-anchor selected_row_idx if the spot at that row has changed (e.g. after
        // a data refresh). If the key still matches, leave the index alone so that
        // navigating among duplicate-key rows (multiple DX spots for the same callsign)
        // keeps the correct row highlighted.
        {
            let still_valid = self.selected_row_idx
                .and_then(|i| self.filtered_keys.get(i))
                .map_or(false, |k| Some(k.as_str()) == self.selected_spot_key.as_deref());
            if !still_valid {
                // The previously selected row is gone or out of bounds — find the
                // first row that still carries the same logical spot key.
                self.selected_row_idx = self.selected_spot_key.as_ref()
                    .and_then(|k| self.filtered_keys.iter().position(|fk| fk == k));
            }
        }

        let filtered_count = self.filtered_keys.len();
        if key_down {
            match self.selected_row_idx {
                // Nothing selected yet — jump to the first visible row.
                None if !self.filtered_keys.is_empty() => {
                    self.selected_row_idx = Some(0);
                    self.selected_spot_key = self.filtered_keys.first().cloned();
                    self.scroll_to_selected = true;
                }
                // Move one row down by index — works correctly even when adjacent
                // rows share the same spot_key (e.g. duplicate DX callsigns).
                Some(idx) if idx + 1 < filtered_count => {
                    let new_idx = idx + 1;
                    self.selected_row_idx = Some(new_idx);
                    self.selected_spot_key = Some(self.filtered_keys[new_idx].clone());
                    self.scroll_to_selected = true;
                }
                _ => {}
            }
        }
        if key_up {
            if let Some(idx) = self.selected_row_idx {
                if idx > 0 {
                    let new_idx = idx - 1;
                    self.selected_row_idx = Some(new_idx);
                    self.selected_spot_key = Some(self.filtered_keys[new_idx].clone());
                    self.scroll_to_selected = true;
                }
            }
        }
        if key_r {
            self.trigger_fetch();
        }
        // `/` — focus the search box (ignored if already typing somewhere)
        if key_slash && !typing {
            self.search_focus_requested = true;
            // show the sidebar if hidden so the focus target exists
            self.show_filters = true;
        }
        // Esc — close context menu, close settings, or clear search
        if key_esc {
            if self.ctx_menu_pos.is_some() {
                self.ctx_menu_pos = None;
                self.ctx_menu_spot = None;
            } else if self.show_settings {
                self.show_settings = false;
            } else if !self.filter_callsign.is_empty() {
                self.filter_callsign.clear();
            }
        }

        // Top panel - title bar and controls
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Logo
                if let Some(ref tex) = self.logo_texture {
                    let size = egui::vec2(36.0, 36.0);
                    ui.image((tex.id(), size));
                }
                ui.label(
                    egui::RichText::new("Base Camp")
                        .strong()
                        .size(18.0),
                );
                ui.separator();

                let (is_fetching, last_fetch) = {
                    let state = self.state.lock().unwrap();
                    (state.is_fetching, state.last_fetch)
                };
                let btn_text = if is_fetching { "⏳ Refreshing..." } else { "🔄 Refresh" };
                if ui
                    .add_enabled(!is_fetching, egui::Button::new(btn_text))
                    .on_hover_text("Fetch spots now (R)")
                    .clicked()
                {
                    self.trigger_fetch();
                }
                if !is_fetching && self.auto_refresh && self.refresh_interval_secs > 0 {
                    let elapsed = last_fetch.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                    let remaining = (self.refresh_interval_secs as u64).saturating_sub(elapsed);
                    ui.label(
                        egui::RichText::new(format!("{}s", remaining))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }

                ui.separator();
                let filter_icon = if self.show_filters { "☰ Filters" } else { "☰ Filters ▸" };
                if ui
                    .button(filter_icon)
                    .on_hover_text("Show/hide filter sidebar")
                    .clicked()
                {
                    self.show_filters = !self.show_filters;
                }

                if ui.button("Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }

                ui.separator();

                // N3FJP listener toggle
                {
                    let is_running = self.n3fjp_listener_running.load(Ordering::SeqCst);
                    let is_connected = *self.n3fjp_listener_connected.lock().unwrap();
                    let color = if is_connected {
                        egui::Color32::from_rgb(80, 200, 80)
                    } else if is_running {
                        egui::Color32::from_rgb(200, 200, 80)
                    } else {
                        egui::Color32::GRAY
                    };
                    if ui
                        .button(egui::RichText::new("N3FJP").color(color))
                        .on_hover_text("Toggle N3FJP AC Log listener")
                        .clicked()
                    {
                        if is_running {
                            self.stop_n3fjp_listener();
                        } else {
                            self.start_n3fjp_listener();
                        }
                    }
                }

                // DX cluster toggle
                {
                    let dx_running = self.dx_running.load(Ordering::SeqCst);
                    let dx_connected = self.dx_state.lock().unwrap().connected;
                    let color = if dx_connected {
                        egui::Color32::from_rgb(80, 200, 80)
                    } else if dx_running {
                        egui::Color32::from_rgb(200, 200, 80)
                    } else {
                        egui::Color32::GRAY
                    };
                    if ui
                        .button(egui::RichText::new("DX Cluster").color(color))
                        .on_hover_text("Toggle DX cluster connection")
                        .clicked()
                    {
                        if dx_running {
                            self.stop_dx_cluster();
                        } else {
                            self.start_dx_cluster();
                        }
                    }
                }

                // ATNO / New entity alert badge — pill button, click to dismiss
                if self.atno_alert_count > 0 {
                    ui.separator();
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(format!("⚡ {} NEW", self.atno_alert_count))
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .size(13.0),
                            )
                            .fill(egui::Color32::from_rgb(200, 40, 40)),
                        )
                        .on_hover_text("New DXCC entities or new band/mode combinations — click to dismiss")
                        .clicked()
                    {
                        self.atno_alert_count = 0;
                    }
                }

                // Compact status on the right (no error text here)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let state = self.state.lock().unwrap();
                    let spot_count = state.spots.len();
                    let elapsed = state
                        .last_fetch
                        .map(|t| format!("{}s ago", t.elapsed().as_secs()))
                        .unwrap_or_else(|| "never".to_string());
                    ui.label(format!("{} spots | {}", spot_count, elapsed));
                });
            });
        });

        // Settings window
        if self.show_settings {
            egui::Window::new("⚙ Settings")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.set_min_width(360.0);
                    ui.label(egui::RichText::new("N3FJP AC Log Connection").strong().size(15.0));
                    ui.horizontal(|ui| {
                        ui.label("Host:");
                        ui.text_edit_singleline(&mut self.n3fjp_host);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Port:");
                        ui.text_edit_singleline(&mut self.n3fjp_port);
                    });
                    if ui.button("Test Connection").clicked() {
                        let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                        let client = N3fjpClient::new(&self.n3fjp_host, port);
                        match client.send_command("<CMD><READBMF></CMD>") {
                            Ok(resp) => {
                                self.n3fjp_status =
                                    format!("✅ Connected! Response: {}", resp.trim());
                            }
                            Err(e) => {
                                self.n3fjp_status = format!("❌ {}", e);
                            }
                        }
                    }
                    if !self.n3fjp_status.is_empty() {
                        ui.label(&self.n3fjp_status);
                    }

                    ui.separator();
                    ui.label(egui::RichText::new("Auto-Refresh").strong().size(15.0));
                    ui.checkbox(&mut self.auto_refresh, "Enable auto-refresh");
                    ui.horizontal(|ui| {
                        ui.label("Interval (seconds):");
                        ui.add(
                            egui::DragValue::new(&mut self.refresh_interval_secs)
                                .range(15..=300)
                                .speed(1),
                        );
                    });

                    ui.separator();
                    ui.label(egui::RichText::new("Auto-Hunt on Log").strong().size(15.0));
                    ui.label("Listens for QSOs logged in AC Log and\nautomatically marks matching spots as hunted.");
                    {
                        let is_running = self.n3fjp_listener_running.load(Ordering::SeqCst);
                        let is_connected = *self.n3fjp_listener_connected.lock().unwrap();

                        ui.horizontal(|ui| {
                            ui.label("Status:");
                            if is_connected {
                                ui.colored_label(
                                    egui::Color32::from_rgb(80, 200, 80),
                                    "● Connected — listening for QSOs",
                                );
                            } else if is_running {
                                ui.colored_label(
                                    egui::Color32::from_rgb(200, 200, 80),
                                    "● Connecting...",
                                );
                            } else {
                                ui.colored_label(
                                    egui::Color32::GRAY,
                                    "● Disconnected",
                                );
                            }
                        });

                        ui.horizontal(|ui| {
                            if !is_running {
                                if ui.button("▶ Start Listener").clicked() {
                                    self.start_n3fjp_listener();
                                }
                            } else {
                                if ui.button("⏹ Stop Listener").clicked() {
                                    self.stop_n3fjp_listener();
                                }
                            }
                        });
                    }

                    ui.separator();
                    ui.label(egui::RichText::new("DX Cluster").strong().size(15.0));
                    ui.label("Connects to DX cluster via telnet.\nSpots are cached for 15 minutes.");

                    ui.horizontal(|ui| {
                        ui.label("Your Callsign:");
                        ui.text_edit_singleline(&mut self.dx_callsign);
                    });

                    ui.label("Cluster nodes (one per line, host:port):");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.dx_nodes)
                            .desired_rows(3)
                            .desired_width(300.0),
                    );

                    ui.checkbox(&mut self.dx_auto_start, "Auto-connect on launch");

                    {
                        let dx_running = self.dx_running.load(Ordering::SeqCst);
                        let dx_st = self.dx_state.lock().unwrap();
                        let dx_connected = dx_st.connected;
                        let dx_spot_count = dx_st.cached_spots.len();
                        let dx_status_text = dx_st.status.clone();
                        drop(dx_st);

                        ui.horizontal(|ui| {
                            ui.label("Status:");
                            if dx_connected {
                                ui.colored_label(
                                    egui::Color32::from_rgb(80, 200, 80),
                                    format!("● {} ({} spots cached)", dx_status_text, dx_spot_count),
                                );
                            } else if dx_running {
                                ui.colored_label(
                                    egui::Color32::from_rgb(200, 200, 80),
                                    format!("● {}", dx_status_text),
                                );
                            } else {
                                ui.colored_label(
                                    egui::Color32::GRAY,
                                    format!("● {}", dx_status_text),
                                );
                            }
                        });

                        ui.horizontal(|ui| {
                            if !dx_running {
                                if ui.button("▶ Start DX Cluster").clicked() {
                                    self.start_dx_cluster();
                                }
                            } else {
                                if ui.button("⏹ Stop DX Cluster").clicked() {
                                    self.stop_dx_cluster();
                                }
                            }
                        });
                    }

                    ui.separator();
                    ui.label(egui::RichText::new("Station Info").strong().size(15.0));
                    ui.horizontal(|ui| {
                        ui.label("My Grid Square:");
                        ui.text_edit_singleline(&mut self.my_grid);
                    });
                    if !self.my_grid.is_empty() {
                        if let Some((lat, lon)) = grid_to_latlon(&self.my_grid) {
                            ui.label(format!("  ({:.2}°N, {:.2}°W)", lat, -lon));
                        } else {
                            ui.colored_label(egui::Color32::RED, "  Invalid grid square");
                        }
                    }

                    ui.separator();
                    ui.label(egui::RichText::new("Display").strong().size(15.0));
                    if ui.checkbox(&mut self.dark_mode, "Dark mode").changed() {
                        ctx.set_visuals(if self.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                    }
                    ui.checkbox(&mut self.hide_dupes, "Collapse duplicate spots (show newest per callsign)");

                    ui.separator();
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("✔ Close & Save Settings")
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(40, 160, 40)),
                        )
                        .clicked()
                    {
                        self.save_current_settings();
                        self.show_settings = false;
                    }
                });
        }

        // Bottom panel - status bar
        egui::TopBottomPanel::bottom("bottom_panel")
            .min_height(26.0)
            .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let hunted_count = self.hunted_set.len();
                ui.label(format!("Hunted: {}", hunted_count));

                ui.separator();

                // N3FJP status indicator
                let listener_connected = *self.n3fjp_listener_connected.lock().unwrap();
                if listener_connected {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "N3FJP: Connected");
                } else {
                    ui.colored_label(egui::Color32::GRAY, "N3FJP: Off");
                }

                ui.separator();

                // DX cluster status indicator
                let dx_connected = self.dx_state.lock().unwrap().connected;
                if dx_connected {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "DX: Connected");
                } else if self.dx_running.load(Ordering::SeqCst) {
                    ui.colored_label(egui::Color32::from_rgb(200, 200, 80), "DX: Connecting");
                } else {
                    ui.colored_label(egui::Color32::GRAY, "DX: Off");
                }

                // N3FJP action status (tuned, logged, etc)
                if !self.n3fjp_status.is_empty() {
                    ui.separator();
                    ui.label(&self.n3fjp_status);
                }

                // Fetch errors (POTA/SOTA/DX)
                let fetch_err = {
                    let state = self.state.lock().unwrap();
                    state.fetch_error.clone()
                };
                if let Some(err) = fetch_err {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
                }
            });
        });

        // Left panel - filters (collapsible)
        if self.show_filters {
            egui::SidePanel::left("filter_panel")
                .default_width(200.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Filters").strong().size(15.0));
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .small_button("⏴")
                                    .on_hover_text("Hide filters")
                                    .clicked()
                                {
                                    self.show_filters = false;
                                }
                            },
                        );
                    });
                    ui.separator();

                    ui.label("Search (call/ref/park):");
                    let search_resp = ui.text_edit_singleline(&mut self.filter_callsign);
                    if self.search_focus_requested {
                        search_resp.request_focus();
                        self.search_focus_requested = false;
                    }

                ui.add_space(6.0);

                ui.label("Country/Entity:");
                egui::ComboBox::from_id_salt("country_filter")
                    .selected_text(&self.filter_country)
                    .show_ui(ui, |ui| {
                        for country in self.available_countries.clone() {
                            ui.selectable_value(
                                &mut self.filter_country,
                                country.clone(),
                                &country,
                            );
                        }
                    });

                ui.add_space(6.0);
                ui.checkbox(&mut self.hide_hunted, "Hide hunted spots");
                ui.checkbox(&mut self.hide_qrt, "Hide QRT spots");

                ui.add_space(6.0);
                ui.label("Max Spot Age:");
                ui.horizontal_wrapped(|ui| {
                    for (label, mins) in &[
                        ("5m", 5i64),
                        ("15m", 15),
                        ("30m", 30),
                        ("1h", 60),
                        ("2h", 120),
                        ("All", 0),
                    ] {
                        let is_selected = self.max_age_mins == *mins;
                        if ui.selectable_label(is_selected, *label).clicked() {
                            self.max_age_mins = *mins;
                        }
                    }
                });

                ui.add_space(8.0);
                if ui.button("Clear All Filters").clicked() {
                    self.filter_bands.clear();
                    self.filter_modes.clear();
                    self.filter_types.clear();
                    self.filter_country = "All".to_string();
                    self.filter_callsign.clear();
                    self.hide_hunted = false;
                    self.hide_qrt = false;
                    self.max_age_mins = 15;
                }

                ui.separator();
                ui.label(egui::RichText::new("Quick Band Filters").strong().size(13.0));
                ui.horizontal_wrapped(|ui| {
                    // "All" button clears the set
                    if ui
                        .selectable_label(self.filter_bands.is_empty(), "All")
                        .clicked()
                    {
                        self.filter_bands.clear();
                    }
                    for band in &[
                        "160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m",
                        "12m", "10m", "6m", "2m",
                    ] {
                        let is_selected = self.filter_bands.contains(*band);
                        let resp = ui.selectable_label(is_selected, *band);
                        if resp.secondary_clicked() {
                            // Right-click: select only this band, clearing all others
                            self.filter_bands.clear();
                            self.filter_bands.insert(band.to_string());
                        } else if resp.clicked() {
                            // Left-click: toggle this band on/off
                            if is_selected {
                                self.filter_bands.remove(*band);
                            } else {
                                self.filter_bands.insert(band.to_string());
                            }
                        }
                    }
                });

                ui.separator();
                ui.label(egui::RichText::new("Quick Mode Filters").strong().size(13.0));
                ui.horizontal_wrapped(|ui| {
                    // "All" button clears the set
                    if ui
                        .selectable_label(self.filter_modes.is_empty(), "All")
                        .clicked()
                    {
                        self.filter_modes.clear();
                    }
                    let modes = self.available_modes.clone();
                    for mode in &modes {
                        let is_selected = self.filter_modes.contains(mode.as_str());
                        let resp = ui.selectable_label(is_selected, mode);
                        if resp.secondary_clicked() {
                            // Right-click: select only this mode, clearing all others
                            self.filter_modes.clear();
                            self.filter_modes.insert(mode.to_string());
                        } else if resp.clicked() {
                            // Left-click: toggle this mode on/off
                            if is_selected {
                                self.filter_modes.remove(mode.as_str());
                            } else {
                                self.filter_modes.insert(mode.to_string());
                            }
                        }
                    }
                });

                ui.separator();
                ui.label(egui::RichText::new("Quick Type Filters").strong().size(13.0));
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(self.filter_types.is_empty(), "All")
                        .clicked()
                    {
                        self.filter_types.clear();
                    }
                    for spot_type in &[SpotType::Pota, SpotType::Sota, SpotType::Dx, SpotType::Wwff] {
                        let is_selected = self.filter_types.contains(spot_type);
                        let resp = ui.selectable_label(is_selected, spot_type.label());
                        if resp.secondary_clicked() {
                            // Right-click: select only this type, clearing all others
                            self.filter_types.clear();
                            self.filter_types.insert(*spot_type);
                        } else if resp.clicked() {
                            // Left-click: toggle this type on/off
                            if is_selected {
                                self.filter_types.remove(spot_type);
                            } else {
                                self.filter_types.insert(*spot_type);
                            }
                        }
                    }
                });

                ui.separator();
                if ui.button("🗑 Clear Hunted List").clicked() {
                    self.hunted_set.clear();
                    self.not_heard_set.clear();
                    let mut state = self.state.lock().unwrap();
                    for entry in &mut state.spots {
                        entry.hunted = false;
                        entry.not_heard = false;
                    }
                }
            });
        }  // end of if self.show_filters

        // Central panel - spot table
        egui::CentralPanel::default().show(ctx, |ui| {
            let filtered = self.get_filtered_spots();

            // Build filtered keys list for keyboard navigation
            self.filtered_keys = filtered.iter().map(|(_, e)| e.spot_key()).collect();

            // Process keyboard shortcuts on the selected spot (by exact row index so
            // duplicate-key rows like multiple DX spots per callsign work correctly).
            if let Some(row_idx) = self.selected_row_idx {
                if let Some((original_idx, entry)) = filtered.get(row_idx).cloned() {

                    if key_enter {
                        let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                        let client = N3fjpClient::new(&self.n3fjp_host, port);
                        let sk = entry.spot_key();
                        match client.tune_to_spot(&entry.spot) {
                            Ok(_) => {
                                self.n3fjp_status = format!(
                                    "Tuned to {} on {} kHz",
                                    entry.spot.activator, entry.spot.frequency
                                );
                                self.last_tuned_key = Some(sk);
                            }
                            Err(e) => {
                                self.n3fjp_status = format!("Tune failed: {}", e);
                            }
                        }
                    }

                    if key_h {
                        let key = entry.spot_key();
                        let new_state = !self.hunted_set.contains(&key);
                        if new_state {
                            self.hunted_set.insert(key);
                        } else {
                            self.hunted_set.remove(&key);
                        }
                        let mut state = self.state.lock().unwrap();
                        if let Some(e) = state.spots.get_mut(original_idx) {
                            e.hunted = new_state;
                        }
                    }

                    if key_l {
                        let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                        let client = N3fjpClient::new(&self.n3fjp_host, port);
                        match client.log_qso(&entry.spot) {
                            Ok(_) => {
                                self.n3fjp_status = format!(
                                    "Logged {} to AC Log",
                                    entry.spot.activator
                                );
                                let key = entry.spot_key();
                                self.hunted_set.insert(key);
                                let mut state = self.state.lock().unwrap();
                                if let Some(e) = state.spots.get_mut(original_idx) {
                                    e.hunted = true;
                                }
                            }
                            Err(e) => {
                                self.n3fjp_status = format!("Log failed: {}", e);
                            }
                        }
                    }

                    if key_n {
                        // Toggle not-heard on the selected spot
                        let spot_key = entry.spot_key();
                        let new_state = !self.not_heard_set.contains(&spot_key);
                        if new_state {
                            self.not_heard_set.insert(spot_key.clone());
                        } else {
                            self.not_heard_set.remove(&spot_key);
                        }
                        {
                            let mut state = self.state.lock().unwrap();
                            if let Some(e) = state.spots.get_mut(original_idx) {
                                e.not_heard = new_state;
                            }
                        }

                        // Only advance when marking NH (not un-marking)
                        if new_state {
                            // Find next workable spot with wrap-around, skipping the
                            // just-marked row.  Search forward first, then from the top.
                            let next_i = (row_idx + 1..filtered.len())
                                .chain(0..row_idx)
                                .find(|&i| {
                                    let (_, ref e) = filtered[i];
                                    !e.hunted && !e.not_heard && !e.is_qrt
                                });

                            if let Some(next_i) = next_i {
                                // Clone what we need before mutably borrowing self
                                let next_key  = filtered[next_i].1.spot_key();
                                let next_spot = filtered[next_i].1.spot.clone();

                                self.selected_row_idx  = Some(next_i);
                                self.selected_spot_key = Some(next_key.clone());
                                self.scroll_to_selected = true;

                                // Auto-tune to the next spot
                                let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                                let client = N3fjpClient::new(&self.n3fjp_host, port);
                                match client.tune_to_spot(&next_spot) {
                                    Ok(_) => {
                                        self.n3fjp_status = format!(
                                            "NH → tuned to {} on {} kHz",
                                            next_spot.activator, next_spot.frequency
                                        );
                                        self.last_tuned_key = Some(next_key);
                                    }
                                    Err(_) => {
                                        self.n3fjp_status = format!(
                                            "NH → {} (N3FJP not connected)",
                                            next_spot.activator
                                        );
                                    }
                                }
                            } else {
                                self.n3fjp_status =
                                    "NH marked — no more workable spots".to_string();
                            }
                        }
                    }
                }
            }

            let (is_fetching, total_spots) = {
                let state = self.state.lock().unwrap();
                (state.is_fetching, state.spots.len())
            };

            if total_spots == 0 && is_fetching {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.spinner();
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("Fetching spots...")
                            .size(18.0)
                            .color(egui::Color32::GRAY),
                    );
                });
                return;
            }

            if total_spots == 0 && !is_fetching {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(
                        egui::RichText::new("No spots yet")
                            .size(20.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Click Refresh Spots or enable the DX cluster to get started.",
                        )
                        .color(egui::Color32::GRAY),
                    );
                });
                return;
            }

            if filtered.is_empty() {
                ui.label(format!(
                    "Showing 0 of {} spots — all filtered out",
                    total_spots
                ));
                ui.separator();
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        egui::RichText::new("No spots match current filters")
                            .size(18.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    if ui.button("Clear all filters").clicked() {
                        self.filter_bands.clear();
                        self.filter_modes.clear();
                        self.filter_types.clear();
                        self.filter_country = "All".to_string();
                        self.filter_callsign.clear();
                        self.hide_hunted = false;
                        self.hide_qrt = false;
                        self.max_age_mins = 0;
                    }
                });
                return;
            }

            ui.label(format!(
                "Showing {} spots  (Up/Down navigate, Enter = tune, H = hunt, N = NH+skip, L = log, R = refresh, / = search)",
                filtered.len()
            ));
            ui.separator();

            egui::ScrollArea::both()
                .id_salt("spot_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("spot_grid")
                        .striped(true)
                        .min_col_width(40.0)
                        .max_col_width(380.0)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            // Header row
                            let headers: Vec<(SortColumn, String)> = vec![
                                (SortColumn::Type, format!("Type{}", self.sort_indicator(SortColumn::Type))),
                                (SortColumn::Band, format!("Band{}", self.sort_indicator(SortColumn::Band))),
                                (SortColumn::Mode, format!("Mode{}", self.sort_indicator(SortColumn::Mode))),
                                (SortColumn::Frequency, format!("Freq (kHz){}", self.sort_indicator(SortColumn::Frequency))),
                                (SortColumn::Activator, format!("Activator{}", self.sort_indicator(SortColumn::Activator))),
                                (SortColumn::Reference, format!("Reference{}", self.sort_indicator(SortColumn::Reference))),
                                (SortColumn::Park, format!("Name{}", self.sort_indicator(SortColumn::Park))),
                                (SortColumn::Location, format!("Location{}", self.sort_indicator(SortColumn::Location))),
                                (SortColumn::Comment, format!("Comment{}", self.sort_indicator(SortColumn::Comment))),
                                (SortColumn::Distance, format!("Dist{}", self.sort_indicator(SortColumn::Distance))),
                                (SortColumn::SpotTime, format!("Age{}", self.sort_indicator(SortColumn::SpotTime))),
                            ];

                            for (col, label) in &headers {
                                // Right-align Freq and Age headers to match their columns
                                let is_right_aligned = matches!(col, SortColumn::Frequency | SortColumn::SpotTime);
                                if is_right_aligned {
                                    // Freq column: constrain header to the same 88px max as data
                                    // cells so the right-aligned text lines up with the numbers.
                                    let mut clicked = false;
                                    if matches!(col, SortColumn::Frequency) {
                                        ui.scope(|ui| {
                                            ui.set_max_width(88.0);
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui
                                                        .add(
                                                            egui::Label::new(
                                                                egui::RichText::new(label).size(15.0).strong(),
                                                            )
                                                            .sense(egui::Sense::click()),
                                                        )
                                                        .clicked()
                                                    {
                                                        clicked = true;
                                                    }
                                                },
                                            );
                                        });
                                    } else {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(
                                                        egui::Label::new(
                                                            egui::RichText::new(label).size(15.0).strong(),
                                                        )
                                                        .sense(egui::Sense::click()),
                                                    )
                                                    .clicked()
                                                {
                                                    clicked = true;
                                                }
                                            },
                                        );
                                    }
                                    if clicked {
                                        self.toggle_sort(*col);
                                    }
                                } else {
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                egui::RichText::new(label).size(15.0).strong(),
                                            )
                                            .sense(egui::Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        self.toggle_sort(*col);
                                    }
                                }
                            }
                            // Action column headers
                            ui.label(egui::RichText::new("Actions").size(15.0).strong());
                            ui.end_row();

                            // Data rows
                            for (row_idx, (original_idx, entry)) in filtered.iter().enumerate() {
                                // Pre-compute row state
                                let spot_key = entry.spot_key();
                                let is_last_tuned = self
                                    .last_tuned_key
                                    .as_ref()
                                    .map_or(false, |k| k == &spot_key);
                                let is_greyed = entry.is_qrt;  // only QRT is greyed
                                let is_not_heard = entry.not_heard && !entry.is_qrt;
                                let is_selected = self.selected_row_idx == Some(row_idx);
                                let is_atno = entry.atno_status.as_deref()
                                    .map_or(false, |s| s == "ATNO" || s == "OW" || s == "OC");

                                // Text color — only not-heard gets a custom colour now;
                                // QRT spots are no longer faded (only the callsign is struck through).
                                let text_color = if is_not_heard {
                                    egui::Color32::from_rgb(80, 110, 180)
                                } else {
                                    egui::Color32::GRAY // unused fallback
                                };

                                let band_color = if is_not_heard {
                                    egui::Color32::from_rgb(80, 110, 180)
                                } else if self.dark_mode {
                                    match entry.band.as_str() {
                                        "160m" => egui::Color32::from_rgb(210, 100, 100),
                                        "80m"  => egui::Color32::from_rgb(210, 150, 80),
                                        "60m"  => egui::Color32::from_rgb(210, 200, 80),
                                        "40m"  => egui::Color32::from_rgb(80,  200, 100),
                                        "30m"  => egui::Color32::from_rgb(80,  200, 180),
                                        "20m"  => egui::Color32::from_rgb(80,  160, 230),
                                        "17m"  => egui::Color32::from_rgb(120, 120, 230),
                                        "15m"  => egui::Color32::from_rgb(180, 100, 230),
                                        "12m"  => egui::Color32::from_rgb(220, 100, 220),
                                        "10m"  => egui::Color32::from_rgb(230, 100, 160),
                                        "6m"   => egui::Color32::from_rgb(230, 120, 100),
                                        "2m"   => egui::Color32::from_rgb(210, 210, 100),
                                        _      => egui::Color32::GRAY,
                                    }
                                } else {
                                    match entry.band.as_str() {
                                        "160m" => egui::Color32::from_rgb(160, 60,  60),
                                        "80m"  => egui::Color32::from_rgb(160, 100, 40),
                                        "60m"  => egui::Color32::from_rgb(150, 140, 30),
                                        "40m"  => egui::Color32::from_rgb(40,  130, 60),
                                        "30m"  => egui::Color32::from_rgb(40,  130, 130),
                                        "20m"  => egui::Color32::from_rgb(40,  100, 180),
                                        "17m"  => egui::Color32::from_rgb(80,  80,  180),
                                        "15m"  => egui::Color32::from_rgb(130, 60,  180),
                                        "12m"  => egui::Color32::from_rgb(170, 60,  170),
                                        "10m"  => egui::Color32::from_rgb(180, 60,  110),
                                        "6m"   => egui::Color32::from_rgb(180, 80,  60),
                                        "2m"   => egui::Color32::from_rgb(160, 160, 60),
                                        _      => egui::Color32::GRAY,
                                    }
                                };

                                // Row background tint
                                let row_color = if is_last_tuned {
                                    Some(egui::Color32::from_rgba_premultiplied(200, 170, 0, 90))
                                } else if is_atno && !is_not_heard {
                                    // Soft red highlight for new entities (including QRT ATNOs)
                                    Some(egui::Color32::from_rgba_premultiplied(220, 60, 60, 50))
                                } else if is_selected {
                                    Some(egui::Color32::from_rgba_premultiplied(80, 120, 200, 60))
                                } else if entry.hunted {
                                    Some(egui::Color32::from_rgba_premultiplied(0, 100, 0, 50))
                                } else if is_not_heard {
                                    Some(egui::Color32::from_rgba_premultiplied(60, 100, 200, 55))
                                } else {
                                    None
                                };

                                // Save row top Y for full-row interaction
                                let row_top = ui.available_rect_before_wrap().top();

                                // Type badge (always full color, even for QRT/greyed spots)
                                let type_color = if self.dark_mode {
                                    match entry.spot_type {
                                        SpotType::Pota => egui::Color32::from_rgb(80,  210, 80),
                                        SpotType::Sota => egui::Color32::from_rgb(230, 160, 40),
                                        SpotType::Dx   => egui::Color32::from_rgb(100, 170, 255),
                                        SpotType::Wwff => egui::Color32::from_rgb(40,  220, 210),
                                    }
                                } else {
                                    match entry.spot_type {
                                        SpotType::Pota => egui::Color32::from_rgb(20,  140, 20),
                                        SpotType::Sota => egui::Color32::from_rgb(180, 100, 0),
                                        SpotType::Dx   => egui::Color32::from_rgb(40,  80,  200),
                                        SpotType::Wwff => egui::Color32::from_rgb(0,   150, 150),
                                    }
                                };
                                ui.label(
                                    egui::RichText::new(entry.spot_type.label())
                                        .strong()
                                        .color(type_color),
                                );

                                // Band
                                let band_text =
                                    egui::RichText::new(&entry.band).color(band_color);
                                ui.label(band_text);

                                // Mode
                                let mode_text = if is_not_heard {
                                    egui::RichText::new(&entry.spot.mode).color(text_color)
                                } else {
                                    egui::RichText::new(&entry.spot.mode)
                                };
                                ui.label(mode_text);

                                // Frequency (monospace, right-aligned)
                                let freq_text = if is_not_heard {
                                    egui::RichText::new(&entry.spot.frequency)
                                        .monospace()
                                        .size(17.0)
                                        .color(text_color)
                                } else {
                                    egui::RichText::new(&entry.spot.frequency)
                                        .monospace()
                                        .size(17.0)
                                };
                                ui.scope(|ui| {
                                    ui.set_max_width(88.0);
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(freq_text);
                                        },
                                    );
                                });

                                // Activator (with ATNO badge)
                                let call_display = if is_atno && !is_not_heard {
                                    format!("{} NEW!", entry.spot.activator)
                                } else {
                                    entry.spot.activator.clone()
                                };
                                // Build the base style (colour / weight) then apply
                                // strikethrough for QRT or hunted as a second step.
                                let call_text = if is_not_heard {
                                    egui::RichText::new(&call_display).color(text_color)
                                } else if is_atno {
                                    egui::RichText::new(&call_display)
                                        .strong()
                                        .color(egui::Color32::from_rgb(220, 50, 50))
                                } else {
                                    egui::RichText::new(&call_display).strong()
                                };
                                // QRT keeps strikethrough; hunted also gets strikethrough.
                                let call_text = if is_greyed || entry.hunted {
                                    call_text.strikethrough()
                                } else {
                                    call_text
                                };
                                // Build tooltip with DXCC info
                                let dxcc_tip = {
                                    let country = entry.dxcc_country.as_deref().unwrap_or("Unknown");
                                    let status = entry.atno_status.as_deref().unwrap_or("...");
                                    let status_desc = match status {
                                        "ATNO" => "All-Time New One!",
                                        "OW" => "Worked (not confirmed on any band)",
                                        "OC" => "Confirmed on another band/mode",
                                        "OWBMW" => "Worked on this band/mode (not confirmed)",
                                        "OCBMW" => "Confirmed on other band, worked this band/mode",
                                        "BMC" => "Confirmed on this band and mode",
                                        "ERR" => "Lookup error",
                                        _ => "Checking...",
                                    };
                                    format!("{}\n{}", country, status_desc)
                                };
                                ui.label(call_text).on_hover_text(dxcc_tip);

                                // Reference
                                let ref_text = if is_not_heard {
                                    egui::RichText::new(&entry.spot.reference).color(text_color)
                                } else {
                                    egui::RichText::new(&entry.spot.reference)
                                };
                                ui.label(ref_text);

                                // Park name (truncated)
                                let park = entry.spot.display_park_name();
                                let park_text = if is_not_heard {
                                    egui::RichText::new(park).color(text_color)
                                } else {
                                    egui::RichText::new(park)
                                };
                                let row_h = ui.text_style_height(&egui::TextStyle::Body);
                                ui.add_sized(
                                    [180.0, row_h],
                                    egui::Label::new(park_text).truncate(),
                                );

                                // Location (with QRT indicator)
                                let loc = entry
                                    .spot
                                    .location_desc
                                    .as_deref()
                                    .unwrap_or("-");
                                let loc_display = if entry.is_qrt {
                                    format!("{} [QRT]", loc)
                                } else if entry.not_heard {
                                    format!("{} [NH]", loc)
                                } else {
                                    loc.to_string()
                                };
                                let loc_text = if is_not_heard {
                                    egui::RichText::new(&loc_display).color(text_color)
                                } else {
                                    egui::RichText::new(&loc_display)
                                };
                                let row_h = ui.text_style_height(&egui::TextStyle::Body);
                                ui.add_sized(
                                    [70.0, row_h],
                                    egui::Label::new(loc_text).truncate(),
                                );

                                // Comment (truncated)
                                let comment = entry
                                    .spot
                                    .comments
                                    .as_deref()
                                    .unwrap_or("");
                                let comment_display = if comment.chars().count() > 28 {
                                    let truncated: String = comment.chars().take(26).collect();
                                    format!("{}…", truncated)
                                } else {
                                    comment.to_string()
                                };
                                let comment_text = if is_not_heard {
                                    egui::RichText::new(&comment_display).color(text_color)
                                } else {
                                    egui::RichText::new(&comment_display)
                                };
                                ui.label(comment_text)
                                    .on_hover_text(comment);

                                // Distance/bearing from my grid
                                let dist_display = if !self.my_grid.is_empty() {
                                    let spot_grid = entry.spot.grid6.as_deref()
                                        .or(entry.spot.grid4.as_deref())
                                        .unwrap_or("");
                                    if !spot_grid.is_empty() {
                                        distance_bearing(&self.my_grid, spot_grid)
                                            .unwrap_or_else(|| "-".to_string())
                                    } else {
                                        "-".to_string()
                                    }
                                } else {
                                    "-".to_string()
                                };
                                let dist_text = if is_not_heard {
                                    egui::RichText::new(&dist_display).color(text_color)
                                } else {
                                    egui::RichText::new(&dist_display)
                                };
                                ui.label(dist_text);

                                // Spot age (with stale fading)
                                let time = entry
                                    .spot
                                    .spot_time
                                    .as_deref()
                                    .unwrap_or("");
                                let age_str = spot_age_str(time);
                                let age_mins = spot_age_minutes(time);
                                let age_color = if is_not_heard {
                                    text_color
                                } else if age_mins > 30 {
                                    egui::Color32::from_rgb(180, 80, 80)
                                } else if age_mins > 15 {
                                    egui::Color32::from_rgb(200, 160, 60)
                                } else {
                                    egui::Color32::from_rgb(80, 180, 80)
                                };
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(&age_str)
                                                .monospace()
                                                .color(age_color),
                                        );
                                    },
                                );

                                // Action buttons (icon-only for compactness)
                                ui.horizontal(|ui| {
                                    // Tune button — highlighted when this is the last tuned spot
                                    let tune_button = if is_last_tuned {
                                        egui::Button::new(
                                            egui::RichText::new("📻")
                                                .strong()
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(egui::Color32::from_rgb(180, 140, 0))
                                        .min_size(egui::vec2(28.0, 0.0))
                                    } else {
                                        egui::Button::new("📻")
                                            .min_size(egui::vec2(28.0, 0.0))
                                    };
                                    let tune_hover = if is_last_tuned {
                                        "Currently tuned (click to re-tune)"
                                    } else {
                                        "Tune radio via N3FJP AC Log"
                                    };

                                    if ui
                                        .add(tune_button)
                                        .on_hover_text(tune_hover)
                                        .clicked()
                                    {
                                        let port: u16 =
                                            self.n3fjp_port.parse().unwrap_or(1100);
                                        let client =
                                            N3fjpClient::new(&self.n3fjp_host, port);
                                        match client.tune_to_spot(&entry.spot) {
                                            Ok(_) => {
                                                self.n3fjp_status = format!(
                                                    "Tuned to {} on {} kHz",
                                                    entry.spot.activator,
                                                    entry.spot.frequency
                                                );
                                                self.last_tuned_key = Some(spot_key.clone());
                                            }
                                            Err(e) => {
                                                self.n3fjp_status =
                                                    format!("Tune failed: {}", e);
                                            }
                                        }
                                    }

                                    // Hunt toggle button (icon-only)
                                    let hunt_icon = if entry.hunted { "✅" } else { "🎯" };
                                    let hunt_hover = if entry.hunted {
                                        "Hunted — click to unmark"
                                    } else {
                                        "Mark as hunted (H)"
                                    };
                                    if ui
                                        .add(egui::Button::new(hunt_icon).min_size(egui::vec2(28.0, 0.0)))
                                        .on_hover_text(hunt_hover)
                                        .clicked()
                                    {
                                        let key = spot_key.clone();
                                        let new_state;
                                        if self.hunted_set.contains(&key) {
                                            self.hunted_set.remove(&key);
                                            new_state = false;
                                        } else {
                                            self.hunted_set.insert(key);
                                            new_state = true;
                                        }
                                        let mut state = self.state.lock().unwrap();
                                        if let Some(entry) =
                                            state.spots.get_mut(*original_idx)
                                        {
                                            entry.hunted = new_state;
                                        }
                                    }

                                    // Log QSO button (icon-only)
                                    if ui
                                        .add(egui::Button::new("📝").min_size(egui::vec2(28.0, 0.0)))
                                        .on_hover_text("Log QSO to N3FJP AC Log (L)")
                                        .clicked()
                                    {
                                        let port: u16 =
                                            self.n3fjp_port.parse().unwrap_or(1100);
                                        let client =
                                            N3fjpClient::new(&self.n3fjp_host, port);
                                        match client.log_qso(&entry.spot) {
                                            Ok(_) => {
                                                self.n3fjp_status = format!(
                                                    "Logged {} to AC Log",
                                                    entry.spot.activator
                                                );
                                                let key = spot_key.clone();
                                                self.hunted_set.insert(key);
                                                let mut state =
                                                    self.state.lock().unwrap();
                                                if let Some(entry) =
                                                    state.spots.get_mut(*original_idx)
                                                {
                                                    entry.hunted = true;
                                                }
                                            }
                                            Err(e) => {
                                                self.n3fjp_status =
                                                    format!("Log failed: {}", e);
                                            }
                                        }
                                    }

                                    // Not-heard toggle button — "NH" to mark, "OK" to clear
                                    let (nh_label, nh_fill, nh_hover) = if entry.not_heard {
                                        (
                                            "OK",
                                            egui::Color32::from_rgb(40, 160, 40),
                                            "Mark as heard (clear not-heard)",
                                        )
                                    } else {
                                        (
                                            "NH",
                                            egui::Color32::from_rgb(80, 110, 170),
                                            "Mark as not heard",
                                        )
                                    };
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new(nh_label)
                                                    .strong()
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(nh_fill)
                                            .min_size(egui::vec2(28.0, 0.0)),
                                        )
                                        .on_hover_text(nh_hover)
                                        .clicked()
                                    {
                                        let key = spot_key.clone();
                                        let new_state;
                                        if self.not_heard_set.contains(&key) {
                                            self.not_heard_set.remove(&key);
                                            new_state = false;
                                        } else {
                                            self.not_heard_set.insert(key);
                                            new_state = true;
                                        }
                                        let mut state = self.state.lock().unwrap();
                                        if let Some(entry) =
                                            state.spots.get_mut(*original_idx)
                                        {
                                            entry.not_heard = new_state;
                                        }
                                    }
                                });

                                // Measure actual row bottom from cursor position
                                let row_bottom = ui.available_rect_before_wrap().top();
                                // Use at least the spacing to avoid zero-height rects
                                let row_height = (row_bottom - row_top).max(20.0);

                                // Paint row background tint (behind text via painter layer)
                                if let Some(c) = row_color {
                                    let full_row = egui::Rect::from_min_max(
                                        egui::pos2(ui.max_rect().left(), row_top),
                                        egui::pos2(ui.max_rect().right(), row_top + row_height),
                                    );
                                    ui.painter().rect_filled(full_row, 0.0, c);
                                }

                                // Pure pointer-based row detection (NO ui.interact - that breaks grids)
                                let row_rect = egui::Rect::from_min_max(
                                    egui::pos2(ui.max_rect().left(), row_top),
                                    egui::pos2(ui.max_rect().right(), row_top + row_height),
                                );

                                // Detect clicks via raw pointer position
                                let (hover_pos, primary_clicked, secondary_clicked, primary_doubled) = ui.input(|i| {
                                    (
                                        i.pointer.hover_pos(),
                                        i.pointer.button_clicked(egui::PointerButton::Primary),
                                        i.pointer.button_clicked(egui::PointerButton::Secondary),
                                        i.pointer.button_double_clicked(egui::PointerButton::Primary),
                                    )
                                });

                                if let Some(pos) = hover_pos {
                                    if row_rect.contains(pos) {
                                        if primary_clicked {
                                            self.selected_row_idx = Some(row_idx);
                                            self.selected_spot_key = Some(spot_key.clone());
                                        }
                                        if primary_doubled {
                                            // Double-click to tune
                                            let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                                            let client = N3fjpClient::new(&self.n3fjp_host, port);
                                            match client.tune_to_spot(&entry.spot) {
                                                Ok(_) => {
                                                    self.n3fjp_status = format!(
                                                        "Tuned to {} on {} kHz",
                                                        entry.spot.activator, entry.spot.frequency
                                                    );
                                                    self.last_tuned_key = Some(spot_key.clone());
                                                }
                                                Err(e) => {
                                                    self.n3fjp_status = format!("Tune failed: {}", e);
                                                }
                                            }
                                        }
                                        if secondary_clicked {
                                            self.selected_row_idx = Some(row_idx);
                                            self.selected_spot_key = Some(spot_key.clone());
                                            self.ctx_menu_row = Some(row_idx);
                                            self.ctx_menu_original_idx = Some(*original_idx);
                                            self.ctx_menu_spot = Some(entry.spot.clone());
                                            self.ctx_menu_spot_type = Some(entry.spot_type);
                                            self.ctx_menu_spot_key = Some(spot_key.clone());
                                            self.ctx_menu_pos = Some(pos);
                                        }
                                    }
                                }

                                // Auto-scroll on keyboard nav — vertical only.
                                // Use the clip rect's X range so scroll_to_rect
                                // considers the row already horizontally in view
                                // and doesn't adjust the horizontal scroll position.
                                if is_selected && self.scroll_to_selected {
                                    let clip = ui.clip_rect();
                                    let v_only = egui::Rect::from_x_y_ranges(
                                        clip.x_range(),
                                        row_rect.y_range(),
                                    );
                                    ui.scroll_to_rect(v_only, Some(egui::Align::Center));
                                    self.scroll_to_selected = false;
                                }

                                ui.end_row();
                            }
                        });
                });
        });

        // Context menu popup - rendered OUTSIDE the grid to avoid layout issues
        if let Some(pos) = self.ctx_menu_pos {
            let menu_id = egui::Id::new("spot_context_menu");
            // Show menu as a popup area at the right-click position
            let mut close_menu = false;
            egui::Area::new(menu_id)
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(180.0);

                        if let Some(ref spot) = self.ctx_menu_spot {
                            // Header
                            ui.label(
                                egui::RichText::new(&spot.activator)
                                    .strong(),
                            );
                            ui.separator();

                            if ui.button("Tune radio").clicked() {
                                let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                                let client = N3fjpClient::new(&self.n3fjp_host, port);
                                match client.tune_to_spot(spot) {
                                    Ok(_) => {
                                        self.n3fjp_status = format!(
                                            "Tuned to {} on {} kHz",
                                            spot.activator, spot.frequency
                                        );
                                        self.last_tuned_key = self.ctx_menu_spot_key.clone();
                                    }
                                    Err(e) => {
                                        self.n3fjp_status = format!("Tune failed: {}", e);
                                    }
                                }
                                close_menu = true;
                            }

                            if ui.button("Log QSO to AC Log").clicked() {
                                let port: u16 = self.n3fjp_port.parse().unwrap_or(1100);
                                let client = N3fjpClient::new(&self.n3fjp_host, port);
                                match client.log_qso(spot) {
                                    Ok(_) => {
                                        self.n3fjp_status = format!(
                                            "Logged {}",
                                            spot.activator
                                        );
                                        if let Some(ref key) = self.ctx_menu_spot_key {
                                            self.hunted_set.insert(key.clone());
                                        }
                                        if let Some(oi) = self.ctx_menu_original_idx {
                                            let mut state = self.state.lock().unwrap();
                                            if let Some(e) = state.spots.get_mut(oi) {
                                                e.hunted = true;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        self.n3fjp_status = format!("Log failed: {}", e);
                                    }
                                }
                                close_menu = true;
                            }

                            ui.separator();

                            if ui.button("Toggle hunted").clicked() {
                                if let Some(ref key) = self.ctx_menu_spot_key {
                                    let new_state = !self.hunted_set.contains(key);
                                    if new_state {
                                        self.hunted_set.insert(key.clone());
                                    } else {
                                        self.hunted_set.remove(key);
                                    }
                                    if let Some(oi) = self.ctx_menu_original_idx {
                                        let mut state = self.state.lock().unwrap();
                                        if let Some(e) = state.spots.get_mut(oi) {
                                            e.hunted = new_state;
                                        }
                                    }
                                }
                                close_menu = true;
                            }

                            if ui.button("Toggle not heard").clicked() {
                                if let Some(ref key) = self.ctx_menu_spot_key {
                                    let new_state = !self.not_heard_set.contains(key);
                                    if new_state {
                                        self.not_heard_set.insert(key.clone());
                                    } else {
                                        self.not_heard_set.remove(key);
                                    }
                                    if let Some(oi) = self.ctx_menu_original_idx {
                                        let mut state = self.state.lock().unwrap();
                                        if let Some(e) = state.spots.get_mut(oi) {
                                            e.not_heard = new_state;
                                        }
                                    }
                                }
                                close_menu = true;
                            }

                            ui.separator();

                            if ui.button("Lookup on QRZ.com").clicked() {
                                let url = format!("https://www.qrz.com/db/{}", spot.activator);
                                let _ = open::that(&url);
                                close_menu = true;
                            }

                            if ui.button("Lookup on HamQTH").clicked() {
                                let url = format!("https://www.hamqth.com/{}", spot.activator);
                                let _ = open::that(&url);
                                close_menu = true;
                            }

                            if let Some(SpotType::Pota) = self.ctx_menu_spot_type {
                                if !spot.reference.is_empty() {
                                    if ui.button("Open on POTA.app").clicked() {
                                        let url = format!("https://pota.app/#/park/{}", spot.reference);
                                        let _ = open::that(&url);
                                        close_menu = true;
                                    }
                                }
                            }
                            if let Some(SpotType::Sota) = self.ctx_menu_spot_type {
                                if !spot.reference.is_empty() {
                                    if ui.button("Open on SOTAdata").clicked() {
                                        let url = format!(
                                            "https://www.sotadata.org.uk/en/summit/{}",
                                            spot.reference
                                        );
                                        let _ = open::that(&url);
                                        close_menu = true;
                                    }
                                }
                            }
                            if let Some(SpotType::Wwff) = self.ctx_menu_spot_type {
                                if !spot.reference.is_empty() {
                                    if ui.button("Open on WWFF Spotline").clicked() {
                                        let url = format!(
                                            "https://spots.wwff.co/references/direct?wwff={}",
                                            spot.reference
                                        );
                                        let _ = open::that(&url);
                                        close_menu = true;
                                    }
                                }
                            }

                            ui.separator();

                            if ui.button("Copy callsign").clicked() {
                                ui.ctx().copy_text(spot.activator.clone());
                                close_menu = true;
                            }
                        }
                    });
                });

            // Close menu on action or if user clicks elsewhere
            if close_menu {
                self.ctx_menu_pos = None;
                self.ctx_menu_spot = None;
            } else {
                // Close if user clicks anywhere outside the menu
                let clicked_elsewhere = ctx.input(|i| {
                    i.pointer.button_clicked(egui::PointerButton::Primary)
                        || i.pointer.button_clicked(egui::PointerButton::Secondary)
                });
                if clicked_elsewhere {
                    // Check if click was outside the menu area
                    if let Some(hover) = ctx.input(|i| i.pointer.hover_pos()) {
                        let menu_rect = egui::Rect::from_min_size(pos, egui::vec2(200.0, 400.0));
                        if !menu_rect.contains(hover) {
                            self.ctx_menu_pos = None;
                            self.ctx_menu_spot = None;
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Icon loading
// ---------------------------------------------------------------------------

fn load_icon() -> Option<egui::IconData> {
    let png_bytes = include_bytes!("../assets/icon_64.png");
    let img = image::load_from_memory(png_bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 800.0])
        .with_min_inner_size([900.0, 500.0])
        .with_title("KM5E's Base Camp v1.19.0 — POTA, SOTA & DX Spot Browser");

    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "KM5E's Base Camp",
        options,
        Box::new(|cc| Ok(Box::new(PotaHunterApp::new(cc)))),
    )
}
