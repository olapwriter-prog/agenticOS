//! cargo run -p agentos-infer --example tokenize -- model.gguf "text"
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let g = agentos_infer::gguf::Gguf::open(std::path::Path::new(&a[1])).unwrap();
    let t = agentos_infer::tokenizer::Tokenizer::from_gguf(&g).unwrap();
    let ids = t.encode(&a[2]);
    println!(
        "{}",
        ids.iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(t.decode(&ids), a[2], "round trip");
}
