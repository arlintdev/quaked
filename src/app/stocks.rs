//! Stocks: the app side (knob, touch, opening a stock).
//! Runs on the main thread with full access to App (it's a child module of app).
use super::*;
use crate::stocks::{Cmd as StocksCmd, Range, Update as StocksUpdate};

impl App {
    /// An update from crate::stocks' thread.
    pub(super) fn on_stocks(&mut self, u: StocksUpdate) {
        match u {
            StocksUpdate::Status(st) => {
                let changed = st != self.stocks;
                self.stocks = st;
                if self.page == PageId::Stocks {
                    self.tile = self.tile.min(self.stocks.quotes.len().saturating_sub(1));
                    // The opened stock dropped out of the list: back to the tiles.
                    if let Level::Detail(sym) = &self.level {
                        if !self.stocks.quotes.iter().any(|q| &q.symbol == sym) {
                            self.level = Level::Focus;
                        }
                    }
                    if changed {
                        self.redraw();
                    }
                }
            }
        }
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn stocks_page_shown(&mut self, shown: bool) {
        let _ = self.stocks_cmd.send(StocksCmd::PageShown(shown));
    }

    fn stocks_range(&self) -> Range {
        match &self.level {
            Level::Detail(sym) => self.stocks.chart.as_ref().filter(|c| &c.symbol == sym).map_or(Range::Day, |c| c.range),
            _ => Range::Day,
        }
    }

    fn stocks_set_range(&mut self, range: Range) {
        if let Level::Detail(sym) = &self.level {
            let _ = self.stocks_cmd.send(StocksCmd::Chart(Some((sym.clone(), range))));
        }
    }

    fn stocks_open(&mut self, i: usize) {
        let Some(q) = self.stocks.quotes.get(i).filter(|q| q.error.is_none()) else { return };
        self.tile = i;
        self.level = Level::Detail(q.symbol.clone());
        let _ = self.stocks_cmd.send(StocksCmd::Chart(Some((q.symbol.clone(), Range::Day))));
    }

    /// Knob turned (past the top level): pick a stock, or change the opened one's range.
    pub(super) fn stocks_turn(&mut self, d: i32) {
        if let Level::Detail(_) = self.level {
            let k = Range::ALL.iter().position(|r| *r == self.stocks_range()).unwrap_or(0) as i32;
            let next = Range::ALL[(k + d).clamp(0, Range::ALL.len() as i32 - 1) as usize];
            if next != self.stocks_range() {
                self.stocks_set_range(next);
            }
            return;
        }
        let n = self.stocks.quotes.len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
    }

    /// Knob pressed (past the top level): open the stock; on an opened one, its Yahoo page.
    pub(super) fn stocks_press(&mut self) {
        match &self.level {
            Level::Detail(sym) => open_target(&format!("https://finance.yahoo.com/quote/{}", sym.replace('^', "%5E"))),
            _ => self.stocks_open(self.tile),
        }
    }

    /// Hit::Feature(_, i): a tile, a range pill or Back.
    pub(super) fn stocks_tap(&mut self, i: usize) {
        use crate::render::stocks::{TAP_BACK, TAP_RANGE};
        if i == TAP_BACK {
            self.level = Level::Focus;
            let _ = self.stocks_cmd.send(StocksCmd::Chart(None));
        } else if i >= TAP_RANGE {
            if let Some(r) = Range::ALL.get(i - TAP_RANGE) {
                self.stocks_set_range(*r);
            }
        } else if self.level != Level::Top && self.tile == i {
            self.stocks_open(i);
        } else {
            self.tile = i;
            self.level = Level::Focus;
        }
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn stocks_json(&self) -> serde_json::Value {
        serde_json::json!({
            "symbols": self.stocks.quotes.iter().map(|q| q.symbol.clone()).collect::<Vec<_>>(),
            // For the settings page's list: whether each symbol was found, and what it is.
            "quotes": self.stocks.quotes.iter().map(|q| serde_json::json!({"symbol": q.symbol, "name": q.name, "price": q.price, "changePct": q.change_pct(), "error": q.error})).collect::<Vec<_>>(),
            "loaded": self.stocks.loaded,
            "errors": self.stocks.quotes.iter().filter(|q| q.error.is_some()).count(),
        })
    }
}
