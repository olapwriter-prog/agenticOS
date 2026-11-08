//! The core: everything that is not `Send` (connectors, executor, kernel) lives here and is driven
//! from one thread. Listeners elsewhere send it commands over a channel.
use crate::anchors;
use crate::config::Config;
use crate::model::{default_intents, Local};
use crate::secrets::Secrets;
use agentos_action::{Registry, Tier};
use agentos_brain::{Engine, Model, Outcome};
use agentos_commercial::CommercialClient;
use agentos_connectors::{Calendar, Messenger, Reminders};
use agentos_executor::Connector;
use agentos_executor::Executor;
use agentos_firewall::Firewall;
use agentos_freeze::{Freezer, FREEZABLE};
use agentos_gate::Gate;
use agentos_gateway::Gateway;
use agentos_hal::{Clock, FakePower, PowerSource};
use agentos_host::LlamaHost;
use agentos_kerneld::{Kernel, Paths};
use agentos_models::{Role, Supervisor};
use agentos_notify::{Decision, Note, Notifier, Policy as NotifyPolicy, Priority};
use agentos_phone::{Contacts, Linux, Phone, PhoneEvent, PhoneTools, Sim};
use agentos_power::ModelBudget;
use agentos_prio::{Limits, Mode, Prio, SysPaths};
use agentos_router::{Job, JobKind, Policy, Sensitivity};
use agentos_scripts::{FunctionStore, FunctionTools};
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
    functions: agentos_scripts::Shared,
    /// Functions being written by the local model, each for someone waiting on the control socket.
    coder_jobs: Vec<crate::coder::Job>,
    coder_prompt: String,
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
        // What a function may ask the phone to do: every tool except the function tools.
        let phone_tools: Vec<agentos_action::Tool> =
            connectors.iter().flat_map(|c| c.tools()).collect();
        let reserved: Vec<&str> = phone_tools.iter().map(|t| t.name).collect();
        let functions = FunctionStore::open(dir.join("functions"), &reserved)?.shared();
        connectors.push(Box::new(FunctionTools(functions.clone())));
        let coder_prompt = crate::coder::system_prompt(&phone_tools);
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
                    timeout: Duration::from_secs(20),
                    system: Some(system),
                })
            }
        };
        let local = Local {
            intents: default_intents(),
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
            coder_jobs: Vec::new(),
            coder_prompt,
            small_url: small_url.clone(),
            large_url: large_url.clone(),
            anchor_key: secrets.anchor_key,
            ledger_path,
            last_activity_ms: now,
            last_anchor_ms: now,
            fixed_minute_of_day: None,
        };
        let _ = core.kernel.record("agentd started");
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
        match self.kernel.model_budget() {
            ModelBudget::Large => vec![Role::Small, Role::Large],
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
        for (_, pid) in &started {
            self.prio.adopt(*pid);
        }
        if !started.is_empty() || roles.is_empty() {
            self.prio.reserve_memory(self.models.loaded_bytes());
        }
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
        self.gateway.handle(body)
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
        let answer = self.chat_inner(text);
        self.prio.set_mode(self.baseline_mode());
        answer
    }

    fn chat_inner(&mut self, text: &str) -> String {
        let job = Job {
            kind: JobKind::Chat,
            sensitivity: Sensitivity::Normal,
            online: self.cfg.online,
            on_charger: self.power.on_charger(),
            local_failures: 0,
        };
        match self.engine.handle(job, text, self.model.as_mut()) {
            Outcome::Run(a) | Outcome::NeedsApproval(a) => {
                let (ok, text) = self.call_tool(&a.tool, &a.args);
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
            Outcome::Blocked(why) => format!("blocked: {why}"),
            Outcome::GaveUp => "I could not turn that into something I am allowed to do.".into(),
        }
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
            if agentos_scripts::is_function_tool(&tool) {
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
            if let Err(e) = self.start_coder(&request, reply.clone()) {
                let _ = reply.send(e);
            }
            return;
        }
        let _ = reply.send(self.ctl(line));
    }

    fn start_coder(
        &mut self,
        request: &str,
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
            .ok_or("no local model is set up. A Claude client can write functions over MCP with write_function.")?;
        let role = if large_up { Role::Large } else { Role::Small };
        if self.models.spec(role).is_some() {
            self.models
                .wait_ready(role, Duration::from_secs(self.cfg.model_wait_secs));
        }
        let messages = vec![
            ("system".to_string(), self.coder_prompt.clone()),
            ("user".to_string(), request.to_string()),
        ];
        let timeout = Duration::from_secs(self.cfg.coder_timeout_secs);
        let rx = crate::coder::generate(&url, &messages, timeout);
        self.coder_jobs.push(crate::coder::Job {
            reply,
            url,
            messages,
            attempt: 1,
            rx,
            timeout,
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
                    match crate::coder::write_args(&text) {
                        Err(e) => Err(e),
                        Ok(args) => match self.call_tool("write_function", &args) {
                            (true, ok) => Ok(ok),
                            (false, why) => Err(why),
                        },
                    }
                }
            };
            match report {
                Ok(ok) => {
                    let name = ok.split_whitespace().next().unwrap_or("").to_string();
                    let _ = self.kernel.record(&format!("function written: {name}"));
                    let _ = job.reply.send(format!(
                        "done (attempt {}): {ok}\nRead it first: agentd ctl show {name}",
                        job.attempt
                    ));
                }
                Err(why) if job.attempt >= self.cfg.coder_attempts => {
                    let _ = job.reply.send(format!(
                        "not done: no working function after {} attempts. Last problem:\n{why}",
                        job.attempt
                    ));
                }
                Err(why) => {
                    job.messages.push((
                        "user".into(),
                        format!("That did not work:\n{why}\nFix the code (or an example that was wrong) and answer again with the whole function."),
                    ));
                    job.attempt += 1;
                    job.rx = crate::coder::generate(&job.url, &job.messages, job.timeout);
                    self.coder_jobs.insert(i, job);
                    i += 1;
                }
            }
        }
        if self.coder_jobs.is_empty() && self.prio.mode() == Mode::Inference {
            self.prio.set_mode(self.baseline_mode());
        }
    }

    /// The control channel: the approval key and operator commands.
    pub fn ctl(&mut self, line_in: &str) -> String {
        let line_in = line_in.trim();
        let (cmd, rest) = line_in.split_once(' ').unwrap_or((line_in, ""));
        match cmd {
            "status" => self.status_line(),
            "stage" => format!("{:?}", self.kernel.stage),
            "chat" => self.chat(rest),
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
                Ok(id) if self.gateway.press_key(id) => format!("approved #{id}; ask again within 10 minutes"),
                _ => "no such pending item".into(),
            },
            "decline" => match rest.trim().trim_start_matches('#').parse::<u64>() {
                Ok(id) if self.gateway.decline(id) => format!("declined #{id}"),
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
            "functions" => self.functions.borrow().list(),
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
            "" => "commands: status stage chat pending press decline power anchor verify notify digest tools phone sim-call sim-sms prio models functions show".into(),
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
        self.sync_models();
        self.prio.set_mode(self.baseline_mode());
        self.prio.tick();
    }

    pub fn tier_of(&self, tool: &str) -> Option<Tier> {
        self.engine.registry.tier_of(tool)
    }
}
