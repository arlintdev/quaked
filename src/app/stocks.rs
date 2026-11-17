//! Stocks: the app side (knob, touch, opening a stock and its time scales).
//! Runs on the main thread with full access to App (it's a child module of app).
//!
//! Tiles (Focus, `tile` = the stock) → press → the four scales (Detail("NVDA"),
//! `scroll` = the pane) → press → one scale big (Detail("NVDA@1M")), where turning walks
//! 1D … 5Y and pressing opens Yahoo Finance. Double-click goes back one step.
use super::*;
use crate::render::stocks::{opened, TAP_BACK, TAP_PANE, TAP_RANGE};
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
                    if let Some((sym, _)) = opened(&self.level) {
                        if !self.stocks.quotes.iter().any(|q| q.symbol == sym) {
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

    fn stocks_open(&mut self, i: usize) {
        let Some(q) = self.stocks.quotes.get(i).filter(|q| q.error.is_none()) else { return };
        self.tile = i;
        self.scroll = 0;
        self.level = Level::Detail(q.symbol.clone());
        let _ = self.stocks_cmd.send(StocksCmd::Open(q.symbol.clone()));
    }

    fn stocks_expand(&mut self, range: Range) {
        if let Some((sym, _)) = opened(&self.level) {
            self.level = Level::Detail(format!("{sym}@{}", range.label()));
        }
    }

    /// Double-click on the page: one scale big → the four; returns false to let the usual
    /// back (four scales → tiles → top) happen.
    pub(super) fn stocks_back(&mut self) -> bool {
        match opened(&self.level) {
            Some((sym, Some(r))) => {
                self.scroll = Range::PANES.iter().position(|p| *p == r).unwrap_or(0);
                self.level = Level::Detail(sym.to_string());
                true
            }
            _ => false,
        }
    }

    /// Knob turned (past the top level).
    pub(super) fn stocks_turn(&mut self, d: i32) {
        match opened(&self.level) {
            Some((_, None)) => self.scroll = (self.scroll as i32 + d).clamp(0, Range::PANES.len() as i32 - 1) as usize,
            Some((_, Some(r))) => {
                let k = Range::ALL.iter().position(|x| *x == r).unwrap_or(0) as i32;
                self.stocks_expand(Range::ALL[(k + d).clamp(0, Range::ALL.len() as i32 - 1) as usize]);
            }
            None => {
                let n = self.stocks.quotes.len() as i32;
                if n > 0 {
                    self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
                }
            }
        }
    }

    /// Knob pressed (past the top level).
    pub(super) fn stocks_press(&mut self) {
        match opened(&self.level) {
            Some((_, None)) => self.stocks_expand(Range::PANES[self.scroll.min(Range::PANES.len() - 1)]),
            Some((sym, Some(_))) => open_target(&format!("https://finance.yahoo.com/quote/{}", sym.replace('^', "%5E"))),
            None => self.stocks_open(self.tile),
        }
    }

    /// Hit::Feature(_, i): a tile, a pane, a range pill or Back.
    pub(super) fn stocks_tap(&mut self, i: usize) {
        match i {
            TAP_BACK => {
                if !self.stocks_back() {
                    self.level = Level::Focus;
                }
            }
            i if i >= TAP_RANGE => {
                if let Some(r) = Range::ALL.get(i - TAP_RANGE) {
                    self.stocks_expand(*r);
                }
            }
            i if i >= TAP_PANE => {
                if let Some(r) = Range::PANES.get(i - TAP_PANE) {
                    self.scroll = i - TAP_PANE;
                    self.stocks_expand(*r);
                }
            }
            i if self.level != Level::Top && self.tile == i => self.stocks_open(i),
            i => {
                self.tile = i;
                self.level = Level::Focus;
            }
        }
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn stocks_json(&self) -> serde_json::Value {
        serde_json::json!({
            "symbols": self.stocks.quotes.iter().map(|q| q.symbol.clone()).collect::<Vec<_>>(),
            // For the settings page's list: whether each symbol was found, and what it is.
            "quotes": self.stocks.quotes.iter().map(|q| serde_json::json!({"symbol": q.symbol, "name": q.name, "price": q.price, "changePct": q.change_pct(), "error": q.error, "days": q.history.len()})).collect::<Vec<_>>(),
            "loaded": self.stocks.loaded,
            "errors": self.stocks.quotes.iter().filter(|q| q.error.is_some()).count(),
        })
    }
}
