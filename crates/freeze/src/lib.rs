//! Pause-on-chat: freeze or thaw the agent and build slices with the cgroup v2 freezer.
use std::io;
use std::path::PathBuf;

pub const FREEZABLE: [&str; 2] = [
    "agentos.slice/agentos-agents.slice",
    "agentos.slice/agentos-build.slice",
];

pub struct Freezer {
    pub root: PathBuf,
}

impl Freezer {
    pub fn system() -> Self {
        Self {
            root: PathBuf::from("/sys/fs/cgroup"),
        }
    }

    pub fn set_frozen(&self, frozen: bool) -> io::Result<()> {
        let v = if frozen { "1" } else { "0" };
        for slice in FREEZABLE {
            std::fs::write(self.root.join(slice).join("cgroup.freeze"), v)?;
        }
        Ok(())
    }
    pub fn is_frozen(&self) -> io::Result<bool> {
        for slice in FREEZABLE {
            let s = std::fs::read_to_string(self.root.join(slice).join("cgroup.freeze"))?;
            if s.trim() != "1" {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("cg-{}-{}", tag, std::process::id()));
        for s in FREEZABLE {
            std::fs::create_dir_all(root.join(s)).unwrap();
        }
        root
    }
    #[test]
    fn freeze_then_thaw() {
        let root = fake_root("a");
        let f = Freezer { root: root.clone() };
        f.set_frozen(true).unwrap();
        assert!(f.is_frozen().unwrap());
        f.set_frozen(false).unwrap();
        assert!(!f.is_frozen().unwrap());
        std::fs::remove_dir_all(root).ok();
    }
    #[test]
    fn missing_slice_is_an_error_not_a_silent_pass() {
        let f = Freezer {
            root: std::env::temp_dir().join("does-not-exist-xyz"),
        };
        assert!(f.set_frozen(true).is_err());
    }
}
