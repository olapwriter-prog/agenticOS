//! agentos-llm: the phone's own LLM server, a drop-in for llama.cpp's llama-server as agentd
//! uses it. Same flags (-m --host --port -c -t --mlock; -ngl is accepted and ignored: this
//! engine runs on the CPU), same endpoints (/health, /completion, /v1/chat/completions with
//! `grammar`, `json_schema` and `chat_template_kwargs.enable_thinking`).
use agentos_infer::engine::{Engine, Msg, Request, Timings};
use agentos_infer::{gguf::Gguf, grammar, model::Model, tokenizer::Tokenizer};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

struct Args {
    model: String,
    host: String,
    port: u16,
    ctx: usize,
    threads: usize,
    mlock: bool,
    lora: Option<String>,
    draft: Option<String>,
    draft_n: usize,
    /// memory for cached prompt states, MiB
    cache_mb: usize,
    /// a folder of named adapters (experts): <name>.lora, or <name>/adapter.lora
    adapters: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        model: String::new(),
        host: "127.0.0.1".into(),
        port: 8080,
        ctx: 4096,
        threads: 0,
        mlock: false,
        lora: None,
        draft: None,
        draft_n: 5,
        cache_mb: 512,
        adapters: None,
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let val = |i: usize| {
        v.get(i + 1)
            .cloned()
            .ok_or(format!("{} needs a value", v[i]))
    };
    while i < v.len() {
        match v[i].as_str() {
            "-m" | "--model" => {
                a.model = val(i)?;
                i += 1;
            }
            "--host" => {
                a.host = val(i)?;
                i += 1;
            }
            "--port" => {
                a.port = val(i)?.parse().map_err(|_| "bad --port")?;
                i += 1;
            }
            "-c" | "--ctx-size" => {
                a.ctx = val(i)?.parse().map_err(|_| "bad -c")?;
                i += 1;
            }
            "-t" | "--threads" => {
                a.threads = val(i)?.parse().map_err(|_| "bad -t")?;
                i += 1;
            }
            "--mlock" => a.mlock = true,
            "--lora" => {
                a.lora = Some(val(i)?);
                i += 1;
            }
            "--draft" => {
                a.draft = Some(val(i)?);
                i += 1;
            }
            "--draft-n" => {
                a.draft_n = val(i)?.parse().map_err(|_| "bad --draft-n")?;
                i += 1;
            }
            "--cache-mb" => {
                a.cache_mb = val(i)?.parse().map_err(|_| "bad --cache-mb")?;
                i += 1;
            }
            "--adapters" => {
                a.adapters = Some(val(i)?);
                i += 1;
            }
            "-ngl" | "--n-gpu-layers" => i += 1,
            // llama-server flags this engine does not need; skip a value if one follows
            other
                if other.starts_with('-') && v.get(i + 1).is_some_and(|n| !n.starts_with('-')) =>
            {
                i += 1
            }
            _ => {}
        }
        i += 1;
    }
    if a.model.is_empty() {
        return Err("usage: agentos-llm -m model.gguf [--host H] [--port P] [-c CTX] [-t THREADS] [--mlock] [--lora A] [--draft small.gguf] [--draft-n N] [--cache-mb MB] [--adapters DIR]".into());
    }
    Ok(a)
}

fn reply(s: &mut TcpStream, code: u16, body: &Value) {
    let b = body.to_string();
    let status = if code == 200 { "OK" } else { "Error" };
    let _ = write!(s, "HTTP/1.1 {code} {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len());
}

fn timings_json(t: &Timings) -> Value {
    json!({
        "prompt_n": t.prompt_n,
        "cache_n": t.prompt_cached,
        "prompt_ms": t.prompt_ms,
        "prompt_per_second": (t.prompt_n - t.prompt_cached) as f64 / (t.prompt_ms / 1000.0).max(1e-9),
        "predicted_n": t.predicted_n,
        "predicted_ms": t.predicted_ms,
        "predicted_per_second": t.predicted_n as f64 / (t.predicted_ms / 1000.0).max(1e-9),
        "draft_n": t.drafted,
        "draft_n_accepted": t.accepted,
    })
}

/// The grammar for a request: an explicit GBNF, or one made from a JSON schema.
fn grammar_of(req: &Value) -> Result<Option<String>, String> {
    if let Some(g) = req["grammar"].as_str().filter(|g| !g.trim().is_empty()) {
        return Ok(Some(g.to_string()));
    }
    let schema = req
        .get("json_schema")
        .or_else(|| req["response_format"].get("schema"))
        .or_else(|| req["response_format"]["json_schema"].get("schema"));
    match schema {
        Some(s) if !s.is_null() => Ok(Some(grammar::json_schema_to_gbnf(s)?)),
        _ => Ok(None),
    }
}

/// The cache as JSON: budget, use, entries and counters.
fn cache_json(e: &Engine) -> Value {
    let c = &e.cache;
    let entries: Vec<Value> = c
        .infos()
        .iter()
        .map(|i| {
            json!({"id": i.id, "adapter": i.adapter, "label": i.label, "tokens": i.tokens,
                   "bytes": i.bytes, "hits": i.hits, "idle": i.idle, "priority": i.priority,
                   "warmed": i.warmed, "build_ms": i.build_ms})
        })
        .collect();
    let st = &c.stats;
    json!({
        "budget_bytes": c.budget, "used_bytes": c.used(), "max_entries": c.max_entries,
        "adapter": e.adapter(), "adapters": e.adapter_names(),
        "entries": entries,
        "stats": {"hits": st.hits, "misses": st.misses, "evictions": st.evictions,
                  "dropped": st.dropped, "warmed": st.warmed, "warm_used": st.warm_used,
                  "saved_ms": st.saved_ms, "peak_bytes": st.peak_bytes}
    })
}

/// The prompt a request reads: raw, or chat messages through the template.
fn prompt_of(e: &Engine, req: &Value) -> Result<String, String> {
    if let Some(p) = req["prompt"].as_str() {
        return Ok(p.to_string());
    }
    let msgs: Vec<Msg> = req["messages"]
        .as_array()
        .ok_or("no prompt or messages")?
        .iter()
        .map(|m| Msg {
            role: m["role"].as_str().unwrap_or("user").to_string(),
            content: m["content"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    let thinking = req["chat_template_kwargs"]["enable_thinking"]
        .as_bool()
        .unwrap_or(false);
    Ok(e.chat_prompt(&msgs, thinking))
}

fn handle(e: &mut Engine, path: &str, body: &str) -> Result<Value, String> {
    let mut req: Value = serde_json::from_str(body).map_err(|e| format!("bad JSON: {e}"))?;
    match path {
        // read a prompt's shared part now, so the request that follows starts after it; the
        // adapter in use stays in use
        "/cache/warm" => {
            // the shared part ends where a user turn starts: give it one to end at
            if let Some(m) = req["messages"].as_array_mut() {
                if m.last().is_none_or(|x| x["role"] != "user") {
                    m.push(json!({"role": "user", "content": "."}));
                }
            }
            let before = e.adapter().to_string();
            if let Some(name) = req["adapter"].as_str() {
                e.use_adapter(name)?;
            }
            let prompt = prompt_of(e, &req)?;
            let label = req["label"].as_str().unwrap_or("warm").to_string();
            let w = e.warm(&prompt, &label);
            e.use_adapter(&before)?;
            let w = w?;
            let mut v = cache_json(e);
            v["warmed"] = json!({"tokens": w.tokens, "bytes": w.bytes, "ms": w.ms,
                                 "reused": w.reused, "already": w.already, "kept": w.kept});
            return Ok(v);
        }
        // the manager's decisions: a budget, priorities, entries to drop
        "/cache/policy" => {
            if let Some(n) = req["max_entries"].as_u64() {
                e.cache.max_entries = n.max(1) as usize;
            }
            for p in req["priorities"].as_array().into_iter().flatten() {
                if let (Some(id), Some(pr)) = (p["id"].as_u64(), p["priority"].as_f64()) {
                    e.cache.set_priority(id, pr as f32);
                }
            }
            for id in req["drop"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_u64())
            {
                e.cache.drop_entry(id);
            }
            if req["clear"].as_bool() == Some(true) {
                e.cache.clear();
            }
            if let Some(b) = req["budget_bytes"].as_u64() {
                e.cache.set_budget(b as usize);
            }
            return Ok(cache_json(e));
        }
        _ => {}
    }
    // a named adapter (expert) for this request; absent keeps the one in use
    if let Some(name) = req["adapter"].as_str() {
        e.use_adapter(name)?;
    }
    let temperature = req["temperature"].as_f64().unwrap_or(0.2) as f32;
    let top_k = req["top_k"].as_u64().unwrap_or(40) as usize;
    let grammar = grammar_of(&req)?;
    match path {
        "/completion" => {
            let r = Request {
                prompt: req["prompt"].as_str().ok_or("no prompt")?.to_string(),
                max_tokens: req["n_predict"].as_u64().unwrap_or(256) as usize,
                temperature,
                top_k,
                grammar,
            };
            let (text, t) = e.generate(&r)?;
            Ok(json!({"content": text, "stop": true, "timings": timings_json(&t)}))
        }
        "/v1/chat/completions" => {
            let msgs: Vec<Msg> = req["messages"]
                .as_array()
                .ok_or("no messages")?
                .iter()
                .map(|m| Msg {
                    role: m["role"].as_str().unwrap_or("user").to_string(),
                    content: m["content"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            let thinking = req["chat_template_kwargs"]["enable_thinking"]
                .as_bool()
                .unwrap_or(false);
            let r = Request {
                prompt: e.chat_prompt(&msgs, thinking),
                max_tokens: req["max_tokens"].as_u64().unwrap_or(512) as usize,
                temperature,
                top_k,
                grammar,
            };
            let (text, t) = e.generate(&r)?;
            Ok(json!({
                "object": "chat.completion",
                "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": text}}],
                "usage": {"prompt_tokens": t.prompt_n, "completion_tokens": t.predicted_n},
                "timings": timings_json(&t)
            }))
        }
        _ => Err(format!("no such endpoint {path}")),
    }
}

fn serve_one(e: &mut Engine, conn: TcpStream) {
    let mut w = match conn.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut r = BufReader::new(conn);
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if len > 16 << 20 {
        return reply(&mut w, 413, &json!({"error": "request too large"}));
    }
    let mut body = vec![0u8; len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    match (method, path) {
        ("GET", "/health") => reply(&mut w, 200, &json!({"status": "ok"})),
        ("GET", "/cache") => reply(&mut w, 200, &cache_json(e)),
        ("POST", p) => match handle(e, p, &String::from_utf8_lossy(&body)) {
            Ok(v) => reply(&mut w, 200, &v),
            Err(err) => reply(&mut w, 400, &json!({"error": {"message": err}})),
        },
        _ => reply(&mut w, 404, &json!({"error": "not found"})),
    }
}

/// `agentos-llm train -m MODEL --data sft.jsonl --out adapter.lora [--grammar FILE]
/// [--steps N] [--batch N] [--rank N] [--lr X] [--max-seconds N] [--memory-mb N] [-t N]`
/// `--memory-mb`: activations kept per example; longer examples recompute layers instead.
/// Prints progress on stderr and one JSON summary line on stdout.
fn train_main(v: &[String]) -> Result<(), String> {
    use agentos_infer::finetune::{run, Job};
    let mut job = Job::default();
    let mut model = String::new();
    let mut i = 0;
    let val = |i: usize| {
        v.get(i + 1)
            .cloned()
            .ok_or(format!("{} needs a value", v[i]))
    };
    let num = |i: usize| val(i).and_then(|s| s.parse::<f64>().map_err(|_| format!("bad {}", v[i])));
    while i < v.len() {
        match v[i].as_str() {
            "-m" | "--model" => model = val(i)?,
            "--data" => job.data = val(i)?.into(),
            "--out" => job.out = val(i)?.into(),
            "--grammar" => {
                let path = val(i)?;
                job.grammar =
                    Some(std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?);
            }
            "--steps" => job.steps = num(i)? as usize,
            "--batch" => job.batch = num(i)? as usize,
            "--rank" => job.rank = num(i)? as usize,
            "--lr" => job.lr = num(i)? as f32,
            "--max-seconds" => job.max_secs = Some(num(i)? as u64),
            "--memory-mb" => job.memory = (num(i)? * 1048576.0) as usize,
            "-t" | "--threads" => {
                let _ = rayon::ThreadPoolBuilder::new()
                    .num_threads(num(i)? as usize)
                    .build_global();
            }
            other => return Err(format!("unknown option {other}")),
        }
        i += 2;
    }
    if model.is_empty() || job.data.as_os_str().is_empty() || job.out.as_os_str().is_empty() {
        return Err("train needs -m, --data and --out".into());
    }
    let g = Gguf::open(std::path::Path::new(&model))?;
    let tok = Tokenizer::from_gguf(&g)?;
    let m = Model::load(g, 4096)?;
    let mut engine = agentos_infer::engine::Engine::new(m, tok);
    let out = run(&mut engine, &job, &mut |line| {
        eprintln!("agentos-llm train: {line}")
    })?;
    println!("{out}");
    Ok(())
}

/// Named adapters in a folder: `<name>.lora`, or `<name>/adapter.lora` (how agentd keeps experts).
fn adapter_files(dir: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let mut v = vec![];
    for ent in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = ent.path();
        let name = p
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if p.is_dir() && p.join("adapter.lora").exists() {
            v.push((
                p.file_name().unwrap().to_string_lossy().to_string(),
                p.join("adapter.lora"),
            ));
        } else if p.extension().is_some_and(|x| x == "lora") {
            v.push((name, p));
        }
    }
    v.sort();
    v
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).is_some_and(|a| a == "train") {
        if let Err(e) = train_main(&argv[2..]) {
            eprintln!("agentos-llm train: {e}");
            std::process::exit(1);
        }
        return;
    }
    let a = args().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    if a.threads > 0 {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(a.threads)
            .build_global();
    }
    let g = Gguf::open(std::path::Path::new(&a.model)).unwrap_or_else(|e| {
        eprintln!("agentos-llm: {e}");
        std::process::exit(1)
    });
    if a.mlock {
        if let Err(e) = g.lock() {
            eprintln!("agentos-llm: --mlock failed ({e}); continuing without");
        }
    }
    let tok = Tokenizer::from_gguf(&g).unwrap_or_else(|e| {
        eprintln!("agentos-llm: {e}");
        std::process::exit(1)
    });
    let mut model = Model::load(g, a.ctx).unwrap_or_else(|e| {
        eprintln!("agentos-llm: {e}");
        std::process::exit(1)
    });
    if let Some(path) = &a.lora {
        let ad = agentos_infer::train::Adapter::load(std::path::Path::new(path))
            .and_then(|ad| ad.fits(&model).map(|_| ad))
            .unwrap_or_else(|e| {
                eprintln!("agentos-llm: adapter {path}: {e}");
                std::process::exit(1)
            });
        eprintln!("agentos-llm: adapter {path} ({} parameters)", ad.params());
        model.adapter = Some(ad);
    }
    eprintln!(
        "agentos-llm: {:?}, {} layers, ctx {}, {} threads, state {} MiB; listening on {}:{}",
        model.cfg.arch,
        model.cfg.n_layer,
        a.ctx,
        rayon::current_num_threads(),
        model.state_bytes() >> 20,
        a.host,
        a.port
    );
    let mut engine = Engine::new(model, tok);
    engine.cache.set_budget(a.cache_mb << 20);
    if let Some(dir) = &a.adapters {
        for (name, path) in adapter_files(std::path::Path::new(dir)) {
            match agentos_infer::train::Adapter::load(&path)
                .and_then(|ad| ad.fits(&engine.model).map(|_| ad))
            {
                Ok(ad) => {
                    eprintln!("agentos-llm: expert {name} ({} parameters)", ad.params());
                    engine.add_adapter(&name, ad);
                }
                Err(err) => eprintln!("agentos-llm: expert {name} skipped: {err}"),
            }
        }
    }
    if let Some(path) = &a.draft {
        let mut load = || -> Result<(), String> {
            let g = Gguf::open(std::path::Path::new(path))?;
            let dtok = Tokenizer::from_gguf(&g)?;
            let dm = Model::load(g, a.ctx)?;
            engine.set_draft(dm, &dtok)
        };
        if let Err(e) = load() {
            eprintln!("agentos-llm: draft {path}: {e}");
            std::process::exit(1)
        }
        engine.draft_n = a.draft_n.max(1);
        eprintln!(
            "agentos-llm: draft {path}, {} tokens a round",
            engine.draft_n
        );
    }
    let listener = TcpListener::bind((a.host.as_str(), a.port)).unwrap_or_else(|e| {
        eprintln!("agentos-llm: cannot listen on {}:{}: {e}", a.host, a.port);
        std::process::exit(1)
    });
    for conn in listener.incoming().flatten() {
        serve_one(&mut engine, conn);
    }
}
