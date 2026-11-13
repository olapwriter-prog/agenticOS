//! Voice: hands-free. An orb in the middle with rings that grow while it listens, the last thing
//! heard as a caption, and the newest answer rising from the bottom as one short card.
//! The phone's speech input is not wired to this client yet: the orb and the microphone show
//! the state, and the caption says so.
use super::{accent_of, headline, turns};
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, icon, row};
use serde_json::Value;

pub fn view(s: &State, st: &Value) -> El {
    let all = turns(s, st);
    let last = all.last();
    let listening = s.voice_on;
    let thinking = last.is_some_and(|t| t.thinking);
    let (state, caption) = if listening {
        (
            "LISTENING",
            "Speak now. Speech input arrives with the phone's microphone service.".to_string(),
        )
    } else if thinking {
        (
            "THINKING",
            last.map(|t| format!("“{}”", t.q)).unwrap_or_default(),
        )
    } else if let Some(t) = last {
        ("ANSWER", format!("“{}”", t.q))
    } else {
        ("READY", "Tap the orb or the microphone to talk".to_string())
    };
    let k = if listening {
        1.08
    } else if thinking {
        0.96
    } else {
        1.0
    };

    let ring = |d: f32, a: f32| {
        el().size(d, d)
            .radius(d / 2.0)
            .border(1.0, VIOLET.alpha(a))
            .scale(k)
            .absolute((300.0 - d) / 2.0, (300.0 - d) / 2.0)
    };
    let orb = row()
        .key("voice-orb")
        .size(180.0, 180.0)
        .center()
        .radius(90.0)
        .fill(ACCENT_FILL)
        .shadow(18.0, 48.0, ACCENT_FILL.alpha(0.45))
        .scale(k)
        .absolute(60.0, 60.0)
        .role("button")
        .label(if listening {
            "Stop listening"
        } else {
            "Start listening"
        })
        .press_scale(0.96)
        .child(icon(&icons::MIC, 46.0).color(rune::Color::WHITE))
        .on_press(|s: &mut State| s.voice_on = !s.voice_on);
    let stage = row()
        .size(300.0, 300.0)
        .child(ring(300.0, 0.1))
        .child(ring(260.0, 0.2))
        .child(ring(220.0, 0.3))
        .child(orb);

    let mut page = col()
        .key("mode-voice-view")
        .w_full()
        .items_center()
        .gap(14.0)
        .px(20.0)
        .child(t(state, LABEL, TEXT2))
        .child(
            t(caption, TextStyle::new(17.0, 600.0), TEXT)
                .w_full()
                .max_lines(2)
                .wrap(),
        )
        .child(stage)
        .child(el().grow(1.0));
    if let (Some(turn), false) = (last, listening) {
        if !turn.thinking {
            let kind = turn.doc.as_ref().map(accent_of).unwrap_or_default();
            let (field, on, _) = signal(&kind);
            let (label, hero, line) = headline(turn);
            page = page.child(
                col()
                    .key("voice-answer")
                    .w_full()
                    .padding(18.0)
                    .gap(4.0)
                    .radius(24.0)
                    .fill(field)
                    .child(t(label.to_uppercase(), LABEL, on.alpha(0.85)))
                    .child(
                        t(
                            if hero.is_empty() {
                                line.clone()
                            } else {
                                hero.clone()
                            },
                            TextStyle::new(30.0, 700.0),
                            on,
                        )
                        .w_full()
                        .max_lines(2)
                        .wrap(),
                    )
                    .child(
                        t(
                            if hero.is_empty() { String::new() } else { line },
                            CAPTION,
                            on.alpha(0.9),
                        )
                        .w_full()
                        .max_lines(2)
                        .wrap(),
                    ),
            );
        }
    }
    page
}
