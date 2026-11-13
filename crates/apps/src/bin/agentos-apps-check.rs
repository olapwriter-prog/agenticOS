//! Calls every app once against the real public APIs (the defaults in agentos_apps::Config) and
//! says which answered. Run it on a machine or phone with internet access:
//!   agentos-apps-check            all apps
//!   agentos-apps-check -v         and print each answer
//! Exit status: 0 if every app answered, 1 otherwise.
use agentos_action::Action;
use agentos_apps::{Apps, Config};
use agentos_executor::Connector;

fn main() {
    let verbose = std::env::args().any(|a| a == "-v");
    let mut apps = Apps {
        cfg: Config::default(),
    };
    let checks: &[(&str, &[(&str, &str)])] = &[
        ("weather", &[("place", "")]),
        ("air_quality", &[("place", "Bengaluru")]),
        ("time_in", &[("place", "London")]),
        ("places", &[("query", "cafe in Indiranagar, Bengaluru")]),
        (
            "directions",
            &[("from", "Bengaluru"), ("to", "Mysore Palace")],
        ),
        (
            "convert_currency",
            &[("amount", "100"), ("from", "USD"), ("to", "INR")],
        ),
        ("wiki", &[("topic", "Mysore Palace")]),
        ("define", &[("word", "serendipity")]),
        ("holidays", &[("country", "germany")]),
        ("books", &[("query", "R. K. Narayan")]),
        ("radio", &[("query", "jazz")]),
        ("podcasts", &[("query", "gst")]),
        ("recipe", &[("dish", "biryani")]),
        (
            "translate",
            &[("text", "good morning"), ("to", "hindi"), ("from", "")],
        ),
        ("crypto", &[("coin", "bitcoin")]),
    ];
    let mut failed = 0;
    for (tool, args) in checks {
        let act = Action {
            tool: tool.to_string(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let t0 = std::time::Instant::now();
        let r = apps.call(&act);
        let ms = t0.elapsed().as_millis();
        match r {
            Ok(reply) => {
                let text = agentos_screen::Ui::from_reply(&reply)
                    .map(|(_, t)| t)
                    .unwrap_or(reply);
                println!(
                    "  OK    {tool:<17} {ms:>5} ms  {}",
                    text.lines().next().unwrap_or("")
                );
                if verbose {
                    for l in text.lines().skip(1).take(8) {
                        println!("          {l}");
                    }
                }
            }
            Err(e) => {
                failed += 1;
                println!("  FAIL  {tool:<17} {ms:>5} ms  {e}");
            }
        }
    }
    println!(
        "\n{} of {} apps answered from the live services",
        checks.len() - failed,
        checks.len()
    );
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
