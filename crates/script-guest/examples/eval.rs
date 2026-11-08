//! Run one Rhai expression through the script runtime: cargo run -p agentos-script-guest --example eval -- 'code'
fn main() {
    let code = std::env::args().nth(1).expect("code");
    let input = serde_json::json!({"code": code, "args": {"word": "Level", "n": "7"}}).to_string();
    println!("{}", agentos_script_guest::execute(input.as_bytes()));
}
