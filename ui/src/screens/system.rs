//! System: the model and the chip, the functions the agent wrote, and what it has learned.
use crate::widgets::{card, line, quiet, str_of};
use rune::El;
use rune::col;
use rune::tok::Theme;

pub fn view(st: &serde_json::Value, t: Theme) -> El {
    let mut cards = col().key("cards").w_full().gap(12.0);
    // the model and the chip
    let mut model = col().gap(4.0);
    for m in st["models"].as_array().cloned().unwrap_or_default() {
        model = model.child(line(str_of(&m), t));
    }
    model = model.child(quiet(str_of(&st["prio"]), t).w_full().wrap());
    cards = cards.child(card("model", "Model and chip", model, t));

    // functions the agent wrote
    let mut fns = col().gap(4.0);
    for f in st["functions"].as_array().cloned().unwrap_or_default() {
        fns = fns.child(line(str_of(&f), t));
    }
    cards = cards.child(card("functions", "Functions", fns, t));
    cards.child(quiet(str_of(&st["learning"]), t).w_full().wrap())
}
