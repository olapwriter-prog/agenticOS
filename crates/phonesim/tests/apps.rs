//! The apps (agentos-apps) against the simulated public APIs: every tool through the connector
//! agentd uses, answers checked, and the errors people will meet (an unknown word, a country a
//! service does not cover, a currency code that is not one).
use agentos_action::Action;
use agentos_apps::{Apps, Config};
use agentos_executor::Connector;
use agentos_screen::Ui;
use std::process::{Child, Command};

struct World(Child, String);
impl Drop for World {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}
fn world() -> World {
    // each test its own port: tests in one file run at the same time
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let port = 19_000 + (std::process::id() % 400) as u16 * 2 + NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let child = Command::new(env!("CARGO_BIN_EXE_agentos-phonesim")).args(["world", &port.to_string()]).spawn().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if ureq::get(&format!("{url}/health")).call().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    World(child, url)
}

fn call(a: &mut Apps, tool: &str, args: &[(&str, &str)]) -> Result<String, String> {
    let act = Action { tool: tool.into(), args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    let r = a.call(&act)?;
    let (ui, text) = Ui::from_reply(&r).unwrap_or_else(|| panic!("{tool}: not a screen: {r}"));
    assert!(!ui.title.is_empty() && !ui.cards.is_empty(), "{tool}: an empty screen");
    Ok(text)
}
fn ok(a: &mut Apps, tool: &str, args: &[(&str, &str)]) -> String {
    call(a, tool, args).unwrap_or_else(|e| panic!("{tool} {args:?}: {e}"))
}

#[test]
fn every_app_against_the_simulated_apis() {
    let w = world();
    let mut a = Apps { cfg: Config::all_at(&w.1) };

    let t = ok(&mut a, "weather", &[("place", "")]);
    assert!(t.starts_with("Weather in Bengaluru (India)") && t.contains("partly cloudy [24°C]") && t.contains("chance of rain 70%") && t.contains("sunrise 06:12"), "{t}");
    let t = ok(&mut a, "weather", &[("place", "Pune, India")]);
    assert!(t.starts_with("Weather in Pune"), "{t}");
    assert!(call(&mut a, "weather", &[("place", "Atlantis")]).unwrap_err().contains("no place called Atlantis"));
    let t = ok(&mut a, "air_quality", &[("place", "Bengaluru")]);
    assert!(t.contains("moderate [AQI 87]") && t.contains("PM2.5 29"), "{t}");
    let t = ok(&mut a, "time_in", &[("place", "London")]);
    assert!(t.contains("Europe/London, UTC+0:00"), "{t}");
    let t = ok(&mut a, "time_in", &[("place", "Mysuru")]);
    assert!(t.contains("UTC+5:30"), "{t}");

    let t = ok(&mut a, "places", &[("query", "cafes in Indiranagar")]);
    assert!(t.contains("Third Wave Coffee") && t.contains("Dyu Art Cafe") && t.contains("amenity cafe"), "{t}");
    let t = ok(&mut a, "directions", &[("from", "Bengaluru"), ("to", "Mysore Palace")]);
    assert!(t.contains("Bengaluru to Mysore Palace") && t.contains("km]") && t.contains("at the roundabout, take exit 2 onto Bengaluru–Mysuru Expressway"), "{t}");
    assert!(t.contains("without live traffic"), "{t}");

    let t = ok(&mut a, "convert_currency", &[("amount", "100"), ("from", "dollars"), ("to", "")]);
    assert!(t.contains("8813.70 INR") && t.contains("1 USD = 88.1370 INR"), "{t}");
    assert!(call(&mut a, "convert_currency", &[("amount", "5"), ("from", "XYZ"), ("to", "INR")]).is_err());
    assert!(call(&mut a, "convert_currency", &[("amount", "five"), ("from", "USD"), ("to", "INR")]).unwrap_err().contains("not an amount"));

    let t = ok(&mut a, "wiki", &[("topic", "mysore palace")]);
    assert!(t.contains("Wadiyar dynasty") && t.contains("en.m.wikipedia.org"), "{t}");
    let t = ok(&mut a, "define", &[("word", "Serendipity")]);
    assert!(t.contains("/ˌsɛɹ.ənˈdɪp.ɪ.ti/") && t.contains("“It was pure serendipity that we met.”") && t.contains("synonyms: chance, fluke, luck"), "{t}");
    assert!(call(&mut a, "define", &[("word", "blorptastic")]).unwrap_err().contains("no definition"));

    let t = ok(&mut a, "holidays", &[("country", "germany")]);
    assert!(t.contains("Christmas Day") && t.contains("Erster Weihnachtstag"), "{t}");
    assert!(call(&mut a, "holidays", &[("country", "")]).unwrap_err().contains("does not cover every country"), "India is not on Nager.Date");

    let t = ok(&mut a, "books", &[("query", "narayan")]);
    assert!(t.contains("Malgudi Days") && t.contains("R. K. Narayan") && t.contains("/works/OL5735363W"), "{t}");
    let t = ok(&mut a, "radio", &[("query", "Jazz")]);
    assert!(t.contains("Radio Indigo Jazz") && t.contains("Swiss Jazz") && t.contains("https://stream.example.net/indigojazz.mp3"), "a genre: {t}");
    let t = ok(&mut a, "radio", &[("query", "radio city")]);
    assert!(t.contains("Radio City Kannada"), "a name, when no genre matches: {t}");
    let t = ok(&mut a, "podcasts", &[("query", "gst")]);
    assert!(t.contains("GST Simplified [88 episodes]"), "{t}");
    let t = ok(&mut a, "recipe", &[("dish", "biryani")]);
    assert!(t.contains("Chicken Biryani (Indian · Chicken") && t.contains("1 kg Chicken") && t.contains("4. Seal and cook on low heat"), "{t}");
    assert!(call(&mut a, "recipe", &[("dish", "moon cheese")]).unwrap_err().contains("no recipe"));
    let t = ok(&mut a, "translate", &[("text", "Good morning"), ("to", "hindi"), ("from", "")]);
    assert!(t.contains("सुप्रभात"), "{t}");
    let t = ok(&mut a, "translate", &[("text", "thank you"), ("to", "kannada"), ("from", "english")]);
    assert!(t.contains("ಧನ್ಯವಾದಗಳು"), "{t}");
    assert!(call(&mut a, "translate", &[("text", "x"), ("to", "klingon"), ("from", "")]).unwrap_err().contains("which language"));
    let t = ok(&mut a, "crypto", &[("coin", "BTC")]);
    assert!(t.contains("8512345.12 INR") && t.contains("-1.23% in 24 hours"), "{t}");
    assert!(call(&mut a, "crypto", &[("coin", "notacoin")]).unwrap_err().contains("no price"));
}

#[test]
fn query_values_are_encoded() {
    assert_eq!(agentos_apps::enc("cafés & bars"), "caf%C3%A9s%20%26%20bars");
    assert_eq!(agentos_apps::wmo(95), "thunderstorm");
}

#[test]
fn answers_carry_widgets_the_screen_draws() {
    let w = world();
    let mut a = Apps { cfg: Config::all_at(&w.1) };
    let ui = |a: &mut Apps, tool: &str, args: &[(&str, &str)]| {
        let act = Action { tool: tool.into(), args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        Ui::from_reply(&a.call(&act).unwrap()).unwrap().0
    };
    let weather = ui(&mut a, "weather", &[("place", "")]);
    assert_eq!(weather.kind, "weather");
    let m = weather.cards[0].metric.as_ref().expect("the temperature as a big number");
    assert_eq!(m.value, "24°");
    let highs = weather.cards.iter().find_map(|c| c.chart.as_ref().filter(|ch| ch.kind == "line")).expect("a chart of the highs");
    assert_eq!(highs.labels.len(), highs.values.len());
    assert_eq!(highs.labels[0], "Today");
    let air = ui(&mut a, "air_quality", &[("place", "Bengaluru")]);
    assert_eq!(air.cards[0].metric.as_ref().unwrap().value, "87");
    assert!((air.cards[0].progress.unwrap() - 87.0 / 300.0).abs() < 0.001);
    let coin = ui(&mut a, "crypto", &[("coin", "BTC")]);
    assert_eq!(coin.cards[0].metric.as_ref().unwrap().delta, "-1.23%");
}
