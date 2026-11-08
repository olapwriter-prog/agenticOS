//! Chipset priority: the phone's CPU, GPU and memory go to the model first.
//!
//! The LLM runs in its own cgroup, `agentos.slice/agentos-llm.slice`. While it is generating:
//!   - it gets the highest CPU weight on the system and its own fast cores (cpuset),
//!   - utilization clamping asks the scheduler for top frequency on those cores (uclamp),
//!   - the cpufreq policies of those cores and the GPU (devfreq) run at `performance`,
//!   - its memory is protected (`memory.min`) and kept out of swap.
//!
//! Two things always win over the model:
//!   - one core (the "reserved" core, normally the slowest) is never given to it, so the modem,
//!     call audio and this daemon keep running during a call;
//!   - temperature: above `hot_c` the boost is dropped and the model is capped to half its cores'
//!     time until the phone cools below `cool_c`.
//!
//! Every path is configurable so tests and the laptop simulator use a fake sysfs tree.
//! A knob the kernel does not have (for example uclamp) is skipped and listed in `missing`.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const LLM_SLICE: &str = "agentos.slice/agentos-llm.slice";
const TOP_SLICE: &str = "agentos.slice";

#[derive(Debug, Clone)]
pub struct SysPaths {
    /// The cgroup v2 mount, normally /sys/fs/cgroup.
    pub cgroup: PathBuf,
    /// /sys/devices/system/cpu
    pub cpu: PathBuf,
    /// /sys/class/devfreq
    pub devfreq: PathBuf,
    /// /sys/class/thermal
    pub thermal: PathBuf,
}

impl SysPaths {
    pub fn system() -> Self {
        SysPaths {
            cgroup: "/sys/fs/cgroup".into(),
            cpu: "/sys/devices/system/cpu".into(),
            devfreq: "/sys/class/devfreq".into(),
            thermal: "/sys/class/thermal".into(),
        }
    }
    pub fn under(root: &Path) -> Self {
        SysPaths {
            cgroup: root.join("cgroup"),
            cpu: root.join("cpu"),
            devfreq: root.join("devfreq"),
            thermal: root.join("thermal"),
        }
    }
}

/// Which cores are fast, and which the model may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub all: Vec<u32>,
    /// The cores with the highest capacity (all of them on a phone with identical cores).
    pub big: Vec<u32>,
    /// Never given to the model.
    pub reserved: Vec<u32>,
    /// big minus reserved; all minus reserved if that is empty.
    pub llm: Vec<u32>,
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Parse a kernel CPU list such as "0-3,6" or "4 5 6 7".
pub fn parse_cpus(s: &str) -> Vec<u32> {
    let mut out = BTreeSet::new();
    for part in s.split([',', ' ', '\n']).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(n) = part.parse() {
                    out.insert(n);
                }
            }
        }
    }
    out.into_iter().collect()
}

pub fn cpu_list(cpus: &[u32]) -> String {
    cpus.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

impl Topology {
    /// Read core capacities (arm64 `cpu_capacity`, else `cpuinfo_max_freq`).
    /// `reserve`: cores to keep from the model; empty means "the slowest core".
    pub fn detect(cpu_root: &Path, reserve: &[u32]) -> Topology {
        let mut caps: BTreeMap<u32, u64> = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(cpu_root) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Some(n) = name.strip_prefix("cpu").and_then(|n| n.parse::<u32>().ok()) else {
                    continue;
                };
                let p = e.path();
                let cap = read(&p.join("cpu_capacity"))
                    .or_else(|| read(&p.join("cpufreq/cpuinfo_max_freq")))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                caps.insert(n, cap);
            }
        }
        if caps.is_empty() {
            caps.insert(0, 1);
        }
        let all: Vec<u32> = caps.keys().copied().collect();
        let max = *caps.values().max().unwrap();
        let min = *caps.values().min().unwrap();
        let big: Vec<u32> = caps
            .iter()
            .filter(|(_, c)| **c == max)
            .map(|(n, _)| *n)
            .collect();
        let reserved: Vec<u32> = if !reserve.is_empty() {
            reserve
                .iter()
                .copied()
                .filter(|c| all.contains(c))
                .collect()
        } else if all.len() > 1 {
            // the first of the slowest cores
            vec![*caps.iter().find(|(_, c)| **c == min).unwrap().0]
        } else {
            vec![]
        };
        let not_reserved = |c: &&u32| !reserved.contains(c);
        let mut llm: Vec<u32> = big.iter().filter(not_reserved).copied().collect();
        if llm.is_empty() {
            llm = all.iter().filter(not_reserved).copied().collect();
        }
        if llm.is_empty() {
            llm = all.clone();
        }
        Topology {
            all,
            big,
            reserved,
            llm,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The model may be loaded but is not working.
    Idle,
    /// The model is answering someone: everything it can get.
    Inference,
    /// Deep stage on the charger: long jobs (agents, learning) get the chip.
    Training,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub hot_c: f32,
    pub cool_c: f32,
}
impl Default for Limits {
    fn default() -> Self {
        Limits {
            hot_c: 80.0,
            cool_c: 70.0,
        }
    }
}

pub struct Prio {
    pub paths: SysPaths,
    pub topo: Topology,
    pub limits: Limits,
    mode: Mode,
    hot: bool,
    /// Governors we replaced, to put back when the boost ends.
    saved: BTreeMap<PathBuf, String>,
    /// Knobs this kernel does not have.
    pub missing: BTreeSet<String>,
    reserve_bytes: u64,
}

impl Prio {
    pub fn new(paths: SysPaths, reserve_cpus: &[u32], limits: Limits) -> Prio {
        let topo = Topology::detect(&paths.cpu, reserve_cpus);
        Prio {
            paths,
            topo,
            limits,
            mode: Mode::Idle,
            hot: false,
            saved: BTreeMap::new(),
            missing: BTreeSet::new(),
            reserve_bytes: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn is_hot(&self) -> bool {
        self.hot
    }
    pub fn llm_dir(&self) -> PathBuf {
        self.paths.cgroup.join(LLM_SLICE)
    }
    /// How many threads the model should use: one per core it owns.
    pub fn threads(&self) -> usize {
        self.topo.llm.len().max(1)
    }

    fn write(&mut self, path: PathBuf, value: &str) -> bool {
        match std::fs::write(&path, value) {
            Ok(()) => true,
            Err(_) => {
                let name = path
                    .strip_prefix(&self.paths.cgroup)
                    .or_else(|_| path.strip_prefix(&self.paths.cpu))
                    .or_else(|_| path.strip_prefix(&self.paths.devfreq))
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                self.missing.insert(name);
                false
            }
        }
    }

    /// Create the slices, turn on the controllers and give the model its cores.
    pub fn setup(&mut self) -> Result<(), String> {
        let llm = self.llm_dir();
        std::fs::create_dir_all(&llm)
            .map_err(|e| format!("cannot create {}: {e}", llm.display()))?;
        // One write per controller: a kernel without one of them refuses the whole line.
        for dir in [self.paths.cgroup.clone(), self.paths.cgroup.join(TOP_SLICE)] {
            for c in ["+cpu", "+cpuset", "+memory"] {
                self.write(dir.join("cgroup.subtree_control"), c);
            }
        }
        let cpus = cpu_list(&self.topo.llm);
        self.write(llm.join("cpuset.cpus"), &cpus);
        self.write(llm.join("memory.swap.max"), "0");
        self.apply();
        Ok(())
    }

    /// Kill whatever is left in the model's cgroup, for example servers orphaned when agentd
    /// crashed (they would hold the ports). Uses `cgroup.kill` (Linux 5.14+). Only call this on
    /// the real cgroup tree, where the process list is the kernel's, not a stale file.
    pub fn evict(&mut self) -> bool {
        let kill = self.llm_dir().join("cgroup.kill");
        kill.exists() && self.write(kill, "1")
    }

    /// Put a process (a model server) in the model's cgroup.
    pub fn adopt(&mut self, pid: u32) -> bool {
        self.write(self.llm_dir().join("cgroup.procs"), &pid.to_string())
    }

    /// Protect this much memory for the model (the size of the loaded weights).
    pub fn reserve_memory(&mut self, bytes: u64) {
        self.reserve_bytes = bytes;
        self.write(self.llm_dir().join("memory.min"), &bytes.to_string());
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            self.mode = mode;
            self.apply();
        }
    }

    /// The hottest thermal zone, in degrees Celsius.
    pub fn temperature(&self) -> Option<f32> {
        let rd = std::fs::read_dir(&self.paths.thermal).ok()?;
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
            .filter_map(|e| read(&e.path().join("temp"))?.parse::<i64>().ok())
            // ignore sensors that report nonsense
            .filter(|t| (-40_000..=150_000).contains(t))
            .map(|t| t as f32 / 1000.0)
            .reduce(f32::max)
    }

    /// Call about once a second: watches the temperature, with hysteresis.
    pub fn tick(&mut self) {
        let Some(t) = self.temperature() else { return };
        let hot = if self.hot {
            t > self.limits.cool_c
        } else {
            t >= self.limits.hot_c
        };
        if hot != self.hot {
            self.hot = hot;
            self.apply();
        }
    }

    fn boosting(&self) -> bool {
        self.mode != Mode::Idle && !self.hot
    }

    /// Write every knob for the current mode and temperature.
    fn apply(&mut self) {
        let top = self.paths.cgroup.join(TOP_SLICE);
        let llm = self.llm_dir();
        let working = self.mode != Mode::Idle;
        let weight = if working { "10000" } else { "100" };
        self.write(top.join("cpu.weight"), weight);
        self.write(llm.join("cpu.weight"), weight);
        let boost = self.boosting();
        self.write(llm.join("cpu.uclamp.min"), if boost { "max" } else { "0" });
        let cap = if self.hot {
            // half of the model's cores' time, per 100 ms period
            format!("{} 100000", self.topo.llm.len() as u64 * 50_000)
        } else {
            "max 100000".to_string()
        };
        self.write(llm.join("cpu.max"), &cap);
        let targets = self.boost_targets();
        if boost {
            for gov in targets {
                if !self.saved.contains_key(&gov) {
                    if let Some(old) = read(&gov) {
                        self.saved.insert(gov.clone(), old);
                    }
                }
                self.write(gov, "performance");
            }
        } else {
            for (gov, old) in std::mem::take(&mut self.saved) {
                self.write(gov, &old);
            }
        }
    }

    /// The governor files to raise: cpufreq policies covering the model's cores, and the GPU.
    fn boost_targets(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.paths.cpu.join("cpufreq")) {
            let mut policies: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .starts_with("policy")
                })
                .collect();
            policies.sort();
            for p in policies {
                let cpus = read(&p.join("related_cpus"))
                    .or_else(|| read(&p.join("affected_cpus")))
                    .map(|s| parse_cpus(&s))
                    .unwrap_or_default();
                if cpus.iter().any(|c| self.topo.llm.contains(c)) {
                    out.push(p.join("scaling_governor"));
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(&self.paths.devfreq) {
            let mut gpus: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase();
                    n.contains("gpu")
                        || n.contains("mali")
                        || n.contains("adreno")
                        || n.contains("kgsl")
                })
                .collect();
            gpus.sort();
            out.extend(gpus.into_iter().map(|p| p.join("governor")));
        }
        out
    }

    pub fn report(&self) -> String {
        let temp = self
            .temperature()
            .map(|t| format!("{t:.1}C"))
            .unwrap_or_else(|| "unknown".into());
        let mut s = format!(
            "mode {:?}{}, model cores [{}] of [{}] (fast [{}], reserved [{}]), {} threads, temp {temp}",
            self.mode,
            if self.hot { " (too hot: boost off, capped)" } else { "" },
            cpu_list(&self.topo.llm),
            cpu_list(&self.topo.all),
            cpu_list(&self.topo.big),
            cpu_list(&self.topo.reserved),
            self.threads(),
        );
        if self.reserve_bytes > 0 {
            s.push_str(&format!(
                ", {} MiB memory protected",
                self.reserve_bytes >> 20
            ));
        }
        if !self.missing.is_empty() {
            s.push_str(&format!(
                "\nnot supported here: {}",
                self.missing.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake sysfs: 4 little cores at 400 and 4 big at 1024, two cpufreq policies, a GPU, a sensor.
    fn tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("prio-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = SysPaths::under(&root);
        for i in 0..8 {
            let d = p.cpu.join(format!("cpu{i}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cpu_capacity"), if i < 4 { "400" } else { "1024" }).unwrap();
        }
        for (pol, cpus) in [("policy0", "0 1 2 3"), ("policy4", "4 5 6 7")] {
            let d = p.cpu.join("cpufreq").join(pol);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("related_cpus"), cpus).unwrap();
            std::fs::write(d.join("scaling_governor"), "schedutil").unwrap();
        }
        let g = p.devfreq.join("5000000.gpu");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("governor"), "simple_ondemand").unwrap();
        std::fs::create_dir_all(p.devfreq.join("soc:bus")).unwrap();
        let z = p.thermal.join("thermal_zone0");
        std::fs::create_dir_all(&z).unwrap();
        std::fs::write(z.join("temp"), "45000").unwrap();
        std::fs::create_dir_all(&p.cgroup).unwrap();
        root
    }
    fn get(p: &Path) -> String {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    #[test]
    fn cpu_lists_parse_both_kernel_formats() {
        assert_eq!(parse_cpus("0-3,6"), vec![0, 1, 2, 3, 6]);
        assert_eq!(parse_cpus("4 5 6 7\n"), vec![4, 5, 6, 7]);
        assert_eq!(cpu_list(&[4, 5]), "4,5");
    }
    #[test]
    fn big_little_gives_the_model_the_big_cores_and_reserves_a_little_one() {
        let root = tree("topo");
        let t = Topology::detect(&SysPaths::under(&root).cpu, &[]);
        assert_eq!(t.big, vec![4, 5, 6, 7]);
        assert_eq!(t.reserved, vec![0]);
        assert_eq!(t.llm, vec![4, 5, 6, 7]);
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn identical_cores_still_keep_one_back_for_calls() {
        let root = std::env::temp_dir().join(format!("prio-same-{}", std::process::id()));
        for i in 0..4 {
            std::fs::create_dir_all(root.join(format!("cpu{i}"))).unwrap();
        }
        let t = Topology::detect(&root, &[]);
        assert_eq!(t.llm, vec![1, 2, 3]);
        assert_eq!(Topology::detect(&root, &[3]).llm, vec![0, 1, 2]);
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn inference_boosts_and_idle_puts_everything_back() {
        let root = tree("boost");
        let paths = SysPaths::under(&root);
        let mut p = Prio::new(paths.clone(), &[], Limits::default());
        p.setup().unwrap();
        let llm = p.llm_dir();
        assert_eq!(get(&llm.join("cpuset.cpus")), "4,5,6,7");
        // a plain file keeps the last write; the kernel's file accumulates them
        assert_eq!(get(&paths.cgroup.join("cgroup.subtree_control")), "+memory");
        assert!(p.adopt(4242));
        assert_eq!(get(&llm.join("cgroup.procs")), "4242");

        p.set_mode(Mode::Inference);
        assert_eq!(get(&llm.join("cpu.weight")), "10000");
        assert_eq!(get(&llm.join("cpu.uclamp.min")), "max");
        let big = paths.cpu.join("cpufreq/policy4/scaling_governor");
        let little = paths.cpu.join("cpufreq/policy0/scaling_governor");
        let gpu = paths.devfreq.join("5000000.gpu/governor");
        assert_eq!(get(&big), "performance");
        assert_eq!(
            get(&little),
            "schedutil",
            "the reserved cluster is not touched"
        );
        assert_eq!(get(&gpu), "performance");
        assert!(
            !paths.devfreq.join("soc:bus/governor").exists(),
            "only GPUs"
        );

        p.set_mode(Mode::Idle);
        assert_eq!(get(&llm.join("cpu.weight")), "100");
        assert_eq!(get(&big), "schedutil");
        assert_eq!(get(&gpu), "simple_ondemand");
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn heat_drops_the_boost_caps_the_model_and_waits_to_cool() {
        let root = tree("hot");
        let paths = SysPaths::under(&root);
        let mut p = Prio::new(paths.clone(), &[], Limits::default());
        p.setup().unwrap();
        p.set_mode(Mode::Inference);
        let zone = paths.thermal.join("thermal_zone0/temp");
        let gpu = paths.devfreq.join("5000000.gpu/governor");
        std::fs::write(&zone, "82000").unwrap();
        p.tick();
        assert!(p.is_hot());
        assert_eq!(get(&p.llm_dir().join("cpu.max")), "200000 100000");
        assert_eq!(get(&p.llm_dir().join("cpu.uclamp.min")), "0");
        assert_eq!(get(&gpu), "simple_ondemand");
        std::fs::write(&zone, "75000").unwrap();
        p.tick();
        assert!(p.is_hot(), "75 is not cool enough yet");
        std::fs::write(&zone, "69000").unwrap();
        p.tick();
        assert!(!p.is_hot());
        assert_eq!(get(&p.llm_dir().join("cpu.max")), "max 100000");
        assert_eq!(get(&gpu), "performance");
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn a_kernel_without_a_knob_is_reported_not_fatal() {
        let root = tree("missing");
        let paths = SysPaths::under(&root);
        let mut p = Prio::new(paths, &[], Limits::default());
        p.setup().unwrap();
        // a read-only directory where a file should be: the write fails
        let knob = p.llm_dir().join("cpu.uclamp.min");
        std::fs::remove_file(&knob).ok();
        std::fs::create_dir_all(&knob).unwrap();
        p.set_mode(Mode::Inference);
        assert!(p
            .report()
            .contains("not supported here: agentos.slice/agentos-llm.slice/cpu.uclamp.min"));
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn bogus_sensors_are_ignored() {
        let root = tree("bogus");
        let paths = SysPaths::under(&root);
        let z = paths.thermal.join("thermal_zone1");
        std::fs::create_dir_all(&z).unwrap();
        std::fs::write(z.join("temp"), "-273000").unwrap();
        let p = Prio::new(paths, &[], Limits::default());
        assert_eq!(p.temperature(), Some(45.0));
        std::fs::remove_dir_all(root).ok();
    }
}
