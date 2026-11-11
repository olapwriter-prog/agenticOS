//! Apps without an app store: the services people open apps for, done by the agent over public
//! APIs that need no account, each answer a generated screen.
//!
//!   weather, air quality, local time   Open-Meteo (forecast, geocoding, air quality)
//!   places, directions                 OpenStreetMap Nominatim, OSRM
//!   currency                           Frankfurter (European Central Bank reference rates)
//!   encyclopedia                       Wikipedia (search and page summaries)
//!   dictionary                         Free Dictionary API (dictionaryapi.dev)
//!   public holidays                    Nager.Date
//!   books                              Open Library
//!   radio                              radio-browser (community station directory)
//!   podcasts                           iTunes Search API
//!   recipes                            TheMealDB
//!   translation                        MyMemory
//!   crypto prices                      CoinGecko
//!
//! Every service's base URL is configurable (`[apps]` in agentd.toml): the real hosts by default,
//! `agentos-phonesim world` in tests, which serves the same paths and the same JSON shapes. The
//! parsing is tolerant: a missing field is shown as missing, not an error. Requests carry a
//! User-Agent naming the project, as Nominatim's and Wikipedia's policies ask.
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_screen::{Card, Ui};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where to ask for the weather etc. when no place is said.
    pub home: String,
    /// Country (ISO 3166 alpha-2) for public holidays when none is said.
    pub country: String,
    /// Currency to show prices in when none is said.
    pub currency: String,
    pub open_meteo: String,
    pub geocoding: String,
    pub air_quality: String,
    pub nominatim: String,
    pub osrm: String,
    pub frankfurter: String,
    pub wikipedia: String,
    pub dictionary: String,
    pub nager: String,
    pub openlibrary: String,
    pub radio: String,
    pub itunes: String,
    pub mealdb: String,
    pub mymemory: String,
    pub coingecko: String,
    pub user_agent: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            home: "Bengaluru".into(),
            country: "IN".into(),
            currency: "INR".into(),
            open_meteo: "https://api.open-meteo.com".into(),
            geocoding: "https://geocoding-api.open-meteo.com".into(),
            air_quality: "https://air-quality-api.open-meteo.com".into(),
            nominatim: "https://nominatim.openstreetmap.org".into(),
            osrm: "https://router.project-osrm.org".into(),
            frankfurter: "https://api.frankfurter.dev".into(),
            wikipedia: "https://en.wikipedia.org".into(),
            dictionary: "https://api.dictionaryapi.dev".into(),
            nager: "https://date.nager.at".into(),
            openlibrary: "https://openlibrary.org".into(),
            radio: "https://de1.api.radio-browser.info".into(),
            itunes: "https://itunes.apple.com".into(),
            mealdb: "https://www.themealdb.com".into(),
            mymemory: "https://api.mymemory.translated.net".into(),
            coingecko: "https://api.coingecko.com".into(),
            user_agent: "agentos/0.1 (+https://github.com/olapwriter-prog/agenticOS)".into(),
        }
    }
}

impl Config {
    /// Every service at one URL (the simulator).
    pub fn all_at(url: &str) -> Config {
        let u = url.trim_end_matches('/').to_string();
        Config {
            open_meteo: u.clone(),
            geocoding: u.clone(),
            air_quality: u.clone(),
            nominatim: u.clone(),
            osrm: u.clone(),
            frankfurter: u.clone(),
            wikipedia: u.clone(),
            dictionary: u.clone(),
            nager: u.clone(),
            openlibrary: u.clone(),
            radio: u.clone(),
            itunes: u.clone(),
            mealdb: u.clone(),
            mymemory: u.clone(),
            coingecko: u,
            ..Config::default()
        }
    }
}

pub struct Apps {
    pub cfg: Config,
}

/// Percent-encoding for a query value.
pub fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            b' ' => o.push_str("%20"),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

impl Apps {
    fn get(&self, url: &str) -> Result<Value, String> {
        let r = ureq::get(url)
            .set("User-Agent", &self.cfg.user_agent)
            .set("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(20))
            .call();
        match r {
            Ok(r) => r.into_json().map_err(|e| format!("{}: {e}", host(url))),
            Err(ureq::Error::Status(404, _)) => Ok(Value::Null),
            Err(ureq::Error::Status(c, r)) => Err(format!(
                "{}: {c} {}",
                host(url),
                r.into_string()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect::<String>()
            )),
            Err(e) => Err(format!("{}: {e}", host(url))),
        }
    }

    /// A place to (name, country, lat, lon, timezone), by Open-Meteo's geocoding.
    fn locate(&self, place: &str) -> Result<(String, String, f64, f64, String), String> {
        let place = if place.trim().is_empty() || place.trim().eq_ignore_ascii_case("here") {
            self.cfg.home.as_str()
        } else {
            place.trim()
        };
        // "Pune, India" -> search "Pune"
        let name = place.split(',').next().unwrap_or(place).trim();
        let v = self.get(&format!(
            "{}/v1/search?name={}&count=1&language=en&format=json",
            self.cfg.geocoding,
            enc(name)
        ))?;
        let r = v["results"]
            .get(0)
            .ok_or(format!("no place called {place}"))?;
        Ok((
            r["name"].as_str().unwrap_or(name).to_string(),
            r["country"].as_str().unwrap_or("").to_string(),
            r["latitude"].as_f64().ok_or("no latitude")?,
            r["longitude"].as_f64().ok_or("no longitude")?,
            r["timezone"].as_str().unwrap_or("auto").to_string(),
        ))
    }

    /// "2026-11-13" as "Fri 13 Nov".
    fn day_label(date: &str) -> String {
        let p: Vec<i64> = date.split('-').filter_map(|x| x.parse().ok()).collect();
        let [y, m, d] = p[..] else {
            return date.to_string();
        };
        // Sakamoto's method: 0 is Sunday
        let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let yy = if m < 3 { y - 1 } else { y };
        let wd = (yy + yy / 4 - yy / 100 + yy / 400 + t[(m - 1).clamp(0, 11) as usize] + d)
            .rem_euclid(7);
        let days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {d} {}",
            days[wd as usize],
            months[(m - 1).clamp(0, 11) as usize]
        )
    }

    fn weather(&self, place: &str) -> Result<Ui, String> {
        let (name, country, lat, lon, _) = self.locate(place)?;
        let v = self.get(&format!(
            "{}/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,weather_code,wind_speed_10m&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset&timezone=auto&forecast_days=4",
            self.cfg.open_meteo
        ))?;
        let c = &v["current"];
        let num = |x: &Value| x.as_f64().map(|f| format!("{f:.0}")).unwrap_or("?".into());
        let mut ui = Ui::new(format!("Weather in {name}"))
            .subtitle(country)
            .kind("weather");
        ui = ui.card(
            Card::new(wmo(c["weather_code"].as_i64().unwrap_or(-1)))
                .metric(
                    format!("{}°", num(&c["temperature_2m"])),
                    format!("feels like {}°", num(&c["apparent_temperature"])),
                    "",
                )
                .badge(format!("{}°C", num(&c["temperature_2m"])))
                .line(format!(
                    "feels like {}°C · humidity {}% · wind {} km/h",
                    num(&c["apparent_temperature"]),
                    num(&c["relative_humidity_2m"]),
                    num(&c["wind_speed_10m"])
                ))
                .line(format!(
                    "rain now {} mm",
                    c["precipitation"]
                        .as_f64()
                        .map(|f| format!("{f:.1}"))
                        .unwrap_or("?".into())
                ))
                .tone("info"),
        );
        let d = &v["daily"];
        let days = d["time"].as_array().cloned().unwrap_or_default();
        for (i, day) in days.iter().enumerate() {
            let at = |k: &str| d[k].get(i).cloned().unwrap_or(Value::Null);
            let title = if i == 0 {
                "Today".to_string()
            } else {
                Self::day_label(day.as_str().unwrap_or(""))
            };
            let mut card = Card::new(title)
                .subtitle(wmo(at("weather_code").as_i64().unwrap_or(-1)))
                .badge(format!(
                    "{}° / {}°",
                    num(&at("temperature_2m_max")),
                    num(&at("temperature_2m_min"))
                ));
            if let Some(p) = at("precipitation_probability_max").as_f64() {
                card = card.line(format!("chance of rain {p:.0}%"));
            }
            if i == 0 {
                let t = |x: Value| {
                    x.as_str()
                        .and_then(|s| s.split('T').nth(1))
                        .unwrap_or("?")
                        .to_string()
                };
                card = card.line(format!(
                    "sunrise {} · sunset {}",
                    t(at("sunrise")),
                    t(at("sunset"))
                ));
            }
            ui = ui.card(card);
        }
        // the coming days as a chart: highs, and the chance of rain
        let highs: Vec<f64> = (0..days.len())
            .filter_map(|i| d["temperature_2m_max"].get(i).and_then(Value::as_f64))
            .collect();
        let rain: Vec<f64> = (0..days.len())
            .filter_map(|i| {
                d["precipitation_probability_max"]
                    .get(i)
                    .and_then(Value::as_f64)
            })
            .collect();
        let labels: Vec<String> = days
            .iter()
            .enumerate()
            .map(|(i, day)| {
                if i == 0 {
                    "Today".to_string()
                } else {
                    Self::day_label(day.as_str().unwrap_or(""))
                        .split(' ')
                        .next()
                        .unwrap_or("?")
                        .to_string()
                }
            })
            .collect();
        if highs.len() == labels.len() && !highs.is_empty() {
            ui = ui.card(Card::new("Highs").chart("line", labels.clone(), highs, "°"));
        }
        if rain.len() == labels.len() && !rain.is_empty() {
            ui = ui.card(Card::new("Chance of rain").chart("bar", labels, rain, "%"));
        }
        Ok(ui.action("Air quality", format!("air quality in {name}")))
    }

    fn air(&self, place: &str) -> Result<Ui, String> {
        let (name, _, lat, lon, _) = self.locate(place)?;
        let v = self.get(&format!("{}/v1/air-quality?latitude={lat}&longitude={lon}&current=us_aqi,pm2_5,pm10,ozone,nitrogen_dioxide", self.cfg.air_quality))?;
        let c = &v["current"];
        let aqi = c["us_aqi"].as_f64().ok_or("no air quality reading here")?;
        let (label, tone) = match aqi as i64 {
            0..=50 => ("good", "ok"),
            51..=100 => ("moderate", "info"),
            101..=150 => ("unhealthy for sensitive groups", "warn"),
            151..=200 => ("unhealthy", "warn"),
            201..=300 => ("very unhealthy", "warn"),
            _ => ("hazardous", "warn"),
        };
        let f = |k: &str| {
            c[k].as_f64()
                .map(|x| format!("{x:.0}"))
                .unwrap_or("?".into())
        };
        Ok(Ui::new(format!("Air in {name}")).kind("air").card(
            Card::new(label)
                .metric(format!("{aqi:.0}"), "US AQI", "")
                .progress((aqi / 300.0) as f32)
                .badge(format!("AQI {aqi:.0}"))
                .line(format!("PM2.5 {} · PM10 {} µg/m³", f("pm2_5"), f("pm10")))
                .line(format!(
                    "ozone {} · NO₂ {} µg/m³",
                    f("ozone"),
                    f("nitrogen_dioxide")
                ))
                .tone(tone),
        ))
    }

    fn time_in(&self, place: &str) -> Result<Ui, String> {
        let (name, country, lat, lon, tz) = self.locate(place)?;
        let v = self.get(&format!(
            "{}/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m&timezone=auto",
            self.cfg.open_meteo
        ))?;
        let off = v["utc_offset_seconds"].as_i64().ok_or("no time zone")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            + off;
        let (h, m) = ((now / 3600).rem_euclid(24), (now / 60).rem_euclid(60));
        let sign = if off < 0 { '-' } else { '+' };
        Ok(Ui::new(format!("Time in {name}")).subtitle(country).card(
            Card::new(format!("{h:02}:{m:02}"))
                .line(format!(
                    "{tz}, UTC{sign}{}:{:02}",
                    off.abs() / 3600,
                    off.abs() % 3600 / 60
                ))
                .tone("info"),
        ))
    }

    /// Nominatim free-text search.
    fn search_places(&self, q: &str, n: usize) -> Result<Vec<Value>, String> {
        let v = self.get(&format!(
            "{}/search?q={}&format=jsonv2&limit={n}&addressdetails=0",
            self.cfg.nominatim,
            enc(q)
        ))?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    fn places(&self, query: &str) -> Result<Ui, String> {
        let q = if query.to_lowercase().contains(" in ") || query.contains(',') {
            query.to_string()
        } else {
            format!("{query}, {}", self.cfg.home)
        };
        let rs = self.search_places(&q, 6)?;
        let mut ui = Ui::new(format!("Places: {query}")).subtitle("OpenStreetMap");
        for r in &rs {
            let full = r["display_name"].as_str().unwrap_or("");
            let name = r["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| full.split(',').next().unwrap_or(full));
            let kind = format!(
                "{} {}",
                r["category"].as_str().or(r["class"].as_str()).unwrap_or(""),
                r["type"].as_str().unwrap_or("")
            )
            .replace('_', " ");
            ui = ui.card(
                Card::new(name)
                    .subtitle(kind.trim().to_string())
                    .line(full.to_string())
                    .button(
                        "Directions",
                        format!(
                            "directions from {} to {}",
                            self.cfg.home,
                            full.split(',').take(3).collect::<Vec<_>>().join(",")
                        ),
                    ),
            );
        }
        if rs.is_empty() {
            ui = ui.card(Card::new("Nothing found"));
        }
        Ok(ui)
    }

    fn directions(&self, from: &str, to: &str) -> Result<Ui, String> {
        let pt = |p: &str| -> Result<(String, f64, f64), String> {
            let p = if p.trim().is_empty() || p.trim().eq_ignore_ascii_case("here") {
                self.cfg.home.clone()
            } else {
                p.trim().to_string()
            };
            let r = self
                .search_places(&p, 1)?
                .into_iter()
                .next()
                .ok_or(format!("no place called {p}"))?;
            let lat: f64 = r["lat"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or(r["lat"].as_f64())
                .ok_or("no latitude")?;
            let lon: f64 = r["lon"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or(r["lon"].as_f64())
                .ok_or("no longitude")?;
            Ok((
                r["display_name"]
                    .as_str()
                    .unwrap_or(&p)
                    .split(',')
                    .next()
                    .unwrap_or(&p)
                    .to_string(),
                lat,
                lon,
            ))
        };
        let (a, b) = (pt(from)?, pt(to)?);
        let v = self.get(&format!(
            "{}/route/v1/driving/{},{};{},{}?overview=false&steps=true",
            self.cfg.osrm, a.2, a.1, b.2, b.1
        ))?;
        if v["code"].as_str() != Some("Ok") {
            return Err(format!(
                "no route: {}",
                v["message"].as_str().or(v["code"].as_str()).unwrap_or("?")
            ));
        }
        let r = &v["routes"][0];
        let km = r["distance"].as_f64().unwrap_or(0.0) / 1000.0;
        let min = r["duration"].as_f64().unwrap_or(0.0) / 60.0;
        let mut steps = Card::new("Turn by turn");
        let mut n = 0;
        for leg in r["legs"].as_array().cloned().unwrap_or_default() {
            for s in leg["steps"].as_array().cloned().unwrap_or_default() {
                let m = &s["maneuver"];
                let kind = m["type"].as_str().unwrap_or("");
                let modi = m["modifier"].as_str().unwrap_or("");
                let road = s["name"]
                    .as_str()
                    .filter(|x| !x.is_empty())
                    .unwrap_or("the road");
                let dist = s["distance"].as_f64().unwrap_or(0.0);
                let line = match kind {
                    "depart" => format!("start on {road}"),
                    "arrive" => "arrive".to_string(),
                    "roundabout" | "rotary" => format!(
                        "at the roundabout, take exit {} onto {road}",
                        m["exit"].as_i64().unwrap_or(1)
                    ),
                    _ => format!(
                        "{} {}onto {road}",
                        kind.replace('_', " "),
                        if modi.is_empty() {
                            String::new()
                        } else {
                            format!("{modi} ")
                        }
                    ),
                };
                steps = steps.line(if dist >= 50.0 {
                    format!("{line} ({:.1} km)", dist / 1000.0)
                } else {
                    line
                });
                n += 1;
                if n >= 14 {
                    break;
                }
            }
        }
        let (h, m) = ((min / 60.0) as i64, (min % 60.0).round() as i64);
        Ok(Ui::new(format!("{} to {}", a.0, b.0))
            .subtitle("by road, OSRM")
            .card(
                Card::new(if h > 0 {
                    format!("{h} h {m} min")
                } else {
                    format!("{m} min")
                })
                .badge(format!("{km:.1} km"))
                .line("without live traffic")
                .tone("info"),
            )
            .card(steps))
    }

    fn currency(&self, amount: &str, from: &str, to: &str) -> Result<Ui, String> {
        let amt: f64 = amount
            .trim()
            .replace(',', "")
            .parse()
            .map_err(|_| format!("{amount:?} is not an amount"))?;
        let from = cur(from).ok_or(format!("{from:?} is not a currency code"))?;
        let to = if to.trim().is_empty() {
            self.cfg.currency.clone()
        } else {
            cur(to).ok_or(format!("{to:?} is not a currency code"))?
        };
        let v = self.get(&format!(
            "{}/v1/latest?base={from}&symbols={to}",
            self.cfg.frankfurter
        ))?;
        let rate = v["rates"][&to]
            .as_f64()
            .ok_or(format!("no rate for {from} to {to}"))?;
        Ok(Ui::new(format!("{amt} {from} in {to}")).card(
            Card::new(format!("{:.2} {to}", amt * rate))
                .line(format!("1 {from} = {rate:.4} {to}"))
                .line(format!(
                    "European Central Bank reference rate, {}",
                    v["date"].as_str().unwrap_or("?")
                ))
                .tone("info"),
        ))
    }

    fn wiki(&self, topic: &str) -> Result<Ui, String> {
        let s = self.get(&format!(
            "{}/w/rest.php/v1/search/page?q={}&limit=1",
            self.cfg.wikipedia,
            enc(topic)
        ))?;
        let key = s["pages"][0]["key"]
            .as_str()
            .ok_or(format!("no article about {topic}"))?
            .to_string();
        let v = self.get(&format!(
            "{}/api/rest_v1/page/summary/{}",
            self.cfg.wikipedia,
            enc(&key)
        ))?;
        let title = v["title"].as_str().unwrap_or(&key).to_string();
        let mut c = Card::new(&title).subtitle(v["description"].as_str().unwrap_or("").to_string());
        for para in v["extract"]
            .as_str()
            .unwrap_or("")
            .split("\n")
            .filter(|p| !p.trim().is_empty())
        {
            c = c.line(para.to_string());
        }
        if let Some(u) = v["content_urls"]["mobile"]["page"]
            .as_str()
            .or(v["content_urls"]["desktop"]["page"].as_str())
        {
            c = c.line(u.to_string());
        }
        Ok(Ui::new(title).subtitle("Wikipedia (CC BY-SA)").card(c))
    }

    fn define(&self, word: &str) -> Result<Ui, String> {
        let w = word.trim().to_lowercase();
        let v = self.get(&format!(
            "{}/api/v2/entries/en/{}",
            self.cfg.dictionary,
            enc(&w)
        ))?;
        let entries = v.as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            return Err(format!("no definition for {w}"));
        }
        let e = &entries[0];
        let phon = e["phonetic"]
            .as_str()
            .map(String::from)
            .or_else(|| {
                e["phonetics"]
                    .as_array()
                    .and_then(|p| p.iter().find_map(|x| x["text"].as_str().map(String::from)))
            })
            .unwrap_or_default();
        let mut ui = Ui::new(e["word"].as_str().unwrap_or(&w).to_string()).subtitle(phon);
        for m in e["meanings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .take(4)
        {
            let mut c = Card::new(m["partOfSpeech"].as_str().unwrap_or(""));
            for d in m["definitions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(3)
            {
                c = c.line(d["definition"].as_str().unwrap_or("").to_string());
                if let Some(x) = d["example"].as_str() {
                    c = c.line(format!("  “{x}”"));
                }
            }
            let syn: Vec<String> = m["synonyms"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(5)
                .filter_map(|s| s.as_str().map(String::from))
                .collect();
            if !syn.is_empty() {
                c = c.line(format!("synonyms: {}", syn.join(", ")));
            }
            ui = ui.card(c);
        }
        Ok(ui)
    }

    fn holidays(&self, country: &str) -> Result<Ui, String> {
        let cc = if country.trim().is_empty() {
            self.cfg.country.to_uppercase()
        } else {
            country_code(country).ok_or(format!(
                "{country:?}: give a two-letter country code (IN, US, DE...)"
            ))?
        };
        let v = self.get(&format!(
            "{}/api/v3/NextPublicHolidays/{cc}",
            self.cfg.nager
        ))?;
        let hs = v.as_array().cloned().unwrap_or_default();
        if hs.is_empty() {
            return Err(format!(
                "no public holidays known for {cc} (Nager.Date does not cover every country)"
            ));
        }
        let mut ui = Ui::new(format!("Next public holidays, {cc}")).subtitle("Nager.Date");
        for h in hs.iter().take(8) {
            let local = h["localName"].as_str().unwrap_or("");
            let name = h["name"].as_str().unwrap_or(local);
            let mut c = Card::new(name).badge(h["date"].as_str().unwrap_or("").to_string());
            if local != name && !local.is_empty() {
                c = c.subtitle(local.to_string());
            }
            if h["global"].as_bool() == Some(false) {
                c = c.line("regional, not everywhere");
            }
            ui = ui.card(c);
        }
        Ok(ui)
    }

    fn books(&self, query: &str) -> Result<Ui, String> {
        let v = self.get(&format!("{}/search.json?q={}&limit=6&fields=title,author_name,first_publish_year,edition_count,key", self.cfg.openlibrary, enc(query)))?;
        let mut ui = Ui::new(format!("Books: {query}")).subtitle(format!(
            "{} found on Open Library",
            v["numFound"].as_u64().unwrap_or(0)
        ));
        for d in v["docs"].as_array().cloned().unwrap_or_default() {
            let authors: Vec<String> = d["author_name"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(3)
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            let mut c = Card::new(d["title"].as_str().unwrap_or("?")).subtitle(authors.join(", "));
            if let Some(y) = d["first_publish_year"].as_u64() {
                c = c.badge(y.to_string());
            }
            if let Some(k) = d["key"].as_str() {
                c = c.line(format!("{}{k}", self.cfg.openlibrary));
            }
            ui = ui.card(c);
        }
        Ok(ui)
    }

    fn radio(&self, query: &str) -> Result<Ui, String> {
        // a genre is a tag ("jazz"); otherwise a station's name
        let base = format!(
            "{}/json/stations/search?limit=6&hidebroken=true&order=votes&reverse=true",
            self.cfg.radio
        );
        let mut v = self.get(&format!("{base}&tag={}", enc(&query.to_lowercase())))?;
        if v.as_array().is_none_or(|a| a.is_empty()) {
            v = self.get(&format!("{base}&name={}", enc(query)))?;
        }
        let mut ui = Ui::new(format!("Radio: {query}")).subtitle("radio-browser.info");
        for s in v.as_array().cloned().unwrap_or_default() {
            let tags: Vec<&str> = s["tags"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .filter(|t| !t.is_empty())
                .take(4)
                .collect();
            ui = ui.card(
                Card::new(s["name"].as_str().unwrap_or("?").trim())
                    .subtitle(format!(
                        "{} · {} {} kbps",
                        s["country"].as_str().unwrap_or(""),
                        s["codec"].as_str().unwrap_or(""),
                        s["bitrate"].as_u64().unwrap_or(0)
                    ))
                    .line(tags.join(", "))
                    .line(
                        s["url_resolved"]
                            .as_str()
                            .or(s["url"].as_str())
                            .unwrap_or("")
                            .to_string(),
                    ),
            );
        }
        if ui.cards.is_empty() {
            ui = ui.card(Card::new("No station found"));
        }
        Ok(ui)
    }

    fn podcasts(&self, query: &str) -> Result<Ui, String> {
        let v = self.get(&format!(
            "{}/search?media=podcast&entity=podcast&limit=6&term={}",
            self.cfg.itunes,
            enc(query)
        ))?;
        let mut ui = Ui::new(format!("Podcasts: {query}")).subtitle("iTunes directory");
        for p in v["results"].as_array().cloned().unwrap_or_default() {
            let mut c = Card::new(p["collectionName"].as_str().unwrap_or("?"))
                .subtitle(p["artistName"].as_str().unwrap_or("").to_string());
            if let Some(n) = p["trackCount"].as_u64() {
                c = c.badge(format!("{n} episodes"));
            }
            if let Some(g) = p["primaryGenreName"].as_str() {
                c = c.line(g.to_string());
            }
            if let Some(f) = p["feedUrl"].as_str() {
                c = c.line(format!("feed {f}"));
            }
            ui = ui.card(c);
        }
        if ui.cards.is_empty() {
            ui = ui.card(Card::new("No podcast found"));
        }
        Ok(ui)
    }

    fn recipe(&self, dish: &str) -> Result<Ui, String> {
        let v = self.get(&format!(
            "{}/api/json/v1/1/search.php?s={}",
            self.cfg.mealdb,
            enc(dish)
        ))?;
        let meals = v["meals"].as_array().cloned().unwrap_or_default();
        let m = meals.first().ok_or(format!("no recipe for {dish}"))?;
        let mut ing = Card::new("Ingredients");
        for i in 1..=20 {
            let name = m[&format!("strIngredient{i}")]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let qty = m[&format!("strMeasure{i}")]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            ing = ing.line(if qty.is_empty() {
                name
            } else {
                format!("{qty} {name}")
            });
        }
        let mut how = Card::new("Method");
        for (n, step) in m["strInstructions"]
            .as_str()
            .unwrap_or("")
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(12)
            .enumerate()
        {
            how = how.line(format!("{}. {step}", n + 1));
        }
        let more = if meals.len() > 1 {
            format!("{} more recipes match", meals.len() - 1)
        } else {
            String::new()
        };
        Ok(Ui::new(m["strMeal"].as_str().unwrap_or(dish).to_string())
            .subtitle(format!(
                "{} · {} {}",
                m["strArea"].as_str().unwrap_or(""),
                m["strCategory"].as_str().unwrap_or(""),
                more
            ))
            .card(ing)
            .card(how))
    }

    fn translate(&self, text: &str, to: &str, from: &str) -> Result<Ui, String> {
        let to = lang(to).ok_or(format!(
            "{to:?}: which language? (hindi, kannada, tamil, french... or a code)"
        ))?;
        let from = if from.trim().is_empty() {
            "en".to_string()
        } else {
            lang(from).ok_or(format!("{from:?}: which language?"))?
        };
        let v = self.get(&format!(
            "{}/get?q={}&langpair={from}|{to}",
            self.cfg.mymemory,
            enc(text.trim())
        ))?;
        let out = v["responseData"]["translatedText"]
            .as_str()
            .ok_or("no translation")?;
        if v["responseStatus"].as_i64().is_some_and(|s| s != 200) {
            return Err(format!("translation failed: {out}"));
        }
        let m = v["responseData"]["match"]
            .as_f64()
            .map(|x| format!(" · match {:.0}%", x * 100.0))
            .unwrap_or_default();
        Ok(Ui::new(format!("{from} → {to}")).card(
            Card::new(out)
                .line(format!("from: {}", text.trim()))
                .line(format!("MyMemory{m}"))
                .tone("info"),
        ))
    }

    fn crypto(&self, coin: &str) -> Result<Ui, String> {
        let id = coin_id(coin);
        let vs = self.cfg.currency.to_lowercase();
        let v = self.get(&format!(
            "{}/api/v3/simple/price?ids={id}&vs_currencies={vs},usd&include_24hr_change=true",
            self.cfg.coingecko
        ))?;
        let c = v.get(&id).ok_or(format!(
            "no price for {coin} (CoinGecko ids: bitcoin, ethereum, solana...)"
        ))?;
        let price = c[&vs].as_f64().ok_or("no price")?;
        let ch = c[&format!("{vs}_24h_change")].as_f64();
        let delta = ch
            .map(|ch| format!("{}{ch:.2}%", if ch >= 0.0 { "+" } else { "" }))
            .unwrap_or_default();
        let mut card = Card::new(format!("{price:.2} {}", vs.to_uppercase())).metric(
            format!("{price:.2}"),
            vs.to_uppercase(),
            delta,
        );
        if let Some(u) = c["usd"].as_f64() {
            card = card.line(format!("{u:.2} USD"));
        }
        if let Some(ch) = ch {
            card = card
                .line(format!(
                    "{}{ch:.2}% in 24 hours",
                    if ch >= 0.0 { "+" } else { "" }
                ))
                .tone(if ch >= 0.0 { "ok" } else { "warn" });
        }
        Ok(Ui::new(id.to_string())
            .kind("money")
            .subtitle("CoinGecko · prices move; not advice")
            .card(card))
    }
}

// ---- small tables ------------------------------------------------------------------------------

/// WMO weather codes, as Open-Meteo reports them.
pub fn wmo(c: i64) -> String {
    match c {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 | 77 => "snow",
        80 | 81 => "rain showers",
        82 => "violent rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "weather unknown",
    }
    .to_string()
}

fn cur(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    let named = [
        ("rupee", "INR"),
        ("rupees", "INR"),
        ("inr", "INR"),
        ("dollar", "USD"),
        ("dollars", "USD"),
        ("euro", "EUR"),
        ("euros", "EUR"),
        ("pound", "GBP"),
        ("pounds", "GBP"),
        ("yen", "JPY"),
        ("dirham", "AED"),
        ("dirhams", "AED"),
    ];
    if let Some((_, c)) = named.iter().find(|(n, _)| *n == s) {
        return Some(c.to_string());
    }
    (s.len() == 3 && s.chars().all(|c| c.is_ascii_alphabetic())).then(|| s.to_uppercase())
}

fn country_code(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    let named = [
        ("india", "IN"),
        ("united states", "US"),
        ("usa", "US"),
        ("us", "US"),
        ("uk", "GB"),
        ("united kingdom", "GB"),
        ("germany", "DE"),
        ("france", "FR"),
        ("japan", "JP"),
        ("singapore", "SG"),
        ("australia", "AU"),
        ("canada", "CA"),
        ("uae", "AE"),
    ];
    if let Some((_, c)) = named.iter().find(|(n, _)| *n == s) {
        return Some(c.to_string());
    }
    (s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic())).then(|| s.to_uppercase())
}

fn lang(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    let named = [
        ("english", "en"),
        ("hindi", "hi"),
        ("kannada", "kn"),
        ("tamil", "ta"),
        ("telugu", "te"),
        ("malayalam", "ml"),
        ("marathi", "mr"),
        ("bengali", "bn"),
        ("gujarati", "gu"),
        ("punjabi", "pa"),
        ("urdu", "ur"),
        ("french", "fr"),
        ("german", "de"),
        ("spanish", "es"),
        ("japanese", "ja"),
        ("chinese", "zh-CN"),
        ("arabic", "ar"),
    ];
    if let Some((_, c)) = named.iter().find(|(n, _)| *n == s) {
        return Some(c.to_string());
    }
    (s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic())).then_some(s)
}

fn coin_id(s: &str) -> String {
    match s.trim().to_lowercase().as_str() {
        "btc" => "bitcoin".into(),
        "eth" | "ether" => "ethereum".into(),
        "sol" => "solana".into(),
        "doge" => "dogecoin".into(),
        x => x.replace(' ', "-"),
    }
}

fn host(url: &str) -> &str {
    url.split("//")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
}

// ---- tools -------------------------------------------------------------------------------------

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "weather",
        args: &["place"],
        tier: Tier::Low,
    },
    Tool {
        name: "air_quality",
        args: &["place"],
        tier: Tier::Low,
    },
    Tool {
        name: "time_in",
        args: &["place"],
        tier: Tier::Low,
    },
    Tool {
        name: "places",
        args: &["query"],
        tier: Tier::Low,
    },
    Tool {
        name: "directions",
        args: &["from", "to"],
        tier: Tier::Low,
    },
    Tool {
        name: "convert_currency",
        args: &["amount", "from", "to"],
        tier: Tier::Low,
    },
    Tool {
        name: "wiki",
        args: &["topic"],
        tier: Tier::Low,
    },
    Tool {
        name: "define",
        args: &["word"],
        tier: Tier::Low,
    },
    Tool {
        name: "holidays",
        args: &["country"],
        tier: Tier::Low,
    },
    Tool {
        name: "books",
        args: &["query"],
        tier: Tier::Low,
    },
    Tool {
        name: "radio",
        args: &["query"],
        tier: Tier::Low,
    },
    Tool {
        name: "podcasts",
        args: &["query"],
        tier: Tier::Low,
    },
    Tool {
        name: "recipe",
        args: &["dish"],
        tier: Tier::Low,
    },
    Tool {
        name: "translate",
        args: &["text", "to", "from"],
        tier: Tier::Low,
    },
    Tool {
        name: "crypto",
        args: &["coin"],
        tier: Tier::Low,
    },
];

pub fn describe(tool: &str) -> &'static str {
    match tool {
        "weather" => "current weather and the next days' forecast for a place",
        "air_quality" => "air quality (AQI, PM2.5) in a place",
        "time_in" => "the local time in a place",
        "places" => "find places (cafes, ATMs, hospitals, a street) on OpenStreetMap",
        "directions" => "road route, distance and time between two places",
        "convert_currency" => "convert an amount between currencies (ECB rates)",
        "wiki" => "a short encyclopedia summary of a topic (Wikipedia)",
        "define" => "a word's meanings, examples and synonyms",
        "holidays" => "upcoming public holidays in a country",
        "books" => "find books by title, author or subject",
        "radio" => "internet radio stations by genre or name",
        "podcasts" => "find podcasts on a topic",
        "recipe" => "a recipe: ingredients and method",
        "translate" => "translate text into another language",
        "crypto" => "a cryptocurrency's price",
        _ => "",
    }
}

impl Connector for Apps {
    fn name(&self) -> &str {
        "apps"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let g = |k: &str| {
            a.args
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default()
        };
        let ui = match a.tool.as_str() {
            "weather" => self.weather(&g("place"))?,
            "air_quality" => self.air(&g("place"))?,
            "time_in" => self.time_in(&g("place"))?,
            "places" => self.places(&g("query"))?,
            "directions" => self.directions(&g("from"), &g("to"))?,
            "convert_currency" => self.currency(&g("amount"), &g("from"), &g("to"))?,
            "wiki" => self.wiki(&g("topic"))?,
            "define" => self.define(&g("word"))?,
            "holidays" => self.holidays(&g("country"))?,
            "books" => self.books(&g("query"))?,
            "radio" => self.radio(&g("query"))?,
            "podcasts" => self.podcasts(&g("query"))?,
            "recipe" => self.recipe(&g("dish"))?,
            "translate" => self.translate(&g("text"), &g("to"), &g("from"))?,
            "crypto" => self.crypto(&g("coin"))?,
            other => return Err(format!("apps cannot {other}")),
        };
        Ok(ui.to_reply())
    }
}
