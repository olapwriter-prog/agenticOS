//! The public APIs the phone's apps use, simulated with their documented paths and JSON shapes
//! (Open-Meteo, Nominatim, OSRM, Frankfurter, Wikipedia REST, dictionaryapi.dev, Nager.Date,
//! Open Library, radio-browser, iTunes Search, TheMealDB, MyMemory, CoinGecko). Small, fixed data;
//! unknown inputs answer the way those services document ("no results", 404, null).
use serde_json::{json, Value};

/// A percent-encoded query value, decoded.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(q: &str, k: &str) -> String {
    q.split('&').find_map(|kv| kv.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| decode(v))).unwrap_or_default()
}

/// (name, country, lat, lon, timezone, utc offset seconds)
const PLACES: &[(&str, &str, f64, f64, &str, i64)] = &[
    ("Bengaluru", "India", 12.97194, 77.59369, "Asia/Kolkata", 19800),
    ("Bangalore", "India", 12.97194, 77.59369, "Asia/Kolkata", 19800),
    ("Pune", "India", 18.51957, 73.85535, "Asia/Kolkata", 19800),
    ("Mysuru", "India", 12.29791, 76.63925, "Asia/Kolkata", 19800),
    ("London", "United Kingdom", 51.50853, -0.12574, "Europe/London", 0),
];

/// Nominatim's data: (display_name, name, category, type, lat, lon, words to match)
const OSM: &[(&str, &str, &str, &str, f64, f64, &str)] = &[
    ("Third Wave Coffee, 100 Feet Road, Indiranagar, Bengaluru, Karnataka, 560038, India", "Third Wave Coffee", "amenity", "cafe", 12.97843, 77.64081, "cafe cafes coffee indiranagar bengaluru"),
    ("Dyu Art Cafe, 12th Main Road, Indiranagar, Bengaluru, Karnataka, 560038, India", "Dyu Art Cafe", "amenity", "cafe", 12.97127, 77.64104, "cafe cafes coffee indiranagar bengaluru"),
    ("Manipal Hospital, Old Airport Road, Bengaluru, Karnataka, 560017, India", "Manipal Hospital", "amenity", "hospital", 12.95873, 77.64881, "hospital hospitals bengaluru"),
    ("Bengaluru, Bangalore North, Bengaluru Urban, Karnataka, India", "Bengaluru", "boundary", "administrative", 12.97194, 77.59369, "bengaluru bangalore"),
    ("Mysuru, Mysuru taluk, Mysuru district, Karnataka, India", "Mysuru", "boundary", "administrative", 12.29791, 76.63925, "mysuru mysore"),
    ("Mysore Palace, Sayyaji Rao Road, Mysuru, Karnataka, 570001, India", "Mysore Palace", "tourism", "attraction", 12.30520, 76.65520, "mysore palace mysuru"),
    ("Kempegowda International Airport, Devanahalli, Bengaluru, Karnataka, 560300, India", "Kempegowda International Airport", "aeroway", "aerodrome", 13.19861, 77.70650, "airport kempegowda bengaluru"),
];

fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2) + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    6371.0 * 2.0 * h.sqrt().asin()
}

pub fn handle(method: &str, seg: &[&str], q: &str, today: &str) -> Option<(u16, &'static str, String)> {
    let ok = |v: Value| Some((200, "application/json", v.to_string()));
    let not_found = |v: Value| Some((404, "application/json", v.to_string()));
    if method != "GET" {
        return None;
    }
    match seg {
        // Open-Meteo geocoding
        ["v1", "search"] => {
            let name = query(q, "name").to_lowercase();
            let hits: Vec<Value> = PLACES.iter().filter(|p| p.0.to_lowercase() == name).map(|p| json!({"id": 1277333, "name": p.0, "latitude": p.2, "longitude": p.3, "country": p.1, "timezone": p.4, "feature_code": "PPLA"})).collect();
            ok(if hits.is_empty() { json!({"generationtime_ms": 0.4}) } else { json!({"results": hits, "generationtime_ms": 0.6}) })
        }
        // Open-Meteo forecast
        ["v1", "forecast"] => {
            let lon: f64 = query(q, "longitude").parse().unwrap_or(0.0);
            let off = if lon > 60.0 { 19800 } else { 0 };
            let days: Vec<String> = (0..4).map(|i| shift(today, i)).collect();
            ok(json!({
                "latitude": query(q, "latitude").parse::<f64>().unwrap_or(0.0), "longitude": lon,
                "utc_offset_seconds": off, "timezone": if off > 0 { "Asia/Kolkata" } else { "Europe/London" },
                "current_units": {"temperature_2m": "°C", "relative_humidity_2m": "%", "wind_speed_10m": "km/h"},
                "current": {"time": format!("{today}T10:00"), "interval": 900, "temperature_2m": 24.3, "relative_humidity_2m": 71, "apparent_temperature": 25.1, "precipitation": 0.0, "weather_code": 2, "wind_speed_10m": 9.4},
                "daily": {
                    "time": days,
                    "weather_code": [2, 61, 80, 1],
                    "temperature_2m_max": [28.1, 26.4, 25.9, 28.8],
                    "temperature_2m_min": [18.2, 18.9, 18.4, 17.6],
                    "precipitation_probability_max": [10, 70, 55, 5],
                    "sunrise": days.iter().map(|d| format!("{d}T06:12")).collect::<Vec<_>>(),
                    "sunset": days.iter().map(|d| format!("{d}T17:53")).collect::<Vec<_>>(),
                }
            }))
        }
        ["v1", "air-quality"] => ok(json!({"current": {"time": format!("{today}T10:00"), "us_aqi": 87, "pm2_5": 29.4, "pm10": 51.2, "ozone": 40.0, "nitrogen_dioxide": 22.5}})),
        // Nominatim (and iTunes, which also uses /search: told apart by media=)
        ["search"] if q.contains("media=") => {
            let term = query(q, "term").to_lowercase();
            let all = [
                ("The Indian Startup Show", "Rohit Rao", "Business", 312, "https://feeds.example.org/indianstartup.xml", "startup business india"),
                ("GST Simplified", "Tax Talk India", "Business", 88, "https://feeds.example.org/gst.xml", "gst tax business india"),
                ("Trek Stories", "Western Ghats Club", "Places & Travel", 140, "https://feeds.example.org/trek.xml", "trek trekking travel outdoors"),
                ("Jazz Hours", "City Radio", "Music", 205, "https://feeds.example.org/jazz.xml", "jazz music"),
            ];
            let res: Vec<Value> = all.iter().filter(|p| term.split_whitespace().any(|w| p.5.contains(w))).map(|p| json!({"wrapperType": "track", "kind": "podcast", "collectionName": p.0, "artistName": p.1, "primaryGenreName": p.2, "trackCount": p.3, "feedUrl": p.4})).collect();
            ok(json!({"resultCount": res.len(), "results": res}))
        }
        ["search"] => {
            let words: Vec<String> = query(q, "q").to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2 && !["the", "near", "and"].contains(w)).map(String::from).collect();
            let limit: usize = query(q, "limit").parse().unwrap_or(10);
            // as Nominatim ranks: the words matched, an exact name, then importance (cities first)
            let full = query(q, "q").to_lowercase();
            let importance = |p: &(&str, &str, &str, &str, f64, f64, &str)| if p.2 == "boundary" { 0.8 } else if p.2 == "tourism" || p.2 == "aeroway" { 0.6 } else { 0.3 };
            let mut scored: Vec<(usize, bool, f64, &(&str, &str, &str, &str, f64, f64, &str))> = OSM
                .iter()
                .map(|p| (words.iter().filter(|w| p.6.split(' ').any(|x| x == w.as_str())).count(), p.1.to_lowercase() == full.trim(), importance(p), p))
                .filter(|(n, _, _, _)| *n > 0)
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.total_cmp(&a.2)));
            let best = scored.first().map(|s| s.0).unwrap_or(0);
            let res: Vec<Value> = scored.iter().filter(|s| s.0 == best).take(limit).map(|(_, _, imp, p)| json!({"place_id": 1, "lat": format!("{:.7}", p.4), "lon": format!("{:.7}", p.5), "category": p.2, "type": p.3, "name": p.1, "display_name": p.0, "importance": imp})).collect();
            ok(json!(res))
        }
        // OSRM
        ["route", "v1", _profile, coords] => {
            let pts: Vec<(f64, f64)> = coords.split(';').filter_map(|c| c.split_once(',')).filter_map(|(lo, la)| Some((la.parse().ok()?, lo.parse().ok()?))).collect();
            if pts.len() < 2 {
                return Some((400, "application/json", json!({"code": "InvalidQuery", "message": "Query string malformed close to position 0"}).to_string()));
            }
            let km = haversine_km(pts[0], pts[1]) * 1.3;
            let d = km * 1000.0;
            ok(json!({"code": "Ok", "routes": [{"distance": d, "duration": km / 42.0 * 3600.0, "legs": [{"steps": [
                {"distance": d * 0.05, "duration": 0, "name": "Old Airport Road", "maneuver": {"type": "depart", "location": [pts[0].1, pts[0].0]}},
                {"distance": d * 0.15, "duration": 0, "name": "Outer Ring Road", "maneuver": {"type": "turn", "modifier": "left", "location": [0, 0]}},
                {"distance": d * 0.70, "duration": 0, "name": "Bengaluru–Mysuru Expressway", "maneuver": {"type": "roundabout", "exit": 2, "location": [0, 0]}},
                {"distance": d * 0.10, "duration": 0, "name": "Sayyaji Rao Road", "maneuver": {"type": "turn", "modifier": "right", "location": [0, 0]}},
                {"distance": 0, "duration": 0, "name": "", "maneuver": {"type": "arrive", "location": [pts[1].1, pts[1].0]}},
            ]}]}], "waypoints": []}))
        }
        // Frankfurter (ECB): rates per euro
        ["v1", "latest"] => {
            let eur: [(&str, f64); 6] = [("EUR", 1.0), ("USD", 1.0832), ("INR", 95.47), ("GBP", 0.8571), ("JPY", 165.21), ("AED", 3.978)];
            let rate = |c: &str| eur.iter().find(|x| x.0 == c).map(|x| x.1);
            let base = query(q, "base");
            let base = if base.is_empty() { "EUR".to_string() } else { base };
            let Some(b) = rate(&base) else { return not_found(json!({"message": "not found"})) };
            let mut rates = serde_json::Map::new();
            for s in query(q, "symbols").split(',').filter(|s| !s.is_empty()) {
                match rate(s) {
                    Some(r) => {
                        rates.insert(s.to_string(), json!(((r / b) * 10000.0).round() / 10000.0));
                    }
                    None => return not_found(json!({"message": "not found"})),
                }
            }
            ok(json!({"amount": 1.0, "base": base, "date": shift(today, -1), "rates": rates}))
        }
        // Wikipedia
        ["w", "rest.php", "v1", "search", "page"] => {
            let t = query(q, "q").to_lowercase();
            let pages = [("Mysore_Palace", "Mysore Palace", "palace"), ("Bengaluru", "Bengaluru", "bengaluru bangalore"), ("Goods_and_Services_Tax_(India)", "Goods and Services Tax (India)", "gst goods services tax")];
            let hit: Vec<Value> = pages.iter().filter(|p| t.split_whitespace().any(|w| p.2.contains(w))).take(1).map(|p| json!({"id": 1, "key": p.0, "title": p.1, "excerpt": "", "description": ""})).collect();
            ok(json!({"pages": hit}))
        }
        ["api", "rest_v1", "page", "summary", key] => {
            let k = decode(key);
            let (title, desc, extract) = match k.as_str() {
                "Mysore_Palace" => ("Mysore Palace", "Historical palace in Mysuru, Karnataka", "Mysore Palace is a historical palace and a royal residence in Mysuru, Karnataka. It was the official residence of the Wadiyar dynasty and the seat of the Kingdom of Mysore."),
                "Bengaluru" => ("Bengaluru", "Capital of Karnataka, India", "Bengaluru, also known as Bangalore, is the capital and largest city of the southern Indian state of Karnataka."),
                "Goods_and_Services_Tax_(India)" => ("Goods and Services Tax (India)", "Indirect tax in India", "The Goods and Services Tax (GST) is a value added tax levied on most goods and services sold for domestic consumption in India."),
                _ => return not_found(json!({"type": "https://mediawiki.org/wiki/HyperSwitch/errors/not_found", "title": "Not found."})),
            };
            ok(json!({"type": "standard", "title": title, "description": desc, "extract": extract, "content_urls": {"desktop": {"page": format!("https://en.wikipedia.org/wiki/{k}")}, "mobile": {"page": format!("https://en.m.wikipedia.org/wiki/{k}")}}}))
        }
        // Free Dictionary API
        ["api", "v2", "entries", "en", word] => match decode(word).as_str() {
            "serendipity" => ok(json!([{"word": "serendipity", "phonetic": "/ˌsɛɹ.ənˈdɪp.ɪ.ti/", "phonetics": [{"text": "/ˌsɛɹ.ənˈdɪp.ɪ.ti/"}], "meanings": [{"partOfSpeech": "noun", "definitions": [{"definition": "A combination of events which have come together by chance to make a surprisingly good or wonderful outcome.", "example": "It was pure serendipity that we met."}], "synonyms": ["chance", "fluke", "luck"]}]}])),
            "invoice" => ok(json!([{"word": "invoice", "phonetics": [{"text": "/ˈɪnvɔɪs/"}], "meanings": [{"partOfSpeech": "noun", "definitions": [{"definition": "A bill; a list of items sold, with their prices, sent to the buyer."}], "synonyms": ["bill"]}, {"partOfSpeech": "verb", "definitions": [{"definition": "To bill; to send an invoice to."}], "synonyms": []}]}])),
            _ => not_found(json!({"title": "No Definitions Found", "message": "Sorry pal, we couldn't find definitions for the word you were looking for.", "resolution": "You can try the search again at later time or head to the web instead."})),
        },
        // Nager.Date: India is not among its countries; the US and Germany are
        ["api", "v3", "NextPublicHolidays", cc] => match *cc {
            "US" => ok(json!([
                {"date": shift(today, 16), "localName": "Thanksgiving Day", "name": "Thanksgiving Day", "countryCode": "US", "global": true, "types": ["Public"]},
                {"date": shift(today, 44), "localName": "Christmas Day", "name": "Christmas Day", "countryCode": "US", "global": true, "types": ["Public"]},
            ])),
            "DE" => ok(json!([
                {"date": shift(today, 44), "localName": "Erster Weihnachtstag", "name": "Christmas Day", "countryCode": "DE", "global": true, "types": ["Public"]},
                {"date": shift(today, 45), "localName": "Zweiter Weihnachtstag", "name": "St. Stephen's Day", "countryCode": "DE", "global": true, "types": ["Public"]},
            ])),
            _ => not_found(json!({"type": "https://tools.ietf.org/html/rfc7231#section-6.5.4", "title": "Not Found", "status": 404})),
        },
        // Open Library
        ["search.json"] => {
            let t = query(q, "q").to_lowercase();
            let all = [
                ("/works/OL17930368W", "The Psychology of Money", "Morgan Housel", 2020, "money finance psychology"),
                ("/works/OL5735363W", "Malgudi Days", "R. K. Narayan", 1943, "malgudi narayan stories india"),
                ("/works/OL20600495W", "The Guide", "R. K. Narayan", 1958, "guide narayan novel india"),
            ];
            let docs: Vec<Value> = all.iter().filter(|b| t.split_whitespace().any(|w| b.4.contains(w))).map(|b| json!({"key": b.0, "title": b.1, "author_name": [b.2], "first_publish_year": b.3, "edition_count": 40})).collect();
            ok(json!({"numFound": docs.len(), "start": 0, "docs": docs}))
        }
        // radio-browser
        ["json", "stations", "search"] => {
            let (tag, name) = (query(q, "tag").to_lowercase(), query(q, "name").to_lowercase());
            let all = [
                ("Radio Indigo Jazz", "India", "jazz,smooth jazz", "MP3", 128, "https://stream.example.net/indigojazz.mp3"),
                ("Swiss Jazz", "Switzerland", "jazz,swing", "AAC", 96, "https://stream.example.net/swissjazz.aac"),
                ("Carnatic Classical", "India", "carnatic,classical,indian", "MP3", 64, "https://stream.example.net/carnatic.mp3"),
                ("Radio City Kannada", "India", "kannada,film,indian", "MP3", 128, "https://stream.example.net/kannada.mp3"),
            ];
            let hit: Vec<Value> = all.iter().filter(|s| (!tag.is_empty() && s.2.split(',').any(|t| t == tag)) || (!name.is_empty() && s.0.to_lowercase().contains(&name))).map(|s| json!({"stationuuid": "x", "name": s.0, "country": s.1, "tags": s.2, "codec": s.3, "bitrate": s.4, "url": s.5, "url_resolved": s.5, "votes": 100})).collect();
            ok(json!(hit))
        }
        // TheMealDB
        ["api", "json", "v1", "1", "search.php"] => {
            let s = query(q, "s").to_lowercase();
            if !s.contains("biryani") {
                return ok(json!({"meals": null}));
            }
            let mut m = serde_json::Map::new();
            m.insert("idMeal".into(), json!("52805"));
            m.insert("strMeal".into(), json!("Chicken Biryani"));
            m.insert("strCategory".into(), json!("Chicken"));
            m.insert("strArea".into(), json!("Indian"));
            m.insert("strInstructions".into(), json!("Marinate the chicken in yoghurt and spices for an hour.\r\nPar-boil the rice with whole spices.\r\nLayer rice over the chicken, add saffron milk and fried onions.\r\nSeal and cook on low heat for 25 minutes."));
            for (i, (ing, qty)) in [("Chicken", "1 kg"), ("Basmati Rice", "500 g"), ("Yoghurt", "1 cup"), ("Onions", "3 sliced"), ("Saffron", "a pinch")].iter().enumerate() {
                m.insert(format!("strIngredient{}", i + 1), json!(ing));
                m.insert(format!("strMeasure{}", i + 1), json!(qty));
            }
            for i in 6..=20 {
                m.insert(format!("strIngredient{i}"), json!(""));
                m.insert(format!("strMeasure{i}"), json!(" "));
            }
            ok(json!({"meals": [m]}))
        }
        // MyMemory
        ["get"] => {
            let (text, pair) = (query(q, "q"), query(q, "langpair"));
            let table = [("good morning", "en|hi", "सुप्रभात"), ("thank you", "en|kn", "ಧನ್ಯವಾದಗಳು"), ("where is the station", "en|fr", "Où est la gare")];
            match table.iter().find(|t| t.0 == text.to_lowercase().trim() && t.1 == pair) {
                Some(t) => ok(json!({"responseData": {"translatedText": t.2, "match": 0.99}, "responseStatus": 200, "matches": []})),
                None if !pair.contains('|') => ok(json!({"responseData": {"translatedText": "INVALID LANGUAGE PAIR SPECIFIED. EXAMPLE: LANGPAIR=EN|IT USING 2 LETTER ISO OR RFC3066 LIKE ZH-CN. ALMOST ALL LANGUAGES SUPPORTED BUT SOME MAY HAVE NO CONTENT"}, "responseStatus": "403"})),
                None => ok(json!({"responseData": {"translatedText": text, "match": 0}, "responseStatus": 200})),
            }
        }
        // CoinGecko
        ["api", "v3", "simple", "price"] => {
            let mut out = serde_json::Map::new();
            for id in query(q, "ids").split(',') {
                let (inr, usd, ch) = match id {
                    "bitcoin" => (8_512_345.12, 89_160.5, -1.23),
                    "ethereum" => (312_456.0, 3_272.8, 2.05),
                    _ => continue,
                };
                out.insert(id.into(), json!({"inr": inr, "inr_24h_change": ch, "usd": usd, "usd_24h_change": ch - 0.1}));
            }
            ok(Value::Object(out))
        }
        _ => None,
    }
}

/// "YYYY-MM-DD" plus n days.
pub fn shift(day: &str, n: i64) -> String {
    let p: Vec<i64> = day.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return day.to_string();
    }
    let (y, m, d) = (p[0], p[1], p[2]);
    // days from civil, then back
    let yy = y - i64::from(m <= 2);
    let era = yy.div_euclid(400);
    let yoe = yy.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let z = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468 + n;
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", yoe + era * 400 + i64::from(m <= 2), m, d)
}
