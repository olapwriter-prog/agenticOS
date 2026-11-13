//! The core: everything that is not `Send` (connectors, executor, kernel) lives here and is driven
//! from one thread. Listeners elsewhere send it commands over a channel.
use crate::anchors;
use crate::config::Config;
use crate::model::{intents, Local};
use crate::predict::Predictor;
use agentos_business::Business;
use crate::secrets::Secrets;
use agentos_action::{Registry, Tier};
use agentos_brain::{Engine, Model, Outcome};
use agentos_commercial::CommercialClient;
use agentos_connectors::{Calendar, Messenger, Reminders};
use agentos_executor::Connector;
use agentos_executor::Executor;
use agentos_firewall::Firewall;
use agentos_freeze::{Freezer, FREEZABLE};
use agentos_functions::{Compiler, FunctionStore, FunctionTools};
use agentos_gate::Gate;
use agentos_gateway::Gateway;
use agentos_hal::{Clock, FakePower, PowerSource};
use agentos_host::LlamaHost;
use agentos_kerneld::{Kernel, Paths};
use agentos_learn::Learner;
use agentos_models::{Role, Supervisor};
use agentos_notify::{Decision, Note, Notifier, Policy as NotifyPolicy, Priority};
use agentos_phone::{Contacts, Linux, Phone, PhoneEvent, PhoneTools, Sim};
use agentos_power::ModelBudget;
use agentos_prio::{Limits, Mode, Prio, SysPaths};
use agentos_router::{Job, JobKind, Policy, Sensitivity};
use agentos_stage::{Event, Stage};
use agentos_status::{line, Facts};
use agentos_store::Store;
use agentos_update::anchor::Verdict;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct Core {
    pub cfg: Config,
    clock: Arc<dyn Clock>,
    power: Arc<dyn PowerSource>,
    sim_power: Option<FakePower>,
    kernel: Kernel,
    gateway: Gateway,
    engine: Engine,
    model: Box<dyn Model>,
    notifier: Notifier,
    phone: agentos_phone::Shared,
    last_phone_poll_ms: u64,
    prio: Prio,
    models: Supervisor,
    functions: agentos_functions::Shared,
    /// Episodes and rewards: what the human approved, declined and corrected.
    learner: Learner,
    /// What kind of request is this? Answers regular requests without the LLM (crate::qa).
    classifier: agentos_understand::Classifier,
    /// The phrase table, to tell an intent from a request the model must answer.
    intents: agentos_intents::IntentTable,
    /// For judging a model on held-out episodes (`learn eval`).
    grammar: String,
    system: String,
    /// Functions being written by the local model, each for someone waiting on the control socket.
    coder_jobs: Vec<crate::coder::Job>,
    trainer: crate::trainer::Trainer,
    coder_prompt: String,
    phone_tool_names: Vec<&'static str>,
    small_url: Option<String>,
    large_url: Option<String>,
    /// Set if the priority manager could not set up its cgroups (not fatal).
    prio_error: Option<String>,
    anchor_key: Vec<u8>,
    ledger_path: PathBuf,
    last_activity_ms: u64,
    last_anchor_ms: u64,
    /// Tests set this so quiet hours do not depend on the real time of day.
    pub fixed_minute_of_day: Option<u32>,
    /// The business this phone runs, if a profile is configured.
    business: Option<std::rc::Rc<std::cell::RefCell<Business>>>,
    /// The retail store whose books this phone keeps, if a profile is configured.
    store: Option<std::rc::Rc<std::cell::RefCell<agentos_retail::books::Books>>>,
    /// The simulator's generator of store days (kept between `shop sim` calls).
    store_gen: Option<agentos_retail::gen::Gen>,
    /// Answers to likely read-only requests, prepared ahead (crate::predict).
    predictor: Predictor,
    /// The book revision the last round of preparing was done for.
    prepared_rev: u64,
    metrics: Metrics,
    /// How the request in hand was understood: intent, shortcut, classifier, model...
    last_source: String,
}

/// Counters for `ctl metrics`: how requests were understood and how long they took.
#[derive(Default)]
pub struct Metrics {
    /// source -> (requests, total microseconds)
    pub chats: std::collections::BTreeMap<String, (u64, u64)>,
    pub retrains: u64,
    pub retrain_us: u64,
    /// The request just answered: (how it was understood, microseconds).
    pub last: (String, u64),
}

#[derive(Debug)]
pub enum BootError {
    /// The ledger does not match the anchor saved earlier. The daemon will not start on it.
    LedgerAnchor(Verdict),
    Other(String),
}
impl From<String> for BootError {
    fn from(s: String) -> Self {
        BootError::Other(s)
    }
}
impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::LedgerAnchor(v) => write!(
                f,
                "ledger does not match its anchor ({v:?}); refusing to start"
            ),
            BootError::Other(s) => write!(f, "{s}"),
        }
    }
}

fn io(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Core {
    pub fn new(cfg: Config, secrets: Secrets, clock: Arc<dyn Clock>) -> Result<Core, BootError> {
        let dir = cfg.data_dir.clone();
        std::fs::create_dir_all(&dir).map_err(io)?;
        let ledger_path = dir.join("ledger.log");

        // 1. Before anything else: does the ledger still match the last anchor?
        match anchors::verify(&dir, &ledger_path, &secrets.anchor_key) {
            Verdict::Ok => {}
            v => return Err(BootError::LedgerAnchor(v)),
        }

        // 2. Hardware: real trees on a phone, a simulated tree under the data directory otherwise.
        let (freezer, cpufreq) = if cfg.real_hardware {
            // Making a directory in the cgroup tree makes the cgroup; the freezer needs them.
            let f = Freezer::system();
            for s in FREEZABLE {
                std::fs::create_dir_all(f.root.join(s))
                    .map_err(|e| format!("cannot create cgroup {s}: {e}"))?;
            }
            (f, PathBuf::from("/sys/devices/system/cpu/cpufreq"))
        } else {
            let sim = dir.join("sim");
            for s in FREEZABLE {
                std::fs::create_dir_all(sim.join("cgroup").join(s)).map_err(io)?;
            }
            for i in 0..4 {
                std::fs::create_dir_all(sim.join("cpufreq").join(format!("policy{i}")))
                    .map_err(io)?;
            }
            (
                Freezer {
                    root: sim.join("cgroup"),
                },
                sim.join("cpufreq"),
            )
        };
        let (power, sim_power): (Arc<dyn PowerSource>, Option<FakePower>) = match &cfg.power_dir {
            Some(d) => (Arc::new(agentos_hal::SysfsPower { dir: d.clone() }), None),
            None => {
                let p = FakePower::new(100, false);
                (Arc::new(p.clone()), Some(p))
            }
        };
        let kernel = Kernel::new(
            clock.clone(),
            power.clone(),
            freezer,
            Paths {
                cpufreq,
                ledger: ledger_path.clone(),
            },
        )
        .map_err(io)?;

        // 3. Tools: connectors behind the executor, behind the gateway.
        let backend: Box<dyn agentos_phone::backend::Backend> = match cfg.phone.as_str() {
            "sim" => Box::new(Sim::new()),
            "modemmanager" => Box::new(Linux::new(cfg.leds_dir.clone(), dir.join("run"))),
            other => {
                return Err(format!("phone = {other:?}: use \"sim\" or \"modemmanager\"").into())
            }
        };
        let phone = Phone::new(backend, Contacts::load(dir.join("contacts.json"))?).shared();
        let tools_store = Store::open(&dir.join("tools.db")).map_err(io)?;
        let mut connectors: Vec<Box<dyn Connector>> = vec![
            Box::new(Calendar::default()),
            Box::new(Reminders::default()),
            Box::new(Messenger::default()),
            Box::new(PhoneTools(phone.clone())),
        ];
        // The business, if this phone runs one. Its book is sealed with a key from the vault.
        let business = match &cfg.business_profile {
            Some(path) => {
                let profile = agentos_business::profile::Profile::load(path)?;
                let b = Business::open(
                    profile,
                    dir.join("business"),
                    &secrets.business_key,
                    Box::new(agentos_business::wa::SimWhatsapp::default()),
                )?;
                Some(std::rc::Rc::new(std::cell::RefCell::new(b)))
            }
            None => None,
        };
        // What a function may ask the phone to do: every tool except the function tools.
        let phone_tools: Vec<agentos_action::Tool> =
            connectors.iter().flat_map(|c| c.tools()).collect();
        let reserved: Vec<&str> = phone_tools.iter().map(|t| t.name).collect();
        // functions are Rust, compiled to WebAssembly by rustc and run in the sandbox
        let compiler = Compiler::new(cfg.rustc.clone(), dir.join("run/compile"));
        let functions = FunctionStore::open(dir.join("functions"), &reserved, compiler)?.shared();
        connectors.push(Box::new(FunctionTools(functions.clone())));
        let store = match &cfg.store_profile {
            Some(path) => {
                let profile = agentos_retail::Profile::load(path)?;
                let b = agentos_retail::books::Books::open(profile, dir.join("store"), &secrets.business_key)?;
                Some(std::rc::Rc::new(std::cell::RefCell::new(b)))
            }
            None => None,
        };
        if let Some(s) = &store {
            connectors.push(Box::new(agentos_retail::tools::RetailTools(s.clone())));
        }
        if let Some(b) = &business {
            connectors.push(Box::new(agentos_business::tools::BusinessTools(b.clone())));
        }
        let coder_prompt = crate::coder::system_prompt(&phone_tools);
        let phone_tool_names: Vec<&'static str> = phone_tools.iter().map(|t| t.name).collect();
        let exec = Executor::new(
            connectors,
            tools_store,
            clock.clone(),
            Gate::new(&secrets.gate_secret, clock.clone(), 10 * 60_000),
        );
        let registry = Registry::new(exec.tools().to_vec());
        let grammar = registry.gbnf();
        let system = crate::model::system_prompt(exec.tools());
        let gateway = Gateway::new(exec, clock.clone());

        // 4. The chip goes to the model first; the supervisor keeps the model servers running.
        let sys_paths = if cfg.real_hardware {
            SysPaths::system()
        } else {
            let sim = SysPaths::under(&dir.join("sim"));
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            for i in 0..n {
                std::fs::create_dir_all(sim.cpu.join(format!("cpu{i}"))).map_err(io)?;
            }
            sim
        };
        let mut prio = Prio::new(
            sys_paths,
            &cfg.reserve_cpus,
            Limits {
                hot_c: cfg.hot_c,
                cool_c: cfg.cool_c,
            },
        );
        let prio_error = prio.setup().err();
        if cfg.real_hardware {
            prio.evict();
        }
        let models = Supervisor::new(
            cfg.llama_server.clone(),
            cfg.models_path(),
            dir.join("logs"),
            cfg.models.clone(),
            prio.threads(),
        );

        // 5. The thinking engine and its model.
        let commercial_on = cfg.commercial_enabled && secrets.commercial_key.is_some();
        let engine = Engine {
            registry,
            firewall: Firewall::new(&["vault key", "passphrase", "gateway token", "api key"]),
            policy: Policy {
                commercial_enabled: commercial_on,
                max_local_failures: 2,
            },
            store: Store::open(&dir.join("brain.db")).map_err(io)?,
            clock: clock.clone(),
            gate: Gate::new(&secrets.gate_secret, clock.clone(), 10 * 60_000),
        };
        let small_url = cfg.small_url.clone().or_else(|| models.url(Role::Small));
        let large_url = cfg.large_url.clone().or_else(|| models.url(Role::Large));
        let (grammar_copy, system_copy) = (grammar.clone(), system.clone());
        let host = match (&small_url, &large_url) {
            (None, None) => None,
            (s, l) => {
                let small = s.clone().or_else(|| l.clone()).unwrap();
                let large = l.clone().unwrap_or_else(|| small.clone());
                Some(LlamaHost {
                    small_url: small,
                    large_url: large,
                    grammar,
                    max_tokens: 128,
                    timeout: Duration::from_secs(cfg.model_timeout_secs),
                    system: Some(system),
                })
            }
        };
        let local = Local {
            intents: intents(business.is_some(), store.is_some()),
            host,
        };
        let model: Box<dyn Model> = match (&secrets.commercial_key, commercial_on) {
            (Some(key), true) => Box::new(CommercialClient::new(local, key, &cfg.commercial_model)),
            _ => Box::new(local),
        };

        let notifier = Notifier::new(NotifyPolicy {
            quiet_start: cfg.quiet_start_min,
            quiet_end: cfg.quiet_end_min,
            daily_budget: cfg.daily_budget,
            vip: Default::default(),
        });
        let now = clock.now_ms();
        let mut core = Core {
            cfg,
            clock,
            power,
            sim_power,
            kernel,
            gateway,
            engine,
            model,
            notifier,
            phone,
            last_phone_poll_ms: 0,
            prio,
            models,
            prio_error,
            functions,
            learner: Learner::open(dir.join("experience.jsonl"))?,
            classifier: crate::qa::classifier(&[]),
            intents: intents(business.is_some(), store.is_some()),
            grammar: grammar_copy,
            system: system_copy,
            coder_jobs: Vec::new(),
            trainer: crate::trainer::Trainer::new(&dir),
            coder_prompt,
            phone_tool_names,
            small_url: small_url.clone(),
            large_url: large_url.clone(),
            anchor_key: secrets.anchor_key,
            ledger_path,
            last_activity_ms: now,
            last_anchor_ms: now,
            fixed_minute_of_day: None,
            predictor: Predictor::open(business.as_ref().map(|_| dir.join("habits.json"))),
            business,
            store,
            store_gen: None,
            prepared_rev: 0,
            metrics: Metrics::default(),
            last_source: String::new(),
        };
        let _ = core.kernel.record("agentd started");
        core.serve_adapter();
        core.retrain();
        core.phone.borrow_mut().poll(); // learn what is already in the inbox
        core.sync_models();
        core.save_anchor()?;
        Ok(core)
    }

    fn wall_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    fn minute_of_day(&self) -> u32 {
        if let Some(m) = self.fixed_minute_of_day {
            return m;
        }
        let local = self.wall_secs() as i64 + self.cfg.utc_offset_minutes as i64 * 60;
        ((local.rem_euclid(86_400)) / 60) as u32
    }

    fn save_anchor(&mut self) -> Result<(), String> {
        anchors::save(
            &self.cfg.data_dir,
            &self.ledger_path,
            &self.anchor_key,
            self.wall_secs(),
        )
        .map_err(io)?;
        self.last_anchor_ms = self.clock.now_ms();
        Ok(())
    }

    pub fn stage(&self) -> Stage {
        self.kernel.stage
    }

    fn touch(&mut self) {
        self.last_activity_ms = self.clock.now_ms();
        let _ = self.kernel.handle(Event::ChatArrived);
    }

    /// Which local models this stage and battery allow.
    fn wanted_roles(&self) -> Vec<Role> {
        // On the charger with enough battery, power is not the limit: keep the large model
        // loaded while the human is here too, so asking for a function does not unload it.
        let plugged = self.power.on_charger() && self.power.level_percent() >= 30;
        if self.trainer.running.is_some() {
            // training needs the memory the large model would take
            return vec![Role::Small];
        }
        match self.kernel.model_budget() {
            ModelBudget::Large => vec![Role::Small, Role::Large],
            _ if plugged => vec![Role::Small, Role::Large],
            ModelBudget::Small => vec![Role::Small],
            ModelBudget::None if self.cfg.keep_small_loaded => vec![Role::Small],
            ModelBudget::None => vec![],
        }
    }

    /// Start or stop model servers for the current stage, and put new ones in the model's cgroup.
    fn sync_models(&mut self) {
        let roles = self.wanted_roles();
        let mut started = self.models.want(&roles);
        started.extend(self.models.check());
        for (name, pid) in &started {
            self.prio.adopt(*pid);
            self.warm(name);
        }
        if !started.is_empty() || roles.is_empty() {
            self.prio.reserve_memory(self.models.loaded_bytes());
        }
    }

    /// Have a freshly started model read the system prompt now, in the background, so the
    /// engine keeps a snapshot of it and the first real request does not wait for it.
    fn warm(&self, name: &str) {
        let Some(spec) = self.models.specs.iter().find(|s| s.name == name) else {
            return;
        };
        let url = format!("http://127.0.0.1:{}", spec.port);
        let (system, grammar) = (self.system.clone(), self.grammar.clone());
        let wait = Duration::from_secs(self.cfg.model_wait_secs);
        let timeout = Duration::from_secs(self.cfg.model_timeout_secs.max(300));
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + wait;
            while !agentos_models::healthy_url(&url) {
                if std::time::Instant::now() > until {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            let host = LlamaHost {
                small_url: url.clone(),
                large_url: url,
                grammar,
                max_tokens: 1,
                timeout,
                system: Some(system),
            };
            let mut host = host;
            let _ = host.complete(agentos_router::Brain::LocalSmall, "hello");
        });
    }

    /// Long jobs get the chip in the Deep stage; otherwise the model idles until asked.
    fn baseline_mode(&self) -> Mode {
        if !self.coder_jobs.is_empty() {
            // the model is writing code in the background: it gets the chip meanwhile
            Mode::Inference
        } else if self.kernel.stage == Stage::Deep {
            Mode::Training
        } else {
            Mode::Idle
        }
    }

    // ---- the three ways in -------------------------------------------------------------------

    /// One MCP JSON-RPC request, already authenticated by the HTTP layer.
    pub fn mcp(&mut self, body: &str) -> Option<String> {
        self.touch();
        // analysis is agentd's own: it needs other tools' data, which a connector cannot fetch
        if let Ok(req) = serde_json::from_str::<Value>(body) {
            let id = req["id"].clone();
            match (req["method"].as_str(), req["params"]["name"].as_str()) {
                (Some("tools/list"), _) => {
                    let mut v: Value = serde_json::from_str(&self.gateway.handle(body)?).ok()?;
                    if let Some(tools) = v["result"]["tools"].as_array_mut() {
                        tools.extend(crate::analyst_mcp::tool_specs());
                    }
                    return Some(v.to_string());
                }
                (Some("tools/call"), Some("data_formats")) => {
                    return Some(crate::analyst_mcp::reply(id, Ok(crate::analyst::formats())));
                }
                (Some("tools/call"), Some("analyze_data")) => {
                    let a = &req["params"]["arguments"];
                    let out = self.analyze(
                        &a["sources"],
                        a["code"].as_str().unwrap_or_default(),
                        a["question"].as_str().unwrap_or_default(),
                    );
                    return Some(crate::analyst_mcp::reply(id, out));
                }
                _ => {}
            }
        }
        self.gateway.handle(body)
    }

    /// Load read-only sources through the executor (Low tier only), then run the code over them
    /// in the sandbox. Only the answer comes back.
    fn analyze(&mut self, sources: &Value, code: &str, question: &str) -> Result<String, String> {
        self.analyze_counted(sources, code, question)
            .map(|(a, _)| a)
    }

    /// The same, also saying how many lines of data the code ran over.
    fn analyze_counted(
        &mut self,
        sources: &Value,
        code: &str,
        question: &str,
    ) -> Result<(String, usize), String> {
        let sources = crate::analyst::parse_sources(sources)?;
        let mut data = std::collections::BTreeMap::new();
        for (tool, args) in &sources {
            if self.engine.registry.tier_of(tool) != Some(Tier::Low) {
                return Err(format!("{tool} is not a read-only tool"));
            }
            let (ok, text) = self.call_tool(tool, args);
            if !ok {
                return Err(format!("could not load {tool}: {text}"));
            }
            data.insert(tool.clone(), text);
        }
        let compiler = self.functions.borrow().compiler.clone();
        let lines = crate::analyst::data_lines(&data);
        let mut answer = crate::analyst::run(&compiler, code, question, data)?;
        if answer.len() > crate::analyst::MAX_ANSWER {
            answer.truncate(crate::analyst::MAX_ANSWER);
            answer.push_str(" [cut: return less]");
        }
        let names: Vec<&str> = sources.iter().map(|(t, _)| t.as_str()).collect();
        let _ = self
            .kernel
            .record(&format!("analysis over {}", names.join(", ")));
        Ok((answer, lines))
    }

    /// One line of plain language from the human.
    pub fn chat(&mut self, text: &str) -> String {
        self.touch();
        self.sync_models();
        if self.models.spec(Role::Small).is_some() {
            // a model still loading: wait a little rather than fail the first question
            self.models
                .wait_ready(Role::Small, Duration::from_secs(self.cfg.model_wait_secs));
        }
        self.prio.set_mode(Mode::Inference);
        self.last_source = "other".into();
        let t0 = std::time::Instant::now();
        let answer = self.chat_inner(text);
        let us = t0.elapsed().as_micros() as u64;
        let e = self.metrics.chats.entry(self.last_source.clone()).or_default();
        e.0 += 1;
        e.1 += us;
        self.metrics.last = (self.last_source.clone(), us);
        self.prio.set_mode(self.baseline_mode());
        // while the owner reads this answer, get the likely next ones ready
        self.prepare_ahead();
        answer
    }

    fn chat_inner(&mut self, text: &str) -> String {
        let now = self.clock.now_ms();
        if let Some(good) = feedback_of(text) {
            self.last_source = "feedback".into();
            let said = self.learner.feedback(good, now);
            self.retrain();
            return match said {
                Some(p) if good => format!("noted: {p:?} was right"),
                Some(p) => format!(
                    "noted: {p:?} was wrong. Do it the right way now (say it differently) and I will learn from that"
                ),
                None => "nothing to give feedback on yet".into(),
            };
        }
        let is_intent = self.intents.resolve(text).is_some();
        // a request the human approved before goes straight to the same action (through the tiers)
        if !is_intent {
            if let Some(a) = self
                .learner
                .shortcut(text)
                .and_then(|j| self.engine.registry.parse(&j).ok())
            {
                return self.act(text, a, "shortcut", now);
            }
        }
        // a command the classifier recognises, with every detail found by rules: no LLM
        if !is_intent {
            if let Some((label, _)) = self.classifier.decide(text, crate::qa::CONFIDENCE) {
                if !label.starts_with("q.") {
                    let contacts = self.contact_names();
                    let business = self.business.clone();
                    if let Some(a) = crate::qa::command(&label, text, &contacts)
                        .or_else(|| business.and_then(|b| b.borrow().command(&label, text)))
                        .and_then(|j| self.engine.registry.parse(&j.to_string()).ok())
                    {
                        return self.act(text, a, "classifier", now);
                    }
                }
            }
        }
        let job = Job {
            kind: JobKind::Chat,
            sensitivity: Sensitivity::Normal,
            online: self.cfg.online,
            on_charger: self.power.on_charger(),
            local_failures: 0,
        };
        // show the model what this person meant by similar requests before
        let examples = if is_intent {
            vec![]
        } else {
            self.learner.examples(text, 4)
        };
        let prompt = if examples.is_empty() {
            text.to_string()
        } else {
            let mut p =
                String::from("Requests this person made before, and the action they wanted:\n");
            for (q, a) in &examples {
                p.push_str(&format!("User: {q}\n{a}\n"));
            }
            p.push_str(&format!("\nThe request now:\n{text}"));
            p
        };
        let source = if is_intent { "intent" } else { "model" };
        match self.engine.handle(job, &prompt, self.model.as_mut()) {
            Outcome::Run(a) | Outcome::NeedsApproval(a) => self.act(text, a, source, now),
            Outcome::Blocked(why) => {
                self.last_source = "blocked".into();
                format!("blocked: {why}")
            }
            Outcome::GaveUp => {
                self.last_source = "gave-up".into();
                let ep = self.learner.begin(now, text, "", source);
                self.learner.gave_up(ep);
                "I could not turn that into something I am allowed to do.".into()
            }
        }
    }

    /// Rebuild the classifier from its seeds and every request the human confirmed. Cheap
    /// (milliseconds), so it runs after each approval, decline or correction.
    fn retrain(&mut self) {
        let confirmed: Vec<(String, String)> = self
            .learner
            .episodes
            .iter()
            .filter_map(|e| {
                let t = Learner::target(e)?;
                let tool = serde_json::from_str::<Value>(t).ok()?["tool"]
                    .as_str()?
                    .to_string();
                Some((tool, e.prompt.clone()))
            })
            .collect();
        let t0 = std::time::Instant::now();
        self.classifier = crate::qa::classifier(&confirmed);
        if self.business.is_some() {
            for (label, phrases) in agentos_business::tools::SEEDS {
                for p in *phrases {
                    self.classifier.train(label, p);
                }
            }
        }
        self.metrics.retrains += 1;
        self.metrics.retrain_us += t0.elapsed().as_micros() as u64;
    }

    fn contact_names(&mut self) -> Vec<String> {
        let (_, text) = self.call_tool("list_contacts", &[]);
        agentos_understand::records::contacts(&text)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    /// A question the classifier recognises, answered by exact code over all the data, with no
    /// LLM. None if it is not such a question or a detail (who?) is missing.
    fn answer_by_code(&mut self, text: &str) -> Option<String> {
        let (label, _) = self.classifier.decide(text, crate::qa::CONFIDENCE)?;
        if !label.starts_with("q.") {
            return None;
        }
        let (_, msgs) = self.call_tool("all_messages", &[]);
        let (_, calls) = self.call_tool("call_log", &[]);
        let (_, contacts) = self.call_tool("list_contacts", &[]);
        let data = crate::qa::Data::parse(&msgs, &calls, &contacts);
        let answer = crate::qa::answer(&label, text, &data)?;
        let now = self.clock.now_ms();
        let ep = self.learner.begin(
            now,
            text,
            &json!({"tool": label, "args": {}}).to_string(),
            "classifier",
        );
        self.learner.ran(ep, now, true);
        Some(format!("answer: {answer}"))
    }

    /// Carry out a chosen action and record how it went, for learning.
    fn act(&mut self, prompt: &str, a: agentos_action::Action, source: &str, now: u64) -> String {
        self.last_source = source.to_string();
        // An amount of money is what the owner said now, or what the book says now: never a
        // number remembered from another day (a shortcut) or a word taken for one ("it").
        let mut a = a;
        if let Some(b) = &self.business {
            b.borrow().fresh_args(&a.tool, &mut a.args, prompt);
        }
        let key = crate::model::action_json(&a);
        let ep = self.learner.begin(now, prompt, &key, source);
        // a read-only business answer may already be prepared (and is used only if the book
        // has not changed since)
        let rev = self
            .business
            .as_ref()
            .filter(|_| agentos_business::tools::reads_only(&a.tool))
            .map(|b| (b.borrow().rev.get(), agentos_business::util::hour_of(b.borrow().now())));
        let kept = rev.and_then(|(r, _)| self.predictor.get(&key, r));
        let (ok, text) = match kept {
            Some(answer) => {
                self.last_source = format!("{source}+ready");
                self.predictor.used(&key);
                (true, answer)
            }
            None => {
                let (ok, text) = self.call_tool(&a.tool, &a.args);
                if let (true, Some((r, _))) = (ok, rev) {
                    self.predictor.put(&key, r, text.clone(), false);
                }
                (ok, text)
            }
        };
        if let Some((_, hour)) = rev {
            self.predictor.observe(&key, hour);
        }
        match approval_id(&text) {
            Some(q) if !ok => self.learner.waiting(ep, q),
            _ => {
                self.learner.ran(ep, now, ok);
                // a working action right after "that was wrong" is a correction: learn it
                self.retrain();
            }
        }
        if !ok {
            return format!("not done: {text}");
        }
        if a.tool == "run_function" {
            if let Some(out) = self.carry_out_requests(&text) {
                return out;
            }
        }
        format!("done: {text}")
    }

    /// Run the read-only requests most likely to come next and keep their answers. Called after
    /// each answer, while the owner is reading it; not while orders stream in with nobody
    /// looking, where the answers would go stale unused. Only Low-tier business tools.
    fn prepare_ahead(&mut self) {
        let Some(b) = self.business.clone() else { return };
        let (rev, hour) = (b.borrow().rev.get(), agentos_business::util::hour_of(b.borrow().now()));
        for key in self.predictor.likely(hour) {
            if self.predictor.fresh(&key, rev) {
                continue;
            }
            let Ok(a) = self.engine.registry.parse(&key) else { continue };
            if !agentos_business::tools::reads_only(&a.tool) {
                continue;
            }
            let (ok, text) = self.call_tool(&a.tool, &a.args);
            if ok {
                self.predictor.put(&key, rev, text, true);
            }
        }
        self.prepared_rev = rev;
    }

    /// One tool call through the gateway, so tiers and the approval key apply. (ok, text)
    fn call_tool(&mut self, tool: &str, args: &[(String, String)]) -> (bool, String) {
        let args: serde_json::Map<String, Value> =
            args.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        let resp = self.gateway.handle(&req.to_string()).unwrap_or_default();
        let v: Value = serde_json::from_str(&resp).unwrap_or(Value::Null);
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("no answer")
            .to_string();
        (v["result"]["isError"] != json!(true), text)
    }

    /// A function asked the phone to do things: do each through the executor, so a text it asks
    /// for still waits for the key. Functions cannot write, install or run functions.
    fn carry_out_requests(&mut self, output: &str) -> Option<String> {
        let v: Value = serde_json::from_str(output).ok()?;
        let calls = v["calls"].as_array()?;
        let mut lines = vec![format!(
            "done: {}",
            v["result"].as_str().unwrap_or_default()
        )];
        for c in calls {
            let tool = c["tool"].as_str().unwrap_or_default().to_string();
            if agentos_functions::is_function_tool(&tool) {
                lines.push(format!(
                    "  {tool}: refused (a function cannot manage functions)"
                ));
                continue;
            }
            let args: Vec<(String, String)> = c["args"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let (ok, text) = self.call_tool(&tool, &args);
            lines.push(format!(
                "  {tool}: {}{text}",
                if ok { "" } else { "not done: " }
            ));
        }
        Some(lines.join("\n"))
    }

    /// The control socket's entry point. Most commands answer at once; asking the local model
    /// to write a function answers later, from `poll_jobs`, so the daemon is never blocked.
    pub fn ctl_reply(&mut self, line: &str, reply: std::sync::mpsc::Sender<String>) {
        let t = line.trim();
        if let Some(request) = t.strip_prefix("chat ").and_then(crate::coder::request_of) {
            let (system, schema) = (self.coder_prompt.clone(), crate::coder::schema());
            if let Err(e) = self.start_job(system, &request, schema, None, reply.clone()) {
                let _ = reply.send(e);
            }
            return;
        }
        if let Some(text) = t.strip_prefix("chat ") {
            self.touch();
            // What the human taught comes first: a fixed phrase, or a request they approved or
            // corrected before, is never re-guessed by the classifier.
            if self.intents.resolve(text).is_some() || self.learner.shortcut(text).is_some() {
                let _ = reply.send(self.ctl(line));
                return;
            }
            let t0 = std::time::Instant::now();
            if let Some(answer) = self.answer_by_code(text) {
                let us = t0.elapsed().as_micros() as u64;
                let e = self.metrics.chats.entry("code-answer".into()).or_default();
                e.0 += 1;
                e.1 += us;
                self.metrics.last = ("code-answer".into(), us);
                let _ = reply.send(answer);
                return;
            }
        }
        // A request the phone already knows how to do (a fixed phrase, a learned shortcut, or a
        // command the classifier is sure of) is never handed to the model to write analysis
        // code for, however much it sounds like a question ("did swiggy pay properly").
        if let Some(text) = t.strip_prefix("chat ") {
            if self.knows(text) {
                let _ = reply.send(self.ctl(line));
                return;
            }
        }
        if let Some(q) = t
            .strip_prefix("chat ")
            .and_then(crate::analyst::question_of)
        {
            let (system, schema) = (crate::analyst::system_prompt(), crate::analyst::schema());
            if let Err(e) = self.start_job(system, &q, schema, Some(q.clone()), reply.clone()) {
                let _ = reply.send(e);
            }
            return;
        }
        let _ = reply.send(self.ctl(line));
    }

    /// Does the phone already know what to do with this request, without writing code for it?
    fn knows(&mut self, text: &str) -> bool {
        self.intents.resolve(text).is_some()
            || self.learner.shortcut(text).is_some()
            || self
                .classifier
                .decide(text, crate::qa::CONFIDENCE)
                .is_some_and(|(label, _)| !label.starts_with("q."))
    }

    /// A background job ended: close its episode and count it.
    fn job_done(&mut self, job: &crate::coder::Job, ok: bool) {
        if ok {
            self.learner.ran(job.episode, self.clock.now_ms(), true);
        } else {
            self.learner.gave_up(job.episode);
        }
        let source = if job.question.is_some() { "analyst" } else { "coder" };
        let us = job.started.elapsed().as_micros() as u64;
        let e = self.metrics.chats.entry(source.to_string()).or_default();
        e.0 += 1;
        e.1 += us;
        self.metrics.last = (source.to_string(), us);
    }

    /// Ask the local model for code in the background: a function to keep (question None) or
    /// analysis code to run once over the phone's data (question Some).
    fn start_job(
        &mut self,
        system: String,
        request: &str,
        schema: Value,
        question: Option<String>,
        reply: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        self.touch();
        self.sync_models();
        // the large model writes better code; use it if it is running
        let large_up = self.models.spec(Role::Large).is_some_and(|s| {
            matches!(
                self.models.state(&s.name),
                agentos_models::State::Running { .. }
            )
        });
        let url = if large_up { self.large_url.clone() } else { None }
            .or_else(|| self.small_url.clone())
            .ok_or("no local model is set up. A Claude client can do this over MCP (write_function, analyze_data).")?;
        let role = if large_up { Role::Large } else { Role::Small };
        if self.models.spec(role).is_some() {
            self.models
                .wait_ready(role, Duration::from_secs(self.cfg.model_wait_secs));
        }
        let messages = vec![
            ("system".to_string(), system),
            ("user".to_string(), request.to_string()),
        ];
        let timeout = Duration::from_secs(self.cfg.coder_timeout_secs);
        let rx = crate::coder::generate(&url, &messages, &schema, timeout);
        // an episode of its own: feedback after this answer is about this request
        let source = if question.is_some() { "analyst" } else { "coder" };
        let episode = self.learner.begin(self.clock.now_ms(), request, "", source);
        self.coder_jobs.push(crate::coder::Job {
            episode,
            started: std::time::Instant::now(),
            reply,
            url,
            messages,
            attempt: 1,
            rx,
            timeout,
            schema,
            question,
            rechecked: false,
        });
        self.prio.set_mode(self.baseline_mode());
        Ok(())
    }

    /// Test what the model wrote; on failure show it the report and ask again. Call often.
    pub fn poll_jobs(&mut self) {
        let mut i = 0;
        while i < self.coder_jobs.len() {
            let answer = match self.coder_jobs[i].rx.try_recv() {
                Ok(a) => a,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("the model thread died".into())
                }
            };
            let mut job = self.coder_jobs.remove(i);
            let report = match answer {
                Err(e) => Err(format!("the model did not answer: {e}")),
                Ok(text) => {
                    job.messages.push(("assistant".into(), text.clone()));
                    match &job.question {
                        Some(q) => serde_json::from_str::<Value>(text.trim())
                            .map_err(|e| format!("your answer was not JSON: {e}"))
                            .and_then(|v| {
                                let code = v["code"].as_str().unwrap_or_default().to_string();
                                let q = q.clone();
                                self.analyze_counted(&v["sources"], &code, &q)
                            })
                            .map(|(answer, lines)| {
                                // "none" over data that has lines: one more look (not an attempt)
                                if !job.rechecked && lines > 0 && crate::analyst::says_zero(&answer)
                                {
                                    job.rechecked = true;
                                    job.messages.push((
                                        "user".into(),
                                        crate::analyst::zero_feedback(&answer, lines),
                                    ));
                                    job.rx = crate::coder::generate(
                                        &job.url,
                                        &job.messages,
                                        &job.schema,
                                        job.timeout,
                                    );
                                    RECHECKING.to_string()
                                } else {
                                    answer
                                }
                            }),
                        None => match crate::coder::write_args(&text) {
                            Err(e) => Err(e),
                            Ok(args) => match self.call_tool("write_function", &args) {
                                (true, ok) => Ok(ok),
                                (false, why) => Err(why),
                            },
                        },
                    }
                }
            };
            match report {
                // sent back for a second look: wait for the new answer
                Ok(answer) if answer == RECHECKING => {
                    self.coder_jobs.insert(i, job);
                    i += 1;
                }
                Ok(answer) if job.question.is_some() => {
                    self.job_done(&job, true);
                    let _ = job
                        .reply
                        .send(format!("answer (attempt {}): {answer}", job.attempt));
                }
                Ok(ok) => {
                    let name = ok.split_whitespace().next().unwrap_or("").to_string();
                    self.job_done(&job, true);
                    let _ = self.kernel.record(&format!("function written: {name}"));
                    let _ = job.reply.send(format!(
                        "done (attempt {}): {ok}\nRead it first: agentd ctl show {name}",
                        job.attempt
                    ));
                }
                Err(why) if job.attempt >= self.cfg.coder_attempts => {
                    let code = job
                        .messages
                        .last()
                        .filter(|(role, _)| role == "assistant")
                        .and_then(|(_, t)| serde_json::from_str::<Value>(t).ok())
                        .and_then(|v| v["code"].as_str().map(String::from))
                        .unwrap_or_default();
                    let what = if job.question.is_some() {
                        "analysis"
                    } else {
                        "function"
                    };
                    self.job_done(&job, false);
                    let _ = job.reply.send(format!(
                        "not done: no working {what} after {} attempts. Last problem:\n{why}\n--- last code ---\n{code}",
                        job.attempt
                    ));
                }
                Err(why) => {
                    let last = job
                        .messages
                        .last()
                        .filter(|(role, _)| role == "assistant")
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default();
                    job.messages.push((
                        "user".into(),
                        crate::coder::feedback(&why, &last, &self.phone_tool_names),
                    ));
                    job.attempt += 1;
                    job.rx =
                        crate::coder::generate(&job.url, &job.messages, &job.schema, job.timeout);
                    self.coder_jobs.insert(i, job);
                    i += 1;
                }
            }
        }
        if self.coder_jobs.is_empty() && self.prio.mode() == Mode::Inference {
            self.prio.set_mode(self.baseline_mode());
        }
    }

    /// Write the training data next to the experience log, and say what there is.
    fn export_learning(&mut self) -> String {
        let (sft, dpo) = self.learner.datasets(&self.system);
        let eval = self.learner.eval_set(5);
        let dir = self.cfg.data_dir.join("learn");
        let write = |name: &str, rows: &[Value]| -> Result<(), String> {
            std::fs::create_dir_all(&dir).map_err(io)?;
            let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
            std::fs::write(dir.join(name), text).map_err(io)
        };
        let eval_rows: Vec<Value> = eval
            .iter()
            .map(|(p, a)| json!({"prompt": p, "expect": a}))
            .collect();
        if let Err(e) = write("sft.jsonl", &sft)
            .and_then(|_| write("dpo.jsonl", &dpo))
            .and_then(|_| write("eval.jsonl", &eval_rows))
            // the grammar the model answers under, so training judges it the same way
            .and_then(|_| std::fs::write(dir.join("grammar.gbnf"), &self.grammar).map_err(io))
        {
            return format!("export failed: {e}");
        }
        format!(
            "{}\nexported {} SFT chats, {} DPO pairs, {} eval cases to {}",
            self.learner.stats(),
            sft.len(),
            dpo.len(),
            eval.len(),
            dir.display()
        )
    }

    /// Score the local small model on held-out episodes (what a retrained model must beat).
    fn evaluate_model(&mut self) -> String {
        let cases = self.learner.eval_set(1);
        let Some(url) = self.small_url.clone() else {
            return "no local model".into();
        };
        if cases.is_empty() {
            return "no labelled episodes yet: approve, decline or correct some answers first"
                .into();
        }
        let mut host = LlamaHost {
            small_url: url.clone(),
            large_url: url,
            grammar: self.grammar.clone(),
            max_tokens: 128,
            timeout: Duration::from_secs(self.cfg.model_timeout_secs),
            system: Some(self.system.clone()),
        };
        let score = agentos_learn::evaluate(&cases, &mut |p| {
            host.complete(agentos_router::Brain::LocalSmall, p)
        });
        format!(
            "small model scores {:.0}% on {} held-out requests",
            score * 100.0,
            cases.len()
        )
    }

    /// Everything a screen shows, in one JSON object, for clients such as the Rune UI.
    fn ui_state(&mut self) -> Value {
        let pending: Vec<Value> = self
            .gateway
            .pending()
            .iter()
            .map(|p| {
                let args: serde_json::Map<String, Value> = p
                    .action
                    .args
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect();
                json!({"id": p.id, "tool": p.action.tool, "args": args})
            })
            .collect();
        let recent: Vec<Value> = self
            .learner
            .episodes
            .iter()
            .rev()
            .take(8)
            .map(|e| {
                let tool = serde_json::from_str::<Value>(&e.action)
                    .ok()
                    .and_then(|a| a["tool"].as_str().map(String::from))
                    .unwrap_or_default();
                json!({"prompt": e.prompt, "tool": tool, "source": e.source, "reward": e.reward})
            })
            .collect();
        let lines = |s: String| s.lines().map(String::from).collect::<Vec<_>>();
        json!({
            "stage": format!("{:?}", self.kernel.stage),
            "status": self.status_line(),
            "phone": self.phone.borrow_mut().status_text().unwrap_or_else(|e| format!("phone: {e}")),
            "pending": pending,
            "recent": recent,
            "models": lines(self.models.report()),
            "prio": self.prio.report().lines().next().unwrap_or_default(),
            "functions": lines(self.functions.borrow().list()),
            "learning": self.learner.stats(),
        })
    }

    /// The control channel: the approval key and operator commands.
    pub fn ctl(&mut self, line_in: &str) -> String {
        let line_in = line_in.trim();
        let (cmd, rest) = line_in.split_once(' ').unwrap_or((line_in, ""));
        match cmd {
            "status" => self.status_line(),
            "stage" => format!("{:?}", self.kernel.stage),
            "chat" => self.chat(rest),
            "state" => self.ui_state().to_string(),
            "pending" => {
                let p = self.gateway.pending();
                if p.is_empty() {
                    "nothing waiting".into()
                } else {
                    p.iter()
                        .map(|p| format!("#{} {} {}", p.id, p.action.tool, p.action.args.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "press" => match rest.trim().trim_start_matches('#').parse::<u64>() {
                Ok(id) if self.gateway.press_key(id) => {
                    self.learner.approved(id, self.clock.now_ms());
                    self.retrain();
                    format!("approved #{id}; ask again within 10 minutes")
                }
                _ => "no such pending item".into(),
            },
            "decline" => match rest.trim().trim_start_matches('#').parse::<u64>() {
                Ok(id) if self.gateway.decline(id) => {
                    self.learner.declined(id);
                    self.retrain();
                    format!("declined #{id}")
                }
                _ => "no such pending item".into(),
            },
            "power" => self.set_power(rest),
            "anchor" => match self.save_anchor() {
                Ok(()) => "anchor saved".into(),
                Err(e) => format!("anchor failed: {e}"),
            },
            "verify" => {
                let n = agentos_ledger::Ledger::open(&self.ledger_path).and_then(|l| l.verify());
                let v = anchors::verify(&self.cfg.data_dir, &self.ledger_path, &self.anchor_key);
                match n {
                    Ok(n) => format!("ledger ok, {n} entries, anchor {v:?}"),
                    Err(e) => format!("ledger BROKEN: {e}"),
                }
            }
            "notify" => self.notify(rest),
            "digest" => {
                let g = self.notifier.digest();
                if g.is_empty() {
                    "no held notices".into()
                } else {
                    agentos_notify::digest_text(&g)
                }
            }
            "biz" => self.biz(rest),
            "shop" => self.shop(rest),
            // what kind of request the classifier thinks this is (the top three, with probability)
            "classify" => self.classifier.predict(rest).iter().take(3).map(|(l, p)| format!("{l} {p:.2}")).collect::<Vec<_>>().join(", "),
            "metrics" => self.metrics_json(rest.trim() == "reset"),
            "phone" => self
                .phone
                .borrow_mut()
                .status_text()
                .unwrap_or_else(|e| format!("phone error: {e}")),
            "sim-call" => self.simulate(|p| p.backend.inject_call(rest.trim())),
            "sim-sms" => match rest.split_once('|') {
                Some((from, text)) => {
                    let (from, text) = (from.trim().to_string(), text.trim().to_string());
                    self.simulate(move |p| p.backend.inject_sms(&from, &text))
                }
                None => "usage: sim-sms <from>|<text>".into(),
            },
            "prio" => match &self.prio_error {
                Some(e) => format!("{}\nsetup failed: {e}", self.prio.report()),
                None => self.prio.report(),
            },
            "models" => {
                if rest.trim() == "reset" {
                    self.models.reset();
                    self.sync_models();
                }
                self.models.refresh_health();
                self.models.report()
            }
            "good" => self.chat_inner("that was right"),
            "bad" => self.chat_inner("that was wrong"),
            "learn" => match rest.trim() {
                "eval" => self.evaluate_model(),
                "status" => self.learn_status(),
                "train" => {
                    if self.trainer.running.is_some() {
                        return "already training".into();
                    }
                    self.trainer.forced = true;
                    self.train_tick();
                    if self.trainer.running.is_none() {
                        self.trainer.forced = false;
                    }
                    self.learn_status()
                }
                "stop" => {
                    if self.trainer.stop() {
                        let _ = self.kernel.record("training stopped by the human");
                        "stopped; the checkpoint is kept".into()
                    } else {
                        "not training".into()
                    }
                }
                "rollback" => match self.trainer.rollback() {
                    Ok(now) => {
                        self.serve_adapter();
                        let what = now.map_or("no adapter".to_string(), |p| p.display().to_string());
                        let _ = self.kernel.record(&format!("adapter rolled back; now {what}"));
                        format!("rolled back; now serving {what}")
                    }
                    Err(e) => e,
                },
                _ => self.export_learning(),
            },
            "functions" => {
                let f = self.functions.borrow();
                let compiler = match f.compiler.check() {
                    Ok(v) => format!("compiler: {v}"),
                    Err(e) => format!("compiler MISSING: {e}"),
                };
                format!("{}\n{compiler}", f.list())
            }
            "show" => {
                let mut it = rest.split_whitespace();
                let name = it.next().unwrap_or("");
                let v = it.next().and_then(|v| v.trim_start_matches('v').parse().ok());
                self.functions.borrow().show(name, v).unwrap_or_else(|e| e)
            }
            "tools" => self
                .gateway
                .executor()
                .tools()
                .iter()
                .map(|t| format!("{} ({:?}) args: {}", t.name, t.tier, t.args.join(", ")))
                .collect::<Vec<_>>()
                .join("\n"),
            "" => "commands: status state stage chat pending press decline good bad learn power anchor verify notify digest tools phone sim-call sim-sms prio models functions show".into(),
            other => format!("unknown command: {other}"),
        }
    }

    pub fn status_line(&mut self) -> String {
        let pending = self.gateway.pending().len();
        let f = Facts::from_power(
            self.kernel.stage,
            self.power.as_ref(),
            0,
            pending,
            !self.cfg.online,
        );
        line(&f)
    }

    /// `power 80 charger` or `power 40`. Only works on the simulated battery.
    fn set_power(&mut self, rest: &str) -> String {
        let Some(p) = &self.sim_power else {
            return "real battery in use; cannot set".into();
        };
        let mut it = rest.split_whitespace();
        let Some(level) = it
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|l| *l <= 100)
        else {
            return "usage: power <0-100> [charger]".into();
        };
        let charger = it.next() == Some("charger");
        p.set_level(level);
        p.set_charger(charger);
        self.react_to_power();
        format!("battery {level}%{}", if charger { " charging" } else { "" })
    }

    fn react_to_power(&mut self) {
        let ready = self.power.on_charger() && self.power.level_percent() >= 20;
        if ready && matches!(self.kernel.stage, Stage::Asleep | Stage::Active) {
            if self.kernel.stage == Stage::Asleep {
                let _ = self.kernel.handle(Event::ChargerReady);
            }
        } else if !ready && self.kernel.stage == Stage::Deep {
            let _ = self.kernel.handle(Event::PowerLost);
        }
        // follow the stage at once: models to load, and whether the model gets the chip
        self.sync_models();
        self.prio.set_mode(self.baseline_mode());
    }

    fn notify(&mut self, rest: &str) -> String {
        // app|sender|priority|text
        let p: Vec<&str> = rest.splitn(4, '|').collect();
        if p.len() != 4 {
            return "usage: notify app|sender|low/normal/urgent|text".into();
        }
        let priority = match p[2] {
            "urgent" => Priority::Urgent,
            "low" => Priority::Low,
            _ => Priority::Normal,
        };
        let note = Note {
            app: p[0].into(),
            sender: p[1].into(),
            text: p[3].into(),
            priority,
        };
        match self.notifier.offer(note, self.minute_of_day()) {
            Decision::Deliver => "deliver now".into(),
            Decision::Hold => format!("held for the digest ({} held)", self.notifier.held_count()),
        }
    }

    /// Pretend the network delivered something, then react as if it had (simulated phone only).
    fn simulate(&mut self, f: impl FnOnce(&mut Phone) -> Result<(), String>) -> String {
        if let Err(e) = f(&mut self.phone.borrow_mut()) {
            return e;
        }
        let ev = self.poll_phone();
        if ev.is_empty() {
            "nothing happened".into()
        } else {
            ev.join("\n")
        }
    }

    /// Look for incoming calls and texts; wake up, log them, and tell the human headlessly
    /// (vibration and sound through feedbackd), respecting quiet hours for texts.
    fn poll_phone(&mut self) -> Vec<String> {
        self.last_phone_poll_ms = self.clock.now_ms();
        let events = self.phone.borrow_mut().poll();
        let mut said = Vec::new();
        for ev in events {
            let (note, ledger, feedback) = match &ev {
                PhoneEvent::IncomingCall { from } => (
                    Note {
                        app: "phone".into(),
                        sender: from.clone(),
                        text: "incoming call".into(),
                        priority: Priority::Urgent,
                    },
                    "phone: incoming call".to_string(),
                    "phone-incoming-call",
                ),
                PhoneEvent::MissedCall { from } => (
                    Note {
                        app: "phone".into(),
                        sender: from.clone(),
                        text: "missed call".into(),
                        priority: Priority::Normal,
                    },
                    "phone: missed call".to_string(),
                    "phone-missed-call",
                ),
                PhoneEvent::NewSms { from, text } => (
                    Note {
                        app: "sms".into(),
                        sender: from.clone(),
                        text: text.clone(),
                        priority: Priority::Normal,
                    },
                    "phone: sms received".to_string(),
                    "message-new-sms",
                ),
            };
            self.touch();
            let _ = self.kernel.record(&ledger);
            let line = format!("{}: {} from {}", note.app, note.text, note.sender);
            match self.notifier.offer(note, self.minute_of_day()) {
                Decision::Deliver => {
                    let _ = self.phone.borrow_mut().backend.feedback(feedback);
                    said.push(format!("{line} (alerted)"));
                }
                Decision::Hold => said.push(format!("{line} (held for the digest)")),
            }
        }
        said
    }

    /// Called about once a second by the run loop.
    pub fn tick(&mut self) {
        let now = self.clock.now_ms();
        if self.kernel.stage == Stage::Active
            && now.saturating_sub(self.last_activity_ms) >= self.cfg.idle_secs * 1000
        {
            let _ = self.kernel.handle(Event::ChatIdle);
        }
        if self.kernel.stage == Stage::WindingDown {
            let _ = self.kernel.handle(Event::WindDownDone);
        }
        self.react_to_power();
        if now.saturating_sub(self.last_phone_poll_ms) >= self.cfg.phone_poll_secs * 1000 {
            self.poll_phone();
        }
        if now.saturating_sub(self.last_anchor_ms) >= self.cfg.anchor_secs * 1000 {
            let _ = self.save_anchor();
        }
        self.train_tick();
        self.sync_models();
        self.prio.set_mode(self.baseline_mode());
        self.prio.tick();
        if let Some(b) = self.business.clone() {
            // a real phone follows the wall clock; the simulated one follows its events
            if self.cfg.phone != "sim" {
                let at = agentos_business::util::local_time(self.wall_secs() * 1000, self.cfg.utc_offset_minutes);
                b.borrow_mut().tick(&at);
            }
            let _ = b;
        }
    }

    /// `biz ...` on the control socket: the channel adapters' side of the business (orders and
    /// events coming in), and the operator's view. None of this is reachable over MCP.
    fn biz(&mut self, rest: &str) -> String {
        let Some(b) = self.business.clone() else {
            return "no business on this phone: set business_profile in the config".into();
        };
        let (cmd, arg) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let arg = arg.trim();
        let mut b = b.borrow_mut();
        let out = match cmd {
            "event" => agentos_business::dataset::play(&mut b, arg),
            "import" => std::fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}")).and_then(|t| {
                agentos_business::dataset::import(&mut b, &t).map(|(ok, bad)| {
                    format!("imported {ok} past events, {} refused{}", bad.len(), bad.iter().take(3).map(|e| format!("\n  {e}")).collect::<String>())
                })
            }),
            "play" => std::fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}")).map(|t| {
                let (mut ok, mut bad) = (0, Vec::new());
                for line in t.lines().filter(|l| !l.trim().is_empty()) {
                    match agentos_business::dataset::play(&mut b, line) {
                        Ok(_) => ok += 1,
                        Err(e) => bad.push(e),
                    }
                }
                format!("{ok} events handled, {} refused{}", bad.len(), bad.iter().take(5).map(|e| format!("\n  {e}")).collect::<String>())
            }),
            // a customer typed an order: phone|name|address|locality|cod or upi|what they wrote
            "wa" => {
                let f: Vec<&str> = arg.splitn(6, '|').map(str::trim).collect();
                if f.len() != 6 {
                    Err("usage: biz wa <phone>|<name>|<address>|<locality>|<cod or upi>|<what they wrote>".to_string())
                } else {
                    match b.profile.locality(f[3]).map(|l| l.km) {
                        None => Err(format!("we do not know {}: ask the customer for a nearby area", f[3])),
                        Some(km) => {
                            let c = agentos_business::CustomerIn { phone: f[0].into(), name: f[1].into(), address: f[2].into() };
                            b.ingest_text(c, f[3], km, f[4], f[5]).map(|said| {
                                let id = said.split_whitespace().next().unwrap_or_default().to_string();
                                match b.profile.auto_bill.then(|| b.send_bill(&id)) {
                                    Some(Ok(t)) => format!("{said}; {t}"),
                                    Some(Err(e)) => format!("{said}; bill not sent: {e}"),
                                    None => said,
                                }
                            })
                        }
                    }
                }
            }
            // simulator: write one invented day for this shop: day seed scale direct-share file
            "gen" => {
                let f: Vec<&str> = arg.split_whitespace().collect();
                if f.len() != 5 {
                    Err("usage: biz gen <day> <seed> <scale> <direct share, 0 for usual> <file>".to_string())
                } else {
                    let ev = agentos_business::dataset::day_events(&b.profile, f[0], f[1].parse().unwrap_or(1), f[2].parse().unwrap_or(1.0), f[3].parse().unwrap_or(0.0));
                    std::fs::write(f[4], ev.join("\n") + "\n").map(|_| format!("{} events for {} in {}", ev.len(), f[0], f[4])).map_err(|e| e.to_string())
                }
            }
            "state" => Ok(b.state().to_string()),
            "clock" => {
                b.tick(arg);
                Ok(format!("it is {}", b.now()))
            }
            // the platform pays for a week (simulator): platform from to [rupees it wrongly keeps]
            "payout" => {
                let f: Vec<&str> = arg.split_whitespace().collect();
                if f.len() < 3 {
                    Err("usage: biz payout <platform> <from> <to> [cut]".to_string())
                } else {
                    let cut = f.get(3).and_then(|c| c.parse().ok()).unwrap_or(0.0);
                    let at = b.now().to_string();
                    let ev = agentos_business::dataset::payout_event(&b, &f[0].to_lowercase(), f[1], f[2], &at, cut);
                    agentos_business::dataset::play(&mut b, &ev)
                }
            }
            "outbox" => {
                let n: usize = arg.parse().unwrap_or(10);
                Ok(b.book.outbox.iter().rev().take(n).rev().map(|s| format!("{} {} → {}: {} ({}, {} bytes)", s.at, s.channel, s.to, s.kind, s.about, s.bytes)).collect::<Vec<_>>().join("\n"))
            }
            "sent" => {
                let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
                for (_, m) in b.wa.sent() {
                    *kinds.entry(m.kind()).or_default() += 1;
                }
                Ok(serde_json::to_string(&kinds).unwrap_or_default())
            }
            // the operator saves a bill to look at it: biz bill <order> <file>
            "bill" => match arg.split_once(' ') {
                Some((id, path)) => b.bill_pdf(id).and_then(|pdf| std::fs::write(path.trim(), pdf).map_err(|e| e.to_string())).map(|_| format!("wrote {}", path.trim())),
                None => Err("usage: biz bill <order> <file>".to_string()),
            },
            other => Err(format!("biz: unknown command {other:?} (event, import, play, wa, state, clock, payout, outbox, sent, bill)")),
        };
        out.unwrap_or_else(|e| format!("refused: {e}"))
    }

    /// `shop ...` on the control socket: the tills' side of the store (days of bills coming
    /// in), the simulator, and the operator's view. Not reachable over MCP.
    fn shop(&mut self, rest: &str) -> String {
        use agentos_retail::gen::{family, inward, Gen};
        let Some(s) = self.store.clone() else {
            return "no store on this phone: set store_profile in the config".into();
        };
        let (cmd, arg) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let f: Vec<&str> = arg.split_whitespace().collect();
        let mut b = s.borrow_mut();
        let out: Result<String, String> = match cmd {
            "state" => Ok(b.state().to_string()),
            "clock" => {
                b.tick(arg.trim());
                Ok(format!("it is {}", b.now()))
            }
            // a day's bills from the tills: a file whose first line is {"day","cash_banked"}
            // (rupees) and whose other lines are bills
            "import" => std::fs::read_to_string(arg.trim()).map_err(|e| format!("{arg}: {e}")).and_then(|text| {
                let mut lines = text.lines().filter(|l| !l.trim().is_empty());
                let head: Value = serde_json::from_str(lines.next().unwrap_or("{}")).map_err(|e| format!("first line: {e}"))?;
                let day = head["day"].as_str().ok_or("the first line needs \"day\"")?.to_string();
                let mut bills = Vec::new();
                for (i, l) in lines.enumerate() {
                    bills.push(serde_json::from_str::<agentos_retail::Bill>(l).map_err(|e| format!("bill on line {}: {e}", i + 2))?);
                }
                b.import_day(&day, &bills, agentos_business::util::rs(head["cash_banked"].as_f64().unwrap_or(0.0)))
            }),
            // simulator: write one invented day as such a file: day seed file
            "gen" if f.len() == 3 => {
                let mut g = Gen::new(f[1].parse().unwrap_or(1));
                let d = g.day(&b.profile, f[0]);
                let mut text = json!({"day": f[0], "cash_banked": d.cash_banked as f64 / 100.0}).to_string();
                for bill in &d.bills {
                    text.push('\n');
                    text.push_str(&serde_json::to_string(bill).unwrap_or_default());
                }
                std::fs::write(f[2], text).map(|_| format!("{} bills for {} in {} ({} faults planted)", d.bills.len(), f[0], f[2], d.truth.len())).map_err(|e| e.to_string())
            }
            // simulator: invent and audit every day from..to, add each finished month's supplier
            // invoices, and score the audit against the faults planted: from to seed
            "sim" if f.len() == 3 => {
                let seed: u64 = f[2].parse().unwrap_or(1);
                // after a restart the series carries on from the books, not from 1
                let mut g = self.store_gen.take().unwrap_or_else(|| {
                    let mut g = Gen::new(seed ^ b.last_no());
                    g.next_no = b.last_no() + 1;
                    g
                });
                let (mut day, mut planted, mut found, mut missed, mut extra, mut bills) = (f[0].to_string(), 0u32, 0u32, Vec::new(), Vec::new(), 0u64);
                let mut by_family: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
                let t0 = std::time::Instant::now();
                let mut err = None;
                while day.as_str() <= f[1] {
                    let d = g.day(&b.profile, &day);
                    bills += d.bills.len() as u64;
                    if let Err(e) = b.import_day(&day, &d.bills, d.cash_banked) {
                        err = Some(e);
                        break;
                    }
                    let got: std::collections::BTreeSet<(u64, String)> = b.days[&day].exceptions.iter().map(|x| (x.bill, family(&x.kind).to_string())).collect();
                    let want: std::collections::BTreeSet<(u64, String)> = d.truth.iter().map(|t| (t.bill, t.kind.to_string())).collect();
                    planted += want.len() as u32;
                    found += got.len() as u32;
                    for w in &want {
                        let e = by_family.entry(w.1.clone()).or_default();
                        e.0 += 1;
                        if got.contains(w) {
                            e.1 += 1;
                        } else if missed.len() < 10 {
                            missed.push(format!("{day} {} {}", w.0, w.1));
                        }
                    }
                    for x in got.difference(&want) {
                        if extra.len() < 10 {
                            extra.push(format!("{day} {} {}", x.0, x.1));
                        }
                    }
                    let next = agentos_business::util::add_days(&day, 1);
                    if next[..7] != day[..7] {
                        let m = b.month(&day[..7]);
                        let cost: i64 = b.days.range(day[..7].to_string()..).take_while(|(k, _)| k.starts_with(&day[..7])).map(|(_, d)| d.cost).sum();
                        let paid: i64 = b.days.range(day[..7].to_string()..).take_while(|(k, _)| k.starts_with(&day[..7])).map(|(_, d)| d.upi + d.card).sum();
                        let rows = inward(&b.profile, &day[..7], cost, m.taxable(), paid, seed);
                        let _ = b.add_inward(rows);
                    }
                    day = next;
                }
                self.store_gen = Some(g);
                match err {
                    Some(e) => Err(e),
                    None => Ok(json!({"bills": bills, "planted": planted, "found": found,
                        "by_family": by_family.iter().map(|(k, (p, f))| (k.clone(), json!({"planted": p, "found": f}))).collect::<serde_json::Map<_, _>>(),
                        "missed": missed, "extra": extra, "seconds": t0.elapsed().as_secs_f64()}).to_string()),
                }
            }
            other => Err(format!("shop: unknown command {other:?} (state, clock, import <file>, gen <day> <seed> <file>, sim <from> <to> <seed>)")),
        };
        out.unwrap_or_else(|e| format!("refused: {e}"))
    }

    fn metrics_json(&mut self, reset: bool) -> String {
        let p = self.predictor.stats;
        let v = json!({
            "chats": self.metrics.chats.iter().map(|(k, (n, us))| (k.clone(), json!({"n": n, "avg_ms": *us as f64 / 1000.0 / (*n).max(1) as f64}))).collect::<serde_json::Map<_, _>>(),
            "retrains": self.metrics.retrains,
            "retrain_avg_ms": self.metrics.retrain_us as f64 / 1000.0 / self.metrics.retrains.max(1) as f64,
            "last": {"source": self.metrics.last.0, "ms": self.metrics.last.1 as f64 / 1000.0,
                "tool": self.learner.episodes.last().and_then(|e| serde_json::from_str::<Value>(&e.action).ok()).and_then(|a| a["tool"].as_str().map(String::from)).unwrap_or_default()},
            "classifier_labels": self.classifier.labels(),
            "episodes": self.learner.episodes.len(),
            "confirmed": self.learner.labelled(),
            "ready": {"ahead_hits": p.ahead_hits, "repeat_hits": p.repeat_hits, "misses": p.misses,
                "prepared": p.prepared, "stale": p.stale, "habits": self.predictor.habits()},
        });
        if reset {
            self.metrics = Metrics::default();
            self.predictor.stats = Default::default();
        }
        v.to_string()
    }

    /// Only agentos-llm reads our adapter format (llama-server's --lora wants its own).
    fn serves_adapters(&self) -> bool {
        self.cfg
            .llama_server
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("agentos-llm"))
    }

    fn small_spec(&self) -> Option<agentos_models::ModelSpec> {
        self.models.spec(Role::Small).cloned()
    }

    /// Serve the small model with the current trained adapter (or none).
    fn serve_adapter(&mut self) {
        let (Some(spec), true) = (self.small_spec(), self.serves_adapters()) else {
            return;
        };
        let active = self.trainer.active();
        if self.models.set_adapter(&spec.name, active.clone()) {
            let what = active.map_or("no adapter".to_string(), |p| p.display().to_string());
            let _ = self.kernel.record(&format!("model {} now served with {what}", spec.name));
        }
    }

    fn train_conditions(&self) -> crate::trainer::Conditions {
        crate::trainer::Conditions {
            deep: self.kernel.stage == Stage::Deep,
            charging: self.power.on_charger(),
            battery: self.power.level_percent(),
            min_battery: self.cfg.train_min_battery,
            temperature: self.prio.temperature(),
            cool_c: self.cfg.cool_c,
            busy: !self.coder_jobs.is_empty(),
            new_examples: self.learner.labelled().saturating_sub(self.trainer.trained_on),
            min_new: self.cfg.train_min_new,
            forced: self.trainer.forced,
        }
    }

    /// Start, stop or finish overnight training.
    fn train_tick(&mut self) {
        if let Some(outcome) = self.trainer.poll() {
            use crate::trainer::Outcome;
            let line = match outcome {
                Outcome::Kept { adapter, base, tuned } => {
                    self.serve_adapter();
                    format!(
                        "training kept {}: {:.0}% -> {:.0}% on held-out requests",
                        adapter.display(),
                        base * 100.0,
                        tuned * 100.0
                    )
                }
                Outcome::Discarded { base, tuned } => format!(
                    "training discarded: {:.0}% -> {:.0}% on held-out requests, not better",
                    base * 100.0,
                    tuned * 100.0
                ),
                Outcome::Paused { step } => format!("training paused at step {step}; continues next time"),
                Outcome::Failed(why) => format!("training failed: {why}"),
            };
            let _ = self.kernel.record(&line);
            self.trainer.last = Some(line);
        }
        let blocker = self.train_conditions().blocker();
        if self.trainer.running.is_some() {
            if let Some(why) = blocker.filter(|w| !w.contains("busy") || !self.coder_jobs.is_empty()) {
                if self.trainer.stop() {
                    let line = format!("training stopped ({why}); continues from its checkpoint next time");
                    let _ = self.kernel.record(&line);
                    self.trainer.last = Some(line);
                }
            }
            return;
        }
        if !self.cfg.train_enabled || blocker.is_some() || !self.serves_adapters() {
            return;
        }
        let Some(spec) = self.small_spec() else {
            return;
        };
        let export = self.export_learning();
        if export.starts_with("export failed") {
            self.trainer.last = Some(export);
            return;
        }
        let examples = self.learner.labelled();
        let model = self.models.path(&spec);
        let bin = self.cfg.llama_server.clone();
        match self.trainer.start(&bin, &model, examples, self.cfg.train_steps, self.cfg.train_chunk_secs) {
            Ok(pid) => {
                self.prio.adopt(pid);
                let line = format!("training {} on {examples} confirmed requests (pid {pid})", spec.name);
                let _ = self.kernel.record(&line);
                self.trainer.last = Some(line);
            }
            Err(e) => {
                self.trainer.forced = false;
                self.trainer.last = Some(format!("training could not start: {e}"));
            }
        }
    }

    fn learn_status(&self) -> String {
        let state = if self.trainer.running.is_some() {
            "training now".to_string()
        } else {
            match self.train_conditions().blocker() {
                Some(why) => format!("waiting: {why}"),
                None => "ready to train".into(),
            }
        };
        let adapter = self
            .trainer
            .active()
            .map_or("none".to_string(), |p| p.display().to_string());
        format!(
            "{state}
adapter in use: {adapter}
last: {}
{}",
            self.trainer.last.as_deref().unwrap_or("-"),
            self.learner.stats()
        )
    }

    pub fn tier_of(&self, tool: &str) -> Option<Tier> {
        self.engine.registry.tier_of(tool)
    }
}

/// Marks an analysis sent back for a second look (never a real answer: it holds a NUL).
const RECHECKING: &str = "\0rechecking";

/// "That was right" / "that was wrong" and the like: feedback on the last answer.
fn feedback_of(text: &str) -> Option<bool> {
    let t = agentos_learn::words(text).join(" ");
    match t.as_str() {
        "that was right" | "that s right" | "thats right" | "correct" | "good" | "right"
        | "yes that s right" | "well done" => Some(true),
        "that was wrong" | "that s wrong" | "thats wrong" | "wrong" | "no that s wrong"
        | "not that" | "that is wrong" | "bad" => Some(false),
        _ => None,
    }
}

/// The queue id in "approval required (id 3): ...".
fn approval_id(text: &str) -> Option<u64> {
    let i = text.find("approval required (id ")?;
    text[i + 22..].split(')').next()?.trim().parse().ok()
}
