//! Personal life: news ranked by what the person cares about, things to do (films, music,
//! adventures, events) picked the same way, and booking them. The interests are learned: what is
//! asked about, read and booked counts, and every day the old weights fade a little, so the
//! picture follows the person.
use crate::ui::{Card, Ui};
use crate::{inr, wallet, Date, Partner};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Booking {
    pub id: String,
    pub event_id: String,
    pub title: String,
    pub date: Date,
    pub qty: u32,
    pub amount: i64,
    pub tickets: Vec<String>,
}

/// A news item from RSS.
#[derive(Debug, Clone)]
pub struct Article {
    pub title: String,
    pub link: String,
    pub description: String,
    pub categories: Vec<String>,
    pub feed: String,
}

/// RSS 2.0 items (title, link, description, category*), with CDATA and the usual entities.
pub fn parse_rss(xml: &str, feed: &str) -> Vec<Article> {
    let tag = |block: &str, name: &str| -> Vec<String> {
        let mut out = vec![];
        let (open, close) = (format!("<{name}"), format!("</{name}>"));
        let mut rest = block;
        while let Some(i) = rest.find(&open) {
            let after = &rest[i + open.len()..];
            // <name> or <name attr="..."> but not <namespaced:name...>
            if !after.starts_with('>') && !after.starts_with(' ') {
                rest = after;
                continue;
            }
            let Some(gt) = after.find('>') else { break };
            let Some(end) = after.find(&close) else { break };
            out.push(unescape(after[gt + 1..end].trim()));
            rest = &after[end + close.len()..];
        }
        out
    };
    xml.split("<item")
        .skip(1)
        .filter_map(|b| {
            let b = b.split("</item>").next()?;
            Some(Article {
                title: tag(b, "title").into_iter().next()?,
                link: tag(b, "link").into_iter().next().unwrap_or_default(),
                description: tag(b, "description").into_iter().next().unwrap_or_default(),
                categories: tag(b, "category").into_iter().map(|c| c.to_lowercase()).collect(),
                feed: feed.to_string(),
            })
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let s = s.strip_prefix("<![CDATA[").and_then(|x| x.strip_suffix("]]>")).unwrap_or(s);
    let s = s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&");
    // drop markup left in descriptions
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Fade every interest by 2% a day since the last time; then a topic seen gains weight.
fn decay(p: &mut Partner) {
    let today = p.today().days();
    if p.data.interests_day == 0 {
        p.data.interests_day = today;
    }
    let days = today - p.data.interests_day;
    if days > 0 {
        let f = 0.98f64.powi(days as i32);
        p.data.interests.values_mut().for_each(|w| *w *= f);
        p.data.interests_day = today;
    }
}

fn learn(p: &mut Partner, topic: &str, by: f64) {
    let t = topic.trim().to_lowercase();
    if !t.is_empty() && t != "any" {
        *p.data.interests.entry(t).or_insert(0.0) += by;
    }
}

fn score(p: &Partner, tags: &[String], title: &str) -> (f64, Vec<String>) {
    let tw: Vec<String> = title.to_lowercase().split(|c: char| !c.is_alphanumeric()).map(String::from).collect();
    let mut s = 0.0;
    let mut why = vec![];
    for (topic, w) in &p.data.interests {
        if tags.iter().any(|t| t == topic) || tw.iter().any(|x| x == topic) {
            s += w;
            why.push(topic.clone());
        }
    }
    (s, why)
}

pub fn news(p: &mut Partner, topic: &str) -> Result<Ui, String> {
    decay(p);
    let topic = topic.trim().to_lowercase();
    let mut all = vec![];
    for f in p.cfg.feeds.clone() {
        let path = if f.starts_with("http") { f.clone() } else { format!("/news/rss/{f}") };
        if let Ok(xml) = p.get_text(&path) {
            all.extend(parse_rss(&xml, &f));
        }
    }
    if topic != "any" && !topic.is_empty() {
        all.retain(|a| a.categories.iter().any(|c| c.contains(&topic)) || a.title.to_lowercase().contains(&topic) || a.feed == topic);
        learn(p, &topic, 1.0);
    }
    let mut ranked: Vec<(f64, Vec<String>, &Article)> = all.iter().map(|a| {
        let (s, why) = score(p, &a.categories, &a.title);
        (s, why, a)
    }).collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut ui = Ui::new(if topic == "any" || topic.is_empty() { "News for you".to_string() } else { format!("News: {topic}") })
        .subtitle(format!("{} stories from {} feeds, ranked by your interests", all.len(), p.cfg.feeds.len()));
    for (_, why, a) in ranked.iter().take(6) {
        let mut c = Card::new(&a.title).subtitle(a.feed.clone());
        if !a.description.is_empty() {
            let d: String = a.description.chars().take(140).collect();
            c = c.line(if a.description.chars().count() > 140 { format!("{d}…") } else { d });
        }
        if !why.is_empty() {
            c = c.line(format!("because you follow {}", why.join(", "))).tone("info");
        }
        if !a.link.is_empty() {
            c = c.line(a.link.clone());
        }
        if let Some(cat) = a.categories.first() {
            c = c.button("More like this", format!("i like {cat}"));
        }
        ui = ui.card(c);
    }
    if all.is_empty() {
        ui = ui.card(Card::new("No stories found"));
    }
    Ok(ui)
}

pub fn interests(p: &mut Partner) -> Result<Ui, String> {
    decay(p);
    let mut v: Vec<(&String, &f64)> = p.data.interests.iter().filter(|(_, w)| **w >= 0.05).collect();
    v.sort_by(|a, b| b.1.total_cmp(a.1));
    let mut c = Card::new("What you care about").subtitle("learned from what you ask about, read and book; fades if you stop");
    for (t, w) in v.iter().take(12) {
        c = c.line(format!("{t} {}", "●".repeat((**w).round().clamp(1.0, 10.0) as usize)));
    }
    Ok(Ui::new("Interests").card(c))
}

pub fn like(p: &mut Partner, topic: &str) -> Result<Ui, String> {
    decay(p);
    learn(p, topic, 3.0);
    let t = topic.trim().to_lowercase();
    Ok(Ui::new(format!("Noted: you like {t}")).card(Card::new(t.clone()).line("news and suggestions will lean that way").tone("ok").button("Suggestions", format!("suggest {t}"))))
}

fn kind_of(k: &str) -> Option<&'static str> {
    let k = k.trim().to_lowercase();
    Some(match k.trim_end_matches('s') {
        "movie" | "film" | "cinema" => "movie",
        "music" | "concert" | "gig" | "live music" => "concert",
        "adventure" | "trek" | "outdoor" | "outing" => "adventure",
        "comedy" | "standup" | "stand-up" => "comedy",
        "sport" | "match" | "game" => "sports",
        _ => return None,
    })
}

pub fn suggest(p: &mut Partner, kind: &str) -> Result<Ui, String> {
    decay(p);
    let today = p.today();
    let city = p.cfg.city.replace(' ', "+");
    let v = p.get(&format!("/events/v1/search?city={city}&from={today}"))?;
    let want = kind_of(kind);
    let mut evs: Vec<(f64, Vec<String>, Value)> = v["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| want.is_none_or(|k| e["kind"].as_str() == Some(k)))
        .filter(|e| e["date"].as_str().and_then(Date::parse).is_some_and(|d| d >= today))
        .map(|e| {
            let mut tags: Vec<String> = e["tags"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str().map(|s| s.to_lowercase())).collect();
            tags.push(e["kind"].as_str().unwrap_or("").to_string());
            let (mut s, why) = score(p, &tags, e["title"].as_str().unwrap_or(""));
            // sooner is a little better
            let days = e["date"].as_str().and_then(Date::parse).map(|d| d.days() - today.days()).unwrap_or(30);
            s += 1.0 / (1.0 + days as f64 / 7.0);
            (s, why, e)
        })
        .collect();
    evs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let title = match want {
        Some("movie") => "Films for you",
        Some("concert") => "Music for you",
        Some("adventure") => "Adventures for you",
        Some(_) => "Things to do",
        None => "Things to do",
    };
    let mut ui = Ui::new(title).subtitle(format!("{} · picked by your interests", p.cfg.city));
    for (_, why, e) in evs.iter().take(5) {
        let id = e["id"].as_str().unwrap_or("");
        let price = e["price_paise"].as_i64().unwrap_or(0);
        let mut c = Card::new(e["title"].as_str().unwrap_or(""))
            .subtitle(format!("{} · {} · {}", e["kind"].as_str().unwrap_or(""), e["date"].as_str().unwrap_or(""), e["venue"].as_str().unwrap_or("")))
            .badge(if price == 0 { "free".into() } else { inr(price) });
        if !why.is_empty() {
            c = c.line(format!("you like {}", why.join(", "))).tone("info");
        }
        if let Some(n) = e["seats_left"].as_u64() {
            if n < 20 {
                c = c.line(format!("only {n} left"));
            }
        }
        c = c.button("Book 1", format!("book {id} for 1")).button("Book 2", format!("book {id} for 2"));
        ui = ui.card(c);
    }
    if evs.is_empty() {
        ui = ui.card(Card::new("Nothing coming up"));
    }
    Ok(ui)
}

pub fn book(p: &mut Partner, event: &str, qty: &str) -> Result<Ui, String> {
    decay(p);
    let qty: u32 = qty.trim().trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok().filter(|q| (1..=10).contains(q)).ok_or(format!("{qty:?}: 1 to 10 tickets"))?;
    let e = p.get(&format!("/events/v1/events/{}", event.trim()))?;
    let title = e["title"].as_str().ok_or(format!("no event {event}"))?.to_string();
    if e["seats_left"].as_u64().is_some_and(|n| n < qty as u64) {
        return Err(format!("only {} seats left for {title}", e["seats_left"]));
    }
    let amount = e["price_paise"].as_i64().unwrap_or(0) * qty as i64;
    let utr = if amount > 0 {
        let vpa = e["merchant_vpa"].as_str().unwrap_or("tickets@upi").to_string();
        wallet::payout(p, &vpa, amount, &format!("{qty} x {title}"))?.utr
    } else {
        String::new()
    };
    let v = p.post("/events/v1/bookings", &json!({"event_id": event.trim(), "qty": qty, "payment_ref": utr, "amount_paise": amount}))?;
    let id = v["booking_id"].as_str().ok_or(format!("not booked: {v}"))?.to_string();
    let tickets: Vec<String> = v["tickets"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str().map(String::from)).collect();
    let date = e["date"].as_str().and_then(Date::parse).unwrap_or(p.today());
    for t in e["tags"].as_array().cloned().unwrap_or_default() {
        if let Some(t) = t.as_str() {
            learn(p, t, 2.0);
        }
    }
    learn(p, e["kind"].as_str().unwrap_or(""), 1.0);
    p.data.bookings.push(Booking { id: id.clone(), event_id: event.trim().into(), title: title.clone(), date, qty, amount, tickets: tickets.clone() });
    let mut c = Card::new(&title).subtitle(format!("{} · {}", date, e["venue"].as_str().unwrap_or(""))).badge(inr(amount)).tone("ok");
    c = c.line(format!("booking {id}, {qty} ticket{}", if qty > 1 { "s" } else { "" }));
    for t in &tickets {
        c = c.line(format!("ticket {t}"));
    }
    if !utr.is_empty() {
        c = c.line(format!("paid from the wallet, UTR {utr}"));
    }
    Ok(Ui::new("Booked").card(c).action("Add to calendar", format!("add {title} to my calendar on {date}")))
}
