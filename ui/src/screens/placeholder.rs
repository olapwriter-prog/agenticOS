//! The tabs that are not built yet: a card with the tab's name, so the bar works end to end.
use crate::widgets::{card, quiet};
use rune::El;
use rune::col;
use rune::tok::Theme;

pub fn view(key: &str, title: &str, t: Theme) -> El {
    col()
        .key(key)
        .w_full()
        .child(card("placeholder", title, quiet("Coming in the next version.", t), t))
}
