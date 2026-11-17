//! Stocks: prices for your symbols from Yahoo Finance's chart endpoint (no account or key;
//! only the symbols are sent).
//!
//! Per symbol the thread keeps today's 5-minute prices (refreshed every `refreshSeconds`
//! while the Stocks page is on screen, every 15 minutes otherwise) and a year of daily
//! closes (every 30 minutes), which 1M, 6M, YTD and 1Y are cut from. 5D (15-minute steps)
//! and 5Y (weekly) are fetched when you open a stock.

use crate::config::StocksSettings as Cfg;
use serde_json::Value;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// (unix seconds, price), oldest first.
pub type Series = Vec<(u64, f32)>;

const DAY: u64 = 86_400;

/// One symbol's quote, today's prices and a year of daily closes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quote {
    pub symbol: String,
    pub name: String,
    pub currency: String,
    pub exchange: String,
    pub price: f64,
    /// Yesterday's close (today's change is from here).
    pub prev_close: f64,
    pub day_high: f64,
    pub day_low: f64,
    pub high_52w: f64,
    pub low_52w: f64,
    pub volume: u64,
    /// When the price is from (unix seconds).
    pub market_time: u64,
    /// The market is in its regular session now (crypto always).
    pub open: bool,
    /// Today's prices (5-minute steps).
    pub series: Series,
    /// A year of daily closes.
    pub history: Series,
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

    /// The prices over `range` and where the range started (yesterday's close for a day,
    /// else its first price). 5D and 5Y come from `extra` once fetched; until then the
    /// year of daily closes stands in.
    pub fn over(&self, range: Range, extra: &[Chart], now: u64) -> (Series, f64) {
        let fetched = || extra.iter().find(|c| c.symbol == self.symbol && c.range == range && !c.points.is_empty()).map(|c| c.points.clone());
        let since = |from: u64| -> Series { self.history.iter().copied().filter(|(t, _)| *t >= from).collect() };
        let points = match range {
            Range::Day => return (self.series.clone(), self.prev_close),
            Range::Week => fetched().unwrap_or_else(|| self.history.iter().rev().take(6).rev().copied().collect()),
            Range::Month => since(now.saturating_sub(30 * DAY)),
            Range::SixMonth => since(now.saturating_sub(182 * DAY)),
            Range::Ytd => since(year_start(now)),
            Range::Year => self.history.clone(),
            Range::FiveYear => fetched().unwrap_or_else(|| self.history.clone()),
        };
        let base = points.first().map_or(0.0, |p| p.1 as f64);
        (points, base)
    }

    /// The change over `range` in percent (to the current price), if there's enough data.
    pub fn change_over(&self, range: Range, extra: &[Chart], now: u64) -> Option<f64> {
        if range == Range::Day {
            return (self.prev_close > 0.0).then(|| self.change_pct());
        }
        let (points, base) = self.over(range, extra, now);
        (points.len() >= 2 && base > 0.0).then(|| (self.price - base) / base * 100.0)
    }
}

/// Midnight on January 1st of `now`'s year, local time (unix seconds).
fn year_start(now: u64) -> u64 {
    let mut tm = crate::calendar::local(now * 1000);
    tm.tm_mon = 0;
    tm.tm_mday = 1;
    tm.tm_hour = 0;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    t.max(0) as u64
}

/// A time scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    SixMonth,
    Ytd,
    Year,
    FiveYear,
}

impl Range {
    pub const ALL: [Range; 7] = [Range::Day, Range::Week, Range::Month, Range::SixMonth, Range::Ytd, Range::Year, Range::FiveYear];
    /// The four the opened stock shows side by side.
    pub const PANES: [Range; 4] = [Range::Day, Range::Week, Range::Month, Range::Year];
    pub fn label(self) -> &'static str {
        match self {
            Range::Day => "1D",
            Range::Week => "5D",
            Range::Month => "1M",
            Range::SixMonth => "6M",
            Range::Ytd => "YTD",
            Range::Year => "1Y",
            Range::FiveYear => "5Y",
        }
    }
    pub fn from_label(s: &str) -> Option<Range> {
        Range::ALL.into_iter().find(|r| r.label() == s)
    }
    /// The ranges fetched on their own when a stock opens: Yahoo's (range, interval), and
    /// how long a fetch stays fresh.
    fn own_fetch(self) -> Option<(&'static str, &'static str, Duration)> {
        match self {
            Range::Week => Some(("5d", "15m", Duration::from_secs(5 * 60))),
            Range::FiveYear => Some(("5y", "1wk", Duration::from_secs(6 * 3600))),
            _ => None,
        }
    }
}

/// A separately fetched range (5D, 5Y) for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub symbol: String,
    pub range: Range,
    pub points: Series,
    pub loading: bool,
    /// When it was fetched (unix ms).
    pub at_ms: u64,
}

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub quotes: Vec<Quote>,
    /// 5D and 5Y charts fetched for opened stocks.
    pub extra: Vec<Chart>,
    /// The first round finished.
    pub loaded: bool,
    /// When the last round finished (unix ms).
    pub updated_ms: u64,
    /// Turned off in the settings.
    pub off: bool,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// The Stocks page was shown / left.
    PageShown(bool),
    /// A stock was opened: fetch its 5D and 5Y charts if they're missing or stale.
    Open(String),
}

#[derive(Debug)]
pub enum Update {
    Status(Status),
}

const QUIET: Duration = Duration::from_secs(15 * 60);
const HISTORY_EVERY: u64 = 30 * 60 * 1000;

pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("stocks".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn symbols(cfg: &Cfg) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in cfg.symbols.iter().map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()) {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.truncate(24);
    out
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut st = Status::default();
    let mut shown = false;
    let mut next = Instant::now();
    // When each symbol's year of closes was last fetched (unix ms).
    let mut history_at: std::collections::HashMap<String, u64> = Default::default();
    let mut open: Option<String> = None;
    loop {
        if Instant::now() >= next {
            if cfg.enabled {
                let now_ms = crate::status::now_ms();
                st.off = false;
                let old = std::mem::take(&mut st.quotes);
                st.quotes = symbols(&cfg)
                    .into_iter()
                    .map(|s| {
                        let mut q = fetch(&s, "1d", "5m").unwrap_or_else(|e| Quote { symbol: s.clone(), error: Some(e), ..Default::default() });
                        let stale = history_at.get(&s).is_none_or(|t| now_ms.saturating_sub(*t) > HISTORY_EVERY);
                        let kept = old.iter().find(|o| o.symbol == q.symbol || o.symbol == s).map(|o| o.history.clone()).unwrap_or_default();
                        if q.error.is_none() && (stale || kept.is_empty()) {
                            if let Ok(y) = fetch(&s, "1y", "1d") {
                                q.history = y.series;
                                history_at.insert(s.clone(), now_ms);
                            } else {
                                q.history = kept;
                            }
                        } else {
                            q.history = kept;
                        }
                        q
                    })
                    .collect();
                st.extra.retain(|c| st.quotes.iter().any(|q| q.symbol == c.symbol));
                st.loaded = true;
                st.updated_ms = now_ms;
            } else {
                st = Status { off: true, loaded: true, ..Default::default() };
            }
            emit(Update::Status(st.clone()));
            next = Instant::now() + if shown { Duration::from_secs(cfg.refresh_seconds.max(15)) } else { QUIET };
        }
        if let Some(symbol) = open.take() {
            for range in [Range::Week, Range::FiveYear] {
                let Some((r, i, fresh)) = range.own_fetch() else { continue };
                let now_ms = crate::status::now_ms();
                let have = st.extra.iter().any(|c| c.symbol == symbol && c.range == range && !c.loading && now_ms.saturating_sub(c.at_ms) < fresh.as_millis() as u64);
                if have {
                    continue;
                }
                st.extra.retain(|c| !(c.symbol == symbol && c.range == range));
                st.extra.push(Chart { symbol: symbol.clone(), range, points: Vec::new(), loading: true, at_ms: now_ms });
                emit(Update::Status(st.clone()));
                let points = fetch(&symbol, r, i).map(|q| q.series).unwrap_or_default();
                if let Some(c) = st.extra.iter_mut().find(|c| c.symbol == symbol && c.range == range) {
                    c.points = points;
                    c.loading = false;
                    c.at_ms = crate::status::now_ms();
                }
                emit(Update::Status(st.clone()));
            }
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
            }
            Ok(Cmd::Open(symbol)) => open = Some(symbol),
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

fn fetch(symbol: &str, range: &str, interval: &str) -> Result<Quote, String> {
    get(symbol, range, interval).and_then(|v| parse_quote(&v, crate::status::now_ms() / 1000))
}

/// A chart answer as a quote (its prices as the series). `now` in unix seconds.
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
    let times = r["timestamp"].as_array().cloned().unwrap_or_default();
    let closes = r["indicators"]["quote"][0]["close"].as_array().cloned().unwrap_or_default();
    let series: Series = times.iter().zip(closes.iter()).filter_map(|(t, c)| Some((t.as_u64()?, c.as_f64()? as f32))).collect();
    let name = [text("shortName"), text("longName")].into_iter().find(|n| !n.is_empty()).unwrap_or_default();
    let prev = if num("previousClose") > 0.0 { num("previousClose") } else { num("chartPreviousClose") };
    let exchange = [text("fullExchangeName"), text("exchangeName")].into_iter().find(|n| !n.is_empty()).unwrap_or_default();
    Ok(Quote {
        symbol: text("symbol"),
        name,
        currency: text("currency"),
        exchange,
        price,
        prev_close: prev,
        day_high: num("regularMarketDayHigh"),
        day_low: num("regularMarketDayLow"),
        high_52w: num("fiftyTwoWeekHigh"),
        low_52w: num("fiftyTwoWeekLow"),
        volume: m["regularMarketVolume"].as_u64().unwrap_or(0),
        market_time: m["regularMarketTime"].as_u64().unwrap_or(0),
        open,
        series,
        history: Vec::new(),
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
    let now = crate::status::now_ms() / 1000;
    // A wandering walk from `from` to `to` over `n` points ending now, `step` seconds apart.
    let walk = |from: f32, to: f32, n: usize, step: u64, wiggle: f32| -> Series {
        (0..n)
            .map(|i| {
                let f = i as f32 / (n - 1) as f32;
                let noise = ((i as f32) * 0.37).sin() * 0.6 + ((i as f32) * 1.13).sin() * 0.4;
                (now - (n - 1 - i) as u64 * step, from + (to - from) * f + noise * wiggle * from)
            })
            .collect()
    };
    let q = |symbol: &str, name: &str, price: f64, prev: f64, year_ago: f32| Quote {
        symbol: symbol.into(),
        name: name.into(),
        currency: "USD".into(),
        exchange: "NasdaqGS".into(),
        price,
        prev_close: prev,
        day_high: price.max(prev) * 1.004,
        day_low: price.min(prev) * 0.996,
        high_52w: price * 1.12,
        low_52w: year_ago as f64 * 0.9,
        volume: 182_400_000,
        market_time: now,
        open: true,
        series: walk(prev as f32, price as f32, 78, 300, 0.002),
        history: walk(year_ago, price as f32, 251, DAY + DAY / 3, 0.03),
        error: None,
    };
    let quotes = vec![
        q("NVDA", "NVIDIA Corporation", 155.46, 156.14, 118.0),
        q("TQQQ", "ProShares UltraPro QQQ", 51.81, 53.03, 61.0),
        q("^GSPC", "S&P 500", 7340.53, 7416.58, 5980.0),
        q("BTC-USD", "Bitcoin USD", 64317.17, 64830.0, 91000.0),
        q("AAPL", "Apple Inc.", 351.91, 352.78, 228.0),
        Quote { symbol: "NOPE".into(), error: Some("No data found, symbol may be delisted".into()), ..Default::default() },
    ];
    let extra = vec![Chart { symbol: "NVDA".into(), range: Range::Week, points: walk(149.0, 155.46, 130, 900 * 2, 0.008), loading: false, at_ms: now * 1000 }];
    Status { quotes, extra, loaded: true, updated_ms: now * 1000, off: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_chart_answer() {
        let v: Value = serde_json::from_str(r#"{"chart":{"result":[{"meta":{"symbol":"AAPL","currency":"USD","shortName":"Apple Inc.","fullExchangeName":"NasdaqGS","regularMarketPrice":351.91,"previousClose":352.78,"regularMarketDayHigh":355.36,"regularMarketDayLow":349.67,"fiftyTwoWeekHigh":360.0,"fiftyTwoWeekLow":200.5,"regularMarketVolume":1234,"regularMarketTime":1794949201,"instrumentType":"EQUITY","currentTradingPeriod":{"regular":{"start":1794925800,"end":1794949200}}},"timestamp":[1,2,3],"indicators":{"quote":[{"close":[352.4,null,351.9]}]}}],"error":null}}"#).unwrap();
        let q = parse_quote(&v, 1794930000).unwrap();
        assert_eq!((q.symbol.as_str(), q.name.as_str(), q.price, q.prev_close, q.exchange.as_str()), ("AAPL", "Apple Inc.", 351.91, 352.78, "NasdaqGS"));
        assert_eq!((q.high_52w, q.low_52w, q.volume), (360.0, 200.5, 1234));
        assert!(q.open);
        assert_eq!(q.series, [(1, 352.4), (3, 351.9)]);
        assert!((q.change_pct() + 0.2466).abs() < 1e-3);
        assert!(!parse_quote(&v, 1794950000).unwrap().open, "after the close");
        let found: Value = serde_json::from_str(r#"{"quotes":[{"symbol":"AAPL","shortname":"Apple Inc.","exchDisp":"NASDAQ","typeDisp":"Equity"},{"exchDisp":"x"}]}"#).unwrap();
        assert_eq!(parse_search(&found), [("AAPL".to_string(), "Apple Inc.".to_string(), "NASDAQ".to_string(), "Equity".to_string())]);
        let bad: Value = serde_json::from_str(r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#).unwrap();
        assert_eq!(parse_quote(&bad, 0), Err("No data found, symbol may be delisted".into()));
    }

    #[test]
    fn ranges_cut_from_the_year() {
        let now = 1_800_000_000u64;
        // A year of daily closes rising 100 -> 200.
        let history: Series = (0..365u64).map(|d| (now - (364 - d) * DAY, 100.0 + d as f32 * (100.0 / 364.0))).collect();
        let q = Quote { symbol: "X".into(), price: 200.0, prev_close: 199.0, history, series: vec![(now - 600, 199.5), (now, 200.0)], ..Default::default() };
        let (m, base) = q.over(Range::Month, &[], now);
        assert!((29..=31).contains(&m.len()));
        assert!((base - 191.8).abs() < 1.0);
        assert!((q.change_over(Range::Year, &[], now).unwrap() - 100.0).abs() < 0.1);
        assert!((q.change_over(Range::Day, &[], now).unwrap() - 0.5025).abs() < 1e-3);
        assert_eq!(q.over(Range::Week, &[], now).0.len(), 6, "until 5D is fetched, the last 6 closes");
        let fetched = Chart { symbol: "X".into(), range: Range::Week, points: vec![(1, 190.0), (2, 200.0)], loading: false, at_ms: 0 };
        assert_eq!(q.over(Range::Week, std::slice::from_ref(&fetched), now), (vec![(1, 190.0), (2, 200.0)], 190.0));
        assert_eq!(Range::from_label("YTD"), Some(Range::Ytd));
        assert!(year_start(now) <= now && now - year_start(now) < 366 * DAY);
    }
}
