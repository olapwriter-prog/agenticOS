//! The rule that decides when the Android sandbox must close so the OS has what it needs. This
//! file is pure logic: it reads two files and returns a decision. It stops nothing itself.
use std::path::Path;

/// What the OS can see of its memory right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pressure {
    /// MemAvailable from meminfo, in MiB.
    pub available_mib: u64,
    /// The "some avg10" memory pressure, in percent of recent time stalled.
    pub psi: f32,
}

impl Pressure {
    /// Read `<proc_root>/meminfo` and `<proc_root>/pressure/memory`. A missing pressure file reads
    /// as 0.0, because not every kernel has pressure stall information.
    pub fn read(proc_root: &Path) -> Pressure {
        let meminfo = std::fs::read_to_string(proc_root.join("meminfo")).unwrap_or_default();
        let psi_text =
            std::fs::read_to_string(proc_root.join("pressure/memory")).unwrap_or_default();
        Pressure {
            available_mib: parse_meminfo(&meminfo),
            psi: parse_psi(&psi_text),
        }
    }
}

/// MemAvailable, in MiB. Zero when the line is missing.
pub fn parse_meminfo(text: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

/// The "some avg10" value from a pressure file. Zero when it is missing.
pub fn parse_psi(text: &str) -> f32 {
    text.lines()
        .find(|l| l.starts_with("some "))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("avg10=")))
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub min_free_mib: u64,
    pub max_psi: f32,
    pub stop_in_deep: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            min_free_mib: 1024,
            max_psi: 10.0,
            stop_in_deep: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    LowMemory(u64),
    Pressure(f32),
    DeepStage,
    HeavyJob(&'static str),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::LowMemory(mib) => write!(f, "only {mib} MiB free"),
            Reason::Pressure(p) => write!(f, "memory pressure {p:.1}%"),
            Reason::DeepStage => write!(f, "the phone is in its deep power stage"),
            Reason::HeavyJob(job) => write!(f, "the phone needs memory for {job}"),
        }
    }
}

/// Why the sandbox must close now, or None. The checks run in this order: free memory, pressure,
/// the deep stage, then a heavy job the OS is running.
pub fn decide(
    p: &Pressure,
    deep: bool,
    heavy_job: Option<&'static str>,
    pol: &Policy,
) -> Option<Reason> {
    if p.available_mib < pol.min_free_mib {
        return Some(Reason::LowMemory(p.available_mib));
    }
    if p.psi > pol.max_psi {
        return Some(Reason::Pressure(p.psi));
    }
    if deep && pol.stop_in_deep {
        return Some(Reason::DeepStage);
    }
    heavy_job.map(Reason::HeavyJob)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:        8000000 kB\nMemFree:         300000 kB\nMemAvailable:   2097152 kB\nBuffers:          1000 kB\n";
    const PSI: &str = "some avg10=2.50 avg60=1.00 avg300=0.50 total=12345\nfull avg10=0.10 avg60=0.00 avg300=0.00 total=99\n";

    fn ok() -> Pressure {
        Pressure {
            available_mib: 2048,
            psi: 1.0,
        }
    }

    #[test]
    fn the_sample_files_parse() {
        assert_eq!(parse_meminfo(MEMINFO), 2048);
        assert_eq!(parse_psi(PSI), 2.5);
        assert_eq!(parse_meminfo(""), 0);
    }

    #[test]
    fn a_missing_pressure_file_reads_as_zero_and_is_not_an_error() {
        let root = std::env::temp_dir().join(format!("gov-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("meminfo"), MEMINFO).unwrap();
        let p = Pressure::read(&root);
        assert_eq!(
            p,
            Pressure {
                available_mib: 2048,
                psi: 0.0
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn each_reason_is_decided_in_turn() {
        let pol = Policy::default();
        assert_eq!(decide(&ok(), false, None, &pol), None);
        assert_eq!(
            decide(
                &Pressure {
                    available_mib: 500,
                    psi: 1.0
                },
                false,
                None,
                &pol
            ),
            Some(Reason::LowMemory(500))
        );
        assert_eq!(
            decide(
                &Pressure {
                    available_mib: 2048,
                    psi: 15.0
                },
                false,
                None,
                &pol
            ),
            Some(Reason::Pressure(15.0))
        );
        assert_eq!(decide(&ok(), true, None, &pol), Some(Reason::DeepStage));
        assert_eq!(
            decide(&ok(), false, Some("training"), &pol),
            Some(Reason::HeavyJob("training"))
        );
    }

    #[test]
    fn the_deep_stage_is_ignored_when_the_policy_says_so() {
        let pol = Policy {
            stop_in_deep: false,
            ..Policy::default()
        };
        assert_eq!(decide(&ok(), true, None, &pol), None);
    }

    #[test]
    fn low_memory_is_reported_before_pressure() {
        let pol = Policy::default();
        let both = Pressure {
            available_mib: 100,
            psi: 50.0,
        };
        assert_eq!(
            decide(&both, true, Some("training"), &pol),
            Some(Reason::LowMemory(100))
        );
    }
}
