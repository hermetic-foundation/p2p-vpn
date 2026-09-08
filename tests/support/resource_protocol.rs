use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 2;
pub const SAMPLE_SECONDS: u64 = 5;
pub const WATCHDOG_SECONDS: u64 = 2400;
pub const ENDPOINT_LOG_BYTES: u64 = 8 * 1024 * 1024;
pub const INFRASTRUCTURE_LOG_BYTES: u64 = 32 * 1024 * 1024;
pub const OBSERVATION_BYTES: u64 = 16 * 1024 * 1024;
pub const RUN_ALLOWANCE_BYTES: u64 =
    2 * ENDPOINT_LOG_BYTES + INFRASTRUCTURE_LOG_BYTES + OBSERVATION_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    Idle,
    Traffic,
    Recovery,
    Pressure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Public,
    Private,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Baseline,
    Current,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub repetition: u8,
    pub cell: u8,
    pub profile: Profile,
    pub workload: Workload,
    pub subjects: [Subject; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    StartTraffic,
    StopTraffic,
    DisconnectLanAndInfrastructure,
    RestoreInfrastructure,
    RenumberAndRestoreLan,
    ShapeAndStartTraffic,
    StopTrafficAndRelease,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    pub seconds: u64,
    pub action: Action,
    pub probe_each_sample: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Traffic {
    pub preload: u32,
    pub requests_per_second: u32,
    pub payload_bytes: u32,
    pub maximum_requests: u32,
    pub seconds: u32,
}

impl Traffic {
    pub fn offered_count_valid(self, sent: u64) -> bool {
        (u64::from(self.maximum_requests) * 98 / 100..=u64::from(self.maximum_requests))
            .contains(&sent)
    }
}

pub fn matrix() -> Vec<Pair> {
    let mut pairs = Vec::with_capacity(24);
    for repetition in 0..3 {
        let mut cell = 0;
        for profile in [Profile::Public, Profile::Private] {
            for workload in [
                Workload::Idle,
                Workload::Traffic,
                Workload::Recovery,
                Workload::Pressure,
            ] {
                pairs.push(Pair {
                    repetition: repetition + 1,
                    cell,
                    profile,
                    workload,
                    subjects: if (cell + repetition) % 2 == 0 {
                        [Subject::Baseline, Subject::Current]
                    } else {
                        [Subject::Current, Subject::Baseline]
                    },
                });
                cell += 1;
            }
        }
    }
    pairs
}

impl Workload {
    pub fn stages(self) -> Vec<Stage> {
        let mut stages = vec![
            Stage {
                name: "startup".to_owned(),
                seconds: 120,
                action: Action::None,
                probe_each_sample: true,
            },
            Stage {
                name: "warmup".to_owned(),
                seconds: 100,
                action: Action::None,
                probe_each_sample: false,
            },
        ];
        let schedule: &[(&str, u64, Action, bool)] = match self {
            Self::Idle => &[("idle", 300, Action::None, false)],
            Self::Traffic => &[
                ("traffic", 180, Action::StartTraffic, false),
                ("drain", 100, Action::StopTraffic, false),
                ("post_load", 180, Action::None, false),
            ],
            Self::Recovery => &[
                ("outage", 130, Action::DisconnectLanAndInfrastructure, true),
                ("relay_recovery", 960, Action::RestoreInfrastructure, true),
                ("direct_recovery", 375, Action::RenumberAndRestoreLan, true),
                ("post_recovery", 180, Action::None, true),
            ],
            Self::Pressure => &[
                ("pressure", 60, Action::ShapeAndStartTraffic, false),
                ("drain", 100, Action::StopTrafficAndRelease, false),
                ("post_release", 180, Action::None, false),
            ],
        };
        stages.extend(schedule.iter().map(|(name, seconds, action, probe)| Stage {
            name: (*name).to_owned(),
            seconds: *seconds,
            action: *action,
            probe_each_sample: *probe,
        }));
        stages
    }

    pub fn traffic(self) -> Option<Traffic> {
        match self {
            Self::Traffic => Some(Traffic {
                preload: 3,
                requests_per_second: 50,
                payload_bytes: 512,
                maximum_requests: 9000,
                seconds: 180,
            }),
            Self::Pressure => Some(Traffic {
                preload: 3,
                requests_per_second: 200,
                payload_bytes: 1000,
                maximum_requests: 12000,
                seconds: 60,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_contains_three_pairs_per_cell_with_balanced_first_subjects() {
        let pairs = matrix();
        assert_eq!(pairs.len(), 24);
        assert_eq!(
            pairs
                .iter()
                .filter(|pair| pair.subjects[0] == Subject::Baseline)
                .count(),
            12
        );
        for cell in 0..8 {
            let found: Vec<_> = pairs.iter().filter(|pair| pair.cell == cell).collect();
            assert_eq!(
                found.iter().map(|pair| pair.repetition).collect::<Vec<_>>(),
                [1, 2, 3]
            );
            assert_ne!(found[0].subjects[0], found[1].subjects[0]);
            assert_eq!(found[0].subjects[0], found[2].subjects[0]);
            assert_eq!(
                found[0].profile,
                if cell < 4 {
                    Profile::Public
                } else {
                    Profile::Private
                }
            );
        }
        let encoded = serde_json::to_vec(&pairs).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<Pair>>(&encoded).unwrap(),
            pairs
        );
    }

    #[test]
    fn schedules_match_frozen_windows_and_leave_watchdog_headroom() {
        for (workload, durations) in [
            (Workload::Idle, vec![120, 100, 300]),
            (Workload::Traffic, vec![120, 100, 180, 100, 180]),
            (Workload::Recovery, vec![120, 100, 130, 960, 375, 180]),
            (Workload::Pressure, vec![120, 100, 60, 100, 180]),
        ] {
            let stages = workload.stages();
            assert_eq!(
                stages.iter().map(|stage| stage.seconds).collect::<Vec<_>>(),
                durations
            );
            assert!(
                stages
                    .iter()
                    .all(|stage| stage.seconds % SAMPLE_SECONDS == 0)
            );
            assert!(
                stages.iter().map(|stage| stage.seconds).sum::<u64>() + 2 * 10 + 60
                    < WATCHDOG_SECONDS
            );
            assert_eq!(stages[0].name, "startup");
            assert_eq!(stages[1].name, "warmup");
        }
        assert!(!Workload::Idle.stages()[2].probe_each_sample);
        assert!(
            Workload::Recovery.stages()[2..]
                .iter()
                .all(|stage| stage.probe_each_sample)
        );
    }

    #[test]
    fn actions_preserve_fault_and_release_order() {
        let actions = |workload: Workload| {
            workload
                .stages()
                .into_iter()
                .map(|stage| stage.action)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actions(Workload::Recovery),
            [
                Action::None,
                Action::None,
                Action::DisconnectLanAndInfrastructure,
                Action::RestoreInfrastructure,
                Action::RenumberAndRestoreLan,
                Action::None
            ]
        );
        assert_eq!(
            actions(Workload::Pressure),
            [
                Action::None,
                Action::None,
                Action::ShapeAndStartTraffic,
                Action::StopTrafficAndRelease,
                Action::None
            ]
        );
        assert_eq!(
            actions(Workload::Traffic),
            [
                Action::None,
                Action::None,
                Action::StartTraffic,
                Action::StopTraffic,
                Action::None
            ]
        );
    }

    #[test]
    fn offered_work_and_storage_are_finite() {
        assert_eq!(VERSION, 2);
        for workload in [Workload::Traffic, Workload::Pressure] {
            let traffic = workload.traffic().unwrap();
            assert_eq!(traffic.preload, 3);
            assert_eq!(
                traffic.requests_per_second * traffic.seconds,
                traffic.maximum_requests
            );
            assert_eq!(u64::from(traffic.seconds), workload.stages()[2].seconds);
        }
        assert_eq!(Workload::Idle.traffic(), None);
        assert_eq!(Workload::Recovery.traffic(), None);
        assert_eq!(RUN_ALLOWANCE_BYTES, 64 * 1024 * 1024);
        assert_eq!(
            matrix().len() as u64 * 2 * RUN_ALLOWANCE_BYTES,
            3 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn offered_rate_gate_rejects_underdriving_and_excess_packets() {
        let traffic = Workload::Pressure.traffic().unwrap();
        assert!(!traffic.offered_count_valid(5956));
        assert!(!traffic.offered_count_valid(11759));
        assert!(traffic.offered_count_valid(11760));
        assert!(traffic.offered_count_valid(12000));
        assert!(!traffic.offered_count_valid(12001));
        assert!(!traffic.offered_count_valid(u64::MAX));
    }
}
