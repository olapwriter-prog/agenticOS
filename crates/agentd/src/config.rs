//! Daemon configuration, read from a TOML file. Every field has a default, so an empty file works.
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where the vault, ledger, databases and anchors live.
    pub data_dir: PathBuf,
    /// MCP endpoint. Keep it on loopback unless you know why not.
    pub listen: String,
    /// Browser origins allowed to call the endpoint. Empty means command-line clients only.
    pub allowed_origins: Vec<String>,
    /// llama-server URLs. If both are absent, only intents answer (no model needed).
    pub small_url: Option<String>,
    pub large_url: Option<String>,
    /// Send hard jobs to the commercial model (needs a key in the vault).
    pub commercial_enabled: bool,
    pub commercial_model: String,
    pub online: bool,
    /// Seconds of silence before Active goes back to Asleep.
    pub idle_secs: u64,
    /// How often to save a ledger anchor.
    pub anchor_secs: u64,
    /// Minutes east of UTC, for quiet hours. 330 is India.
    pub utc_offset_minutes: i32,
    pub quiet_start_min: u32,
    pub quiet_end_min: u32,
    pub daily_budget: u32,
    /// A Linux power_supply directory (for example /sys/class/power_supply/BAT0). None means simulated.
    pub power_dir: Option<PathBuf>,
    /// Use the real cgroup and cpufreq trees. False writes to a simulated tree under data_dir.
    pub real_hardware: bool,
    /// Offer the demo messenger, which sends nothing and only records. Off on a real phone.
    pub demo_connectors: bool,
    /// Vault key-derivation memory in MiB. Raise it on the phone; tests use 1.
    pub kdf_mib: u32,
    /// "sim" keeps a simulated phone in memory; "modemmanager" drives the real modem, Wi-Fi,
    /// LEDs and feedback through ModemManager, NetworkManager, feedbackd and sysfs.
    pub phone: String,
    /// Where the flash LED lives (real phone only).
    pub leds_dir: PathBuf,
    /// How often to look for incoming calls and texts.
    pub phone_poll_secs: u64,
    /// Watch ModemManager's D-Bus signals (with this dbus-monitor) so calls and texts are seen
    /// as they arrive; polling then only backs it up, every `phone_signal_poll_secs`.
    /// Used with phone = "modemmanager". Unset: poll only.
    pub dbus_monitor: Option<PathBuf>,
    /// The user whose session runs callaudiod, feedbackd and PipeWire (the agentd-session
    /// service). Unset: those tools run as agentd's own user.
    pub session_user: Option<String>,
    pub phone_signal_poll_secs: u64,
    /// The control socket. Defaults to control.sock in data_dir.
    pub control_socket: Option<PathBuf>,
    /// The approval key: the input device of a hardware button (e.g.
    /// /dev/input/by-path/platform-gpio-keys-event). Unset: approve with `agentd ctl press N`.
    pub approval_key: Option<PathBuf>,
    /// Which key on that device (Linux key code; 114 = volume down).
    pub approval_keycode: u16,
    /// How long the key must be held to approve.
    pub approval_hold_ms: u64,

    /// llama.cpp's server binary. agentd starts one per configured model.
    pub llama_server: PathBuf,
    /// Where model files live. Defaults to models/ in data_dir.
    pub models_dir: Option<PathBuf>,
    /// Local models (`[[models]]` tables). When set, small_url/large_url default to them.
    pub models: Vec<agentos_models::ModelSpec>,
    /// Keep the small model loaded even while asleep, so the first answer is fast.
    pub keep_small_loaded: bool,
    /// How long a chat waits for a model that is still loading.
    pub model_wait_secs: u64,
    /// Cores never given to the model (kept for calls and the system). Empty: the slowest core.
    pub reserve_cpus: Vec<u32>,
    /// Above this temperature the model's boost is dropped; it returns below cool_c.
    pub hot_c: f32,
    pub cool_c: f32,
    /// How many times the local model may try to write a working function.
    pub coder_attempts: u32,
    /// How long one attempt may take (a phone writing code is slow).
    pub coder_timeout_secs: u64,
    /// The Rust compiler, with the wasm32-unknown-unknown target, for the agent's functions.
    pub rustc: PathBuf,
    /// How long one model request may take. A CPU engine on a phone reading a long prompt
    /// for the first time needs a while; after that the prompt is reused.
    pub model_timeout_secs: u64,
    /// Overnight training of the small model on the requests the human confirmed.
    pub train_enabled: bool,
    /// Train again once this many new confirmed requests have come in.
    pub train_min_new: usize,
    /// Battery the phone must have (on the charger) to train.
    pub train_min_battery: u8,
    /// Optimizer steps per training run.
    pub train_steps: usize,
    /// A run stops and checkpoints after this long, and continues on the next tick.
    pub train_chunk_secs: u64,
    /// Train every night on the charger on what was confirmed that day, even if fewer than
    /// train_min_new: the model follows the person day by day.
    pub train_nightly: bool,
    /// The business partner and personal assistant (`[partner]` table): channels, invoices
    /// and GST, payments, shopping, news and bookings. Absent: not offered.
    pub partner: Option<agentos_partner::Config>,
    /// The skill library built by scripts/fetch-skills.sh (index.json and skills/). Defaults to
    /// skills/ in data_dir; without one, the skill tools are not offered.
    pub skills_dir: Option<PathBuf>,
    /// Tokens a skill excerpt may take: about a sixth of a 4B model's context.
    pub skill_budget: usize,
    /// Where Word, Excel, PowerPoint and PDF files are read from. Defaults to documents/ in
    /// data_dir.
    pub documents_dir: Option<PathBuf>,
    /// The apps over public APIs (weather, maps, currency, ...): where each service is, and the
    /// home place and country. `apps_enabled = false` leaves them out (a phone kept offline).
    pub apps: agentos_apps::Config,
    pub apps_enabled: bool,
    /// Remote MCP servers ([[mcp_servers]]): each one's tools are offered as connectors, behind
    /// the same tiers and approval key. A server that cannot be reached is skipped at boot.
    pub mcp_servers: Vec<McpServer>,
    /// Sign-in providers for `oauth begin` and `oauth finish` ([[oauth_providers]]).
    pub oauth_providers: Vec<OauthProvider>,
    /// The Android sandbox: "none", "sim" (in memory) or "waydroid".
    pub android: String,
    /// The user who owns the Wayland session; Waydroid runs as this user.
    pub android_user: String,
    /// The environment for Waydroid's commands.
    pub android_env: String,
    /// Close the sandbox when less than this much memory is free (MiB).
    pub android_min_free_mib: u64,
    /// Close the sandbox when memory pressure (some avg10, percent) is above this.
    pub android_max_psi: f32,
    /// Close the sandbox in the deep power stage.
    pub android_stop_in_deep: bool,
    /// How often the governor looks, in seconds.
    pub android_check_secs: u64,
    /// The proc filesystem, for meminfo and the pressure files.
    pub proc_root: PathBuf,
    /// Move the screen's workspace with the sandbox (sway): the Android window on workspace 2.
    pub android_sway: bool,
    /// The store's catalogue (TOML). Missing: an empty store.
    pub catalog: PathBuf,
    /// Where `store refresh` fetches a signed catalogue from (catalog.toml, catalog.json, catalog.sig).
    pub catalog_url: Option<String>,
    /// The owner's public key for those catalogues, as 64 hex digits.
    pub catalog_pubkey: Option<String>,
}

/// An OAuth provider the owner registered (for example a Google client of type Desktop app).
/// Its client secret, if any, stays in this file; tokens go to the vault.
#[derive(Debug, Clone, Deserialize)]
pub struct OauthProvider {
    pub name: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_redirect")]
    pub redirect: String,
}

fn default_redirect() -> String {
    "http://127.0.0.1:8765/oauth/callback".into()
}

/// One remote MCP server. `secret` names a vault entry (conn.<name>) holding its bearer token.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub secret: Option<String>,
    /// The tier of its tools: "low", "medium" or "high". Anything else is High.
    #[serde(default = "high_tier")]
    pub tier: String,
    /// Believe its readOnlyHint and destructiveHint annotations.
    #[serde(default)]
    pub trust_hints: bool,
    /// When not empty, only these remote tools are offered.
    #[serde(default)]
    pub allow: Vec<String>,
}

fn high_tier() -> String {
    "high".into()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            data_dir: PathBuf::from("./agentd-data"),
            listen: "127.0.0.1:8765".into(),
            allowed_origins: vec![],
            small_url: None,
            large_url: None,
            commercial_enabled: false,
            commercial_model: "claude-sonnet-5-5".into(),
            online: true,
            idle_secs: 120,
            anchor_secs: 600,
            utc_offset_minutes: 330,
            quiet_start_min: 22 * 60,
            quiet_end_min: 7 * 60,
            daily_budget: 5,
            power_dir: None,
            real_hardware: false,
            demo_connectors: true,
            kdf_mib: 32,
            phone: "sim".into(),
            leds_dir: PathBuf::from("/sys/class/leds"),
            phone_poll_secs: 2,
            dbus_monitor: None,
            session_user: None,
            phone_signal_poll_secs: 60,
            control_socket: None,
            approval_key: None,
            approval_keycode: crate::button::KEY_VOLUMEDOWN,
            approval_hold_ms: 2000,
            llama_server: PathBuf::from("/usr/bin/llama-server"),
            models_dir: None,
            models: vec![],
            keep_small_loaded: true,
            model_wait_secs: 20,
            reserve_cpus: vec![],
            hot_c: 80.0,
            cool_c: 70.0,
            coder_attempts: 3,
            coder_timeout_secs: 300,
            rustc: PathBuf::from("rustc"),
            model_timeout_secs: 180,
            train_enabled: true,
            train_min_new: 20,
            train_min_battery: 50,
            train_steps: 60,
            train_chunk_secs: 1800,
            train_nightly: true,
            partner: None,
            skills_dir: None,
            skill_budget: 700,
            documents_dir: None,
            apps: agentos_apps::Config::default(),
            apps_enabled: true,
            mcp_servers: Vec::new(),
            oauth_providers: Vec::new(),
            android: "none".into(),
            android_user: "user".into(),
            android_env: "XDG_RUNTIME_DIR=/run/user/10000 WAYLAND_DISPLAY=wayland-1".into(),
            android_min_free_mib: 1024,
            android_max_psi: 10.0,
            android_stop_in_deep: true,
            android_check_secs: 5,
            proc_root: PathBuf::from("/proc"),
            android_sway: true,
            catalog: PathBuf::from("/usr/share/agentd/catalog.toml"),
            catalog_url: None,
            catalog_pubkey: None,
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
    pub fn control_path(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.data_dir.join("control.sock"))
    }
    pub fn models_path(&self) -> PathBuf {
        self.models_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("models"))
    }
    pub fn load(path: &std::path::Path) -> Result<Config, String> {
        let t = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_empty_file_gives_defaults() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c.listen, "127.0.0.1:8765");
        assert!(!c.commercial_enabled);
        assert!(c.small_url.is_none());
        assert_eq!(c.phone, "sim");
        assert_eq!(
            c.control_path(),
            PathBuf::from("./agentd-data/control.sock")
        );
    }
    #[test]
    fn fields_override_defaults() {
        let c = Config::from_toml(
            "listen = \"127.0.0.1:9\"\nidle_secs = 5\nsmall_url = \"http://127.0.0.1:8080\"",
        )
        .unwrap();
        assert_eq!(c.listen, "127.0.0.1:9");
        assert_eq!(c.idle_secs, 5);
        assert_eq!(c.small_url.as_deref(), Some("http://127.0.0.1:8080"));
    }
    #[test]
    fn models_are_tables() {
        let c = Config::from_toml(
            "[[models]]\nname = \"qwen\"\nrole = \"small\"\nfile = \"q.gguf\"\nport = 8080\n",
        )
        .unwrap();
        assert_eq!(c.models.len(), 1);
        assert_eq!(c.models_path(), PathBuf::from("./agentd-data/models"));
    }
    #[test]
    fn the_shipped_phone_config_parses() {
        let c = Config::from_toml(include_str!("../../../os/postmarketos/agentd.toml")).unwrap();
        assert_eq!(c.phone, "modemmanager");
        assert!(c.real_hardware);
        assert!(
            !c.demo_connectors,
            "a real phone must not offer a messenger that sends nothing"
        );
        assert_eq!(c.models.len(), 2);
        assert!(c
            .models
            .iter()
            .all(|m| m.sha256.as_ref().is_some_and(|h| h.len() == 64)));
    }
    #[test]
    fn a_misspelt_type_is_an_error_not_a_default() {
        assert!(Config::from_toml("idle_secs = \"soon\"").is_err());
    }
}
