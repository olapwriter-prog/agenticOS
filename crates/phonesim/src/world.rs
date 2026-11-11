//! The world outside the phone, for the partner services: WhatsApp Business (Cloud API shapes),
//! an email box, a UPI payout and collection API, a GST Suvidha Provider, three shopping
//! platforms, news feeds (RSS) and an events platform. One HTTP server, state in memory, seeded
//! with a small Bengaluru business's day. Every name, brand and story here is made up.
//!
//! It checks what the real systems check: a payout needs the balance; an order or a booking needs
//! a payment to that merchant for that amount; a return is filed once per month; WhatsApp needs
//! messaging_product "whatsapp" and a text body.
//!
//! Control endpoints (the network's side, for tests and demos):
//!   POST /sim/whatsapp {wa_id, name, text}            a customer writes
//!   POST /sim/mail {from, from_name, subject, body}  an email arrives
//!   POST /sim/pay/collect {from_vpa, amount_paise, note}   a customer pays us by UPI
//!   POST /sim/price {platform, id, price_paise}      a platform changes a price
//!   GET  /sim/state                                  everything, for checks
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

fn today_days() -> i64 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    ((secs + 19_800) / 86_400) as i64
}

/// Days since 1970-01-01 as "YYYY-MM-DD".
fn date(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", y + i64::from(m <= 2), m, d)
}

pub struct World {
    v: Value,
    seq: u64,
}

impl World {
    pub fn seeded() -> World {
        let t = today_days();
        let d = |n: i64| date(t + n);
        let wa = |id: &str, name: &str, msgs: Vec<(&str, &str)>| {
            json!({"wa_id": id, "name": name, "messages": msgs.iter().enumerate().map(|(i, (dir, text))| json!({"id": format!("wamid.seed.{id}.{i}"), "direction": dir, "text": text, "timestamp": d(-1)})).collect::<Vec<_>>()})
        };
        let product = |id: &str, title: &str, price: i64, delivery: i64, rating: f64, eta: u32, stock: bool| {
            json!({"id": id, "title": title, "price_paise": price, "delivery_paise": delivery, "rating": rating, "eta_days": eta, "in_stock": stock})
        };
        let order = |id: &str, title: &str, price: i64, days_ago: i64| json!({"order_id": id, "title": title, "qty": 1, "price_paise": price, "date": d(-days_ago)});
        let bill = |id: &str, from: &str, supplier: &str, amount: &str, gst: &str, gstin: &str, due: i64, vpa: &str| {
            json!({"id": id, "from": from, "from_name": supplier, "subject": format!("Bill from {supplier}"), "date": d(-3),
                   "body": format!("Dear customer,\nYour bill for this month.\nAmount due: {amount}\nGST: {gst}\nGSTIN: {gstin}\nDue date: {}\nPay to UPI: {vpa}\n", d(due))})
        };
        let story = |title: &str, cats: &[&str], desc: &str| json!({"title": title, "categories": cats, "description": desc});
        let event = |id: &str, kind: &str, title: &str, venue: &str, days: i64, price: i64, tags: &[&str], seats: u32| {
            json!({"id": id, "kind": kind, "title": title, "venue": venue, "city": "Bengaluru", "date": d(days), "price_paise": price, "tags": tags, "seats_left": seats, "merchant_vpa": "tickets@upi"})
        };
        let v = json!({
            "whatsapp": {"conversations": [
                wa("919811111111", "Sharma Traders", vec![("in", "Hi, what's your rate for the Widget these days?")]),
                wa("919822222222", "Patil Stores", vec![("out", "Thank you for your order last month!"), ("in", "Please send 10 widgets and 2 repairs by Friday")]),
                wa("919833333333", "Asha Menon", vec![("in", "Good morning! Is the repair service available this week?")]),
                wa("919844444444", "Kiran Rao", vec![("in", "Hello, do you deliver to Mysuru?")]),
            ]},
            "mail": {"inbox": [
                bill("m-101", "billing@citypower.example", "City Power", "₹ 4,720.00", "₹ 720.00", "29AAFCD5862R1ZR", 4, "citypower@upi"),
                bill("m-102", "accounts@primespaces.example", "Prime Spaces (office rent)", "₹ 35,400.00", "₹ 5,400.00", "29AABCT1332L1ZA", 9, "primespaces@upi"),
                bill("m-103", "billing@fibernet.example", "FiberNet Broadband", "₹ 1,179.00", "₹ 179.84", "33AAACR5055K1ZE", 1, "fibernet@upi"),
                {"id": "m-104", "from": "orders@meenakshi.example", "from_name": "Meenakshi Caterers", "subject": "Quote request", "date": d(-1), "body": "Could you send us your price list for widgets? We may need 40."},
            ], "sent": []},
            "pay": {"balance_paise": 15_000_000, "payouts": [], "collections": [], "merchants": {}},
            "gsp": {"filings": []},
            "shop": {
                "shopkart": {"products": [
                    product("SK-11", "Golden Harvest Whole Wheat Atta 10 kg", 46_500, 0, 4.4, 2, true),
                    product("SK-12", "Royal Grain Basmati Rice 5 kg", 62_900, 4_000, 4.3, 3, true),
                    product("SK-13", "A4 Copier Paper 75 gsm 500 sheets", 31_900, 0, 4.5, 1, true),
                    product("SK-14", "Leaf & Bud Green Tea 100 bags", 39_900, 0, 4.1, 2, true),
                    product("SK-15", "Sonic X200 Bluetooth Earbuds", 149_900, 0, 4.2, 3, true),
                ], "orders": [order("SK-O-881", "Golden Harvest Whole Wheat Atta 10 kg", 47_900, 52), order("SK-O-912", "Golden Harvest Whole Wheat Atta 10 kg", 46_900, 21), order("SK-O-915", "A4 Copier Paper 75 gsm 500 sheets", 32_900, 19)]},
                "bazaarnow": {"products": [
                    product("BN-201", "Golden Harvest Whole Wheat Atta 10 kg", 44_900, 2_900, 4.2, 1, true),
                    product("BN-202", "Royal Grain Basmati Rice 5 kg", 59_900, 0, 4.4, 2, true),
                    product("BN-203", "A4 Copier Paper 75 gsm 500 sheets", 29_900, 4_900, 3.9, 4, true),
                    product("BN-204", "Leaf & Bud Green Tea 100 bags", 35_500, 0, 4.0, 3, true),
                    product("BN-205", "Sonic X200 Bluetooth Earbuds", 139_900, 0, 3.2, 5, true),
                ], "orders": [order("BN-O-77", "Golden Harvest Whole Wheat Atta 10 kg", 45_900, 37), order("BN-O-91", "Leaf & Bud Green Tea 100 bags", 37_500, 30)]},
                "quickmart": {"products": [
                    product("QM-1", "Golden Harvest Whole Wheat Atta 10 kg", 43_900, 0, 4.6, 1, true),
                    product("QM-2", "Royal Grain Basmati Rice 5 kg", 61_500, 0, 4.5, 1, false),
                    product("QM-3", "A4 Copier Paper 75 gsm 500 sheets", 30_500, 0, 4.4, 1, true),
                    product("QM-4", "Leaf & Bud Green Tea 100 bags", 36_900, 1_500, 4.3, 1, true),
                    product("QM-5", "Sonic X200 Bluetooth Earbuds", 144_900, 0, 4.0, 2, true),
                ], "orders": [order("QM-O-5", "A4 Copier Paper 75 gsm 500 sheets", 31_500, 6)]},
            },
            "news": {
                "business": [
                    story("GST Council trims rates on packaged staples", &["gst", "business", "tax"], "Small retailers see lower prices on atta and rice from next month; filing dates stay the same."),
                    story("UPI crosses a new monthly record", &["upi", "payments", "business"], "Merchant payments grew fastest among small shops in tier-2 cities."),
                    story("Small businesses get a simpler quarterly return option", &["gst", "small business"], "Turnover up to ₹5 crore may file quarterly with monthly payments."),
                ],
                "tech": [
                    story("Phones that run their own AI, offline", &["tech", "ai", "phones"], "On-device models now answer in seconds on mid-range phones."),
                    story("Open-source chips gain ground", &["tech", "hardware"], "RISC-V boards reach hobbyist prices."),
                ],
                "travel": [
                    story("Monsoon's end opens the Western Ghats treks", &["travel", "trekking", "outdoors"], "Forest department reopens trails near Bengaluru from this weekend."),
                    story("Weekend kayaking near the city", &["travel", "kayaking", "outdoors"], "Hesaraghatta lake sessions for beginners."),
                ],
                "music": [
                    story("Indie bands return to the city's small stages", &["music", "indie", "rock"], "A month of gigs across Indiranagar and Koramangala."),
                    story("Jazz festival line-up announced", &["music", "jazz"], "Three nights, two stages, one rooftop."),
                ],
                "movies": [
                    story("Sci-fi classic returns in IMAX", &["movies", "sci-fi"], "A restored print for one week only."),
                    story("This week's Kannada releases", &["movies", "kannada"], "Two dramas and a comedy."),
                ],
                "sports": [
                    story("City side wins the T20 final", &["sports", "cricket"], "A last-over finish at Chinnaswamy."),
                ],
            },
            "events": {"list": [
                event("EV-101", "concert", "Indie Rock Night", "The Humming Tree, Indiranagar", 3, 79_900, &["music", "indie", "rock"], 42),
                event("EV-102", "adventure", "Sunrise trek to Skandagiri", "Skandagiri, Chikkaballapur", 4, 149_900, &["trekking", "outdoors", "travel"], 12),
                event("EV-103", "adventure", "Beginners' kayaking at Hesaraghatta", "Hesaraghatta Lake", 6, 120_000, &["kayaking", "outdoors", "travel"], 8),
                event("EV-104", "concert", "Jazz on the Roof", "Rooftop, MG Road", 9, 99_900, &["music", "jazz"], 60),
                event("EV-105", "movie", "Space Odyssey (IMAX re-release)", "IMAX, Whitefield", 2, 45_000, &["movies", "sci-fi"], 120),
                event("EV-106", "comedy", "Stand-up: Office Life", "Comedy Club, Koramangala", 5, 59_900, &["comedy"], 30),
                event("EV-107", "sports", "T20 league: city derby", "Chinnaswamy Stadium", 11, 150_000, &["sports", "cricket"], 200),
            ], "bookings": []},
        });
        World { v, seq: 1000 }
    }

    fn id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}{}", self.seq)
    }

    /// One request; (status, content type, body).
    pub fn handle(&mut self, method: &str, path: &str, body: &[u8]) -> (u16, &'static str, String) {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let q = |k: &str| query.split('&').find_map(|kv| kv.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.replace('+', " ").replace("%20", " "))).unwrap_or_default();
        let b: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
        let ok = |v: Value| (200, "application/json", v.to_string());
        let err = |code: u16, m: &str| (code, "application/json", json!({"error": {"message": m}}).to_string());
        let today = date(today_days());
        match (method, seg.as_slice()) {
            // ---- WhatsApp Business (Cloud API shapes) ----
            ("GET", ["whatsapp", _, _, "conversations"]) => ok(json!({"data": self.v["whatsapp"]["conversations"]})),
            ("POST", ["whatsapp", _, _, "messages"]) => {
                if b["messaging_product"] != "whatsapp" {
                    return err(400, "(#100) The parameter messaging_product is required.");
                }
                let to = b["to"].as_str().unwrap_or("").to_string();
                let text = b["text"]["body"].as_str().unwrap_or("").to_string();
                if to.is_empty() || text.is_empty() {
                    return err(400, "(#100) Param text['body'] is required.");
                }
                let id = self.id("wamid.");
                let convs = self.v["whatsapp"]["conversations"].as_array_mut().unwrap();
                let m = json!({"id": id, "direction": "out", "text": text, "timestamp": today});
                match convs.iter_mut().find(|c| c["wa_id"] == to.as_str()) {
                    Some(c) => c["messages"].as_array_mut().unwrap().push(m),
                    None => convs.push(json!({"wa_id": to, "name": to, "messages": [m]})),
                }
                ok(json!({"messaging_product": "whatsapp", "contacts": [{"input": to, "wa_id": to}], "messages": [{"id": id}]}))
            }
            // ---- email ----
            ("GET", ["mail", "inbox"]) => ok(json!({"messages": self.v["mail"]["inbox"]})),
            ("POST", ["mail", "send"]) => {
                if b["to"].as_str().unwrap_or("").is_empty() {
                    return err(400, "no recipient");
                }
                let id = self.id("mail-");
                let mut m = b.clone();
                m["id"] = json!(id);
                self.v["mail"]["sent"].as_array_mut().unwrap().push(m);
                ok(json!({"id": id}))
            }
            // ---- UPI payouts and collections ----
            ("GET", ["pay", "v1", "balance"]) => ok(json!({"balance_paise": self.v["pay"]["balance_paise"]})),
            ("GET", ["pay", "v1", "payouts"]) => ok(json!({"items": self.v["pay"]["payouts"]})),
            ("GET", ["pay", "v1", "collections"]) => ok(json!({"items": self.v["pay"]["collections"]})),
            ("POST", ["pay", "v1", "payouts"]) => {
                let amount = b["amount_paise"].as_i64().unwrap_or(0);
                let vpa = b["vpa"].as_str().unwrap_or("").to_string();
                if amount <= 0 || !vpa.contains('@') {
                    return err(400, "invalid payout");
                }
                let bal = self.v["pay"]["balance_paise"].as_i64().unwrap_or(0);
                if bal < amount {
                    return err(400, "insufficient balance");
                }
                self.v["pay"]["balance_paise"] = json!(bal - amount);
                let (id, utr) = (self.id("pout_"), format!("UTR{}{}", today.replace('-', ""), self.seq));
                let p = json!({"id": id, "utr": utr, "vpa": vpa, "amount_paise": amount, "purpose": b["purpose"], "status": "processed", "date": today});
                self.v["pay"]["payouts"].as_array_mut().unwrap().push(p.clone());
                ok(p)
            }
            // ---- GST Suvidha Provider ----
            ("GET", ["gsp", "v1", "returns"]) => ok(json!({"filings": self.v["gsp"]["filings"]})),
            ("POST", ["gsp", "v1", "returns", form]) => {
                let gstin = b["gstin"].as_str().unwrap_or("");
                let period = b["ret_period"].as_str().unwrap_or("");
                if gstin.len() != 15 || period.len() != 6 || !period.chars().all(|c| c.is_ascii_digit()) || b["data"].is_null() {
                    return err(400, "RET191106: invalid return");
                }
                let form = form.to_uppercase();
                if self.v["gsp"]["filings"].as_array().unwrap().iter().any(|f| f["form"] == form.as_str() && f["ret_period"] == period && f["gstin"] == gstin) {
                    return err(409, "RET191113: return already filed for this period");
                }
                let arn = format!("AA{}{}{:07}", &gstin[..2], &period[..4], self.seq + 1);
                self.seq += 1;
                self.v["gsp"]["filings"].as_array_mut().unwrap().push(json!({"form": form, "gstin": gstin, "ret_period": period, "arn": arn, "data": b["data"], "date": today}));
                ok(json!({"status": "filed", "arn": arn}))
            }
            // ---- shopping platforms ----
            ("GET", ["shop", pl, "search"]) => {
                let Some(shop) = self.v["shop"].get(*pl) else { return err(404, "no such platform") };
                let qw: Vec<String> = q("q").to_lowercase().split_whitespace().map(String::from).collect();
                let results: Vec<Value> = shop["products"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|p| {
                        let t = p["title"].as_str().unwrap_or("").to_lowercase();
                        let hits = qw.iter().filter(|w| t.contains(w.as_str())).count();
                        !qw.is_empty() && hits * 2 >= qw.len()
                    })
                    .map(|p| {
                        let mut p = p.clone();
                        p["merchant_vpa"] = json!(format!("{pl}@upi"));
                        p
                    })
                    .collect();
                ok(json!({"results": results}))
            }
            ("GET", ["shop", pl, "products", id]) => match self.v["shop"].get(*pl).and_then(|s| s["products"].as_array().unwrap().iter().find(|p| p["id"] == *id)) {
                Some(p) => ok(p.clone()),
                None => err(404, "no such product"),
            },
            ("GET", ["shop", pl, "orders"]) => ok(json!({"orders": self.v["shop"].get(*pl).map(|s| s["orders"].clone()).unwrap_or(json!([]))})),
            ("POST", ["shop", pl, "orders"]) => {
                let pl = pl.to_string();
                let Some(p) = self.v["shop"].get(&pl).and_then(|s| s["products"].as_array().unwrap().iter().find(|p| p["id"] == b["product_id"])).cloned() else {
                    return err(404, "no such product");
                };
                if p["in_stock"] == false {
                    return err(409, "out of stock");
                }
                let due = p["price_paise"].as_i64().unwrap_or(0) + p["delivery_paise"].as_i64().unwrap_or(0);
                if !self.paid(&format!("{pl}@upi"), due, b["payment_ref"].as_str().unwrap_or("")) {
                    return err(402, "no payment of that amount to this merchant");
                }
                let id = self.id(&format!("{}-O-", pl[..2].to_uppercase()));
                let o = json!({"order_id": id, "title": p["title"], "qty": b["qty"].as_u64().unwrap_or(1), "price_paise": due, "date": today});
                self.v["shop"][&pl]["orders"].as_array_mut().unwrap().push(o);
                ok(json!({"order_id": id, "eta_days": p["eta_days"]}))
            }
            // ---- news (RSS 2.0) ----
            ("GET", ["news", "rss", feed]) => {
                let Some(items) = self.v["news"].get(*feed).and_then(|f| f.as_array()) else { return err(404, "no such feed") };
                let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                let mut x = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\"><channel><title>City Daily: {feed}</title><link>https://citydaily.example/{feed}</link>\n");
                for (i, it) in items.iter().enumerate() {
                    x.push_str(&format!("<item><title>{}</title><link>https://citydaily.example/{feed}/{i}</link><description>{}</description>", esc(it["title"].as_str().unwrap_or("")), esc(it["description"].as_str().unwrap_or(""))));
                    for c in it["categories"].as_array().unwrap() {
                        x.push_str(&format!("<category>{}</category>", esc(c.as_str().unwrap_or(""))));
                    }
                    x.push_str(&format!("<pubDate>{today}</pubDate></item>\n"));
                }
                x.push_str("</channel></rss>\n");
                (200, "application/rss+xml", x)
            }
            // ---- events ----
            ("GET", ["events", "v1", "search"]) => {
                let city = q("city").to_lowercase();
                let from = q("from");
                let evs: Vec<Value> = self.v["events"]["list"].as_array().unwrap().iter().filter(|e| (city.is_empty() || e["city"].as_str().unwrap_or("").to_lowercase() == city) && e["date"].as_str().unwrap_or("") >= from.as_str()).cloned().collect();
                ok(json!({"events": evs}))
            }
            ("GET", ["events", "v1", "events", id]) => match self.v["events"]["list"].as_array().unwrap().iter().find(|e| e["id"] == *id) {
                Some(e) => ok(e.clone()),
                None => err(404, "no such event"),
            },
            ("POST", ["events", "v1", "bookings"]) => {
                let Some(i) = self.v["events"]["list"].as_array().unwrap().iter().position(|e| e["id"] == b["event_id"]) else { return err(404, "no such event") };
                let qty = b["qty"].as_u64().unwrap_or(1);
                let e = self.v["events"]["list"][i].clone();
                if e["seats_left"].as_u64().unwrap_or(0) < qty {
                    return err(409, "sold out");
                }
                let due = e["price_paise"].as_i64().unwrap_or(0) * qty as i64;
                if due > 0 && !self.paid("tickets@upi", due, b["payment_ref"].as_str().unwrap_or("")) {
                    return err(402, "no payment of that amount for these tickets");
                }
                self.v["events"]["list"][i]["seats_left"] = json!(e["seats_left"].as_u64().unwrap_or(0) - qty);
                let id = self.id("BK-");
                let tickets: Vec<String> = (1..=qty).map(|n| format!("{id}-T{n}")).collect();
                self.v["events"]["bookings"].as_array_mut().unwrap().push(json!({"booking_id": id, "event_id": b["event_id"], "qty": qty, "tickets": tickets}));
                ok(json!({"booking_id": id, "tickets": tickets}))
            }
            // ---- the network's side ----
            ("POST", ["sim", "whatsapp"]) => {
                let id = self.id("wamid.in.");
                let wa = b["wa_id"].as_str().unwrap_or("").to_string();
                let m = json!({"id": id, "direction": "in", "text": b["text"], "timestamp": today});
                let convs = self.v["whatsapp"]["conversations"].as_array_mut().unwrap();
                match convs.iter_mut().find(|c| c["wa_id"] == wa.as_str()) {
                    Some(c) => c["messages"].as_array_mut().unwrap().push(m),
                    None => convs.push(json!({"wa_id": wa, "name": b["name"].as_str().unwrap_or(&wa), "messages": [m]})),
                }
                ok(json!({"id": id}))
            }
            ("POST", ["sim", "mail"]) => {
                let id = self.id("m-");
                let mut m = b.clone();
                m["id"] = json!(id);
                m["date"] = json!(today);
                self.v["mail"]["inbox"].as_array_mut().unwrap().push(m);
                ok(json!({"id": id}))
            }
            ("POST", ["sim", "pay", "collect"]) => {
                let id = self.id("col_");
                let amount = b["amount_paise"].as_i64().unwrap_or(0);
                let bal = self.v["pay"]["balance_paise"].as_i64().unwrap_or(0);
                self.v["pay"]["balance_paise"] = json!(bal + amount);
                self.v["pay"]["collections"].as_array_mut().unwrap().push(json!({"id": id, "from_vpa": b["from_vpa"], "amount_paise": amount, "note": b["note"], "date": today}));
                ok(json!({"id": id}))
            }
            ("POST", ["sim", "price"]) => {
                let pl = b["platform"].as_str().unwrap_or("").to_string();
                let Some(ps) = self.v["shop"].get_mut(&pl).and_then(|s| s["products"].as_array_mut()) else { return err(404, "no such platform") };
                match ps.iter_mut().find(|p| p["id"] == b["id"]) {
                    Some(p) => {
                        p["price_paise"] = b["price_paise"].clone();
                        ok(json!({"ok": true}))
                    }
                    None => err(404, "no such product"),
                }
            }
            ("GET", ["sim", "state"]) => ok(self.v.clone()),
            ("GET", ["health"]) => ok(json!({"status": "ok"})),
            _ => crate::apps_world::handle(method, &seg, query, &today).unwrap_or_else(|| err(404, &format!("no route {method} {path}"))),
        }
    }

    /// Was `amount` paid to `vpa` with this UTR (and not used before)?
    fn paid(&mut self, vpa: &str, amount: i64, utr: &str) -> bool {
        if utr.is_empty() {
            return false;
        }
        let used = &mut self.v["pay"]["merchants"];
        if used.get(utr).is_some() {
            return false;
        }
        let ok = self.v["pay"]["payouts"].as_array().unwrap().iter().any(|p| p["utr"] == utr && p["vpa"] == vpa && p["amount_paise"].as_i64() == Some(amount));
        if ok {
            self.v["pay"]["merchants"][utr] = json!(vpa);
        }
        ok
    }
}

fn serve(world: &Arc<Mutex<World>>, s: TcpStream) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; len.min(4 << 20)];
    r.read_exact(&mut body)?;
    let (code, ctype, out) = world.lock().unwrap().handle(&method, &path, &body);
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        402 => "Payment Required",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let mut w = s;
    write!(w, "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len())?;
    w.flush()
}

/// Serve the world on 127.0.0.1:port until killed.
pub fn run(port: u16) {
    let world = Arc::new(Mutex::new(World::seeded()));
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
        eprintln!("error: world on port {port}: {e}");
        std::process::exit(1)
    });
    eprintln!("world: http://127.0.0.1:{port}");
    for s in l.incoming().flatten() {
        let w = world.clone();
        std::thread::spawn(move || {
            let _ = serve(&w, s);
        });
    }
}

/// A control request to a running world (for the CLI): POST path with a JSON body.
pub fn post(url: &str, path: &str, body: &Value) -> Result<Value, String> {
    let hostport = url.trim_start_matches("http://").trim_end_matches('/');
    let mut s = TcpStream::connect(hostport).map_err(|e| format!("{url}: {e}"))?;
    let b = body.to_string();
    write!(s, "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    serde_json::from_str(body).map_err(|e| format!("{e}: {body}"))
}
