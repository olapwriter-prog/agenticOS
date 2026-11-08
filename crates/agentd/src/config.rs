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
    /// Vault key-derivation memory in MiB. Raise it on the phone; tests use 1.
    pub kdf_mib: u32,
    /// "sim" keeps a simulated phone in memory; "modemmanager" drives the real modem, Wi-Fi,
    /// LEDs and feedback through ModemManager, NetworkManager, feedbackd and sysfs.
    pub phone: String,
    /// Where the flash LED lives (real phone only).
    pub leds_dir: PathBuf,
    /// How often to look for incoming calls and texts.
    pub phone_poll_secs: u64,
    /// The control socket. Defaults to control.sock in data_dir.
    pub control_socket: Option<PathBuf>,

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
            kdf_mib: 32,
            phone: "sim".into(),
            leds_dir: PathBuf::from("/sys/class/leds"),
            phone_poll_secs: 2,
            control_socket: None,
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
