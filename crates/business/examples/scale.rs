//! How the answers scale with the size of the book: `cargo run --release -p agentos-business --example scale`
use agentos_business::dataset::{day_events, import, payout_event, play};
use agentos_business::profile::Profile;
use agentos_business::util::add_days;
use agentos_business::wa::SimWhatsapp;
use agentos_business::Business;
use std::time::Instant;

fn main() {
    let profile = Profile::load(std::path::Path::new("examples/cafe/profile.toml")).unwrap();
    println!("{:>5} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10}", "days", "orders", "dashboard", "plan", "gst", "payouts", "issues", "state", "new order");
    for days in [30, 90, 180, 365] {
        let dir = std::env::temp_dir().join(format!("biz-scale-{days}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut b = Business::open(profile.clone(), dir.clone(), &[3u8; 32], Box::new(SimWhatsapp::default())).unwrap();
        b.profile.auto_erase = false;
        let first = add_days("2026-11-13", -(days as i64) + 1);
        let mut text = String::new();
        for d in 0..days {
            text.push_str(&day_events(&b.profile, &add_days(&first, d as i64), 7, 1.0, 0.0).join("\n"));
            text.push('\n');
        }
        import(&mut b, &text).unwrap();
        // a payout a week, as platforms pay
        let mut from = first.clone();
        while add_days(&from, 9) <= "2026-11-13".to_string() {
            for pf in ["swiggy", "zomato"] {
                let ev = payout_event(&b, pf, &from, &add_days(&from, 6), "2026-11-13T09:00", 0.0);
                play(&mut b, &ev).unwrap();
            }
            from = add_days(&from, 7);
        }
        let time = |f: &dyn Fn() -> usize| {
            let t = Instant::now();
            let mut n = 0;
            for _ in 0..5 {
                n += f();
            }
            let _ = n;
            t.elapsed().as_secs_f64() * 1000.0 / 5.0
        };
        let t_dash = time(&|| b.dashboard().len());
        let t_plan = time(&|| agentos_business::plan::plan(&b).text().len());
        let t_gst = time(&|| b.gst_text("2026-11").len());
        let t_pay = time(&|| b.payouts_text().len());
        let t_iss = time(&|| b.issues().len());
        let t_state = time(&|| b.state().to_string().len());
        let t = Instant::now();
        for k in 0..20 {
            play(&mut b, &format!(r#"{{"t":"order","id":"SW-X{k}","channel":"swiggy","at":"2026-11-13T23:0{}","items":[{{"code":"R3","qty":1}}],"locality":"Siripuram","km":2}}"#, k % 10)).unwrap();
        }
        let t_order = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        println!("{days:>5} {:>7} {t_dash:>7.1}ms {t_plan:>7.1}ms {t_gst:>7.1}ms {t_pay:>7.1}ms {t_iss:>7.1}ms {t_state:>7.1}ms {t_order:>8.2}ms", b.book.orders.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
