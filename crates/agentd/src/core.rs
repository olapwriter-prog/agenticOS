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
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// The MCP servers the store installed, from mcp_servers.toml in the data directory.
fn read_store_servers(dir: &std::path::Path) -> Vec<crate::config::McpServer> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        mcp_servers: Vec<crate::config::McpServer>,
    }
    std::fs::read_to_string(dir.join("mcp_servers.toml"))
        .ok()
        .and_then(|t| toml::from_str::<File>(&t).ok())
        .map(|f| f.mcp_servers)
        .unwrap_or_default()
}

/// Push, and drop the oldest entries past 20.
fn push_capped(log: &mut std::collections::VecDeque<Value>, v: Value) {
    log.push_back(v);
    while log.len() > 20 {
        log.pop_front();
    }
}

fn setup_screen_json(s: &crate::setup::Screen) -> Value {
    json!({
        "id": s.id,
        "index": s.index,
        "total": s.total,
        "title": s.title,
        "body": s.body,
        "options": s.options,
        "input": s.input,
        "can_skip": s.can_skip,
        "error": s.error,
    })
}

/// The vault name of the approval PIN. Not a conn. name, so the secret commands cannot reach it.
const PIN_NAME: &str = "approval_pin";

/// The Android phrases, added after the others so the specific ones still win. Only when the
/// sandbox is configured: otherwise "open ..." would name a tool that does not exist.
fn intents_with_android(
    mut t: agentos_intents::IntentTable,
    on: bool,
) -> agentos_intents::IntentTable {
    if on {
        t.add("open {app}", "android_open");
        t.add("android apps", "android_apps");
        t.add("close android apps", "android_close_all");
    }
    t
}

type GoogleSlot = Rc<RefCell<Option<agentos_services::oauth::Tokens>>>;

/// The tokens as the vault keeps them: JSON with access, refresh and expires_at_ms.
fn tokens_json(t: &agentos_services::oauth::Tokens) -> String {
    json!({"access": t.access, "refresh": t.refresh, "expires_at_ms": t.expires_at_ms}).to_string()
}

fn parse_tokens(bytes: &[u8]) -> Option<agentos_services::oauth::Tokens> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    Some(agentos_services::oauth::Tokens {
        access: v["access"].as_str()?.to_string(),
        refresh: v["refresh"].as_str().map(str::to_string),
        expires_at_ms: v["expires_at_ms"].as_u64()?,
    })
}

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
    /// The last screen a tool answered with (a UI document), for the display.
    screen: Option<Value>,
    /// The last answer screens, newest last (at most 8): the dashboard's "recent answers".
    screens: std::collections::VecDeque<Value>,
    /// The skill library, for the model's prompts when it writes code.
    skill_lib: Option<agentos_skillpack::tools::SkillTools>,
    /// ModemManager's signals are being watched, so polling can be slow.
    signals_up: bool,
    /// A signal came in: look again shortly, as a text can arrive in parts.
    poll_again_at: Option<u64>,
    prio: Prio,
    models: Supervisor,
    functions: agentos_functions::Shared,
    /// First-boot setup, saved in setup.json after every answer.
    setup: crate::setup::Setup,
    setup_path: PathBuf,
    /// The last calls and messages the phone reported, newest last (at most 20 each).
    calls_log: std::collections::VecDeque<Value>,
    messages_log: std::collections::VecDeque<Value>,
    /// The phone's status text, kept for 5 seconds: reading it runs the modem tools.
    phone_status_cache: Option<(u64, String)>,
    /// The same reading, as data, for the screen's status bar.
    net_cache: Option<(u64, Option<agentos_phone::backend::NetStatus>)>,
    /// Wrong approval PINs in a row, and until when presses are refused.
    pin_failures: u32,
    pin_locked_until_ms: u64,
    /// The Android sandbox, if configured, and the governor's word on opening apps.
    android: Option<agentos_android::Shared>,
    android_blocked: agentos_android::Blocked,
    android_policy: agentos_android::governor::Policy,
    android_last_check_ms: u64,
    /// The Android bridge, if configured: events in, commands out.
    bridge: Option<agentos_bridge::SharedBridge>,
    bridge_last_ms: u64,
    /// The last time the governor closed the sandbox: (reason, at ms).
    android_last_stop: Option<(String, u64)>,
    /// The app list, read at most every 30 seconds: (read at ms, apps).
    /// None until the first read: a clock that starts at 0 must not look "fresh" at boot.
    android_apps: Option<(u64, Vec<agentos_android::App>)>,
    /// New Google tokens from a refresh, waiting to be written to the vault by the next tick.
    google_slot: GoogleSlot,
    /// Remote MCP servers that were signed in by discovery (mcp signin): their providers.
    mcp_providers: std::collections::HashMap<String, agentos_services::oauth::Provider>,
    /// Sign-ins started with `oauth begin` and not yet finished, by provider.
    oauth_pending: std::collections::HashMap<String, agentos_services::oauth::Pending>,
    /// Remote MCP servers as they were at boot: (name, state, tools).
    mcp_status: Vec<(String, String, usize)>,
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
    /// Every tool the executor offers: an installed agent is limited to a subset of these.
    all_tools: Vec<agentos_action::Tool>,
    small_url: Option<String>,
    large_url: Option<String>,
    /// Set if the priority manager could not set up its cgroups (not fatal).
    prio_error: Option<String>,
    anchor_key: Vec<u8>,
    /// The vault: service tokens under names starting with conn.
    vault: agentos_vault::Vault,
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
        let mut kernel = Kernel::new(
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
            "modemmanager" => {
                let linux = Linux::new(cfg.leds_dir.clone(), dir.join("run"));
                Box::new(match &cfg.session_user {
                    Some(user) => linux.with_session(user)?,
                    None => linux,
                })
            }
            other => {
                return Err(format!("phone = {other:?}: use \"sim\" or \"modemmanager\"").into())
            }
        };
        let phone = Phone::new(backend, Contacts::load(dir.join("contacts.json"))?).shared();
        let tools_store = Store::open(&dir.join("tools.db")).map_err(io)?;
        let mut connectors: Vec<Box<dyn Connector>> = vec![
            Box::new(Calendar::load(dir.join("calendar.json"))),
            Box::new(Reminders::load(dir.join("reminders.json"))),
            Box::new(PhoneTools(phone.clone())),
        ];
        if cfg.demo_connectors {
            connectors.insert(2, Box::new(Messenger::default()));
        }
        let documents = cfg
            .documents_dir
            .clone()
            .unwrap_or_else(|| dir.join("documents"));
        std::fs::create_dir_all(&documents).map_err(|e| format!("{}: {e}", documents.display()))?;
        connectors.push(Box::new(agentos_docs::Docs {
            dir: documents,
            budget: cfg.skill_budget,
        }));
        if cfg.apps_enabled {
            connectors.push(Box::new(agentos_apps::Apps {
                cfg: cfg.apps.clone(),
            }));
        }
        let skills_dir = cfg.skills_dir.clone().unwrap_or_else(|| dir.join("skills"));
        let skill_lib = if skills_dir.join("index.json").exists() {
            connectors.push(Box::new(agentos_skillpack::tools::SkillTools::open(
                &skills_dir,
                cfg.skill_budget,
            )?));
            // a second handle for the model's own prompts (the connector is the executor's)
            Some(agentos_skillpack::tools::SkillTools::open(
                &skills_dir,
                cfg.skill_budget,
            )?)
        } else {
            None
        };
        if let Some(pc) = &cfg.partner {
            let p = agentos_partner::Partner::open(pc.clone(), dir.join("partner.json"))?;
            connectors.push(Box::new(p));
        }
        // What a function may ask the phone to do: every tool except the function tools.
        let phone_tools: Vec<agentos_action::Tool> =
            connectors.iter().flat_map(|c| c.tools()).collect();
        let reserved: Vec<&str> = phone_tools.iter().map(|t| t.name).collect();
        // functions are Rust, compiled to WebAssembly by rustc and run in the sandbox
        let compiler = Compiler::new(cfg.rustc.clone(), dir.join("run/compile"));
        let functions = FunctionStore::open(dir.join("functions"), &reserved, compiler)?.shared();
        // the Android sandbox, if configured: its tools come before the functions, as the others
        let android_blocked: agentos_android::Blocked = Rc::new(RefCell::new(None));
        let android: Option<agentos_android::Shared> = match cfg.android.as_str() {
            "none" => None,
            "sim" => Some(Rc::new(RefCell::new(
                Box::new(agentos_android::SimSandbox {
                    installed: vec!["org.example.notes".into()],
                    ..Default::default()
                }) as Box<dyn agentos_android::Sandbox>,
            ))),
            "waydroid" => Some(Rc::new(RefCell::new(Box::new(agentos_android::Waydroid {
                runner: Box::new(agentos_android::System),
                user: cfg.android_user.clone(),
                env: cfg.android_env.clone(),
                sway: cfg.android_sway,
            })
                as Box<dyn agentos_android::Sandbox>))),
            other => {
                return Err(
                    format!("android = {other:?}: use \"none\", \"sim\" or \"waydroid\"").into(),
                )
            }
        };
        // the shipped catalogue, then the one `store refresh` saved (which overrides it)
        let mut catalog_files = vec![cfg.catalog.clone()];
        catalog_files.push(dir.join("catalog.toml"));
        catalog_files.retain(|p| p.exists());
        let store_catalog = agentos_catalog::Catalog::load(&catalog_files).unwrap_or_default();
        connectors.push(Box::new(agentos_catalog::store::StoreTools {
            catalog: store_catalog,
            dir: dir.clone(),
            functions: Some(functions.clone()),
            sandbox: android.clone(),
        }));
        // the camera and the photos
        let camera: Option<Box<dyn agentos_vision::Camera>> = match cfg.camera.as_str() {
            "none" => None,
            "command" => Some(Box::new(agentos_vision::CommandCamera {
                command: cfg.camera_command.clone(),
            })),
            _ => Some(Box::new(agentos_vision::SimCamera::default())),
        };
        if let Some(camera) = camera {
            connectors.push(Box::new(agentos_vision::Photos {
                dir: dir.join("photos"),
                camera,
                vision_url: cfg.vision_url.clone(),
            }));
        }
        // the Android bridge: events and commands move through a spool folder (or the simulator)
        let bridge: Option<agentos_bridge::SharedBridge> = match cfg.bridge.as_str() {
            "none" => None,
            "sim" => {
                let t = agentos_bridge::Sim::new(dir.join("run").join("bridge-sim"))
                    .map_err(|e| format!("bridge: {e}"))?;
                Some(Rc::new(RefCell::new(agentos_bridge::Bridge::load(
                    Box::new(t),
                    &dir,
                ))))
            }
            "spool" => {
                let Some(root) = cfg.bridge_dir.clone() else {
                    return Err("bridge = \"spool\" needs bridge_dir".to_string().into());
                };
                let t = agentos_bridge::Spool::new(root).map_err(|e| format!("bridge: {e}"))?;
                Some(Rc::new(RefCell::new(agentos_bridge::Bridge::load(
                    Box::new(t),
                    &dir,
                ))))
            }
            other => {
                return Err(
                    format!("bridge = {other:?}: use \"none\", \"sim\" or \"spool\"").into(),
                )
            }
        };
        if let Some(b) = &bridge {
            connectors.push(Box::new(agentos_bridge::BridgeTools(b.clone())));
        }
        let android_on = android.is_some();
        let setup_path = dir.join("setup.json");
        let android_policy = agentos_android::governor::Policy {
            min_free_mib: cfg.android_min_free_mib,
            max_psi: cfg.android_max_psi,
            stop_in_deep: cfg.android_stop_in_deep,
        };
        if let Some(sb) = &android {
            connectors.push(Box::new(agentos_android::AndroidTools {
                sandbox: sb.clone(),
                apks: dir.join("apks"),
                blocked: android_blocked.clone(),
            }));
        }
        connectors.push(Box::new(FunctionTools(functions.clone())));
        // a Telegram bot, only when its token is in the vault
        if let Some(token) = secrets
            .vault
            .get("conn.telegram")
            .map(|b| String::from_utf8_lossy(b).to_string())
        {
            let http = Box::new(agentos_services::Ureq::default());
            connectors.push(Box::new(agentos_services::telegram::Telegram::new(
                token, http,
            )));
        }
        // Google Calendar and Gmail, when the owner has made a client and signed in
        let google_slot: GoogleSlot = Rc::new(RefCell::new(None));
        if let (Some(client), Some(tokens)) = (
            secrets
                .vault
                .get("conn.google_client")
                .map(|b| String::from_utf8_lossy(b).to_string()),
            secrets.vault.get("conn.google").and_then(parse_tokens),
        ) {
            let slot = google_slot.clone();
            let clock2 = clock.clone();
            let google = agentos_services::google::Google::new(
                agentos_services::google::provider(&client),
                tokens,
                Box::new(agentos_services::Ureq::default()),
                Box::new(move || clock2.now_ms()),
                cfg.utc_offset_minutes,
                Box::new(move |t: &agentos_services::oauth::Tokens| {
                    *slot.borrow_mut() = Some(t.clone())
                }),
            );
            connectors.push(Box::new(google));
        }
        // remote servers come after the functions, so the functions cannot call their tools
        let mut mcp_status = Vec::new();
        // the servers in the config, then those the store installed (mcp_servers.toml)
        let mut mcp_servers = cfg.mcp_servers.clone();
        mcp_servers.extend(read_store_servers(&dir));
        for s in &mcp_servers {
            let token = s
                .secret
                .as_ref()
                .and_then(|n| secrets.vault.get(n))
                .map(|b| String::from_utf8_lossy(b).to_string());
            let transport = Box::new(agentos_mcpc::Http {
                url: s.url.clone(),
                token,
            });
            let options = agentos_mcpc::Options {
                default_tier: Self::parse_tier(&s.tier),
                trust_hints: s.trust_hints,
                allow: s.allow.clone(),
            };
            match agentos_mcpc::Remote::connect(&s.name, transport, options) {
                Ok(r) => {
                    mcp_status.push((s.name.clone(), "connected".into(), r.tool_count()));
                    connectors.push(Box::new(r));
                }
                Err(e) => {
                    let _ = kernel.record(&format!("mcp {} unavailable: {e}", s.name));
                    mcp_status.push((s.name.clone(), "unavailable".into(), 0));
                }
            }
        }
        let coder_prompt = crate::coder::system_prompt(&phone_tools);
        let phone_tool_names: Vec<&'static str> = phone_tools.iter().map(|t| t.name).collect();
        let exec = Executor::new(
            connectors,
            tools_store,
            clock.clone(),
            Gate::new(&secrets.gate_secret, clock.clone(), 10 * 60_000),
        );
        let registry_tools = exec.tools().to_vec();
        let registry = Registry::new(registry_tools.clone());
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
            intents: intents_with_android(default_intents(), android_on),
            host,
            tools: registry_tools.clone(),
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
            mcp_status,
            google_slot,
            setup: crate::setup::Setup::load(&setup_path),
            setup_path,
            calls_log: std::collections::VecDeque::new(),
            messages_log: std::collections::VecDeque::new(),
            phone_status_cache: None,
            net_cache: None,
            pin_failures: 0,
            pin_locked_until_ms: 0,
            android,
            android_blocked,
            android_policy,
            android_last_check_ms: now,
            bridge,
            bridge_last_ms: now,
            android_last_stop: None,
            android_apps: None,
            mcp_providers: std::collections::HashMap::new(),
            oauth_pending: std::collections::HashMap::new(),
            gateway,
            engine,
            model,
            notifier,
            phone,
            last_phone_poll_ms: 0,
            screen: None,
            screens: std::collections::VecDeque::new(),
            skill_lib,
            signals_up: false,
            poll_again_at: None,
            prio,
            models,
            prio_error,
            functions,
            learner: Learner::open(dir.join("experience.jsonl"))?,
            classifier: crate::qa::classifier(&[]),
            intents: intents_with_android(default_intents(), android_on),
            grammar: grammar_copy,
            system: system_copy,
            coder_jobs: Vec::new(),
            trainer: crate::trainer::Trainer::new(&dir),
            coder_prompt,
            phone_tool_names,
            all_tools: registry_tools.clone(),
            small_url: small_url.clone(),
            large_url: large_url.clone(),
            anchor_key: secrets.anchor_key,
            vault: secrets.vault,
            ledger_path,
            last_activity_ms: now,
            last_anchor_ms: now,
            fixed_minute_of_day: None,
        };
        let _ = core.kernel.record("agentd started");
        core.serve_adapter();
        core.retrain();
        core.phone.borrow_mut().poll(); // learn what is already in the inbox
        core.sync_models();
        core.save_anchor()?;
        Ok(core)
    }

    /// Today, in days since 1970, at the configured UTC offset.
    fn local_day(&self) -> i64 {
        (self.wall_secs() as i64 + self.cfg.utc_offset_minutes as i64 * 60).div_euclid(86_400)
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
        let answer = self.chat_inner(text);
        self.prio.set_mode(self.baseline_mode());
        answer
    }

    fn chat_inner(&mut self, text: &str) -> String {
        let now = self.clock.now_ms();
        if let Some(good) = feedback_of(text) {
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
        if let Some(rest) = text.trim().strip_prefix("run skill ") {
            return self.run_skill(rest);
        }
        if let Some(rest) = text
            .trim()
            .strip_prefix('@')
            .or_else(|| text.trim().strip_prefix("ask "))
        {
            let (name, ask) = rest.split_once(' ').unwrap_or((rest, ""));
            return self.ask_agent(name, ask);
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
                    if let Some(a) = crate::qa::command(&label, text, &contacts)
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
            Outcome::Blocked(why) => format!("blocked: {why}"),
            Outcome::GaveUp => {
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
        self.classifier = crate::qa::classifier(&confirmed);
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
        // a phrase the phone knows exactly ("messages from mum") is a request, not a question
        if self.intents.resolve(text).is_some() {
            return None;
        }
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
    /// @<agent> <text>: an installed agent answers. An agent with tools may pick one of them, and
    /// only those; a pick that is not on its list is refused. An agent with no tools answers in text.
    fn ask_agent(&mut self, name: &str, ask: &str) -> String {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return "usage: @<agent> <text>".into();
        }
        let path = self
            .cfg
            .data_dir
            .join("agents")
            .join(format!("{name}.toml"));
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return format!("no agent called {name} is installed"),
        };
        #[derive(serde::Deserialize)]
        struct AgentFile {
            instructions: String,
            #[serde(default)]
            tools: Vec<String>,
        }
        let agent: AgentFile = match toml::from_str(&text) {
            Ok(a) => a,
            Err(e) => return format!("agent {name} is broken: {e}"),
        };
        let Some(url) = self.small_url.clone() else {
            return "no small model is configured, so agents cannot answer".into();
        };
        if agent.tools.is_empty() {
            let schema = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]});
            let messages = vec![
                ("system".to_string(), agent.instructions.clone()),
                ("user".to_string(), ask.to_string()),
            ];
            return match agentos_host::chat_json(
                &url,
                &messages,
                &schema,
                256,
                Duration::from_secs(self.cfg.model_timeout_secs),
            ) {
                Ok(raw) => serde_json::from_str::<Value>(&raw)
                    .ok()
                    .and_then(|v| v["answer"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "the agent did not answer".into()),
                Err(e) => format!("the agent could not answer: {e}"),
            };
        }
        let tools: Vec<agentos_action::Tool> = self
            .all_tools
            .iter()
            .filter(|t| agent.tools.iter().any(|n| n == t.name))
            .cloned()
            .collect();
        if tools.is_empty() {
            return format!("agent {name} lists no tools this phone has");
        }
        let registry = Registry::new(tools);
        let mut host = LlamaHost {
            small_url: url.clone(),
            large_url: url,
            grammar: registry.gbnf(),
            max_tokens: 128,
            timeout: Duration::from_secs(self.cfg.model_timeout_secs),
            system: Some(agent.instructions.clone()),
        };
        let out = agentos_brain::Model::complete(&mut host, agentos_router::Brain::LocalSmall, ask);
        match registry.parse(&out) {
            Ok(action) => {
                let now = self.clock.now_ms();
                self.act(ask, action, "agent", now)
            }
            Err(_) => format!("{name} did not pick an action it may use"),
        }
    }

    /// mcp signin <server>: find the server's sign-in details from its origin, register a client
    /// there, and start the pasted-link sign-in. Then: oauth finish <server> <pasted address>. The
    /// tokens go to the vault as conn.<server>; the server's config names that secret.
    fn mcp_signin(&mut self, name: &str) -> String {
        let url = self
            .cfg
            .mcp_servers
            .iter()
            .map(|s| (s.name.clone(), s.url.clone()))
            .chain(
                read_store_servers(&self.cfg.data_dir)
                    .into_iter()
                    .map(|s| (s.name.clone(), s.url.clone())),
            )
            .find(|(n, _)| n == name)
            .map(|(_, u)| u);
        let Some(url) = url else {
            return format!("no remote server called {name} in the config");
        };
        let redirect = "http://127.0.0.1:8765/oauth/callback";
        let mut http = agentos_services::Ureq::default();
        let meta = match agentos_services::mcp_oauth::discover(&mut http, &url) {
            Ok(m) => m,
            Err(e) => return format!("cannot find {name}'s sign-in: {e}"),
        };
        let Some(endpoint) = meta.registration_endpoint.clone() else {
            return format!("{name} does not register clients; sign in by hand with oauth begin");
        };
        let client = match agentos_services::mcp_oauth::register(&mut http, &endpoint, redirect) {
            Ok(c) => c,
            Err(e) => return format!("cannot register with {name}: {e}"),
        };
        let provider = agentos_services::mcp_oauth::provider(name, &meta, &client, redirect);
        self.mcp_providers.insert(name.to_string(), provider);
        let _ = self
            .kernel
            .record(&format!("registered a client with {name}"));
        self.oauth_cmd(&format!("begin {name}"))
    }

    /// store refresh: fetch the signed catalogue, check it, and save it for the next restart.
    fn store_refresh(&mut self) -> String {
        let (Some(url), Some(key)) = (
            self.cfg.catalog_url.clone(),
            self.cfg.catalog_pubkey.clone(),
        ) else {
            return "no catalog_url and catalog_pubkey in the config".into();
        };
        let pubkey = match agentos_catalog::update::pubkey_from_hex(&key) {
            Ok(k) => k,
            Err(e) => return e,
        };
        let base = url.trim_end_matches('/').to_string();
        let mut http = agentos_services::Ureq::default();
        let fetch = |http: &mut agentos_services::Ureq, name: &str| {
            agentos_services::Http::get(http, &format!("{base}/{name}"), None)
        };
        let (toml_text, json, sig) = match (
            fetch(&mut http, "catalog.toml"),
            fetch(&mut http, "catalog.json"),
            fetch(&mut http, "catalog.sig"),
        ) {
            (Ok(t), Ok(j), Ok(s)) => (t, j, s),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                return format!("cannot fetch the catalogue: {e}")
            }
        };
        let installed = std::fs::read_to_string(self.cfg.data_dir.join("catalog.version"))
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let manifest = match agentos_catalog::update::check(
            &pubkey,
            installed,
            json.as_bytes(),
            sig.trim(),
            toml_text.as_bytes(),
        ) {
            Ok(m) => m,
            Err(e) => {
                let _ = self.kernel.record(&format!("store catalogue refused: {e}"));
                return e;
            }
        };
        if let Err(e) = agentos_catalog::parse(&toml_text) {
            return format!("catalogue refused: it does not parse: {e}");
        }
        let save =
            std::fs::write(self.cfg.data_dir.join("catalog.toml"), &toml_text).and_then(|_| {
                std::fs::write(
                    self.cfg.data_dir.join("catalog.version"),
                    manifest.version.to_string(),
                )
            });
        match save {
            Ok(()) => {
                let _ = self.kernel.record(&format!(
                    "store catalogue saved, version {}",
                    manifest.version
                ));
                format!(
                    "catalogue version {} saved; restart agentd to use it",
                    manifest.version
                )
            }
            Err(e) => format!("cannot save the catalogue: {e}"),
        }
    }

    /// run skill <name> with k=v; k=v. Runs the steps in order through the same tools and tiers.
    /// It stops at the first step that waits for the key; after the key, say it again.
    fn run_skill(&mut self, rest: &str) -> String {
        let (name, given_text) = rest.split_once(" with ").unwrap_or((rest, ""));
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return "usage: run skill <name> with k=v; k=v".into();
        }
        let path = self
            .cfg
            .data_dir
            .join("skills")
            .join(format!("{name}.toml"));
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return format!("no skill called {name} is installed"),
        };
        let skill = match agentos_skills::Skill::from_toml(&text) {
            Ok(s) => s,
            Err(e) => return format!("skill {name} is broken: {e:?}"),
        };
        let mut given = std::collections::HashMap::new();
        for pair in given_text
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            match pair.split_once('=') {
                Some((k, v)) => {
                    given.insert(k.trim().to_string(), v.trim().to_string());
                }
                None => return format!("write each parameter as name=value, not {pair:?}"),
            }
        }
        let actions = match skill.instantiate(&given) {
            Ok(a) => a,
            Err(agentos_skills::SkillError::MissingParam(p)) => {
                return format!("missing parameter {p} for {name}")
            }
            Err(e) => return format!("skill {name}: {e:?}"),
        };
        let mut done = Vec::new();
        for (i, a) in actions.iter().enumerate() {
            let (ok, out) = self.call_tool(&a.tool, &a.args);
            if !ok && out.contains("approval required") {
                return format!(
                    "{name} stopped at step {} ({}): {out}. Press the key, then say the skill again.",
                    i + 1,
                    a.tool
                );
            }
            if !ok {
                return format!("{name} failed at step {} ({}): {out}", i + 1, a.tool);
            }
            done.push(out);
        }
        format!("{name} done: {}", done.join("; "))
    }

    fn act(&mut self, prompt: &str, a: agentos_action::Action, source: &str, now: u64) -> String {
        let ep = self
            .learner
            .begin(now, prompt, &crate::model::action_json(&a), source);
        let (ok, text) = self.call_tool(&a.tool, &a.args);
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
        // a screen: kept for the display; the conversation gets its text form
        if let Some((ui, said)) = agentos_screen::Ui::from_reply(&text) {
            let shown = json!({"tool": a.tool, "ui": ui, "at": now});
            self.screen = Some(shown.clone());
            self.screens.push_back(shown);
            while self.screens.len() > 8 {
                self.screens.pop_front();
            }
            return format!("done: {said}");
        }
        if a.tool == "run_function" {
            if let Some(out) = self.carry_out_requests(&text) {
                return out;
            }
        }
        format!("done: {text}")
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
            // a skill guide that fits the request well, cut to the budget, goes with it
            let request = match self.skill_lib.as_ref().and_then(|l| l.context_for(&request, self.cfg.skill_budget / 2, 12.0)) {
                Some(guide) => format!("{request}\n\nA guide that may help (follow it only where it fits Rust for this sandbox):\n{guide}"),
                None => request,
            };
            if let Err(e) = self.start_job(system, &request, schema, None, reply.clone()) {
                let _ = reply.send(e);
            }
            return;
        }
        if let Some(text) = t.strip_prefix("chat ") {
            self.touch();
            if let Some(answer) = self.answer_by_code(text) {
                let _ = reply.send(answer);
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
        self.coder_jobs.push(crate::coder::Job {
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
                    let _ = job
                        .reply
                        .send(format!("answer (attempt {}): {answer}", job.attempt));
                }
                Ok(ok) => {
                    let name = ok.split_whitespace().next().unwrap_or("").to_string();
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
            "phone": self.phone_text(),
            "calls": self.calls_log.iter().cloned().collect::<Vec<_>>(),
            "messages": self.messages_log.iter().cloned().collect::<Vec<_>>(),
            "contacts": self.phone.borrow().contacts.list.iter().map(|c| json!({"name": c.name, "number": c.number})).collect::<Vec<_>>(),
            "connectors": self.mcp_status.iter().map(|(n, st, k)| json!({"name": n, "state": st, "tools": k})).collect::<Vec<_>>(),
            "pin_set": self.vault.get(PIN_NAME).is_some(),
            "pending": pending,
            "recent": recent,
            "screen": self.screen,
            "models": lines(self.models.report()),
            "prio": self.prio.report().lines().next().unwrap_or_default(),
            "functions": lines(self.functions.borrow().list()),
            "learning": self.learner.stats(),
            "setup": self.setup_state(),
            "device": self.device_state(),
            "dashboard": self.dashboard(),
            "screens": self.screens.iter().rev().cloned().collect::<Vec<_>>(),
            "owner": self.setup.answers.get("owner").cloned(),
            "store": {"installed": agentos_catalog::store::installed(&self.cfg.data_dir)},
            "android": self.android_state(),
            "memory": self.memory_state(),
            "inbox": self.inbox_json(),
            "bridge": self.bridge_json(),
        })
    }

    /// The newest 30 messages, newest first.
    fn inbox_json(&self) -> Value {
        match &self.bridge {
            None => json!([]),
            Some(b) => {
                let b = b.borrow();
                let v: Vec<Value> = b
                    .state
                    .inbox
                    .iter()
                    .rev()
                    .take(30)
                    .map(|m| {
                        json!({"id": m.id, "time_ms": m.time_ms, "app": m.app, "package": m.package,
                               "conversation": m.conversation, "sender": m.sender, "text": m.text,
                               "files": m.files, "read": m.read, "replied": m.replied})
                    })
                    .collect();
                json!(v)
            }
        }
    }

    /// Whether the bridge is up, what is unread, the owner's rules and the newest files.
    fn bridge_json(&self) -> Value {
        match &self.bridge {
            None => json!({"up": false, "unread": 0, "apps": [], "received": []}),
            Some(b) => {
                let b = b.borrow();
                json!({
                    "up": b.transport.up(),
                    "unread": b.unread(),
                    "apps": b.state.rules,
                    "received": b.state.received.iter().rev().take(20).cloned().collect::<Vec<_>>(),
                })
            }
        }
    }

    /// Reads the bridge's events into the inbox, at most every bridge_poll_secs.
    fn bridge_tick(&mut self, now_ms: u64) {
        let Some(b) = self.bridge.clone() else { return };
        if now_ms.saturating_sub(self.bridge_last_ms) < self.cfg.bridge_poll_secs * 1000 {
            return;
        }
        self.bridge_last_ms = now_ms;
        let lines = b.borrow_mut().pump(now_ms);
        for l in lines {
            let _ = self.kernel.record(&l);
        }
    }

    /// bridge status | bridge allow <package> <read|reply|files> <on|off> | bridge sim <json>
    fn bridge_cmd(&mut self, rest: &str) -> String {
        let Some(b) = self.bridge.clone() else {
            return "no Android bridge (bridge = \"none\")".into();
        };
        let rest = rest.trim();
        let (sub, args) = rest.split_once(' ').unwrap_or((rest, ""));
        match sub {
            "status" => {
                let call = agentos_bridge::BridgeTools(b.clone()).call(&agentos_action::Action {
                    tool: "android_bridge_status".into(),
                    args: vec![],
                });
                call.unwrap_or_else(|e| e)
            }
            "allow" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let [pkg, what, onoff] = parts[..] else {
                    return "usage: bridge allow <package> <read|reply|files> <on|off>".into();
                };
                let on = match onoff {
                    "on" => true,
                    "off" => false,
                    _ => return "usage: bridge allow <package> <read|reply|files> <on|off>".into(),
                };
                let res = b.borrow_mut().set_rule(pkg, what, on);
                match res {
                    Ok(()) => {
                        let _ = b.borrow().save();
                        let _ = self.kernel.record(&format!(
                            "bridge rule: {pkg} {what} {}",
                            if on { "on" } else { "off" }
                        ));
                        "ok".into()
                    }
                    Err(e) => e,
                }
            }
            "sim" => {
                let e: agentos_bridge::Event = match serde_json::from_str(args) {
                    Ok(e) => e,
                    Err(e) => return format!("not an event: {e}"),
                };
                if !b.borrow_mut().transport.inject(e) {
                    return "only with bridge = \"sim\"".into();
                }
                let now = self.clock.now_ms();
                let lines = b.borrow_mut().pump(now);
                for l in &lines {
                    let _ = self.kernel.record(l);
                }
                "ok".into()
            }
            _ => "usage: bridge status | bridge allow <package> <read|reply|files> <on|off> | bridge sim <json>".into(),
        }
    }

    /// The control channel: the approval key and operator commands.
    /// "low", "medium" or "high"; anything else is High, so a typo never lowers a tier.
    fn parse_tier(s: &str) -> agentos_action::Tier {
        match s {
            "low" => Tier::Low,
            "medium" => Tier::Medium,
            _ => Tier::High,
        }
    }

    /// The sandbox for the screen. The app list is read at most every 30 seconds.
    fn android_state(&mut self) -> Value {
        let Some(sb) = self.android.clone() else {
            return json!({"mode": "none", "running": false, "apps": [], "last_stop": null});
        };
        let now = self.clock.now_ms();
        let running = sb.borrow_mut().running();
        let stale = self
            .android_apps
            .as_ref()
            .is_none_or(|(at, _)| now.saturating_sub(*at) >= 30_000);
        if running && stale {
            let apps = sb.borrow_mut().apps().unwrap_or_default();
            self.android_apps = Some((now, apps));
        }
        let apps: Vec<Value> = if running {
            self.android_apps
                .iter()
                .flat_map(|(_, apps)| apps.iter())
                .map(|a| json!({"name": a.name, "package": a.package}))
                .collect()
        } else {
            vec![]
        };
        let last = self
            .android_last_stop
            .as_ref()
            .map(|(r, at)| json!({"reason": r, "at_ms": at}));
        json!({"mode": self.cfg.android, "running": running, "apps": apps, "last_stop": last})
    }

    /// The dashboard, generated from what the phone knows now, as a screen document like any
    /// tool's answer: battery, messages, calls, what waits, memory, and the last photo.
    fn dashboard(&mut self) -> Value {
        use agentos_screen::{Card, Ui};
        let mut ui = Ui::new("Today").kind("dashboard");
        let battery = self.power.level_percent();
        let charging = self.power.on_charger();
        ui = ui.card(
            Card::new("Battery")
                .metric(
                    format!("{battery}%"),
                    if charging { "charging" } else { "on battery" },
                    "",
                )
                .progress(battery as f32 / 100.0)
                .tone(if battery <= 20 && !charging {
                    "warn"
                } else {
                    "ok"
                }),
        );
        let waiting = self.gateway.pending().len();
        if waiting > 0 {
            ui = ui.card(
                Card::new("Waiting for you")
                    .metric(waiting.to_string(), "to approve", "")
                    .tone("warn"),
            );
        }
        let msgs: Vec<&Value> = self.messages_log.iter().rev().take(2).collect();
        let mut m = Card::new("Messages").metric(self.messages_log.len().to_string(), "today", "");
        for x in &msgs {
            m = m.line(format!(
                "{}: {}",
                x["who"].as_str().unwrap_or("?"),
                x["text"].as_str().unwrap_or("")
            ));
        }
        ui = ui.card(m.button("Read", "read my messages"));
        let missed: Vec<&Value> = self
            .calls_log
            .iter()
            .filter(|c| c["state"] == "missed")
            .collect();
        let mut c = Card::new("Calls").metric(missed.len().to_string(), "missed", "");
        if let Some(last) = self.calls_log.back() {
            let who = last["who"].as_str().unwrap_or("?").to_string();
            c = c
                .line(format!(
                    "last: {who} ({})",
                    last["state"].as_str().unwrap_or("")
                ))
                .button("Call back", format!("call {who}"));
        }
        ui = ui.card(c);
        let p = agentos_android::governor::Pressure::read(&self.cfg.proc_root);
        if p.available_mib > 0 {
            ui = ui.card(
                Card::new("Memory")
                    .metric(
                        format!("{:.1} GB", p.available_mib as f32 / 1024.0),
                        "free",
                        "",
                    )
                    .progress((p.available_mib as f32 / 8192.0).min(1.0)),
            );
        }
        // the last photo taken or edited, if any
        let photo = self.screens.iter().rev().find_map(|s| {
            let ui = &s["ui"];
            (ui["kind"] == "photo")
                .then(|| ui["cards"][0]["image"].as_str().map(str::to_string))
                .flatten()
        });
        if let Some(img) = photo {
            ui = ui.card(
                Card::new("Last photo")
                    .image(img)
                    .button("Photos", "photos"),
            );
        }
        if let Some(b) = &self.bridge {
            let b = b.borrow();
            let unread = b.unread();
            let last = b.state.inbox.iter().rev().find(|m| !m.read);
            let mut card = Card::new("Android messages").metric(unread.to_string(), "unread", "");
            if let Some(m) = last {
                card = card.line(format!(
                    "{}: {}",
                    m.sender,
                    m.text.chars().take(40).collect::<String>()
                ));
            }
            ui = ui.card(card.button("Open", "android inbox"));
        }
        serde_json::to_value(&ui).unwrap_or(Value::Null)
    }

    /// What the status bar shows: time, battery, signal, network and Wi-Fi, read at most every
    /// 5 seconds (the modem is asked through its tools).
    fn device_state(&mut self) -> Value {
        let now = self.clock.now_ms();
        let fresh = self
            .net_cache
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_sub(*at) < 5_000);
        if !fresh {
            let net = self.phone.borrow_mut().backend.status().ok();
            self.net_cache = Some((now, net));
        }
        let net = self.net_cache.as_ref().and_then(|(_, n)| n.clone());
        // the wall clock in the configured time zone (the daemon's own clock may be simulated)
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            + self.cfg.utc_offset_minutes as i64 * 60;
        let minute = secs.div_euclid(60).rem_euclid(24 * 60);
        json!({
            "time": format!("{}:{:02}", minute / 60, minute % 60),
            "battery": self.power.level_percent(),
            "charging": self.power.on_charger(),
            "signal": net.as_ref().map(|n| n.signal),
            "tech": net.as_ref().map(|n| n.tech.to_uppercase()),
            "operator": net.as_ref().map(|n| n.operator.clone()),
            "wifi": net.as_ref().and_then(|n| n.wifi.clone()),
            "mobile_data": net.as_ref().map(|n| n.mobile_data),
            "airplane": net.as_ref().map(|n| n.airplane),
        })
    }

    /// The setup screen while setup is not done, null afterwards.
    fn setup_state(&mut self) -> Value {
        if self.setup.is_done() {
            return Value::Null;
        }
        let facts = self.setup_facts();
        setup_screen_json(&self.setup.screen(&facts))
    }

    fn memory_state(&self) -> Value {
        let p = agentos_android::governor::Pressure::read(&self.cfg.proc_root);
        json!({"available_mib": p.available_mib, "psi": p.psi})
    }

    /// do <tool> <json object>: call a tool exactly as an MCP client would, with the same tiers
    /// and approval steps. The answer is the text of the tool's result.
    fn do_cmd(&mut self, rest: &str) -> String {
        let (tool, json_args) = rest.trim().split_once(' ').unwrap_or((rest.trim(), "{}"));
        let args: Value = match serde_json::from_str(json_args.trim()) {
            Ok(v @ Value::Object(_)) => v,
            _ => return "usage: do <tool> <json object>".into(),
        };
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}).to_string();
        let reply = match self.mcp(&body) {
            Some(r) => r,
            None => return format!("no answer from {tool}"),
        };
        let v: Value = serde_json::from_str(&reply).unwrap_or(Value::Null);
        if let Some(msg) = v["error"]["message"].as_str() {
            return format!("error: {msg}");
        }
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("error: no text in the answer from {tool}"));
        if v["result"]["isError"] == json!(true) {
            return format!("error: {text}");
        }
        text
    }

    fn setup_facts(&mut self) -> crate::setup::Facts {
        let android_available = match self.android.clone() {
            Some(sb) => sb.borrow_mut().available(),
            None => false,
        };
        crate::setup::Facts {
            networks: vec![],
            known_apps: vec![],
            android_available,
        }
    }

    /// setup: the current screen. setup answer <text> | setup skip | setup back | setup reset.
    /// Each change is saved and the new screen is printed.
    fn setup_cmd(&mut self, rest: &str) -> String {
        let (sub, text) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let facts = self.setup_facts();
        let effects = match sub {
            "" => vec![],
            "answer" => self.setup.answer(text, &facts),
            "skip" => {
                self.setup.skip();
                vec![]
            }
            "back" => {
                self.setup.back();
                vec![]
            }
            "reset" => {
                self.setup = crate::setup::Setup::default();
                vec![]
            }
            _ => {
                return "usage: setup | setup answer <text> | setup skip | setup back | setup reset"
                    .into()
            }
        };
        if let Err(e) = self.setup.save(&self.setup_path) {
            let _ = self.kernel.record(&format!("setup: cannot save: {e}"));
        }
        self.run_setup_effects(effects);
        let facts = self.setup_facts();
        setup_screen_json(&self.setup.screen(&facts)).to_string()
    }

    /// Run what the setup answers asked for, through the same tools and vault as everything else.
    fn run_setup_effects(&mut self, effects: Vec<crate::setup::Effect>) {
        use crate::setup::Effect;
        for e in effects {
            match e {
                Effect::RunTool { tool, args } => {
                    let (ok, text) = self.call_tool(&tool, &args);
                    if !ok {
                        let _ = self
                            .kernel
                            .record(&format!("setup: {tool} did not run: {text}"));
                    }
                }
                Effect::SetSecret { name, value } => {
                    let _ = self.vault.put(&name, value.as_bytes());
                    let _ = self.kernel.record(&format!("setup: secret set: {name}"));
                }
                Effect::SetPin(pin) => {
                    let _ = self.vault.put(PIN_NAME, pin.as_bytes());
                    let _ = self.kernel.record("setup: approval PIN set");
                }
                Effect::PullModels => {
                    // downloads run on their own thread; the daemon keeps answering meanwhile
                    let specs = self.cfg.models.clone();
                    let dests: Vec<PathBuf> = specs.iter().map(|m| self.models.path(m)).collect();
                    std::thread::spawn(move || {
                        for (spec, dest) in specs.iter().zip(dests) {
                            if dest.exists() {
                                continue;
                            }
                            if let Err(e) = agentos_models::pull(spec, &dest, &mut |_, _| {}) {
                                eprintln!("model {}: {e}", spec.name);
                            }
                        }
                    });
                }
                Effect::ImportContacts(path) => match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let (added, skipped) = self.phone.borrow_mut().import_vcard(&text);
                        let _ = self.kernel.record(&format!(
                            "setup: {added} contacts imported, {skipped} skipped"
                        ));
                    }
                    Err(e) => {
                        let _ = self
                            .kernel
                            .record(&format!("setup: cannot read {path}: {e}"));
                    }
                },
                Effect::Finish => {
                    let _ = self.kernel.record("setup finished");
                }
            }
        }
    }

    /// press <id> [<pin>]: approves a waiting action. With a PIN set, the PIN is required; five
    /// wrong PINs lock presses for a minute.
    fn press_cmd(&mut self, rest: &str) -> String {
        let mut parts = rest.split_whitespace();
        let id = match parts
            .next()
            .map(|t| t.trim_start_matches('#').parse::<u64>())
        {
            Some(Ok(id)) => id,
            _ => return "no such pending item".into(),
        };
        let given = parts.next();
        if let Some(stored) = self.vault.get(PIN_NAME).map(|b| b.to_vec()) {
            let now = self.clock.now_ms();
            if now < self.pin_locked_until_ms {
                return format!(
                    "presses are locked after wrong PINs; try again in {} s",
                    (self.pin_locked_until_ms - now) / 1000 + 1
                );
            }
            let Some(given) = given else {
                return format!("a PIN is needed: press {id} <pin>");
            };
            if !agentos_gateway::http::ct_eq(given.as_bytes(), &stored) {
                self.pin_failures += 1;
                if self.pin_failures >= 5 {
                    self.pin_failures = 0;
                    self.pin_locked_until_ms = now + 60_000;
                    let _ = self
                        .kernel
                        .record("five wrong approval PINs; presses refused for 60 s");
                    return "wrong PIN; too many tries, presses are locked for 60 s".into();
                }
                return "wrong PIN".into();
            }
            self.pin_failures = 0;
        }
        if self.gateway.press_key(id) {
            self.learner.approved(id, self.clock.now_ms());
            self.retrain();
            format!("approved #{id}; ask again within 10 minutes")
        } else {
            "no such pending item".into()
        }
    }

    /// pin set <new> [<old>]: 4 to 8 digits. Changing a PIN needs the old one.
    fn pin_cmd(&mut self, rest: &str) -> String {
        let mut parts = rest.split_whitespace();
        if parts.next() != Some("set") {
            return "usage: pin set <new> [<old>]".into();
        }
        let new = parts.next().unwrap_or("");
        let old = parts.next();
        if !(4..=8).contains(&new.len()) || !new.bytes().all(|b| b.is_ascii_digit()) {
            return "a PIN is 4 to 8 digits".into();
        }
        if let Some(stored) = self.vault.get(PIN_NAME).map(|b| b.to_vec()) {
            let Some(old) = old else {
                return "changing the PIN needs the old one: pin set <new> <old>".into();
            };
            if !agentos_gateway::http::ct_eq(old.as_bytes(), &stored) {
                return "the old PIN is wrong".into();
            }
        }
        match self.vault.put(PIN_NAME, new.as_bytes()) {
            Ok(()) => {
                let _ = self.kernel.record("approval PIN set");
                "PIN set".into()
            }
            Err(e) => format!("cannot store the PIN: {e:?}"),
        }
    }

    /// oauth begin <provider> | oauth finish <provider> <pasted address or code>
    fn oauth_cmd(&mut self, rest: &str) -> String {
        let (sub, args) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let (name, pasted) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let name = name.to_string();
        let provider = match self.cfg.oauth_providers.iter().find(|p| p.name == name) {
            Some(p) => agentos_services::oauth::Provider {
                name: name.clone(),
                client_id: p.client_id.clone(),
                client_secret: p.client_secret.clone(),
                auth_url: p.auth_url.clone(),
                token_url: p.token_url.clone(),
                scopes: p.scopes.clone(),
                redirect: p.redirect.clone(),
            },
            // a server signed in by discovery (mcp signin) keeps its provider here
            None if self.mcp_providers.contains_key(&name) => self.mcp_providers[&name].clone(),
            // Google needs no config entry once its client is in the vault
            None => match (name.as_str(), self.vault.get("conn.google_client")) {
                ("google", Some(c)) => {
                    agentos_services::google::provider(&String::from_utf8_lossy(c))
                }
                _ => return format!("no sign-in provider {name:?} in the config"),
            },
        };
        match sub {
            "begin" => {
                let mut random = [0u8; 48];
                if let Err(e) = getrandom::getrandom(&mut random) {
                    return format!("cannot make a sign-in: {e}");
                }
                let (url, pending) = agentos_services::oauth::begin(&provider, &random);
                self.oauth_pending.insert(name.clone(), pending);
                format!(
                    "open this in a browser and approve:\n{url}\nthen paste the address it lands on: oauth finish {} <address>",
                    name
                )
            }
            "finish" => {
                let Some(pending) = self.oauth_pending.get(&name) else {
                    return format!(
                        "no sign-in is waiting for {}; run oauth begin {} first",
                        name, name
                    );
                };
                let mut http = agentos_services::Ureq::default();
                let now = self.clock.now_ms();
                match agentos_services::oauth::finish(&provider, pending, pasted, &mut http, now) {
                    Ok(t) => {
                        let stored = json!({"access": t.access, "refresh": t.refresh, "expires_at_ms": t.expires_at_ms});
                        if let Err(e) = self
                            .vault
                            .put(&format!("conn.{}", name), stored.to_string().as_bytes())
                        {
                            return format!("cannot store the sign-in: {e:?}");
                        }
                        self.oauth_pending.remove(&name);
                        let _ = self.kernel.record(&format!("signed in to {}", name));
                        format!("signed in to {}", name)
                    }
                    Err(e) => e,
                }
            }
            _ => "usage: oauth begin <provider> | oauth finish <provider> <pasted address or code>"
                .into(),
        }
    }

    /// secret set <name> <value> | secret list | secret rm <name>. Names must start with conn.;
    /// values are never printed or written to the ledger.
    fn secret_cmd(&mut self, rest: &str) -> String {
        let (sub, args) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        match sub {
            "list" => {
                let names: Vec<&str> = self
                    .vault
                    .names()
                    .into_iter()
                    .filter(|n| n.starts_with("conn."))
                    .collect();
                if names.is_empty() {
                    "no service secrets".into()
                } else {
                    names.join("\n")
                }
            }
            "set" => {
                let (name, value) = args.split_once(' ').unwrap_or((args, ""));
                if !name.starts_with("conn.") || name.len() <= 5 {
                    return format!("refused: a service secret name starts with conn. ({name})");
                }
                if value.is_empty() {
                    return "usage: secret set <name> <value>".into();
                }
                match self.vault.put(name, value.as_bytes()) {
                    Ok(()) => {
                        let _ = self.kernel.record(&format!("secret set: {name}"));
                        format!("stored {name}")
                    }
                    Err(e) => format!("cannot store {name}: {e:?}"),
                }
            }
            "rm" => {
                let name = args.trim();
                if !name.starts_with("conn.") {
                    return format!("refused: a service secret name starts with conn. ({name})");
                }
                match self.vault.delete(name) {
                    Ok(true) => {
                        let _ = self.kernel.record(&format!("secret removed: {name}"));
                        format!("removed {name}")
                    }
                    Ok(false) => format!("no secret {name}"),
                    Err(e) => format!("cannot remove {name}: {e:?}"),
                }
            }
            _ => "usage: secret set <name> <value> | secret list | secret rm <name>".into(),
        }
    }

    pub fn ctl(&mut self, line_in: &str) -> String {
        let line_in = line_in.trim();
        let (cmd, rest) = line_in.split_once(' ').unwrap_or((line_in, ""));
        match cmd {
            "status" => self.status_line(),
            "do" => self.do_cmd(rest),
            "oauth" => self.oauth_cmd(rest),
            "mcp" => match rest.trim().strip_prefix("signin ") {
                Some(name) => self.mcp_signin(name.trim()),
                None => "usage: mcp signin <server name>".into(),
            },
            "connectors" => {
                if self.mcp_status.is_empty() {
                    "no remote servers configured".into()
                } else {
                    self.mcp_status
                        .iter()
                        .map(|(n, st, k)| format!("{n}: {st}, {k} tools"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "restart" => {
                // the core loop sees STOP within a second; the service manager starts agentd again
                crate::server::STOP.store(true, std::sync::atomic::Ordering::SeqCst);
                "stopping; the service manager starts agentd again".into()
            }
            "secret" => self.secret_cmd(rest),
            "stage" => format!("{:?}", self.kernel.stage),
            "chat" => self.chat(rest),
            "state" => self.ui_state().to_string(),
            "bridge" => self.bridge_cmd(rest),
            "screen" => self.screen.as_ref().map_or("no screen yet".into(), |s| s.to_string()),
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
            "press" => self.press_cmd(rest),
            "pin" => self.pin_cmd(rest),
            "setup" => self.setup_cmd(rest),
            "store" => match rest.trim() {
                "refresh" => self.store_refresh(),
                _ => "usage: store refresh".into(),
            },
            "import" => match rest.trim().strip_prefix("contacts ") {
                Some(path) => match std::fs::read_to_string(path.trim()) {
                    Ok(text) => {
                        let (added, skipped) = self.phone.borrow_mut().import_vcard(&text);
                        format!("added {added} contacts, skipped {skipped}")
                    }
                    Err(e) => format!("cannot read {}: {e}", path.trim()),
                },
                None => "usage: import contacts <path to a vCard file>".into(),
            },
            // the hardware approval key: approves the one waiting action, never a guess among several
            "key" => {
                let p = self.gateway.pending();
                match p.len() {
                    0 => "nothing waiting".into(),
                    1 => {
                        let id = p[0].id;
                        let what = p[0].action.tool.to_string();
                        if self.gateway.press_key(id) {
                            self.learner.approved(id, self.clock.now_ms());
                            self.retrain();
                            let _ = self.kernel.record(&format!("approved #{id} {what} with the approval key"));
                            format!("approved #{id} {what}")
                        } else {
                            "no such pending item".into()
                        }
                    }
                    n => {
                        let _ = self.kernel.record(&format!("approval key held with {n} actions waiting; none approved"));
                        format!("{n} actions waiting; the key approves only when one is: use press N")
                    }
                }
            }
            "decline" => match rest.trim().trim_start_matches('#').parse::<u64>() {
                Ok(id) if self.gateway.decline(id) => {
                    self.learner.declined(id);
                    self.retrain();
                    format!("declined #{id}")
                }
                _ => "no such pending item".into(),
            },
            // from the D-Bus watcher (server.rs): "up", "down", or what arrived
            "phone-signal" => match rest.trim() {
                "up" => {
                    self.signals_up = true;
                    "watching ModemManager signals".into()
                }
                "down" => {
                    self.signals_up = false;
                    "polling".into()
                }
                _ => {
                    self.poll_again_at = Some(self.clock.now_ms() + 2000);
                    let said = self.poll_phone();
                    if said.is_empty() { "nothing new".into() } else { said.join("\n") }
                }
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
            "" => "commands: status state stage chat pending press key decline good bad learn power anchor verify notify digest tools phone sim-call sim-sms prio models functions show secret do restart connectors oauth pin import setup store".into(),
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
    /// Keep the last 20 calls and messages for the screen.
    fn remember_phone_event(&mut self, ev: &PhoneEvent) {
        let at = self.clock.now_ms();
        match ev {
            PhoneEvent::IncomingCall { from } => push_capped(
                &mut self.calls_log,
                json!({"who": from, "state": "incoming", "at": at}),
            ),
            PhoneEvent::MissedCall { from } => push_capped(
                &mut self.calls_log,
                json!({"who": from, "state": "missed", "at": at}),
            ),
            PhoneEvent::NewSms { from, text } => push_capped(
                &mut self.messages_log,
                json!({"who": from, "text": text, "at": at}),
            ),
        }
    }

    /// The phone's status text, read at most every 5 seconds.
    fn phone_text(&mut self) -> String {
        let now = self.clock.now_ms();
        if let Some((at, text)) = &self.phone_status_cache {
            if now.saturating_sub(*at) < 5_000 {
                return text.clone();
            }
        }
        let text = self
            .phone
            .borrow_mut()
            .status_text()
            .unwrap_or_else(|e| format!("phone: {e}"));
        self.phone_status_cache = Some((now, text.clone()));
        text
    }

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
            self.remember_phone_event(&ev);
            let _ = self.kernel.record(&ledger);
            let line = format!("{}: {} from {}", note.app, note.text, note.sender);
            match self.notifier.offer(note, self.minute_of_day()) {
                Decision::Deliver => match self.phone.borrow_mut().backend.feedback(feedback) {
                    Ok(()) => said.push(format!("{line} (alerted)")),
                    // no sound or vibration reached the human: say so, do not claim it did
                    Err(e) => {
                        let _ = self.kernel.record(&format!("{ledger}: alert failed"));
                        said.push(format!("{line} (alert failed: {e})"));
                    }
                },
                Decision::Hold => said.push(format!("{line} (held for the digest)")),
            }
        }
        said
    }

    /// Called about once a second by the run loop.
    /// The heavy job the governor must not compete with, if any.
    fn heavy_job(&self) -> Option<&'static str> {
        if !self.coder_jobs.is_empty() {
            Some("writing a function")
        } else if self.trainer.running.is_some() {
            Some("training")
        } else if self.wanted_roles().contains(&Role::Large) {
            Some("loading the large model")
        } else {
            None
        }
    }

    /// Every android_check_secs: decide whether the sandbox must close, close it if so, and say
    /// whether apps may be opened now.
    fn android_check(&mut self, now: u64) {
        let Some(sb) = self.android.clone() else {
            return;
        };
        let pressure = agentos_android::governor::Pressure::read(&self.cfg.proc_root);
        let deep = self.kernel.stage == Stage::Deep;
        let reason = agentos_android::governor::decide(
            &pressure,
            deep,
            self.heavy_job(),
            &self.android_policy,
        );
        *self.android_blocked.borrow_mut() = reason.as_ref().map(|r| r.to_string());
        let running = sb.borrow_mut().running();
        if let (true, Some(r)) = (running, reason) {
            let _ = sb.borrow_mut().stop_all();
            let _ = self.kernel.record(&format!("android sandbox closed: {r}"));
            self.android_last_stop = Some((r.to_string(), now));
        }
    }

    pub fn tick(&mut self) {
        let now_ms = self.clock.now_ms();
        self.bridge_tick(now_ms);
        if self.android.is_some()
            && now_ms.saturating_sub(self.android_last_check_ms)
                >= self.cfg.android_check_secs * 1000
        {
            self.android_last_check_ms = now_ms;
            self.android_check(now_ms);
        }
        let fresh = self.google_slot.borrow_mut().take();
        if let Some(t) = fresh {
            if self
                .vault
                .put("conn.google", tokens_json(&t).as_bytes())
                .is_err()
            {
                let _ = self
                    .kernel
                    .record("google: could not save the refreshed sign-in");
            }
        }
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
        let every = if self.signals_up {
            self.cfg.phone_signal_poll_secs
        } else {
            self.cfg.phone_poll_secs
        };
        let again = self.poll_again_at.is_some_and(|t| now >= t);
        if again || now.saturating_sub(self.last_phone_poll_ms) >= every * 1000 {
            self.poll_again_at = None;
            self.poll_phone();
        }
        if now.saturating_sub(self.last_anchor_ms) >= self.cfg.anchor_secs * 1000 {
            let _ = self.save_anchor();
        }
        self.train_tick();
        self.sync_models();
        self.prio.set_mode(self.baseline_mode());
        self.prio.tick();
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
            let _ = self
                .kernel
                .record(&format!("model {} now served with {what}", spec.name));
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
            new_examples: self
                .learner
                .labelled()
                .saturating_sub(self.trainer.trained_on),
            min_new: self.cfg.train_min_new,
            forced: self.trainer.forced,
            nightly_due: self.cfg.train_nightly
                && self.learner.labelled() > self.trainer.trained_on
                && self.trainer.last_day != Some(self.local_day()),
        }
    }

    /// Start, stop or finish overnight training.
    fn train_tick(&mut self) {
        if let Some(outcome) = self.trainer.poll() {
            use crate::trainer::Outcome;
            let line = match outcome {
                Outcome::Kept {
                    adapter,
                    base,
                    tuned,
                } => {
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
                Outcome::Paused { step } => {
                    format!("training paused at step {step}; continues next time")
                }
                Outcome::Failed(why) => format!("training failed: {why}"),
            };
            let _ = self.kernel.record(&line);
            self.trainer.last = Some(line);
        }
        let blocker = self.train_conditions().blocker();
        if self.trainer.running.is_some() {
            if let Some(why) =
                blocker.filter(|w| !w.contains("busy") || !self.coder_jobs.is_empty())
            {
                if self.trainer.stop() {
                    let line = format!(
                        "training stopped ({why}); continues from its checkpoint next time"
                    );
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
        match self.trainer.start(
            &bin,
            &model,
            examples,
            self.cfg.train_steps,
            self.cfg.train_chunk_secs,
        ) {
            Ok(pid) => {
                self.prio.adopt(pid);
                let today = self.local_day();
                self.trainer.started_on(today);
                let line = format!(
                    "training {} on {examples} confirmed requests (pid {pid})",
                    spec.name
                );
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
