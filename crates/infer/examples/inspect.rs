//! Print a GGUF file's metadata and tensors: cargo run -p agentos-infer --example inspect -- model.gguf
fn main() {
    let path = std::env::args().nth(1).expect("model.gguf");
    let g = agentos_infer::gguf::Gguf::open(std::path::Path::new(&path)).unwrap();
    println!(
        "GGUF v{}, arch {}, {} tensors",
        g.version,
        g.arch(),
        g.tensors.len()
    );
    for (k, v) in &g.meta {
        let s = match v {
            agentos_infer::gguf::Value::Array(a) => format!("[{} items]", a.len()),
            v => format!("{v:?}"),
        };
        println!("  {k} = {}", s.chars().take(110).collect::<String>());
    }
    let mut kinds = std::collections::BTreeMap::new();
    for t in &g.tensors {
        let base = t
            .name
            .split('.')
            .filter(|p| p.parse::<u32>().is_err())
            .collect::<Vec<_>>()
            .join(".");
        kinds
            .entry(base)
            .or_insert_with(Vec::new)
            .push(format!("{:?}{:?}", t.dtype, t.dims));
    }
    for (k, v) in kinds {
        println!("  {k:40} x{:<3} {}", v.len(), v[0]);
    }
}
