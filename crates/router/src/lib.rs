//! The model router: which brain handles a job?
//! Commercial model for planning and stuck jobs, local models for everything else.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brain {
    LocalSmall,
    LocalLarge,
    Commercial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Label,
    Plan,
    Design,
    Forge,
    Unstick,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    Normal,
    /// Never leaves the phone, whatever the policy says.
    HumanOnly,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobKind,
    pub sensitivity: Sensitivity,
    pub online: bool,
    pub on_charger: bool,
    pub local_failures: u32,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub commercial_enabled: bool,
    pub max_local_failures: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            commercial_enabled: true,
            max_local_failures: 2,
        }
    }
}

fn local(job: &Job) -> Brain {
    if job.on_charger {
        Brain::LocalLarge
    } else {
        Brain::LocalSmall
    }
}

pub fn route(job: &Job, policy: &Policy) -> Brain {
    // Privacy and connectivity rules come first and cannot be overridden by job kind.
    if job.sensitivity == Sensitivity::HumanOnly || !job.online || !policy.commercial_enabled {
        return local(job);
    }
    let stuck = job.local_failures >= policy.max_local_failures;
    match job.kind {
        JobKind::Label => Brain::LocalSmall,
        JobKind::Plan => Brain::Commercial,
        JobKind::Design | JobKind::Forge | JobKind::Unstick if stuck => Brain::Commercial,
        JobKind::Design | JobKind::Forge => Brain::LocalLarge,
        JobKind::Unstick | JobKind::Chat => local(job),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: JobKind) -> Job {
        Job {
            kind,
            sensitivity: Sensitivity::Normal,
            online: true,
            on_charger: false,
            local_failures: 0,
        }
    }

    #[test]
    fn labels_stay_on_the_small_model() {
        let mut j = job(JobKind::Label);
        j.on_charger = true;
        assert_eq!(route(&j, &Policy::default()), Brain::LocalSmall);
    }
    #[test]
    fn planning_goes_to_the_commercial_model() {
        assert_eq!(
            route(&job(JobKind::Plan), &Policy::default()),
            Brain::Commercial
        );
    }
    #[test]
    fn human_only_never_leaves_even_for_planning() {
        let mut j = job(JobKind::Plan);
        j.sensitivity = Sensitivity::HumanOnly;
        assert_ne!(route(&j, &Policy::default()), Brain::Commercial);
    }
    #[test]
    fn offline_planning_falls_back_to_local() {
        let mut j = job(JobKind::Plan);
        j.online = false;
        assert_eq!(route(&j, &Policy::default()), Brain::LocalSmall);
    }
    #[test]
    fn commercial_switch_off_means_never() {
        let p = Policy {
            commercial_enabled: false,
            ..Policy::default()
        };
        let mut j = job(JobKind::Unstick);
        j.local_failures = 99;
        assert_ne!(route(&j, &p), Brain::Commercial);
    }
    #[test]
    fn design_uses_large_until_stuck_then_escalates() {
        let mut j = job(JobKind::Design);
        assert_eq!(route(&j, &Policy::default()), Brain::LocalLarge);
        j.local_failures = 2;
        assert_eq!(route(&j, &Policy::default()), Brain::Commercial);
    }
    #[test]
    fn chat_picks_size_by_charger() {
        let mut j = job(JobKind::Chat);
        assert_eq!(route(&j, &Policy::default()), Brain::LocalSmall);
        j.on_charger = true;
        assert_eq!(route(&j, &Policy::default()), Brain::LocalLarge);
    }
    /// Exhaustive: no combination of inputs sends a HumanOnly job to the commercial model.
    #[test]
    fn human_only_never_commercial_exhaustive() {
        let kinds = [
            JobKind::Label,
            JobKind::Plan,
            JobKind::Design,
            JobKind::Forge,
            JobKind::Unstick,
            JobKind::Chat,
        ];
        for kind in kinds {
            for online in [true, false] {
                for charger in [true, false] {
                    for fails in 0..5 {
                        for enabled in [true, false] {
                            let j = Job {
                                kind,
                                sensitivity: Sensitivity::HumanOnly,
                                online,
                                on_charger: charger,
                                local_failures: fails,
                            };
                            let p = Policy {
                                commercial_enabled: enabled,
                                max_local_failures: 2,
                            };
                            assert_ne!(route(&j, &p), Brain::Commercial);
                        }
                    }
                }
            }
        }
    }
}
