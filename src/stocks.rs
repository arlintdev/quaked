//! Stocks: prices for your symbols from Yahoo Finance's chart endpoint (no account or key;
//! only the symbols are sent). The thread refreshes them every `refreshSeconds` while the
//! Stocks page is on screen, every 15 minutes otherwise, and fetches a longer chart (5 days,
//! a month, a year) for the stock you open.

use crate::config::StocksSettings as Cfg;
use serde_json::Value;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// One symbol's quote and today's chart.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quote {
    pub symbol: String,
    pub name: String,
    pub currency: String,
    pub price: f64,
    /// Yesterday's close (today's change is from here).
    pub prev_close: f64,
    pub day_high: f64,
    pub day_low: f64,
    /// When the price is from (unix seconds).
    pub market_time: u64,
    /// The market is in its regular session now (crypto always).
    pub open: bool,
    /// Today's prices, oldest first (5-minute steps).
    pub series: Vec<f32>,
    /// Why there's no quote ("No data found, symbol may be delisted").
    pub error: Option<String>,
}

impl Quote {
    pub fn change(&self) -> f64 {
        self.price - self.prev_close
    }
    pub fn change_pct(&self) -> f64 {
        if self.prev_close == 0.0 {
            0.0
        } else {
            self.change() / self.prev_close * 100.0
        }
    }
}

/// A longer chart for the stock that's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    Year,
}

impl Range {
    pub const ALL: [Range; 4] = [Range::Day, Range::Week, Range::Month, Range::Year];
    pub fn label(self) -> &'static str {
        match self {
            Range::Day => "1D",
            Range::Week => "5D",
            Range::Month => "1M",
            Range::Year => "1Y",
        }
    }
    /// Yahoo's (range, interval).
    fn query(self) -> (&'static str, &'static str) {
        match self {
            Range::Day => ("1d", "5m"),
            Range::Week => ("5d", "30m"),
            Range::Month => ("1mo", "1d"),
            Range::Year => ("1y", "1wk"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub symbol: String,
    pub range: Range,
    pub points: Vec<f32>,
    /// Where the range started (its first price, or yesterday's close for a day).
    pub base: f64,
    pub loading: bool,
}

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub quotes: Vec<Quote>,
    /// The first round finished.
    pub loaded: bool,
    /// When the last round finished (unix ms).
    pub updated_ms: u64,
    pub chart: Option<Chart>,
    /// Turned off in the settings.
    pub off: bool,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// The Stocks page was shown / left.
    PageShown(bool),
    /// Open (or close, None) the longer chart for a symbol.
    Chart(Option<(String, Range)>),
}

#[derive(Debug)]
pub enum Update {
    Status(Status),
}

const QUIET: Duration = Duration::from_secs(15 * 60);

pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("stocks".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut st = Status::default();
    let mut shown = false;
    let mut next = Instant::now();
    let mut want_chart: Option<(String, Range)> = None;
    loop {
        if Instant::now() >= next {
            if cfg.enabled {
                st.off = false;
                st.quotes = cfg.symbols.iter().map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).take(24).map(|s| fetch_quote(&s)).collect();
                st.loaded = true;
                st.updated_ms = crate::status::now_ms();
            } else {
                st = Status { off: true, loaded: true, ..Default::default() };
            }
            emit(Update::Status(st.clone()));
            let every = if shown { Duration::from_secs(cfg.refresh_seconds.max(15)) } else { QUIET };
            next = Instant::now() + every;
        }
        if let Some((symbol, range)) = want_chart.take() {
            st.chart = Some(Chart { symbol: symbol.clone(), range, points: Vec::new(), base: 0.0, loading: true });
            emit(Update::Status(st.clone()));
            st.chart = Some(fetch_chart(&symbol, range));
            emit(Update::Status(st.clone()));
        }
        match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Configure(c)) => {
                if c != cfg {
                    cfg = c;
                    next = Instant::now();
                }
            }
            Ok(Cmd::PageShown(s)) => {
                // Fresh prices when the page opens, if the last round is a minute old.
                if s && !shown && crate::status::now_ms().saturating_sub(st.updated_ms) > 60_000 {
                    next = Instant::now();
                }
                shown = s;
                if !s && st.chart.take().is_some() {
                    emit(Update::Status(st.clone()));
                }
            }
            Ok(Cmd::Chart(c)) => match c {
                Some(want) => want_chart = Some(want),
                None => {
                    if st.chart.take().is_some() {
                        emit(Update::Status(st.clone()));
                    }
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn get(symbol: &str, range: &str, interval: &str) -> Result<Value, String> {
    // Symbols go in the path: keep them to what tickers are made of.
    let clean: String = symbol.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '^' | '-' | '.' | '=')).collect();
    let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{}?range={range}&interval={interval}", clean.replace('^', "%5E"));
    let out = Command::new("/usr/bin/curl").args(["-s", "--max-time", "10", "-A", "Mozilla/5.0 (Macintosh)", &url]).output().map_err(|e| e.to_string())?;
    if out.stdout.is_empty() {
        return Err("no answer from Yahoo Finance".into());
    }
    serde_json::from_slice(&out.stdout).map_err(|_| "unexpected answer from Yahoo Finance".into())
}

fn fetch_quote(symbol: &str) -> Quote {
    match get(symbol, "1d", "5m").and_then(|v| parse_quote(&v, crate::status::now_ms() / 1000)) {
        Ok(q) => q,
        Err(e) => Quote { symbol: symbol.to_string(), error: Some(e), ..Default::default() },
    }
}

fn fetch_chart(symbol: &str, range: Range) -> Chart {
    let (r, i) = range.query();
    let parsed = get(symbol, r, i).and_then(|v| parse_quote(&v, crate::status::now_ms() / 1000));
    match parsed {
        Ok(q) => {
            let base = if range == Range::Day { q.prev_close } else { q.series.first().copied().unwrap_or(0.0) as f64 };
            Chart { symbol: symbol.to_string(), range, points: q.series, base, loading: false }
        }
        Err(_) => Chart { symbol: symbol.to_string(), range, points: Vec::new(), base: 0.0, loading: false },
    }
}

/// A chart answer as a quote (and its prices as the series). `now` in unix seconds.
pub fn parse_quote(v: &Value, now: u64) -> Result<Quote, String> {
    let chart = &v["chart"];
    if let Some(e) = chart["error"].get("description").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    let r = chart["result"].get(0).ok_or("no data")?;
    let m = &r["meta"];
    let num = |k: &str| m[k].as_f64().unwrap_or(0.0);
    let text = |k: &str| m[k].as_str().unwrap_or("").to_string();
    let price = num("regularMarketPrice");
    if price == 0.0 {
        return Err("no price".into());
    }
    let regular = &m["currentTradingPeriod"]["regular"];
    let (start, end) = (regular["start"].as_u64().unwrap_or(0), regular["end"].as_u64().unwrap_or(0));
    let open = text("instrumentType") == "CRYPTOCURRENCY" || (start <= now && now < end);
    let series: Vec<f32> = r["indicators"]["quote"][0]["close"].as_array().map(|a| a.iter().filter_map(Value::as_f64).map(|x| x as f32).collect()).unwrap_or_default();
    let name = [text("shortName"), text("longName")].into_iter().find(|n| !n.is_empty()).unwrap_or_default();
    let prev = if num("previousClose") > 0.0 { num("previousClose") } else { num("chartPreviousClose") };
    Ok(Quote {
        symbol: text("symbol"),
        name,
        currency: text("currency"),
        price,
        prev_close: prev,
        day_high: num("regularMarketDayHigh"),
        day_low: num("regularMarketDayLow"),
        market_time: m["regularMarketTime"].as_u64().unwrap_or(0),
        open,
        series,
        error: None,
    })
}

/// Symbols matching a company name or ticker ("apple" -> AAPL), from Yahoo Finance's search:
/// (symbol, name, exchange, kind).
pub fn search(q: &str) -> Vec<(String, String, String, String)> {
    let q: String = q.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '^' | '-' | '.' | '=' | '&')).take(40).collect();
    if q.trim().is_empty() {
        return Vec::new();
    }
    let url = format!("https://query1.finance.yahoo.com/v1/finance/search?q={}&quotesCount=8&newsCount=0", q.trim().replace(' ', "%20").replace('^', "%5E").replace('&', "%26"));
    let Ok(out) = Command::new("/usr/bin/curl").args(["-s", "--max-time", "8", "-A", "Mozilla/5.0 (Macintosh)", &url]).output() else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return Vec::new() };
    parse_search(&v)
}

pub fn parse_search(v: &Value) -> Vec<(String, String, String, String)> {
    v["quotes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let symbol = x["symbol"].as_str()?.to_string();
                    let name = x["shortname"].as_str().or(x["longname"].as_str()).unwrap_or("").to_string();
                    let exch = x["exchDisp"].as_str().unwrap_or("").to_string();
                    let kind = x["typeDisp"].as_str().unwrap_or("").to_string();
                    Some((symbol, name, exch, kind))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Sample data for `quaked preview`.
pub fn sample() -> Status {
    let wave = |base: f32, drift: f32, n: usize| (0..n).map(|i| base + drift * i as f32 / n as f32 + ((i as f32) * 0.45).sin() * base * 0.002).collect::<Vec<f32>>();
    let q = |symbol: &str, name: &str, price: f64, prev: f64, open: bool| Quote {
        symbol: symbol.into(),
        name: name.into(),
        currency: "USD".into(),
        price,
        prev_close: prev,
        day_high: price.max(prev) * 1.004,
        day_low: price.min(prev) * 0.996,
        market_time: crate::status::now_ms() / 1000,
        open,
        series: wave(prev as f32, (price - prev) as f32, 78),
        error: None,
    };
    let quotes = vec![
        q("^GSPC", "S&P 500", 6812.40, 6774.12, true),
        q("^IXIC", "NASDAQ Composite", 22541.83, 22610.55, true),
        q("^DJI", "Dow Jones Industrial Average", 47205.10, 46990.33, true),
        q("BTC-USD", "Bitcoin USD", 64484.17, 65912.00, true),
        q("AAPL", "Apple Inc.", 351.91, 352.78, true),
        q("NVDA", "NVIDIA Corporation", 212.45, 205.18, true),
        Quote { symbol: "NOPE".into(), error: Some("No data found, symbol may be delisted".into()), ..Default::default() },
    ];
    let chart = Chart { symbol: "^GSPC".into(), range: Range::Month, points: wave(6600.0, 212.4, 22), base: 6600.0, loading: false };
    Status { quotes, loaded: true, updated_ms: crate::status::now_ms(), chart: Some(chart), off: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_chart_answer() {
        let v: Value = serde_json::from_str(r#"{"chart":{"result":[{"meta":{"symbol":"AAPL","currency":"USD","shortName":"Apple Inc.","regularMarketPrice":351.91,"previousClose":352.78,"regularMarketDayHigh":355.36,"regularMarketDayLow":349.67,"regularMarketTime":1794949201,"instrumentType":"EQUITY","currentTradingPeriod":{"regular":{"start":1794925800,"end":1794949200}}},"timestamp":[1,2,3],"indicators":{"quote":[{"close":[352.4,null,351.9]}]}}],"error":null}}"#).unwrap();
        let q = parse_quote(&v, 1794930000).unwrap();
        assert_eq!((q.symbol.as_str(), q.name.as_str(), q.price, q.prev_close), ("AAPL", "Apple Inc.", 351.91, 352.78));
        assert!(q.open);
        assert_eq!(q.series, [352.4, 351.9]);
        assert!((q.change() + 0.87).abs() < 1e-9);
        assert!((q.change_pct() + 0.2466).abs() < 1e-3);
        assert!(!parse_quote(&v, 1794950000).unwrap().open, "after the close");
        let found: Value = serde_json::from_str(r#"{"quotes":[{"symbol":"AAPL","shortname":"Apple Inc.","exchDisp":"NASDAQ","typeDisp":"Equity"},{"exchDisp":"x"}]}"#).unwrap();
        assert_eq!(parse_search(&found), [("AAPL".to_string(), "Apple Inc.".to_string(), "NASDAQ".to_string(), "Equity".to_string())]);
        let bad: Value = serde_json::from_str(r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#).unwrap();
        assert_eq!(parse_quote(&bad, 0), Err("No data found, symbol may be delisted".into()));
    }
}
