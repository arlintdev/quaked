//! Night mode: a dark ambient screen (clock, date, weather, the first thing on tomorrow's
//! calendar) when the board has sat untouched at night. The app decides when it shows
//! (app/night.rs); this thread only fetches the weather from Open-Meteo every 30 minutes
//! around the night window, for a location from the settings or estimated from the
//! Cloudflare colo the network goes through (never from an IP-geolocation service).

use crate::config::NightSettings as Cfg;
use crate::device::log;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);
/// Weather older than this isn't shown.
const STALE_MS: u64 = 3 * 3_600_000;

/// Local wall-clock time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1-12
    pub month: u32,
    /// 1-31
    pub day: u32,
    /// 0 = Sunday
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
}

impl LocalTime {
    pub fn date(&self) -> (i32, u32, u32) {
        (self.year, self.month, self.day)
    }

    /// "2:14" and "AM".
    pub fn clock(&self) -> (String, &'static str) {
        clock12(self.hour, self.minute)
    }

    /// "Tuesday, November 17"
    pub fn date_line(&self) -> String {
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        format!("{}, {} {}", DAYS[self.weekday as usize % 7], MONTHS[(self.month.max(1) as usize - 1) % 12], self.day)
    }
}

pub fn clock12(hour: u32, minute: u32) -> (String, &'static str) {
    let h = match hour % 12 {
        0 => 12,
        h => h,
    };
    (format!("{h}:{minute:02}"), if hour < 12 { "AM" } else { "PM" })
}

/// Local time at a unix second.
pub fn local(secs: i64) -> LocalTime {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    LocalTime { year: tm.tm_year + 1900, month: (tm.tm_mon + 1) as u32, day: tm.tm_mday as u32, weekday: tm.tm_wday as u32, hour: tm.tm_hour as u32, minute: tm.tm_min as u32 }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Is `hour` in [start, end)? Wraps past midnight (22 -> 7); start == end means all day.
pub fn in_window(hour: u32, start: u32, end: u32) -> bool {
    let (s, e) = (start % 24, end % 24);
    if s == e {
        true
    } else if s < e {
        hour >= s && hour < e
    } else {
        hour >= s || hour < e
    }
}

// ---------------------------------------------------------------- location

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    /// "Salt Lake City" when estimated from the colo; empty for coordinates from the settings.
    pub place: String,
}

/// Rough coordinates of common Cloudflare data centers (IATA code), for the weather.
const COLOS: &[(&str, f64, f64, &str)] = &[
    ("SLC", 40.76, -111.89, "Salt Lake City"),
    ("DEN", 39.74, -104.99, "Denver"),
    ("LAX", 34.05, -118.24, "Los Angeles"),
    ("SEA", 47.61, -122.33, "Seattle"),
    ("SJC", 37.34, -121.89, "San Jose"),
    ("SFO", 37.77, -122.42, "San Francisco"),
    ("PDX", 45.52, -122.68, "Portland"),
    ("PHX", 33.45, -112.07, "Phoenix"),
    ("LAS", 36.17, -115.14, "Las Vegas"),
    ("DFW", 32.78, -96.80, "Dallas"),
    ("IAH", 29.76, -95.37, "Houston"),
    ("ORD", 41.88, -87.63, "Chicago"),
    ("MSP", 44.98, -93.27, "Minneapolis"),
    ("ATL", 33.75, -84.39, "Atlanta"),
    ("MIA", 25.76, -80.19, "Miami"),
    ("IAD", 39.04, -77.49, "Ashburn"),
    ("EWR", 40.74, -74.17, "Newark"),
    ("BOS", 42.36, -71.06, "Boston"),
    ("YYZ", 43.65, -79.38, "Toronto"),
    ("LHR", 51.51, -0.13, "London"),
    ("FRA", 50.11, 8.68, "Frankfurt"),
    ("AMS", 52.37, 4.90, "Amsterdam"),
    ("CDG", 48.86, 2.35, "Paris"),
    ("NRT", 35.68, 139.69, "Tokyo"),
    ("SIN", 1.35, 103.82, "Singapore"),
    ("SYD", -33.87, 151.21, "Sydney"),
    ("PHL", 39.95, -75.17, "Philadelphia"),
    ("CLT", 35.23, -80.84, "Charlotte"),
    ("MCI", 39.10, -94.58, "Kansas City"),
    ("DTW", 42.33, -83.05, "Detroit"),
    ("YVR", 49.28, -123.12, "Vancouver"),
    ("MAD", 40.42, -3.70, "Madrid"),
    ("HKG", 22.32, 114.17, "Hong Kong"),
    ("ICN", 37.57, 126.98, "Seoul"),
];

pub fn colo_location(colo: &str) -> Option<Location> {
    let code = colo.trim().to_ascii_uppercase();
    COLOS.iter().find(|(c, ..)| *c == code).map(|(_, lat, lon, place)| Location { lat: *lat, lon: *lon, place: place.to_string() })
}

/// The settings' coordinates, else the colo's.
pub fn location(cfg: &Cfg, colo: &str) -> Option<Location> {
    match (cfg.latitude, cfg.longitude) {
        (Some(lat), Some(lon)) if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) => Some(Location { lat, lon, place: String::new() }),
        _ => colo_location(colo),
    }
}

// ---------------------------------------------------------------- weather

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Day {
    pub high: f32,
    pub low: f32,
    pub code: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Weather {
    /// °F
    pub temp: f32,
    /// WMO weather code.
    pub code: u32,
    pub is_day: bool,
    /// Today, tomorrow.
    pub days: Vec<Day>,
    pub fetched_ms: u64,
}

pub fn weather_url(loc: &Location) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.3}&longitude={:.3}&current=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min,weather_code&temperature_unit=fahrenheit&timezone=auto&forecast_days=2",
        loc.lat, loc.lon
    )
}

pub fn parse_weather(json: &str) -> Result<Weather, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| "unexpected reply".to_string())?;
    if v.get("error").and_then(|e| e.as_bool()) == Some(true) {
        return Err(v.get("reason").and_then(|r| r.as_str()).unwrap_or("error").to_string());
    }
    let cur = v.get("current").ok_or("no current weather")?;
    let temp = cur.get("temperature_2m").and_then(|t| t.as_f64()).ok_or("no temperature")? as f32;
    let code = cur.get("weather_code").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
    let is_day = cur.get("is_day").and_then(|d| d.as_u64()).unwrap_or(0) == 1;
    let daily = v.get("daily");
    let col = |k: &str| -> Vec<serde_json::Value> { daily.and_then(|d| d.get(k)).and_then(|a| a.as_array()).cloned().unwrap_or_default() };
    let (hi, lo, codes) = (col("temperature_2m_max"), col("temperature_2m_min"), col("weather_code"));
    let days = hi
        .iter()
        .zip(lo.iter())
        .enumerate()
        .filter_map(|(i, (h, l))| Some(Day { high: h.as_f64()? as f32, low: l.as_f64()? as f32, code: codes.get(i).and_then(|c| c.as_u64()).unwrap_or(code as u64) as u32 }))
        .collect();
    Ok(Weather { temp, code, is_day, days, fetched_ms: now_secs() as u64 * 1000 })
}

/// An SF Symbol for a WMO weather code.
pub fn weather_symbol(code: u32, day: bool) -> &'static str {
    match code {
        0 => {
            if day {
                "sun.max.fill"
            } else {
                "moon.stars.fill"
            }
        }
        1 | 2 => {
            if day {
                "cloud.sun.fill"
            } else {
                "cloud.moon.fill"
            }
        }
        3 => "cloud.fill",
        45 | 48 => "cloud.fog.fill",
        51 | 53 | 55 => "cloud.drizzle.fill",
        56 | 57 | 66 | 67 => "cloud.sleet.fill",
        61 | 63 => "cloud.rain.fill",
        65 | 82 => "cloud.heavyrain.fill",
        71 | 73 | 75 | 85 | 86 => "cloud.snow.fill",
        77 => "snowflake",
        80 | 81 => {
            if day {
                "cloud.sun.rain.fill"
            } else {
                "cloud.moon.rain.fill"
            }
        }
        95 => "cloud.bolt.fill",
        96 | 99 => "cloud.bolt.rain.fill",
        _ => "thermometer.medium",
    }
}

pub fn weather_text(code: u32) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 | 73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 | 81 => "Showers",
        82 => "Heavy showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorms",
        96 | 99 => "Thunderstorms, hail",
        _ => "",
    }
}

// ---------------------------------------------------------------- calendar

/// The first thing on the calendar for the morning ahead.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NextEvent {
    pub title: String,
    /// "Today" (after midnight) or "Tomorrow" (before it).
    pub day: &'static str,
    /// "9:30 AM" or "All day".
    pub when: String,
}

/// After noon "tomorrow" is the next date; after midnight it's the morning of today. Its
/// first timed event still to come, else its first all-day event.
pub fn next_event(events: &[crate::calendar::Event], now_ms: u64) -> Option<NextEvent> {
    let now_s = (now_ms / 1000) as i64;
    let now = local(now_s);
    let ahead = now.hour >= 12;
    let target = local(now_s + if ahead { 86_400 } else { 0 }).date();
    let day = if ahead { "Tomorrow" } else { "Today" };
    let on_target = |ms: u64| local((ms / 1000) as i64).date() == target;
    let timed = events.iter().filter(|e| !e.all_day && e.start_ms > now_ms && on_target(e.start_ms)).min_by_key(|e| e.start_ms);
    if let Some(e) = timed {
        let t = local((e.start_ms / 1000) as i64);
        let (hm, ampm) = t.clock();
        return Some(NextEvent { title: e.title.clone(), day, when: format!("{hm} {ampm}") });
    }
    // All-day events start at local midnight; check the middle of the day to be safe.
    events.iter().filter(|e| e.all_day && on_target(e.start_ms + 12 * 3_600_000)).min_by_key(|e| e.start_ms).map(|e| NextEvent { title: e.title.clone(), day, when: "All day".into() })
}

// ---------------------------------------------------------------- status & thread

/// What the board shows. The app sets everything but `weather`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The ambient night screen is showing.
    pub active: bool,
    /// The clock to show (updated every minute while active).
    pub time: LocalTime,
    pub weather: Option<Weather>,
    /// Where the weather is for ("Salt Lake City"), if estimated.
    pub place: String,
    pub next: Option<NextEvent>,
    /// The calendar answered (so no `next` means nothing's on it).
    pub calendar_ok: bool,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Where to get the weather for (None: unknown).
    Location(Option<Location>),
}

/// What the module's thread sends the app (handled in app/night.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Weather(Option<Weather>),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("night".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

/// Weather is wanted from an hour before the night starts until it ends.
fn weather_wanted(cfg: &Cfg, hour: u32) -> bool {
    cfg.enabled && in_window(hour, (cfg.start_hour + 23) % 24, cfg.end_hour)
}

fn fetch(loc: &Location) -> Result<Weather, String> {
    let out = std::process::Command::new("/usr/bin/curl")
        .args(["-s", "--max-time", "10", &weather_url(loc)])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.stdout.is_empty() {
        return Err("no answer from Open-Meteo".into());
    }
    parse_weather(&String::from_utf8_lossy(&out.stdout))
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut loc: Option<Location> = None;
    let mut have: Option<Weather> = None;
    let mut next_fetch = Instant::now();
    loop {
        let hour = local(now_secs()).hour;
        if let Some(l) = loc.as_ref().filter(|_| weather_wanted(&cfg, hour) && Instant::now() >= next_fetch) {
            match fetch(l) {
                Ok(w) => {
                    have = Some(w.clone());
                    emit(Update::Weather(Some(w)));
                    next_fetch = Instant::now() + REFRESH;
                }
                Err(e) => {
                    log(format!("night: weather: {e}"));
                    if have.as_ref().is_some_and(|w| (now_secs() as u64 * 1000).saturating_sub(w.fetched_ms) > STALE_MS) {
                        have = None;
                        emit(Update::Weather(None));
                    }
                    next_fetch = Instant::now() + RETRY;
                }
            }
        }
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Cmd::Configure(c)) => cfg = c,
            Ok(Cmd::Location(l)) => {
                if l != loc {
                    loc = l;
                    next_fetch = Instant::now();
                    if loc.is_none() && have.take().is_some() {
                        emit(Update::Weather(None));
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Sample data for `quaked preview`: the night screen at 2:14 AM with the weather.
pub fn sample() -> Status {
    Status {
        active: true,
        time: LocalTime { year: 2026, month: 11, day: 17, weekday: 2, hour: 2, minute: 14 },
        weather: Some(Weather { temp: 34.0, code: 1, is_day: false, days: vec![Day { high: 48.0, low: 29.0, code: 3 }, Day { high: 51.0, low: 32.0, code: 61 }], fetched_ms: 1 }),
        place: "Salt Lake City".into(),
        next: Some(NextEvent { title: "Standup with the platform team".into(), day: "Today", when: "9:30 AM".into() }),
        calendar_ok: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_wraps_past_midnight() {
        // 22 -> 7
        for h in [22, 23, 0, 3, 6] {
            assert!(in_window(h, 22, 7), "{h}");
        }
        for h in [7, 8, 12, 21] {
            assert!(!in_window(h, 22, 7), "{h}");
        }
        // Same-day window, 1 -> 5
        assert!(in_window(1, 1, 5) && in_window(4, 1, 5));
        assert!(!in_window(5, 1, 5) && !in_window(0, 1, 5) && !in_window(23, 1, 5));
        // start == end: all day
        assert!(in_window(13, 9, 9));
        // Out-of-range hours wrap.
        assert!(in_window(23, 46, 7));
    }

    #[test]
    fn weather_wanted_an_hour_early() {
        let cfg = Cfg::default(); // 22 -> 7
        assert!(weather_wanted(&cfg, 21));
        assert!(weather_wanted(&cfg, 2));
        assert!(!weather_wanted(&cfg, 7));
        assert!(!weather_wanted(&cfg, 20));
        assert!(!weather_wanted(&Cfg { enabled: false, ..Cfg::default() }, 23));
        let midnight = Cfg { start_hour: 0, end_hour: 6, ..Cfg::default() };
        assert!(weather_wanted(&midnight, 23));
    }

    #[test]
    fn colo_table() {
        let slc = colo_location("SLC").unwrap();
        assert_eq!(slc.place, "Salt Lake City");
        assert!((slc.lat - 40.76).abs() < 0.5 && (slc.lon + 111.9).abs() < 0.5);
        assert_eq!(colo_location(" lhr ").unwrap().place, "London");
        assert!(colo_location("SYD").unwrap().lat < 0.0);
        assert_eq!(colo_location("XXX"), None);
        assert_eq!(colo_location(""), None);
        for (code, lat, lon, place) in COLOS {
            assert_eq!(code.len(), 3);
            assert!((-90.0..=90.0).contains(lat) && (-180.0..=180.0).contains(lon) && !place.is_empty());
        }
        let n = COLOS.len();
        let mut codes: Vec<&str> = COLOS.iter().map(|c| c.0).collect();
        codes.dedup();
        assert_eq!(codes.len(), n);
    }

    #[test]
    fn settings_coordinates_win() {
        let cfg = Cfg { latitude: Some(41.0), longitude: Some(-112.0), ..Cfg::default() };
        assert_eq!(location(&cfg, "LHR"), Some(Location { lat: 41.0, lon: -112.0, place: String::new() }));
        assert_eq!(location(&Cfg::default(), "DEN").unwrap().place, "Denver");
        let half = Cfg { latitude: Some(41.0), ..Cfg::default() };
        assert_eq!(location(&half, "DEN").unwrap().place, "Denver");
        let bad = Cfg { latitude: Some(141.0), longitude: Some(0.0), ..Cfg::default() };
        assert_eq!(location(&bad, ""), None);
    }

    #[test]
    fn open_meteo_reply() {
        let json = r#"{"latitude":40.764416,"longitude":-111.88908,"generationtime_ms":0.05,"utc_offset_seconds":-25200,"timezone":"America/Denver","timezone_abbreviation":"GMT-7","elevation":1290.0,
          "current_units":{"time":"iso8601","interval":"seconds","temperature_2m":"°F","weather_code":"wmo code","is_day":""},
          "current":{"time":"2026-11-17T02:15","interval":900,"temperature_2m":34.5,"weather_code":3,"is_day":0},
          "daily_units":{"time":"iso8601","temperature_2m_max":"°F","temperature_2m_min":"°F","weather_code":"wmo code"},
          "daily":{"time":["2026-11-17","2026-11-18"],"temperature_2m_max":[48.2,51.0],"temperature_2m_min":[30.1,32.4],"weather_code":[3,61]}}"#;
        let w = parse_weather(json).unwrap();
        assert_eq!((w.temp, w.code, w.is_day), (34.5, 3, false));
        assert_eq!(w.days, vec![Day { high: 48.2, low: 30.1, code: 3 }, Day { high: 51.0, low: 32.4, code: 61 }]);
        assert!(w.fetched_ms > 0);
        // Without the daily part, and without daily codes.
        let bare = r#"{"current":{"temperature_2m":-3.0,"weather_code":71,"is_day":1}}"#;
        let w = parse_weather(bare).unwrap();
        assert!(w.days.is_empty() && w.is_day && w.temp == -3.0);
        let no_codes = r#"{"current":{"temperature_2m":50,"weather_code":2,"is_day":1},"daily":{"temperature_2m_max":[60],"temperature_2m_min":[40]}}"#;
        assert_eq!(parse_weather(no_codes).unwrap().days, vec![Day { high: 60.0, low: 40.0, code: 2 }]);
        assert_eq!(parse_weather(r#"{"error":true,"reason":"Latitude must be in range of -90 to 90°."}"#), Err("Latitude must be in range of -90 to 90°.".into()));
        assert!(parse_weather("").is_err());
        assert!(parse_weather(r#"{"current":{}}"#).is_err());
    }

    #[test]
    fn weather_codes_have_symbols() {
        assert_eq!(weather_symbol(0, true), "sun.max.fill");
        assert_eq!(weather_symbol(0, false), "moon.stars.fill");
        assert_eq!(weather_symbol(61, false), "cloud.rain.fill");
        assert_eq!(weather_symbol(73, false), "cloud.snow.fill");
        assert_eq!(weather_symbol(1234, false), "thermometer.medium");
        for code in [0, 1, 2, 3, 45, 48, 51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 71, 73, 75, 77, 80, 81, 82, 85, 86, 95, 96, 99] {
            assert!(!weather_text(code).is_empty(), "{code}");
            assert_ne!(weather_symbol(code, false), "thermometer.medium", "{code}");
        }
    }

    #[test]
    fn clock_and_date() {
        assert_eq!(clock12(0, 5), ("12:05".into(), "AM"));
        assert_eq!(clock12(2, 14), ("2:14".into(), "AM"));
        assert_eq!(clock12(12, 0), ("12:00".into(), "PM"));
        assert_eq!(clock12(23, 59), ("11:59".into(), "PM"));
        assert_eq!(sample().time.date_line(), "Tuesday, November 17");
        // localtime_r round trip: a minute later is a minute later.
        let now = now_secs();
        let (a, b) = (local(now), local(now + 60));
        assert!(a.month >= 1 && a.month <= 12 && a.day >= 1 && a.hour < 24 && a.minute < 60);
        assert!(b.minute == (a.minute + 1) % 60);
    }

    fn event(title: &str, start_s: i64, all_day: bool) -> crate::calendar::Event {
        crate::calendar::Event { title: title.into(), start_ms: start_s as u64 * 1000, end_ms: start_s as u64 * 1000 + 1_800_000, all_day, ..Default::default() }
    }

    /// The unix second of a local wall-clock time (via mktime).
    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = month as i32 - 1;
        tm.tm_mday = day as i32;
        tm.tm_hour = hour as i32;
        tm.tm_min = minute as i32;
        tm.tm_isdst = -1;
        unsafe { libc::mktime(&mut tm) as i64 }
    }

    #[test]
    fn next_event_for_the_morning_ahead() {
        // 11 PM on the 16th: tomorrow is the 17th.
        let late = at(2026, 11, 16, 23, 0) as u64 * 1000;
        let events = [
            event("Tonight", at(2026, 11, 16, 23, 30), false),
            event("Holiday", at(2026, 11, 17, 0, 0), true),
            event("Lunch", at(2026, 11, 17, 12, 0), false),
            event("Standup", at(2026, 11, 17, 9, 30), false),
            event("Day after", at(2026, 11, 18, 8, 0), false),
        ];
        assert_eq!(next_event(&events, late), Some(NextEvent { title: "Standup".into(), day: "Tomorrow", when: "9:30 AM".into() }));
        // 2:14 AM on the 17th: that's "today".
        let early = at(2026, 11, 17, 2, 14) as u64 * 1000;
        assert_eq!(next_event(&events, early).map(|e| (e.title, e.day)), Some(("Standup".into(), "Today")));
        // Only an all-day event.
        assert_eq!(next_event(&events[1..2], early).map(|e| (e.title, e.when)), Some(("Holiday".into(), "All day".into())));
        // Already started, or another day: nothing.
        let after = at(2026, 11, 17, 12, 30) as u64 * 1000;
        assert_eq!(next_event(&events[2..4], after), None);
        assert_eq!(next_event(&[], late), None);
        // The evening before, it skips past today's events.
        assert_eq!(next_event(&events[4..], at(2026, 11, 17, 21, 0) as u64 * 1000).map(|e| e.title), Some("Day after".into()));
    }
}
