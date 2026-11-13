//! agentos-skillpack: build and query the phone's skill library.
//!
//!   agentos-skillpack import PACK_DIR REPO_DIR --source NAME --repo URL --commit SHA
//!                     [--repo-license SPDX --evidence "where it says so"]
//!   agentos-skillpack search PACK_DIR TOPIC...
//!   agentos-skillpack excerpt PACK_DIR SKILL [--budget TOKENS] QUESTION...
//!
//! import prints a JSON report: kept, and every skipped folder with the reason.
use agentos_skillpack::{excerpt, excerpt_text, import, write_pack, Library, Source};
use std::path::Path;

fn die(m: &str) -> ! {
    eprintln!("agentos-skillpack: {m}");
    std::process::exit(2)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let opt = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    match a.first().map(String::as_str) {
        Some("import") if a.len() >= 3 => {
            let src = Source {
                name: opt("--source").unwrap_or_else(|| die("--source NAME")),
                repo: opt("--repo").unwrap_or_default(),
                commit: opt("--commit").unwrap_or_else(|| "HEAD".into()),
                repo_license: opt("--repo-license").map(|l| (l, opt("--evidence").unwrap_or_else(|| "repository LICENSE".into()))),
            };
            let (kept, skipped) = import(Path::new(&a[2]), &src);
            let note = format!("{} at {}{}", src.repo, src.commit, src.repo_license.as_ref().map(|(l, w)| format!("; {l} ({w})")).unwrap_or_default());
            let total = write_pack(Path::new(&a[1]), &kept, (&src.name, &note)).unwrap_or_else(|e| die(&e));
            let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
            for s in &skipped {
                let r = if s.reason.starts_with("duplicate") { "duplicate".to_string() } else { s.reason.clone() };
                *reasons.entry(r).or_default() += 1;
            }
            println!("{}", serde_json::json!({"source": src.name, "kept": kept.len(), "skipped": skipped.len(), "skipped_by_reason": reasons, "skipped_folders": skipped, "pack_total": total}));
        }
        Some("search") if a.len() >= 3 => {
            let lib = Library::open(Path::new(&a[1])).unwrap_or_else(|e| die(&e));
            for (e, s) in lib.search(&a[2..].join(" "), 8) {
                println!("{s:6.2}  {:<45} {}", e.id, e.description.chars().take(90).collect::<String>());
            }
        }
        Some("excerpt") if a.len() >= 4 => {
            let lib = Library::open(Path::new(&a[1])).unwrap_or_else(|e| die(&e));
            let budget: usize = opt("--budget").and_then(|b| b.parse().ok()).unwrap_or(700);
            let words: Vec<&String> = a[3..].iter().filter(|w| *w != "--budget" && Some(*w) != opt("--budget").as_ref()).collect();
            let e = lib.find(&a[2]).unwrap_or_else(|| die("no such skill"));
            let s = lib.load(e).unwrap_or_else(|e| die(&e));
            let q = words.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            let (secs, used) = excerpt(&s, &q, budget);
            println!("{}\n[{used} tokens of {budget}]", excerpt_text(&s, &secs));
        }
        _ => die("import PACK REPO --source NAME --repo URL --commit SHA [--repo-license SPDX --evidence TEXT] | search PACK TOPIC | excerpt PACK SKILL [--budget N] QUESTION"),
    }
}
