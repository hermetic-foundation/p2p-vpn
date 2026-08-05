use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, autonat, core::ConnectedPoint, dcutr, identify, kad,
    multiaddr::Protocol, relay, swarm::SwarmEvent,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        Config, ConfigError, DiscoveryConfig, InterfaceConfig, NetworkConfig, PacketPlaneConfig,
        PeerConfig, QueueConfig, RelayConfig, ResourceConfig,
    },
    identity::{IdentityError, NodeIdentity},
    membership::{
        SignedMembershipRecord, merge_membership_records_at, trusted_membership_issuers_at,
    },
    runtime::{
        control::MAX_CONTROL_MEMBERSHIP_RECORDS,
        p2p::{
            BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node,
            kademlia_membership_records_key,
        },
    },
};

pub const PUBLIC_RELAY_CANDIDATE_LIMIT: usize = 8;
pub const PUBLIC_RELAY_SCAN_LIMIT: usize = 16;
pub const PUBLIC_RELAY_SCAN_CANDIDATE_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapCheckThreshold {
    Any,
    All,
}

#[derive(Debug)]
pub struct BootstrapCheckReport {
    pub threshold: BootstrapCheckThreshold,
    pub requirements: BootstrapCheckRequirements,
    pub kademlia_protocol: String,
    pub ipfs_compatible: bool,
    pub dcutr: BootstrapDcutrCheck,
    pub configured_bootstrap_peers: usize,
    pub connected_bootstrap_peers: usize,
    pub dial_failures: usize,
    pub configured_relay_reservations: usize,
    pub accepted_relay_reservations: usize,
    pub relayed_listen_addresses: usize,
    pub configured_relayed_peer_circuits: usize,
    pub connected_relayed_peer_circuits: usize,
    pub relayed_connection_addresses: Vec<String>,
    pub direct_connection_addresses: Vec<String>,
    pub autonat_probe_servers_registered: usize,
    pub autonat_status: BootstrapAutoNatStatus,
    pub kademlia: BootstrapKademliaCheck,
    pub membership_records: BootstrapMembershipRecordDhtCheck,
    pub peer_results: Vec<BootstrapPeerCheck>,
    pub relay_results: Vec<RelayReservationCheck>,
    pub relayed_peer_results: Vec<RelayedPeerCircuitCheck>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BootstrapCheckRequirements {
    pub relay_reservations: bool,
    pub autonat_status: bool,
    pub dcutr_ready: bool,
    pub dcutr_success: bool,
    pub relayed_peer_circuits: bool,
    pub membership_records: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BootstrapAutoNatStatus {
    #[default]
    Unknown,
    Public,
    Private,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapDcutrCheck {
    pub enabled: bool,
    pub ready: bool,
    pub successes: usize,
    pub direct_connections: usize,
    pub failures: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub struct BootstrapKademliaCheck {
    pub bootstrap_started: bool,
    pub rendezvous_lookup_started: bool,
    pub rendezvous_advertise_started: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapMembershipRecordDhtCheck {
    pub configured_records: usize,
    pub publish_started: bool,
    pub publish_succeeded: bool,
    pub publish_failures: usize,
    pub lookup_started: bool,
    pub found_records: usize,
    pub verified_records: usize,
    pub accepted_records: usize,
    pub invalid_records: usize,
    pub last_error: Option<String>,
}

impl BootstrapCheckReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        let has_bootstrap_work = self.configured_bootstrap_peers > 0;
        let bootstrap_ok = !has_bootstrap_work
            || match self.threshold {
                BootstrapCheckThreshold::Any => self.connected_bootstrap_peers > 0,
                BootstrapCheckThreshold::All => {
                    self.connected_bootstrap_peers == self.configured_bootstrap_peers
                }
            };
        let relay_ready = relay_reservations_ready(
            self.configured_relay_reservations,
            self.accepted_relay_reservations,
            self.relays_with_listen_addresses(),
        );
        let relay_ok = !self.requirements.relay_reservations || relay_ready;
        let autonat_ok = !self.requirements.autonat_status
            || (self.autonat_probe_servers_registered > 0 && self.autonat_status.is_observed());
        let dcutr_ok = !self.requirements.dcutr_ready || self.dcutr.ready;
        let dcutr_success_ok = !self.requirements.dcutr_success
            || (self.dcutr.enabled
                && self.dcutr.successes > 0
                && self.dcutr.direct_connections > 0);

        let relayed_peer_circuits_ok = !self.requirements.relayed_peer_circuits
            || (self.configured_relayed_peer_circuits > 0
                && self.connected_relayed_peer_circuits == self.configured_relayed_peer_circuits);
        let membership_records_ok = !self.requirements.membership_records
            || (self.membership_records.configured_records > 0
                && self.membership_records.publish_succeeded
                && self.membership_records.found_records > 0
                && self.membership_records.verified_records > 0);

        (has_bootstrap_work
            || self.requirements.relay_reservations
            || self.requirements.autonat_status
            || self.requirements.dcutr_ready
            || self.requirements.dcutr_success
            || self.requirements.relayed_peer_circuits
            || self.requirements.membership_records)
            && bootstrap_ok
            && relay_ok
            && autonat_ok
            && dcutr_ok
            && dcutr_success_ok
            && relayed_peer_circuits_ok
            && membership_records_ok
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "bootstrap check: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("success threshold: {}", self.threshold.as_str()),
            format!(
                "require relay reservations: {}",
                self.requirements.relay_reservations
            ),
            format!(
                "require autonat status: {}",
                self.requirements.autonat_status
            ),
            format!("require dcutr ready: {}", self.requirements.dcutr_ready),
            format!("require dcutr success: {}", self.requirements.dcutr_success),
            format!(
                "require relayed peer circuits: {}",
                self.requirements.relayed_peer_circuits
            ),
            format!(
                "require membership records: {}",
                self.requirements.membership_records
            ),
            format!("kademlia protocol: {}", self.kademlia_protocol),
            format!("ipfs compatible: {}", self.ipfs_compatible),
            format!("dcutr enabled: {}", self.dcutr.enabled),
            format!("dcutr ready: {}", self.dcutr.ready),
            format!(
                "dcutr readiness_reason: {}",
                dcutr_readiness_reason(self).as_str()
            ),
            format!("dcutr successes: {}", self.dcutr.successes),
            format!(
                "dcutr direct_connections: {}",
                self.dcutr.direct_connections
            ),
            format!(
                "dcutr success_reason: {}",
                dcutr_success_reason(self).as_str()
            ),
            format!("dcutr failures: {}", self.dcutr.failures),
            format!(
                "dcutr last_error: {}",
                self.dcutr.last_error.as_deref().unwrap_or("none")
            ),
            format!(
                "kademlia bootstrap started: {}",
                self.kademlia.bootstrap_started
            ),
            format!(
                "kademlia rendezvous lookup started: {}",
                self.kademlia.rendezvous_lookup_started
            ),
            format!(
                "kademlia rendezvous advertise started: {}",
                self.kademlia.rendezvous_advertise_started
            ),
            format!(
                "autonat probe servers registered: {}",
                self.autonat_probe_servers_registered
            ),
            format!("autonat status: {}", self.autonat_status.as_str()),
            format!(
                "bootstrap peers: {} connected {} dial_failures {}",
                self.configured_bootstrap_peers, self.connected_bootstrap_peers, self.dial_failures
            ),
            format!(
                "relay reservations: {} accepted {} relayed_listen_addresses {}",
                self.configured_relay_reservations,
                self.accepted_relay_reservations,
                self.relayed_listen_addresses
            ),
            format!(
                "relayed peer circuits: {} connected {}",
                self.configured_relayed_peer_circuits, self.connected_relayed_peer_circuits
            ),
        ];

        append_membership_record_lines(&mut lines, &self.membership_records);
        append_bootstrap_peer_lines(&mut lines, self);

        lines
    }

    fn relays_with_listen_addresses(&self) -> usize {
        self.relay_results
            .iter()
            .filter(|relay| relay.relayed_listen_address)
            .count()
    }
}

fn append_bootstrap_peer_lines(lines: &mut Vec<String>, report: &BootstrapCheckReport) {
    for address in &report.relayed_connection_addresses {
        lines.push(format!("relayed peer connection address: {address}"));
    }

    for address in &report.direct_connection_addresses {
        lines.push(format!("direct peer connection address: {address}"));
    }

    for peer in &report.peer_results {
        lines.push(format!(
            "bootstrap peer: {} connected {} dial_failures {} last_error {} address {}",
            peer.peer_id,
            peer.connected,
            peer.dial_failures,
            peer.last_error.as_deref().unwrap_or("none"),
            peer.address
        ));
    }

    for relay in &report.relay_results {
        lines.push(format!(
            "relay reservation: {} accepted {} relayed_listen_address {} address {}",
            relay.relay_peer_id, relay.accepted, relay.relayed_listen_address, relay.address
        ));
    }

    for peer in &report.relayed_peer_results {
        lines.push(format!(
            "relayed peer circuit: {} connected {} outbound_circuit {} dial_failures {} last_error {} address {}",
            peer.peer_id,
            peer.connected,
            peer.outbound_circuit,
            peer.dial_failures,
            peer.last_error.as_deref().unwrap_or("none"),
            peer.address
        ));
    }
}

fn append_membership_record_lines(
    lines: &mut Vec<String>,
    membership_records: &BootstrapMembershipRecordDhtCheck,
) {
    lines.extend([
        format!(
            "kademlia membership records configured: {}",
            membership_records.configured_records
        ),
        format!(
            "kademlia membership records publish started: {}",
            membership_records.publish_started
        ),
        format!(
            "kademlia membership records publish succeeded: {}",
            membership_records.publish_succeeded
        ),
        format!(
            "kademlia membership records publish failures: {}",
            membership_records.publish_failures
        ),
        format!(
            "kademlia membership records lookup started: {}",
            membership_records.lookup_started
        ),
        format!(
            "kademlia membership records found: {}",
            membership_records.found_records
        ),
        format!(
            "kademlia membership records verified: {}",
            membership_records.verified_records
        ),
        format!(
            "kademlia membership records accepted: {}",
            membership_records.accepted_records
        ),
        format!(
            "kademlia membership records invalid: {}",
            membership_records.invalid_records
        ),
        format!(
            "kademlia membership records last_error: {}",
            membership_records.last_error.as_deref().unwrap_or("none")
        ),
    ]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootstrapDcutrReadinessReason {
    Ready,
    Disabled,
    NoRelayReservationsConfigured,
    MissingRelayReservation,
    MissingRelayedListenAddress,
    IncompleteReadinessEvidence,
}

impl BootstrapDcutrReadinessReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disabled => "disabled",
            Self::NoRelayReservationsConfigured => "no_relay_reservations_configured",
            Self::MissingRelayReservation => "missing_relay_reservation",
            Self::MissingRelayedListenAddress => "missing_relayed_listen_address",
            Self::IncompleteReadinessEvidence => "incomplete_readiness_evidence",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootstrapDcutrSuccessReason {
    Ready,
    Disabled,
    NoHolePunchSuccess,
    MissingDirectConnectionEvidence,
}

impl BootstrapDcutrSuccessReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disabled => "disabled",
            Self::NoHolePunchSuccess => "no_hole_punch_success",
            Self::MissingDirectConnectionEvidence => "missing_direct_connection_evidence",
        }
    }
}

fn dcutr_readiness_reason(report: &BootstrapCheckReport) -> BootstrapDcutrReadinessReason {
    if report.dcutr.ready {
        return BootstrapDcutrReadinessReason::Ready;
    }
    if !report.dcutr.enabled {
        return BootstrapDcutrReadinessReason::Disabled;
    }
    if report.configured_relay_reservations == 0 {
        return BootstrapDcutrReadinessReason::NoRelayReservationsConfigured;
    }
    if report.accepted_relay_reservations < report.configured_relay_reservations {
        return BootstrapDcutrReadinessReason::MissingRelayReservation;
    }
    if report.relays_with_listen_addresses() < report.configured_relay_reservations {
        return BootstrapDcutrReadinessReason::MissingRelayedListenAddress;
    }
    BootstrapDcutrReadinessReason::IncompleteReadinessEvidence
}

fn dcutr_success_reason(report: &BootstrapCheckReport) -> BootstrapDcutrSuccessReason {
    if report.dcutr.enabled && report.dcutr.successes > 0 && report.dcutr.direct_connections > 0 {
        return BootstrapDcutrSuccessReason::Ready;
    }
    if !report.dcutr.enabled {
        return BootstrapDcutrSuccessReason::Disabled;
    }
    if report.dcutr.successes == 0 {
        return BootstrapDcutrSuccessReason::NoHolePunchSuccess;
    }
    BootstrapDcutrSuccessReason::MissingDirectConnectionEvidence
}

impl BootstrapCheckThreshold {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
}

impl BootstrapAutoNatStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    const fn is_observed(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

#[derive(Debug)]
pub struct BootstrapPeerCheck {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct RelayReservationCheck {
    pub relay_peer_id: Libp2pPeerId,
    pub address: String,
    pub accepted: bool,
    pub relayed_listen_address: bool,
}

#[derive(Debug)]
pub struct RelayedPeerCircuitCheck {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub outbound_circuit: bool,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicRelayProbeMode {
    RelayedPeerCircuit,
    DcutrSuccess,
}

#[derive(Debug)]
pub struct PublicRelayProbeReport {
    pub mode: PublicRelayProbeMode,
    pub candidates: Vec<PublicRelayCandidateReport>,
}

#[derive(Debug)]
pub struct PublicRelayCandidateReport {
    pub address: String,
    pub succeeded: bool,
    pub failure_stage: PublicRelayCandidateFailureStage,
    pub error: Option<String>,
    pub bootstrap: Option<BootstrapCheckReport>,
    pub elapsed_millis: u64,
}

impl PublicRelayCandidateReport {
    #[must_use]
    pub fn diagnosis(&self) -> PublicRelayCandidateDiagnosis {
        if self.succeeded {
            return PublicRelayCandidateDiagnosis::Success;
        }

        let Some(bootstrap) = &self.bootstrap else {
            return match self.failure_stage {
                PublicRelayCandidateFailureStage::CandidateSetup => {
                    PublicRelayCandidateDiagnosis::CandidateSetupFailed
                }
                _ => PublicRelayCandidateDiagnosis::UnknownFailure,
            };
        };

        match self.failure_stage {
            PublicRelayCandidateFailureStage::None => PublicRelayCandidateDiagnosis::UnknownFailure,
            PublicRelayCandidateFailureStage::CandidateSetup => {
                PublicRelayCandidateDiagnosis::CandidateSetupFailed
            }
            PublicRelayCandidateFailureStage::RelayReservation => {
                diagnose_relay_reservation_failure(bootstrap)
            }
            PublicRelayCandidateFailureStage::RelayedPeerCircuit => {
                diagnose_relayed_peer_circuit_failure(bootstrap)
            }
            PublicRelayCandidateFailureStage::DcutrSuccess => diagnose_dcutr_failure(bootstrap),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicRelayCandidateFailureStage {
    None,
    CandidateSetup,
    RelayReservation,
    RelayedPeerCircuit,
    DcutrSuccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicRelayCandidateDiagnosis {
    Success,
    CandidateSetupFailed,
    RelayReservationNotAccepted,
    RelayRelayedListenAddressMissing,
    RelayedPeerCircuitNotConnected,
    DcutrDisabled,
    DcutrNotReady,
    DcutrNoHolePunchSuccess,
    DcutrMissingDirectConnection,
    UnknownFailure,
}

impl PublicRelayCandidateDiagnosis {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::CandidateSetupFailed => "candidate_setup_failed",
            Self::RelayReservationNotAccepted => "relay_reservation_not_accepted",
            Self::RelayRelayedListenAddressMissing => "relay_relayed_listen_address_missing",
            Self::RelayedPeerCircuitNotConnected => "relayed_peer_circuit_not_connected",
            Self::DcutrDisabled => "dcutr_disabled",
            Self::DcutrNotReady => "dcutr_not_ready",
            Self::DcutrNoHolePunchSuccess => "dcutr_no_hole_punch_success",
            Self::DcutrMissingDirectConnection => "dcutr_missing_direct_connection",
            Self::UnknownFailure => "unknown_failure",
        }
    }
}

fn diagnose_relay_reservation_failure(
    bootstrap: &BootstrapCheckReport,
) -> PublicRelayCandidateDiagnosis {
    if bootstrap.accepted_relay_reservations < bootstrap.configured_relay_reservations {
        return PublicRelayCandidateDiagnosis::RelayReservationNotAccepted;
    }
    if bootstrap.relays_with_listen_addresses() < bootstrap.configured_relay_reservations {
        return PublicRelayCandidateDiagnosis::RelayRelayedListenAddressMissing;
    }
    PublicRelayCandidateDiagnosis::UnknownFailure
}

fn diagnose_relayed_peer_circuit_failure(
    bootstrap: &BootstrapCheckReport,
) -> PublicRelayCandidateDiagnosis {
    if bootstrap.connected_relayed_peer_circuits < bootstrap.configured_relayed_peer_circuits {
        return PublicRelayCandidateDiagnosis::RelayedPeerCircuitNotConnected;
    }
    PublicRelayCandidateDiagnosis::UnknownFailure
}

fn diagnose_dcutr_failure(bootstrap: &BootstrapCheckReport) -> PublicRelayCandidateDiagnosis {
    if !bootstrap.dcutr.enabled {
        return PublicRelayCandidateDiagnosis::DcutrDisabled;
    }
    if bootstrap.dcutr.successes == 0 {
        return PublicRelayCandidateDiagnosis::DcutrNoHolePunchSuccess;
    }
    if bootstrap.dcutr.direct_connections == 0 {
        return PublicRelayCandidateDiagnosis::DcutrMissingDirectConnection;
    }
    if !bootstrap.dcutr.ready {
        return PublicRelayCandidateDiagnosis::DcutrNotReady;
    }
    PublicRelayCandidateDiagnosis::UnknownFailure
}

#[derive(Debug)]
struct PublicRelayProbeFailure {
    stage: PublicRelayCandidateFailureStage,
    message: String,
    bootstrap: Option<BootstrapCheckReport>,
}

impl PublicRelayProbeFailure {
    fn without_bootstrap(message: impl Into<String>) -> Self {
        Self::at_stage(
            PublicRelayCandidateFailureStage::CandidateSetup,
            message.into(),
        )
    }

    fn at_stage(stage: PublicRelayCandidateFailureStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            bootstrap: None,
        }
    }

    fn with_bootstrap(
        stage: PublicRelayCandidateFailureStage,
        message: impl Into<String>,
        bootstrap: BootstrapCheckReport,
    ) -> Self {
        Self {
            stage,
            message: message.into(),
            bootstrap: Some(bootstrap),
        }
    }

    fn candidate_timeout(stage: PublicRelayCandidateFailureStage) -> Self {
        Self::at_stage(
            stage,
            format!(
                "candidate timeout exhausted before {}",
                stage.as_description()
            ),
        )
    }
}

#[derive(Debug)]
pub struct PublicRelayScanReport {
    pub scanned_bootstrap_peers: usize,
    pub scanned_peers: usize,
    pub discovered_routing_peers: usize,
    pub dialed_routing_peers: usize,
    pub closest_peer_lookup_started: bool,
    pub closest_peer_lookup_finished: bool,
    pub closest_peer_results: usize,
    pub closest_peer_errors: usize,
    pub connected_bootstrap_peers: usize,
    pub identified_peers: usize,
    pub relay_capable_peers: usize,
    pub dial_failures: usize,
    pub candidates: Vec<PublicRelayScanCandidate>,
    pub peer_results: Vec<PublicRelayScanPeer>,
}

#[derive(Debug)]
pub struct PublicRelayScanCandidate {
    pub peer_id: Libp2pPeerId,
    pub address: String,
}

#[derive(Debug)]
pub struct PublicRelayScanPeer {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub identified: bool,
    pub relay_hop: bool,
    pub candidate_addresses: usize,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PublicDcutrListenerDescriptor {
    pub schema_version: u8,
    pub relay_candidate: String,
    pub relay_peer: String,
    pub listener_peer: String,
    pub relayed_address: String,
    pub listen_addresses: Vec<String>,
    pub created_unix_seconds: u64,
}

pub struct PublicDcutrListener {
    descriptor: PublicDcutrListenerDescriptor,
    reservation_evidence: PublicDcutrReservationEvidence,
    node: P2pNode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicDcutrReservationEvidence {
    pub connected_to_relay: bool,
    pub reservation_accepted: bool,
    pub relayed_listen_address_observed: bool,
    pub listen_addresses: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicDcutrListenStartError {
    pub message: String,
    pub reservation_evidence: Option<PublicDcutrReservationEvidence>,
}

impl PublicDcutrListenStartError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reservation_evidence: None,
        }
    }

    fn with_reservation_evidence(
        message: impl Into<String>,
        reservation_evidence: PublicDcutrReservationEvidence,
    ) -> Self {
        Self {
            message: message.into(),
            reservation_evidence: Some(reservation_evidence),
        }
    }
}

impl std::fmt::Display for PublicDcutrListenStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PublicDcutrListenStartError {}

impl PublicDcutrReservationEvidence {
    fn from_listener(
        listener: &P2pNode,
        connected_to_relay: bool,
        reservation_accepted: bool,
        relayed_listen_address_observed: bool,
        last_error: Option<String>,
    ) -> Self {
        let mut listen_addresses = listener
            .swarm
            .listeners()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        listen_addresses.sort();
        Self {
            connected_to_relay,
            reservation_accepted,
            relayed_listen_address_observed,
            listen_addresses,
            last_error,
        }
    }

    fn error_message(&self) -> String {
        let last_error = self.last_error.as_deref().unwrap_or("none");
        format!(
            "relay reservation timed out connected {} accepted {} relayed_listen_address {} last_error {}",
            self.connected_to_relay,
            self.reservation_accepted,
            self.relayed_listen_address_observed,
            last_error
        )
    }
}

impl PublicDcutrListenerDescriptor {
    pub const SCHEMA_VERSION: u8 = 1;

    pub fn listener_peer_id(&self) -> Result<Libp2pPeerId, String> {
        self.listener_peer
            .parse()
            .map_err(|error| format!("invalid listener peer id: {error}"))
    }

    pub fn relayed_multiaddr(&self) -> Result<Multiaddr, String> {
        let address = self
            .relayed_address
            .parse::<Multiaddr>()
            .map_err(|error| format!("invalid relayed address: {error}"))?;
        if relay_peer_from_relayed_address(&address).is_none() {
            return Err("relayed address must include /p2p-circuit".to_owned());
        }
        let address_peer = relayed_target_peer(&address)
            .ok_or_else(|| "relayed address must include /p2p/LISTENER".to_owned())?;
        let listener_peer = self.listener_peer_id()?;
        if address_peer != listener_peer {
            return Err(format!(
                "relayed address peer {address_peer} does not match listener peer {listener_peer}"
            ));
        }
        Ok(address)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(format!(
                "unsupported public DCUtR listener descriptor schema {}",
                self.schema_version
            ));
        }
        let relay = self
            .relay_candidate
            .parse::<Multiaddr>()
            .map_err(|error| format!("invalid relay candidate: {error}"))?;
        let relay_peer = address_peer(&relay)
            .ok_or_else(|| "relay candidate must include /p2p/RELAY".to_owned())?;
        if relay_peer.to_string() != self.relay_peer {
            return Err(format!(
                "relay candidate peer {relay_peer} does not match relay peer {}",
                self.relay_peer
            ));
        }
        self.relayed_multiaddr()?;
        Ok(())
    }
}

impl PublicDcutrListener {
    #[must_use]
    pub const fn descriptor(&self) -> &PublicDcutrListenerDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub const fn reservation_evidence(&self) -> &PublicDcutrReservationEvidence {
        &self.reservation_evidence
    }

    pub async fn serve_for(mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout.max(Duration::from_millis(1));
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(()) = tokio::time::timeout(remaining, async {
                let _ = self.node.swarm.select_next_some().await;
            })
            .await
            else {
                break;
            };
        }
    }
}

impl PublicRelayProbeReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.candidates.iter().any(|candidate| candidate.succeeded)
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let succeeded = self
            .candidates
            .iter()
            .filter(|candidate| candidate.succeeded)
            .count();
        let mut lines = vec![
            format!(
                "public relay probe: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("public relay probe mode: {}", self.mode.as_str()),
            format!(
                "public relay candidates: {} succeeded {}",
                self.candidates.len(),
                succeeded
            ),
            format!(
                "public relay candidate failure stages: candidate_setup {} relay_reservation {} relayed_peer_circuit {} dcutr_success {}",
                self.failure_stage_count(PublicRelayCandidateFailureStage::CandidateSetup),
                self.failure_stage_count(PublicRelayCandidateFailureStage::RelayReservation),
                self.failure_stage_count(PublicRelayCandidateFailureStage::RelayedPeerCircuit),
                self.failure_stage_count(PublicRelayCandidateFailureStage::DcutrSuccess),
            ),
        ];

        for candidate in &self.candidates {
            lines.push(format!(
                "public relay candidate: {} succeeded {} failure_stage {} diagnosis {} error {}",
                candidate.address,
                candidate.succeeded,
                candidate.failure_stage.as_str(),
                candidate.diagnosis().as_str(),
                candidate.error.as_deref().unwrap_or("none")
            ));
            if candidate.succeeded
                && let Some(config) = public_relay_candidate_config_hint(&candidate.address)
            {
                lines.push(format!(
                    "public relay candidate config: relay_peer {} relay_reservation {}",
                    config.relay_peer_arg, config.relay_reservation
                ));
            }
            if let Some(report) = &candidate.bootstrap {
                for line in report.lines() {
                    lines.push(format!("public relay candidate detail: {line}"));
                }
            }
        }

        lines
    }

    fn failure_stage_count(&self, stage: PublicRelayCandidateFailureStage) -> usize {
        self.candidates
            .iter()
            .filter(|candidate| !candidate.succeeded && candidate.failure_stage == stage)
            .count()
    }
}

impl PublicRelayCandidateFailureStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CandidateSetup => "candidate_setup",
            Self::RelayReservation => "relay_reservation",
            Self::RelayedPeerCircuit => "relayed_peer_circuit",
            Self::DcutrSuccess => "dcutr_success",
        }
    }

    const fn as_description(self) -> &'static str {
        match self {
            Self::None => "candidate success",
            Self::CandidateSetup => "candidate setup",
            Self::RelayReservation => "relay reservation",
            Self::RelayedPeerCircuit => "relayed peer circuit check",
            Self::DcutrSuccess => "dcutr success check",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PublicRelayCandidateConfigHint {
    relay_peer_arg: String,
    relay_reservation: String,
}

fn public_relay_candidate_config_hint(address: &str) -> Option<PublicRelayCandidateConfigHint> {
    let address = address.parse::<Multiaddr>().ok()?;
    let relay_peer = address_peer(&address)?;
    Some(PublicRelayCandidateConfigHint {
        relay_peer_arg: format!("{relay_peer}={address}"),
        relay_reservation: address.with(Protocol::P2pCircuit).to_string(),
    })
}

impl PublicRelayScanReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.candidates.is_empty()
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "public relay scan: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("public relay scan peers: {}", self.scanned_bootstrap_peers),
            format!("public relay scan total_peers: {}", self.scanned_peers),
            format!(
                "public relay scan routing_peers: {} dialed {}",
                self.discovered_routing_peers, self.dialed_routing_peers
            ),
            format!(
                "public relay scan closest_peer_lookup: started {} finished {} results {} errors {}",
                self.closest_peer_lookup_started,
                self.closest_peer_lookup_finished,
                self.closest_peer_results,
                self.closest_peer_errors
            ),
            format!(
                "public relay scan connected: {}",
                self.connected_bootstrap_peers
            ),
            format!("public relay scan identified: {}", self.identified_peers),
            format!(
                "public relay scan relay_capable: {}",
                self.relay_capable_peers
            ),
            format!("public relay scan dial_failures: {}", self.dial_failures),
            format!("public relay candidates: {}", self.candidates.len()),
        ];

        for candidate in &self.candidates {
            lines.push(format!(
                "public relay candidate: {} peer {}",
                candidate.address, candidate.peer_id
            ));
        }

        for peer in &self.peer_results {
            lines.push(format!(
                "public relay scan peer: {} connected {} identified {} relay_hop {} candidate_addresses {} dial_failures {} last_error {} address {}",
                peer.peer_id,
                peer.connected,
                peer.identified,
                peer.relay_hop,
                peer.candidate_addresses,
                peer.dial_failures,
                peer.last_error.as_deref().unwrap_or("none"),
                peer.address
            ));
        }

        lines
    }
}

impl PublicRelayProbeMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RelayedPeerCircuit => "relayed_peer_circuit",
            Self::DcutrSuccess => "dcutr_success",
        }
    }
}

pub async fn check_config_bootstrap(
    config: &Config,
    timeout: Duration,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
) -> Result<BootstrapCheckReport, BootstrapCheckError> {
    config.validate_runtime()?;
    let mut node = build_node(&bootstrap_check_host_config(config)?)?;
    let bootstrap_peers = node.bootstrap_peer_addresses.clone();
    let relay_reservations = node.relay_peer_addresses.clone();
    let relayed_peers = relayed_peer_addresses(&node.configured_peer_addresses);
    let membership_tag = config.membership_tag()?;
    let previous_membership_tags = config.previous_membership_tags()?;
    let poll_context = BootstrapPollContext {
        config,
        membership_tag: membership_tag.as_deref(),
        previous_membership_tags: &previous_membership_tags,
        bootstrap_peers: &bootstrap_peers,
        relay_reservations: &relay_reservations,
        relayed_peers: &relayed_peers,
    };
    let poll =
        poll_bootstrap_events(&mut node, &poll_context, timeout, threshold, requirements).await;
    let dcutr_ready = node.startup.dcutr_enabled
        && relay_reservations_ready(
            relay_reservations.len(),
            poll.accepted_relay_reservations.len(),
            relays_with_listen_addresses(&relay_reservations, &poll.relayed_listen_addresses),
        );
    let dcutr_last_error = poll
        .dcutr_last_error
        .clone()
        .or_else(|| dcutr_direct_dial_last_error(requirements, &poll));

    Ok(BootstrapCheckReport {
        threshold,
        requirements,
        kademlia_protocol: node.discovery.kademlia_protocol.clone(),
        ipfs_compatible: node.discovery.kademlia_protocol == "/ipfs/kad/1.0.0",
        dcutr: BootstrapDcutrCheck {
            enabled: node.startup.dcutr_enabled,
            ready: dcutr_ready,
            successes: poll.dcutr_successes,
            direct_connections: poll.direct_connected_relayed_peers.len(),
            failures: poll.dcutr_failures,
            last_error: dcutr_last_error,
        },
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: poll.connected_bootstrap_peers.len(),
        dial_failures: poll.dial_failures.len(),
        configured_relay_reservations: relay_reservations.len(),
        accepted_relay_reservations: poll.accepted_relay_reservations.len(),
        relayed_listen_addresses: poll.relayed_listen_addresses.len(),
        configured_relayed_peer_circuits: relayed_peers.len(),
        connected_relayed_peer_circuits: poll.connected_relayed_peers.len(),
        relayed_connection_addresses: sorted_connection_addresses(&poll.connected_relayed_peers),
        direct_connection_addresses: sorted_connection_addresses(
            &poll.direct_connected_relayed_peers,
        ),
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: poll.autonat_status,
        kademlia: BootstrapKademliaCheck {
            bootstrap_started: node.startup.kademlia.bootstrap_started,
            rendezvous_lookup_started: node.startup.kademlia.rendezvous_lookup_started,
            rendezvous_advertise_started: node.startup.kademlia.rendezvous_advertise_started,
        },
        membership_records: poll.membership_records.clone(),
        peer_results: bootstrap_peer_results(bootstrap_peers, &poll),
        relay_results: relay_reservation_results(relay_reservations, &poll),
        relayed_peer_results: relayed_peer_results(relayed_peers, &poll),
    })
}

pub async fn scan_public_relay_candidates(
    config: &Config,
    timeout: Duration,
    max_candidates: usize,
) -> Result<PublicRelayScanReport, BootstrapCheckError> {
    config.validate_runtime()?;
    let mut node = build_node(&bootstrap_check_host_config(config)?)?;
    let bootstrap_peers = node.bootstrap_peer_addresses.clone();
    let result = poll_public_relay_scan_events(
        &mut node,
        &bootstrap_peers,
        timeout,
        max_candidates.min(PUBLIC_RELAY_SCAN_CANDIDATE_LIMIT),
    )
    .await;

    Ok(public_relay_scan_report(&bootstrap_peers, &result))
}

pub fn parse_public_relay_addresses(raw: &str) -> Result<Vec<Multiaddr>, String> {
    parse_public_relay_addresses_with_limit(raw, PUBLIC_RELAY_CANDIDATE_LIMIT)
}

pub fn parse_public_relay_addresses_with_limit(
    raw: &str,
    candidate_limit: usize,
) -> Result<Vec<Multiaddr>, String> {
    let mut addresses = Vec::new();
    for candidate in raw
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
    {
        if addresses.len() == candidate_limit {
            return Err(format!(
                "too many public relay candidates: maximum is {candidate_limit}"
            ));
        }
        let address = candidate
            .parse::<Multiaddr>()
            .map_err(|error| format!("{candidate}: {error}"))?;
        if address_peer(&address).is_none() {
            return Err(format!("{candidate}: missing /p2p/RELAY"));
        }
        if relay_peer_from_relayed_address(&address).is_some() {
            return Err(format!(
                "{candidate}: relay candidate must be the relay's direct address, without /p2p-circuit"
            ));
        }
        addresses.push(address);
    }
    Ok(addresses)
}

fn public_relay_scan_report(
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    poll: &PublicRelayScanPollResult,
) -> PublicRelayScanReport {
    let relay_capable_peers = poll
        .identified_peers
        .values()
        .filter(|peer| peer.relay_hop)
        .count();
    let candidates = poll
        .candidates
        .iter()
        .map(|(peer_id, address)| PublicRelayScanCandidate {
            peer_id: *peer_id,
            address: address.to_string(),
        })
        .collect();
    let peer_results = poll
        .scan_peers
        .iter()
        .map(|(peer_id, address)| {
            let identify = poll.identified_peers.get(peer_id);
            PublicRelayScanPeer {
                peer_id: *peer_id,
                address: address.to_string(),
                connected: poll.connected_bootstrap_peers.contains(peer_id),
                identified: identify.is_some(),
                relay_hop: identify.is_some_and(|identify| identify.relay_hop),
                candidate_addresses: identify.map_or(0, |identify| identify.candidate_addresses),
                dial_failures: poll
                    .dial_failures
                    .iter()
                    .filter(|(failed_peer, _)| failed_peer == peer_id)
                    .count(),
                last_error: poll
                    .dial_failures
                    .iter()
                    .rev()
                    .find_map(|(failed_peer, error)| {
                        (failed_peer == peer_id).then(|| error.clone())
                    }),
            }
        })
        .collect();

    PublicRelayScanReport {
        scanned_bootstrap_peers: bootstrap_peers.len(),
        scanned_peers: poll.scan_peers.len(),
        discovered_routing_peers: poll.discovered_routing_peers,
        dialed_routing_peers: poll.dialed_routing_peers,
        closest_peer_lookup_started: poll.closest_peer_lookup_started,
        closest_peer_lookup_finished: poll.closest_peer_lookup_finished,
        closest_peer_results: poll.closest_peer_results,
        closest_peer_errors: poll.closest_peer_errors,
        connected_bootstrap_peers: poll.connected_bootstrap_peers.len(),
        identified_peers: poll.identified_peers.len(),
        relay_capable_peers,
        dial_failures: poll.dial_failures.len(),
        candidates,
        peer_results,
    }
}

pub async fn check_public_relay_candidates(
    relay_addresses: &[Multiaddr],
    mode: PublicRelayProbeMode,
    timeout: Duration,
) -> PublicRelayProbeReport {
    let mut candidates = Vec::new();
    for relay_address in relay_addresses {
        let started = Instant::now();
        let result = match mode {
            PublicRelayProbeMode::RelayedPeerCircuit => {
                Box::pin(live_public_relayed_peer_circuit(relay_address, timeout)).await
            }
            PublicRelayProbeMode::DcutrSuccess => {
                Box::pin(live_public_dcutr_success(relay_address, timeout)).await
            }
        };
        let elapsed_millis = elapsed_millis_since(started);
        let candidate = match result {
            Ok(report) => PublicRelayCandidateReport {
                address: relay_address.to_string(),
                succeeded: true,
                failure_stage: PublicRelayCandidateFailureStage::None,
                error: None,
                bootstrap: Some(report),
                elapsed_millis,
            },
            Err(failure) => PublicRelayCandidateReport {
                address: relay_address.to_string(),
                succeeded: false,
                failure_stage: failure.stage,
                error: Some(failure.message),
                bootstrap: failure.bootstrap,
                elapsed_millis,
            },
        };
        let succeeded = candidate.succeeded;
        candidates.push(candidate);
        if succeeded {
            break;
        }
    }

    PublicRelayProbeReport { mode, candidates }
}

fn elapsed_millis_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn bootstrap_peer_results(
    bootstrap_peers: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<BootstrapPeerCheck> {
    bootstrap_peers
        .into_iter()
        .map(|(peer_id, address)| BootstrapPeerCheck {
            peer_id,
            address: address.to_string(),
            connected: poll.connected_bootstrap_peers.contains(&peer_id),
            dial_failures: poll
                .dial_failures
                .iter()
                .filter(|(failed_peer, _)| *failed_peer == peer_id)
                .count(),
            last_error: poll
                .dial_failures
                .iter()
                .rev()
                .find_map(|(failed_peer, error)| (*failed_peer == peer_id).then(|| error.clone())),
        })
        .collect()
}

fn relay_reservation_results(
    relay_reservations: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<RelayReservationCheck> {
    relay_reservations
        .into_iter()
        .map(|(relay_peer_id, address)| RelayReservationCheck {
            relay_peer_id,
            address: address.to_string(),
            accepted: poll.accepted_relay_reservations.contains(&relay_peer_id),
            relayed_listen_address: poll.relayed_listen_addresses.contains_key(&relay_peer_id),
        })
        .collect()
}

fn relayed_peer_results(
    relayed_peers: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<RelayedPeerCircuitCheck> {
    relayed_peers
        .into_iter()
        .map(|(peer_id, address)| {
            let relay_peer = relay_peer_from_relayed_address(&address);
            RelayedPeerCircuitCheck {
                peer_id,
                address: address.to_string(),
                connected: poll.connected_relayed_peers.contains_key(&peer_id),
                outbound_circuit: relay_peer.is_some_and(|relay| {
                    poll.outbound_circuit_relays.contains(&relay)
                        || poll.connected_relayed_peers.contains_key(&peer_id)
                }),
                dial_failures: poll
                    .relayed_peer_dial_failures
                    .iter()
                    .filter(|(failed_peer, _)| *failed_peer == peer_id)
                    .count(),
                last_error: poll.relayed_peer_dial_failures.iter().rev().find_map(
                    |(failed_peer, error)| (*failed_peer == peer_id).then(|| error.clone()),
                ),
            }
        })
        .collect()
}

async fn live_public_relayed_peer_circuit(
    relay_address: &Multiaddr,
    timeout: Duration,
) -> Result<BootstrapCheckReport, PublicRelayProbeFailure> {
    let deadline = public_relay_candidate_deadline(timeout);
    let relay_peer = address_peer(relay_address).ok_or_else(|| {
        PublicRelayProbeFailure::without_bootstrap("live relay multiaddr must include /p2p/RELAY")
    })?;
    let relay_reservation = relay_address.to_owned().with(Protocol::P2pCircuit);
    let discovery = relay_probe_discovery();
    let mut listener_node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519()
            .map_err(|error| PublicRelayProbeFailure::without_bootstrap(format!("{error:?}")))?,
        network_name: "lab".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: vec![relay_reservation.clone()],
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery: discovery.clone(),
    })
    .map_err(|error| PublicRelayProbeFailure::without_bootstrap(format!("{error:?}")))?;
    let listener_peer = listener_node.local_peer_id;
    let relayed_target_address = relay_reservation.with(Protocol::P2p(listener_peer));

    wait_for_external_relay_reservation(
        &mut listener_node,
        relayed_target_address.clone(),
        relay_peer,
        public_relay_candidate_remaining(deadline).ok_or_else(|| {
            PublicRelayProbeFailure::candidate_timeout(
                PublicRelayCandidateFailureStage::RelayReservation,
            )
        })?,
    )
    .await
    .map_err(|error| {
        PublicRelayProbeFailure::at_stage(
            PublicRelayCandidateFailureStage::RelayReservation,
            error.error_message(),
        )
    })?;

    let _listener_task = tokio::spawn(async move {
        loop {
            let _ = listener_node.swarm.select_next_some().await;
        }
    });

    let config = relay_probe_config_with_relayed_peer(listener_peer, &relayed_target_address)
        .map_err(PublicRelayProbeFailure::without_bootstrap)?;
    let report = check_config_bootstrap(
        &config,
        public_relay_candidate_remaining(deadline).ok_or_else(|| {
            PublicRelayProbeFailure::candidate_timeout(
                PublicRelayCandidateFailureStage::RelayedPeerCircuit,
            )
        })?,
        BootstrapCheckThreshold::Any,
        BootstrapCheckRequirements {
            relay_reservations: false,
            autonat_status: false,
            dcutr_ready: false,
            dcutr_success: false,
            relayed_peer_circuits: true,
            membership_records: false,
        },
    )
    .await
    .map_err(|error| {
        PublicRelayProbeFailure::at_stage(
            PublicRelayCandidateFailureStage::RelayedPeerCircuit,
            format!("{error:?}"),
        )
    })?;

    if report.succeeded() {
        Ok(report)
    } else {
        Err(PublicRelayProbeFailure::with_bootstrap(
            PublicRelayCandidateFailureStage::RelayedPeerCircuit,
            "relayed peer circuit check did not meet success threshold",
            report,
        ))
    }
}

async fn live_public_dcutr_success(
    relay_address: &Multiaddr,
    timeout: Duration,
) -> Result<BootstrapCheckReport, PublicRelayProbeFailure> {
    let deadline = public_relay_candidate_deadline(timeout);
    let relay_peer = address_peer(relay_address).ok_or_else(|| {
        PublicRelayProbeFailure::without_bootstrap("live relay multiaddr must include /p2p/RELAY")
    })?;
    let relay_reservation = relay_address.to_owned().with(Protocol::P2pCircuit);
    let discovery = dcutr_probe_discovery();
    let mut listener_node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519()
            .map_err(|error| PublicRelayProbeFailure::without_bootstrap(format!("{error:?}")))?,
        network_name: "lab".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: dcutr_probe_listen_addresses(),
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: vec![relay_reservation.clone()],
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery,
    })
    .map_err(|error| PublicRelayProbeFailure::without_bootstrap(format!("{error:?}")))?;
    let listener_peer = listener_node.local_peer_id;
    let relayed_target_address = relay_reservation.with(Protocol::P2p(listener_peer));

    wait_for_external_relay_reservation(
        &mut listener_node,
        relayed_target_address.clone(),
        relay_peer,
        public_relay_candidate_remaining(deadline).ok_or_else(|| {
            PublicRelayProbeFailure::candidate_timeout(
                PublicRelayCandidateFailureStage::RelayReservation,
            )
        })?,
    )
    .await
    .map_err(|error| {
        PublicRelayProbeFailure::at_stage(
            PublicRelayCandidateFailureStage::RelayReservation,
            error.error_message(),
        )
    })?;

    let _listener_task = tokio::spawn(async move {
        loop {
            let _ = listener_node.swarm.select_next_some().await;
        }
    });

    let mut config = relay_probe_config_with_relayed_peer_discovery(
        listener_peer,
        &relayed_target_address,
        dcutr_probe_discovery(),
    )
    .map_err(PublicRelayProbeFailure::without_bootstrap)?;
    config.network.listen_addresses = dcutr_probe_listen_address_strings();
    let report = check_config_bootstrap(
        &config,
        public_relay_candidate_remaining(deadline).ok_or_else(|| {
            PublicRelayProbeFailure::candidate_timeout(
                PublicRelayCandidateFailureStage::DcutrSuccess,
            )
        })?,
        BootstrapCheckThreshold::Any,
        BootstrapCheckRequirements {
            relay_reservations: false,
            autonat_status: false,
            dcutr_ready: false,
            dcutr_success: true,
            relayed_peer_circuits: true,
            membership_records: false,
        },
    )
    .await
    .map_err(|error| {
        PublicRelayProbeFailure::at_stage(
            PublicRelayCandidateFailureStage::DcutrSuccess,
            format!("{error:?}"),
        )
    })?;

    if report.succeeded() {
        Ok(report)
    } else {
        Err(PublicRelayProbeFailure::with_bootstrap(
            PublicRelayCandidateFailureStage::DcutrSuccess,
            "dcutr success check did not meet success threshold",
            report,
        ))
    }
}

pub async fn start_public_dcutr_listener(
    relay_address: &Multiaddr,
    reservation_timeout: Duration,
) -> Result<PublicDcutrListener, PublicDcutrListenStartError> {
    let relay_peer = address_peer(relay_address).ok_or_else(|| {
        PublicDcutrListenStartError::new("live relay multiaddr must include /p2p/RELAY")
    })?;
    let relay_reservation = relay_address.to_owned().with(Protocol::P2pCircuit);
    let discovery = dcutr_probe_discovery();
    let mut node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519()
            .map_err(|error| PublicDcutrListenStartError::new(format!("{error:?}")))?,
        network_name: "lab".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: dcutr_probe_listen_addresses(),
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: vec![relay_reservation.clone()],
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery,
    })
    .map_err(|error| PublicDcutrListenStartError::new(format!("{error:?}")))?;

    let listener_peer = node.local_peer_id;
    let relayed_address = relay_reservation.with(Protocol::P2p(listener_peer));
    let reservation_evidence = wait_for_external_relay_reservation(
        &mut node,
        relayed_address.clone(),
        relay_peer,
        reservation_timeout,
    )
    .await
    .map_err(|evidence| {
        PublicDcutrListenStartError::with_reservation_evidence(evidence.error_message(), evidence)
    })?;

    let created_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());

    Ok(PublicDcutrListener {
        descriptor: PublicDcutrListenerDescriptor {
            schema_version: PublicDcutrListenerDescriptor::SCHEMA_VERSION,
            relay_candidate: relay_address.to_string(),
            relay_peer: relay_peer.to_string(),
            listener_peer: listener_peer.to_string(),
            relayed_address: relayed_address.to_string(),
            listen_addresses: reservation_evidence.listen_addresses.clone(),
            created_unix_seconds,
        },
        reservation_evidence,
        node,
    })
}

pub async fn check_public_dcutr_descriptor(
    descriptor: &PublicDcutrListenerDescriptor,
    timeout: Duration,
) -> Result<BootstrapCheckReport, String> {
    descriptor.validate()?;
    let listener_peer = descriptor.listener_peer_id()?;
    let relayed_address = descriptor.relayed_multiaddr()?;
    let mut config = relay_probe_config_with_relayed_peer_discovery(
        listener_peer,
        &relayed_address,
        dcutr_probe_discovery(),
    )?;
    config.network.listen_addresses = dcutr_probe_listen_address_strings();
    check_config_bootstrap(
        &config,
        timeout,
        BootstrapCheckThreshold::Any,
        BootstrapCheckRequirements {
            relay_reservations: false,
            autonat_status: false,
            dcutr_ready: false,
            dcutr_success: true,
            relayed_peer_circuits: true,
            membership_records: false,
        },
    )
    .await
    .map_err(|error| format!("{error:?}"))
}

fn public_relay_candidate_deadline(timeout: Duration) -> Instant {
    Instant::now() + timeout.max(Duration::from_millis(1))
}

fn public_relay_candidate_remaining(deadline: Instant) -> Option<Duration> {
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    Some(deadline.saturating_duration_since(now))
}

async fn wait_for_external_relay_reservation(
    listener: &mut P2pNode,
    relayed_address: Multiaddr,
    relay_peer: Libp2pPeerId,
    timeout: Duration,
) -> Result<PublicDcutrReservationEvidence, PublicDcutrReservationEvidence> {
    let mut listen_addr_reported = false;
    let mut reservation_accepted = false;
    let mut connected = listener.swarm.is_connected(&relay_peer);
    let mut last_error = None;
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, listener.swarm.select_next_some()).await
        else {
            break;
        };
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == relay_peer => {
                connected = true;
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } if peer_id == relay_peer => {
                last_error = Some(format!("{error:?}"));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Relay(
                relay::client::Event::ReservationReqAccepted {
                    relay_peer_id,
                    renewal,
                    ..
                },
            )) if relay_peer_id == relay_peer && !renewal => {
                reservation_accepted = true;
            }
            SwarmEvent::NewListenAddr { address, .. } if address == relayed_address => {
                listen_addr_reported = true;
            }
            _ => {}
        }

        if listen_addr_reported && reservation_accepted {
            return Ok(PublicDcutrReservationEvidence::from_listener(
                listener,
                connected,
                reservation_accepted,
                listen_addr_reported,
                last_error,
            ));
        }
    }

    Err(PublicDcutrReservationEvidence::from_listener(
        listener,
        connected,
        reservation_accepted,
        listen_addr_reported,
        last_error,
    ))
}

fn address_peer(address: &Multiaddr) -> Option<Libp2pPeerId> {
    address.iter().find_map(|protocol| match protocol {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn relay_probe_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: false,
        kademlia_provider_advertisement: false,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn dcutr_probe_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        dcutr: true,
        autonat: true,
        ..relay_probe_discovery()
    }
}

fn dcutr_probe_listen_addresses() -> Vec<Multiaddr> {
    dcutr_probe_listen_address_strings()
        .into_iter()
        .map(|address| address.parse().expect("static DCUtR probe listen address"))
        .collect()
}

fn dcutr_probe_listen_address_strings() -> Vec<String> {
    vec![
        "/ip4/0.0.0.0/tcp/0".to_owned(),
        "/ip4/0.0.0.0/udp/0/quic-v1".to_owned(),
    ]
}

fn relay_probe_config_with_relayed_peer(
    peer: Libp2pPeerId,
    address: &Multiaddr,
) -> Result<Config, String> {
    relay_probe_config_with_relayed_peer_discovery(peer, address, relay_probe_discovery())
}

fn relay_probe_config_with_relayed_peer_discovery(
    peer: Libp2pPeerId,
    address: &Multiaddr,
    discovery: DiscoveryConfig,
) -> Result<Config, String> {
    let identity = NodeIdentity::generate_ed25519().map_err(|error| format!("{error:?}"))?;
    Ok(Config {
        network: NetworkConfig {
            name: "lab".to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key),
            membership_key: None,
            previous_membership_tags: Vec::new(),
            member_records: Vec::new(),
            routes: Vec::new(),
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            discovery,
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: "hs0".to_owned(),
            mtu: 1280,
        },
        peers: vec![PeerConfig {
            id: peer.to_string(),
            name: Some("relay-probe-listener".to_owned()),
            addresses: vec![address.to_string()],
            routes: Vec::new(),
        }],
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
    })
}

fn bootstrap_check_host_config(config: &Config) -> Result<HostConfig, BootstrapCheckError> {
    Ok(HostConfig {
        identity: config.identity()?,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })
}

struct BootstrapPollContext<'a> {
    config: &'a Config,
    membership_tag: Option<&'a str>,
    previous_membership_tags: &'a [String],
    bootstrap_peers: &'a [(Libp2pPeerId, libp2p::Multiaddr)],
    relay_reservations: &'a [(Libp2pPeerId, libp2p::Multiaddr)],
    relayed_peers: &'a [(Libp2pPeerId, libp2p::Multiaddr)],
}

async fn poll_bootstrap_events(
    node: &mut P2pNode,
    context: &BootstrapPollContext<'_>,
    timeout: Duration,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
) -> BootstrapPollResult {
    let mut result = BootstrapPollResult {
        connected_bootstrap_peers: context
            .bootstrap_peers
            .iter()
            .filter_map(|(peer, _)| node.swarm.is_connected(peer).then_some(*peer))
            .collect(),
        ..BootstrapPollResult::default()
    };
    if requirements.relayed_peer_circuits || requirements.dcutr_success {
        dial_relayed_peer_targets(node, context.relayed_peers, &mut result);
    }
    start_bootstrap_membership_record_dht(
        node,
        context.config,
        context.membership_tag,
        &mut result,
    );
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while should_continue_polling(&PollingStatus {
        threshold,
        configured_bootstrap_peers: context.bootstrap_peers.len(),
        connected_bootstrap_peers: result.connected_bootstrap_peers.len(),
        requirements,
        configured_relay_reservations: context.relay_reservations.len(),
        accepted_relay_reservations: result.accepted_relay_reservations.len(),
        relayed_listen_addresses: result.relayed_listen_addresses.len(),
        configured_relayed_peer_circuits: context.relayed_peers.len(),
        connected_relayed_peer_circuits: result.connected_relayed_peers.len(),
        direct_connected_relayed_peer_circuits: result.direct_connected_relayed_peers.len(),
        dcutr_successes: result.dcutr_successes,
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: result.autonat_status,
        membership_records: &result.membership_records,
        now: Instant::now(),
        deadline,
    }) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, node.swarm.select_next_some()).await else {
            break;
        };
        record_bootstrap_event(event, context, &mut result);
    }

    result
}

fn dcutr_direct_dial_last_error(
    requirements: BootstrapCheckRequirements,
    poll: &BootstrapPollResult,
) -> Option<String> {
    if !requirements.dcutr_success || poll.dcutr_last_error.is_some() {
        return None;
    }

    poll.relayed_peer_dial_failures
        .iter()
        .rev()
        .find(|(peer, _)| poll.connected_relayed_peers.contains_key(peer))
        .map(|(_, error)| format!("direct_dial: {error}"))
}

fn sorted_connection_addresses(connections: &HashMap<Libp2pPeerId, Multiaddr>) -> Vec<String> {
    let mut addresses: Vec<_> = connections
        .iter()
        .map(|(peer, address)| format!("{peer} {address}"))
        .collect();
    addresses.sort();
    addresses
}

fn start_public_relay_closest_peer_lookup(node: &mut P2pNode) -> bool {
    if !node.discovery.kademlia {
        return false;
    }

    let local_peer = *node.swarm.local_peer_id();
    node.swarm.behaviour_mut().kad.get_closest_peers(local_peer);
    true
}

async fn poll_public_relay_scan_events(
    node: &mut P2pNode,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    timeout: Duration,
    max_candidates: usize,
) -> PublicRelayScanPollResult {
    let mut result = PublicRelayScanPollResult {
        scan_peers: bootstrap_peers.to_vec(),
        connected_bootstrap_peers: bootstrap_peers
            .iter()
            .filter_map(|(peer, _)| node.swarm.is_connected(peer).then_some(*peer))
            .collect(),
        ..PublicRelayScanPollResult::default()
    };
    result.closest_peer_lookup_started = start_public_relay_closest_peer_lookup(node);
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while should_continue_public_relay_scan(
        bootstrap_peers.len(),
        max_candidates,
        public_relay_kademlia_lookup_state(node.discovery.kademlia, &result),
        Instant::now() < deadline,
        &result,
    ) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, node.swarm.select_next_some()).await else {
            break;
        };
        record_public_relay_scan_event(event, node, bootstrap_peers, max_candidates, &mut result);
    }

    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicRelayKademliaLookupState {
    Disabled,
    NotStarted,
    Waiting,
    Finished,
}

fn public_relay_kademlia_lookup_state(
    kademlia_enabled: bool,
    result: &PublicRelayScanPollResult,
) -> PublicRelayKademliaLookupState {
    if !kademlia_enabled {
        PublicRelayKademliaLookupState::Disabled
    } else if result.closest_peer_lookup_finished {
        PublicRelayKademliaLookupState::Finished
    } else if result.closest_peer_lookup_started {
        PublicRelayKademliaLookupState::Waiting
    } else {
        PublicRelayKademliaLookupState::NotStarted
    }
}

fn should_continue_public_relay_scan(
    bootstrap_peers: usize,
    max_candidates: usize,
    kademlia_lookup: PublicRelayKademliaLookupState,
    within_deadline: bool,
    result: &PublicRelayScanPollResult,
) -> bool {
    if !within_deadline || bootstrap_peers == 0 {
        return false;
    }

    let closest_peer_lookup_waiting = kademlia_lookup == PublicRelayKademliaLookupState::Waiting
        && result.scan_peers.len() < PUBLIC_RELAY_SCAN_LIMIT;

    let candidate_discovery_waiting = result.candidates.len() < max_candidates
        && (result.identified_peers.len() < result.scan_peers.len()
            || (kademlia_lookup != PublicRelayKademliaLookupState::Disabled
                && result.scan_peers.len() < PUBLIC_RELAY_SCAN_LIMIT));

    candidate_discovery_waiting || closest_peer_lookup_waiting
}

fn record_public_relay_scan_event(
    event: SwarmEvent<BehaviourEvent>,
    node: &mut P2pNode,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    max_candidates: usize,
    result: &mut PublicRelayScanPollResult,
) {
    match event {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result.connected_bootstrap_peers.insert(peer_id);
            }
        }
        SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            error,
            ..
        } => {
            if scan_peers_include(&result.scan_peers, peer_id) {
                result.dial_failures.push((peer_id, format!("{error:?}")));
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) if scan_peers_include(&result.scan_peers, peer_id) => {
            let relay_hop = identify_protocols_include_relay_hop(&info.protocols);
            let mut accepted_candidates = 0;
            if relay_hop {
                for address in relay_scan_candidate_addresses(peer_id, &info, &result.scan_peers) {
                    if add_public_relay_candidate(
                        &mut result.candidates,
                        max_candidates,
                        peer_id,
                        address,
                    ) {
                        accepted_candidates += 1;
                    }
                }
            }
            result.identified_peers.insert(
                peer_id,
                PublicRelayIdentifyResult {
                    relay_hop,
                    candidate_addresses: accepted_candidates,
                },
            );
        }
        SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Error {
            peer_id,
            error,
            ..
        })) if scan_peers_include(&result.scan_peers, peer_id) => {
            result
                .dial_failures
                .push((peer_id, format!("identify failed: {error}")));
        }
        SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::RoutingUpdated {
            peer,
            addresses,
            ..
        })) => {
            record_public_relay_routing_peer(peer, addresses.iter(), node, result);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
            result: kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk { peers, .. })),
            ..
        })) => {
            result.closest_peer_lookup_finished = true;
            record_public_relay_closest_peer_results(peers, node, result);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
            result:
                kad::QueryResult::GetClosestPeers(Err(kad::GetClosestPeersError::Timeout {
                    peers, ..
                })),
            ..
        })) => {
            result.closest_peer_lookup_finished = true;
            result.closest_peer_errors += 1;
            record_public_relay_closest_peer_results(peers, node, result);
        }
        _ => {}
    }
}

fn add_public_relay_candidate(
    candidates: &mut Vec<(Libp2pPeerId, Multiaddr)>,
    max_candidates: usize,
    peer: Libp2pPeerId,
    address: Multiaddr,
) -> bool {
    if max_candidates == 0 {
        return false;
    }

    let candidate = (peer, address);
    if candidates.contains(&candidate) {
        return false;
    }

    if candidates.len() < max_candidates {
        candidates.push(candidate);
        return true;
    }

    if candidates
        .iter()
        .any(|(candidate_peer, _)| *candidate_peer == peer)
    {
        return false;
    }

    let Some(replace_index) = replaceable_public_relay_candidate_index(candidates) else {
        return false;
    };
    candidates.remove(replace_index);
    candidates.push(candidate);
    true
}

fn replaceable_public_relay_candidate_index(
    candidates: &[(Libp2pPeerId, Multiaddr)],
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, (peer, _))| {
            let peer_candidates = candidates
                .iter()
                .filter(|(candidate_peer, _)| candidate_peer == peer)
                .count();
            (peer_candidates > 1).then_some(index)
        })
}

fn record_public_relay_closest_peer_results(
    peers: Vec<kad::PeerInfo>,
    node: &mut P2pNode,
    result: &mut PublicRelayScanPollResult,
) {
    result.closest_peer_results += peers.len();
    for peer in peers {
        record_public_relay_routing_peer(peer.peer_id, peer.addrs.iter(), node, result);
    }
}

fn record_public_relay_routing_peer<'a>(
    peer: Libp2pPeerId,
    mut addresses: impl Iterator<Item = &'a Multiaddr>,
    node: &mut P2pNode,
    result: &mut PublicRelayScanPollResult,
) {
    if peer == *node.swarm.local_peer_id()
        || scan_peers_include(&result.scan_peers, peer)
        || result.scan_peers.len() >= PUBLIC_RELAY_SCAN_LIMIT
    {
        return;
    }

    let Some(address) = addresses.find_map(|address| public_relay_candidate_address(peer, address))
    else {
        return;
    };

    result.discovered_routing_peers += 1;
    result.scan_peers.push((peer, address.clone()));
    if node.swarm.is_connected(&peer) {
        return;
    }
    match node.swarm.dial(address.clone()) {
        Ok(()) => result.dialed_routing_peers += 1,
        Err(error) => result.dial_failures.push((peer, format!("{error:?}"))),
    }
}

fn scan_peers_include(peers: &[(Libp2pPeerId, Multiaddr)], peer: Libp2pPeerId) -> bool {
    peers.iter().any(|(candidate, _)| *candidate == peer)
}

fn identify_protocols_include_relay_hop(protocols: &[libp2p::StreamProtocol]) -> bool {
    protocols
        .iter()
        .any(|protocol| protocol.as_ref() == relay::HOP_PROTOCOL_NAME.as_ref())
}

fn relay_scan_candidate_addresses(
    peer: Libp2pPeerId,
    info: &identify::Info,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
) -> Vec<Multiaddr> {
    bootstrap_peers
        .iter()
        .filter(|(candidate_peer, _)| *candidate_peer == peer)
        .map(|(_, address)| public_relay_candidate_address(peer, address))
        .chain(
            info.listen_addrs
                .iter()
                .map(|address| public_relay_candidate_address(peer, address)),
        )
        .flatten()
        .fold(Vec::new(), |mut addresses, address| {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
            addresses
        })
}

fn public_relay_candidate_address(peer: Libp2pPeerId, address: &Multiaddr) -> Option<Multiaddr> {
    if relay_peer_from_relayed_address(address).is_some() {
        return None;
    }
    if !supports_public_relay_candidate_transport(address) {
        return None;
    }
    if address_peer(address).is_some_and(|address_peer| address_peer != peer) {
        return None;
    }
    if address_peer(address).is_some() {
        return Some(address.clone());
    }
    address.clone().with_p2p(peer).ok()
}

fn supports_public_relay_candidate_transport(address: &Multiaddr) -> bool {
    let mut has_supported_transport = false;
    for protocol in address {
        match protocol {
            Protocol::Tcp(_) | Protocol::Quic | Protocol::QuicV1 => {
                has_supported_transport = true;
            }
            Protocol::Http
            | Protocol::Https
            | Protocol::P2pWebRtcDirect
            | Protocol::P2pWebRtcStar
            | Protocol::P2pWebSocketStar
            | Protocol::Tls
            | Protocol::WebRTC
            | Protocol::WebRTCDirect
            | Protocol::WebTransport
            | Protocol::Ws(_)
            | Protocol::Wss(_) => return false,
            _ => {}
        }
    }

    has_supported_transport
}

fn dial_relayed_peer_targets(
    node: &mut P2pNode,
    relayed_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    result: &mut BootstrapPollResult,
) {
    for (peer, address) in relayed_peers {
        let dial_address = match peer_dial_address(*peer, address.clone()) {
            Ok(address) => address,
            Err(address) => {
                result
                    .relayed_peer_dial_failures
                    .push((*peer, format!("address lacks valid /p2p/{peer}: {address}")));
                continue;
            }
        };

        if let Err(error) = node.swarm.dial(dial_address) {
            result
                .relayed_peer_dial_failures
                .push((*peer, format!("{error:?}")));
        }
    }
}

fn record_bootstrap_event(
    event: SwarmEvent<BehaviourEvent>,
    context: &BootstrapPollContext<'_>,
    result: &mut BootstrapPollResult,
) {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            record_bootstrap_connection_established(peer_id, &endpoint, context, result);
        }
        SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            error,
            ..
        } => {
            record_bootstrap_connection_error(peer_id, &format!("{error:?}"), context, result);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal: false,
                ..
            },
        )) if context
            .relay_reservations
            .iter()
            .any(|(peer, _)| *peer == relay_peer_id) =>
        {
            result.accepted_relay_reservations.insert(relay_peer_id);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. },
        )) => {
            result.outbound_circuit_relays.insert(relay_peer_id);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event { result: event, .. })) => {
            match event {
                Ok(_) => {
                    result.dcutr_successes += 1;
                }
                Err(error) => {
                    result.dcutr_failures += 1;
                    result.dcutr_last_error = Some(format!("{error:?}"));
                }
            }
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            if let Some(relay_peer) = relay_peer_from_relayed_address(&address)
                && context
                    .relay_reservations
                    .iter()
                    .any(|(peer, _)| *peer == relay_peer)
            {
                result.relayed_listen_addresses.insert(relay_peer, address);
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
            new,
            ..
        })) => {
            result.autonat_status = BootstrapAutoNatStatus::from_nat_status(&new);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
            result: query_result,
            ..
        })) => {
            record_membership_record_dht_result(
                &query_result,
                context.config,
                context.membership_tag,
                context.previous_membership_tags,
                &mut result.membership_records,
            );
        }
        _ => {}
    }
}

fn record_bootstrap_connection_established(
    peer_id: Libp2pPeerId,
    endpoint: &ConnectedPoint,
    context: &BootstrapPollContext<'_>,
    result: &mut BootstrapPollResult,
) {
    if context
        .bootstrap_peers
        .iter()
        .any(|(peer, _)| *peer == peer_id)
    {
        result.connected_bootstrap_peers.insert(peer_id);
    }
    if context
        .relayed_peers
        .iter()
        .any(|(peer, _)| *peer == peer_id)
    {
        let address = connected_point_address(endpoint).clone();
        if endpoint.is_relayed() {
            result.connected_relayed_peers.insert(peer_id, address);
        } else {
            result
                .direct_connected_relayed_peers
                .insert(peer_id, address);
        }
    }
}

fn record_bootstrap_connection_error(
    peer_id: Libp2pPeerId,
    error: &str,
    context: &BootstrapPollContext<'_>,
    result: &mut BootstrapPollResult,
) {
    if context
        .bootstrap_peers
        .iter()
        .any(|(peer, _)| *peer == peer_id)
    {
        result.dial_failures.push((peer_id, error.to_owned()));
    }
    if context
        .relayed_peers
        .iter()
        .any(|(peer, _)| *peer == peer_id)
    {
        result
            .relayed_peer_dial_failures
            .push((peer_id, error.to_owned()));
    }
}

fn connected_point_address(endpoint: &ConnectedPoint) -> &Multiaddr {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { local_addr, .. } => local_addr,
    }
}

fn current_unix_seconds_lossy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Deserialize, Serialize)]
struct BootstrapMembershipRecordBundle {
    version: u8,
    network_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_tag: Option<String>,
    records: Vec<SignedMembershipRecord>,
}

const MAX_BOOTSTRAP_MEMBERSHIP_RECORD_BYTES: usize = 64 * 1024;

fn start_bootstrap_membership_record_dht(
    node: &mut P2pNode,
    config: &Config,
    membership_tag: Option<&str>,
    result: &mut BootstrapPollResult,
) {
    result.membership_records.configured_records = config.network.member_records.len();
    if !config.network.discovery.kademlia || config.network.member_records.is_empty() {
        return;
    }

    let record_key = kademlia_membership_records_key(&config.network.name, membership_tag);
    result.membership_records.lookup_started = true;
    node.swarm
        .behaviour_mut()
        .kad
        .get_record(record_key.clone());

    let records = config
        .network
        .member_records
        .iter()
        .take(MAX_CONTROL_MEMBERSHIP_RECORDS)
        .cloned()
        .collect::<Vec<_>>();
    match encode_bootstrap_membership_records(&config.network.name, membership_tag, records) {
        Ok(value) => {
            result.membership_records.publish_started = true;
            let record = kad::Record {
                key: record_key,
                value,
                publisher: Some(*node.swarm.local_peer_id()),
                expires: None,
            };
            if let Err(error) = node
                .swarm
                .behaviour_mut()
                .kad
                .put_record(record, kad::Quorum::One)
            {
                result.membership_records.publish_failures += 1;
                result.membership_records.last_error = Some(format!("{error:?}"));
            }
        }
        Err(error) => {
            result.membership_records.publish_failures += 1;
            result.membership_records.last_error = Some(format!("encode_failed:{error}"));
        }
    }
}

fn encode_bootstrap_membership_records(
    network_name: &str,
    membership_tag: Option<&str>,
    records: Vec<SignedMembershipRecord>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&BootstrapMembershipRecordBundle {
        version: 1,
        network_name: network_name.to_owned(),
        membership_tag: membership_tag.map(str::to_owned),
        records,
    })
}

fn record_membership_record_dht_result(
    query_result: &kad::QueryResult,
    config: &Config,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    result: &mut BootstrapMembershipRecordDhtCheck,
) {
    match query_result {
        kad::QueryResult::PutRecord(Ok(kad::PutRecordOk { .. })) => {
            result.publish_succeeded = true;
        }
        kad::QueryResult::PutRecord(Err(error)) => {
            result.publish_failures += 1;
            result.last_error = Some(format!("{error:?}"));
        }
        kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) => {
            match validate_bootstrap_membership_record_value(
                config,
                current_membership_tag,
                previous_membership_tags,
                &peer_record.record.value,
            ) {
                Ok((verified, accepted)) => {
                    result.found_records += 1;
                    result.verified_records += verified;
                    result.accepted_records += accepted;
                }
                Err(error) => {
                    result.invalid_records += 1;
                    result.last_error = Some(error);
                }
            }
        }
        kad::QueryResult::GetRecord(Err(error)) => {
            result.last_error = Some(format!("{error:?}"));
        }
        _ => {}
    }
}

fn validate_bootstrap_membership_record_value(
    config: &Config,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    value: &[u8],
) -> Result<(usize, usize), String> {
    if value.len() > MAX_BOOTSTRAP_MEMBERSHIP_RECORD_BYTES {
        return Err("too_large".to_owned());
    }
    let bundle: BootstrapMembershipRecordBundle =
        serde_json::from_slice(value).map_err(|error| format!("decode_failed:{error}"))?;
    if bundle.version != 1 {
        return Err("unsupported_version".to_owned());
    }
    if bundle.network_name != config.network.name {
        return Err("wrong_network".to_owned());
    }
    if !bootstrap_membership_tag_allowed(
        bundle.membership_tag.as_deref(),
        current_membership_tag,
        previous_membership_tags,
    ) {
        return Err("wrong_membership_scope".to_owned());
    }
    if bundle.records.len() > MAX_CONTROL_MEMBERSHIP_RECORDS {
        return Err("too_many_records".to_owned());
    }

    let now = current_unix_seconds_lossy();
    let mut records = config.network.member_records.clone();
    let trusted_issuers = trusted_membership_issuers_at(&records, &config.network.name, now)
        .map_err(|error| format!("invalid_trust_roots:{error:?}"))?;
    let stats = merge_membership_records_at(
        &mut records,
        &bundle.records,
        &config.network.name,
        now,
        &trusted_issuers,
        MAX_CONTROL_MEMBERSHIP_RECORDS,
    )
    .map_err(|error| format!("invalid_record:{error:?}"))?;
    Ok((bundle.records.len(), stats.accepted))
}

fn bootstrap_membership_tag_allowed(
    tag: Option<&str>,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> bool {
    tag == current_membership_tag
        || tag.is_some_and(|tag| {
            previous_membership_tags
                .iter()
                .any(|previous_tag| previous_tag == tag)
        })
}

#[derive(Debug, Default)]
struct BootstrapPollResult {
    connected_bootstrap_peers: HashSet<Libp2pPeerId>,
    dial_failures: Vec<(Libp2pPeerId, String)>,
    accepted_relay_reservations: HashSet<Libp2pPeerId>,
    relayed_listen_addresses: HashMap<Libp2pPeerId, libp2p::Multiaddr>,
    connected_relayed_peers: HashMap<Libp2pPeerId, Multiaddr>,
    direct_connected_relayed_peers: HashMap<Libp2pPeerId, Multiaddr>,
    outbound_circuit_relays: HashSet<Libp2pPeerId>,
    relayed_peer_dial_failures: Vec<(Libp2pPeerId, String)>,
    dcutr_successes: usize,
    dcutr_failures: usize,
    dcutr_last_error: Option<String>,
    autonat_status: BootstrapAutoNatStatus,
    membership_records: BootstrapMembershipRecordDhtCheck,
}

#[derive(Debug, Default)]
struct PublicRelayScanPollResult {
    scan_peers: Vec<(Libp2pPeerId, Multiaddr)>,
    connected_bootstrap_peers: HashSet<Libp2pPeerId>,
    identified_peers: HashMap<Libp2pPeerId, PublicRelayIdentifyResult>,
    dial_failures: Vec<(Libp2pPeerId, String)>,
    candidates: Vec<(Libp2pPeerId, Multiaddr)>,
    discovered_routing_peers: usize,
    dialed_routing_peers: usize,
    closest_peer_lookup_started: bool,
    closest_peer_lookup_finished: bool,
    closest_peer_results: usize,
    closest_peer_errors: usize,
}

#[derive(Debug)]
struct PublicRelayIdentifyResult {
    relay_hop: bool,
    candidate_addresses: usize,
}

#[derive(Clone, Debug)]
struct PollingStatus<'a> {
    threshold: BootstrapCheckThreshold,
    configured_bootstrap_peers: usize,
    connected_bootstrap_peers: usize,
    requirements: BootstrapCheckRequirements,
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relayed_listen_addresses: usize,
    configured_relayed_peer_circuits: usize,
    connected_relayed_peer_circuits: usize,
    direct_connected_relayed_peer_circuits: usize,
    dcutr_successes: usize,
    autonat_probe_servers_registered: usize,
    autonat_status: BootstrapAutoNatStatus,
    membership_records: &'a BootstrapMembershipRecordDhtCheck,
    now: Instant,
    deadline: Instant,
}

fn should_continue_polling(status: &PollingStatus<'_>) -> bool {
    if (status.configured_bootstrap_peers == 0
        && !status.requirements.relay_reservations
        && !status.requirements.autonat_status
        && !status.requirements.dcutr_ready
        && !status.requirements.dcutr_success
        && !status.requirements.relayed_peer_circuits
        && !status.requirements.membership_records)
        || status.now >= status.deadline
    {
        return false;
    }

    let bootstrap_waiting = status.configured_bootstrap_peers > 0
        && match status.threshold {
            BootstrapCheckThreshold::Any => status.connected_bootstrap_peers == 0,
            BootstrapCheckThreshold::All => {
                status.connected_bootstrap_peers < status.configured_bootstrap_peers
            }
        };
    let relay_waiting = (status.requirements.relay_reservations || status.requirements.dcutr_ready)
        && status.configured_relay_reservations > 0
        && (status.accepted_relay_reservations < status.configured_relay_reservations
            || status.relayed_listen_addresses < status.configured_relay_reservations);
    let autonat_waiting = status.requirements.autonat_status
        && status.autonat_probe_servers_registered > 0
        && !status.autonat_status.is_observed();
    let relayed_peer_waiting = status.requirements.relayed_peer_circuits
        && status.configured_relayed_peer_circuits > 0
        && status.connected_relayed_peer_circuits < status.configured_relayed_peer_circuits;
    let dcutr_success_waiting = status.requirements.dcutr_success
        && (status.dcutr_successes == 0 || status.direct_connected_relayed_peer_circuits == 0);
    let membership_records_waiting = status.requirements.membership_records
        && status.membership_records.configured_records > 0
        && (!status.membership_records.publish_succeeded
            || status.membership_records.found_records == 0
            || status.membership_records.verified_records == 0);

    bootstrap_waiting
        || relay_waiting
        || autonat_waiting
        || relayed_peer_waiting
        || dcutr_success_waiting
        || membership_records_waiting
}

const fn relay_reservations_ready(
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relays_with_listen_addresses: usize,
) -> bool {
    configured_relay_reservations > 0
        && accepted_relay_reservations == configured_relay_reservations
        && relays_with_listen_addresses == configured_relay_reservations
}

fn relays_with_listen_addresses(
    relay_reservations: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relayed_listen_addresses: &HashMap<Libp2pPeerId, libp2p::Multiaddr>,
) -> usize {
    relay_reservations
        .iter()
        .filter(|(relay, _)| relayed_listen_addresses.contains_key(relay))
        .count()
}

impl BootstrapAutoNatStatus {
    fn from_nat_status(status: &autonat::NatStatus) -> Self {
        match status {
            autonat::NatStatus::Unknown => Self::Unknown,
            autonat::NatStatus::Public(_) => Self::Public,
            autonat::NatStatus::Private => Self::Private,
        }
    }
}

fn relay_peer_from_relayed_address(address: &libp2p::Multiaddr) -> Option<Libp2pPeerId> {
    let mut relay_peer = None;
    for protocol in address {
        match protocol {
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit => return relay_peer,
            _ => {}
        }
    }
    None
}

fn relayed_target_peer(address: &libp2p::Multiaddr) -> Option<Libp2pPeerId> {
    let mut after_circuit = false;
    for protocol in address {
        match protocol {
            Protocol::P2pCircuit => after_circuit = true,
            Protocol::P2p(peer) if after_circuit => return Some(peer),
            _ => {}
        }
    }
    None
}

fn relayed_peer_addresses(
    configured_peer_addresses: &[(Libp2pPeerId, libp2p::Multiaddr)],
) -> Vec<(Libp2pPeerId, libp2p::Multiaddr)> {
    configured_peer_addresses
        .iter()
        .filter(|(_, address)| relay_peer_from_relayed_address(address).is_some())
        .cloned()
        .collect()
}

fn peer_dial_address(
    peer: Libp2pPeerId,
    address: libp2p::Multiaddr,
) -> Result<libp2p::Multiaddr, libp2p::Multiaddr> {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2p(address_peer) if address_peer == peer))
    {
        return Ok(address);
    }

    address.with_p2p(peer)
}

#[derive(Debug)]
pub enum BootstrapCheckError {
    Config(ConfigError),
    Identity(IdentityError),
    Build(P2pBuildError),
}

impl From<ConfigError> for BootstrapCheckError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<IdentityError> for BootstrapCheckError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<P2pBuildError> for BootstrapCheckError {
    fn from(error: P2pBuildError) -> Self {
        Self::Build(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{env, time::Duration};

    use futures::StreamExt as _;
    use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::Keypair, swarm::SwarmEvent};

    use super::*;
    use crate::{
        config::{
            BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, NetworkConfig,
            PacketPlaneConfig, PeerConfig, QueueConfig, RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        membership::{MembershipRecordOptions, MembershipRole, issue_membership_record_at},
        runtime::p2p::{HostConfig, build_node},
    };

    const LIVE_RELAY_MULTIADDR_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDR";
    const LIVE_RELAY_MULTIADDRS_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDRS";
    const LIVE_RELAY_TIMEOUT_SECONDS_ENV: &str = "P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS";

    #[test]
    fn public_dcutr_listener_descriptor_validates_relayed_address() {
        let descriptor = public_dcutr_listener_descriptor();

        assert_eq!(
            descriptor.listener_peer_id().expect("listener peer"),
            descriptor
                .listener_peer
                .parse::<Libp2pPeerId>()
                .expect("listener peer id")
        );
        assert_eq!(
            descriptor
                .relayed_multiaddr()
                .expect("relayed address")
                .to_string(),
            descriptor.relayed_address
        );
        descriptor.validate().expect("descriptor validates");
    }

    #[test]
    fn public_dcutr_listener_descriptor_rejects_mismatched_relay_peer() {
        let mut descriptor = public_dcutr_listener_descriptor();
        descriptor.relay_peer = "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb".to_owned();

        assert!(
            descriptor
                .validate()
                .expect_err("mismatched relay peer should fail")
                .contains("does not match relay peer")
        );
    }

    #[test]
    fn public_dcutr_listener_descriptor_rejects_direct_listener_address() {
        let mut descriptor = public_dcutr_listener_descriptor();
        descriptor.relayed_address = format!(
            "/ip4/203.0.113.10/tcp/4001/p2p/{}",
            descriptor.listener_peer
        );

        assert!(
            descriptor
                .validate()
                .expect_err("direct listener address should fail")
                .contains("/p2p-circuit")
        );
    }

    #[tokio::test]
    async fn bootstrap_check_connects_to_configured_bootstrap_peer() {
        let mut bootstrap = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("bootstrap identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("bootstrap node");
        let bootstrap_peer = bootstrap.local_peer_id;
        let bootstrap_address = next_listen_address(&mut bootstrap).await;
        let _bootstrap_task = tokio::spawn(async move {
            loop {
                let _ = bootstrap.swarm.select_next_some().await;
            }
        });
        let config = config_with_bootstrap_peer(bootstrap_peer, &bootstrap_address);

        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(5),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements::default(),
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded());
        assert_eq!(report.configured_bootstrap_peers, 1);
        assert_eq!(report.connected_bootstrap_peers, 1);
        assert!(report.lines().contains(&"bootstrap check: ok".to_owned()));
    }

    #[tokio::test]
    async fn bootstrap_check_can_require_relay_reservation_acceptance() {
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node).await;
        relay_node.swarm.add_external_address(relay_address.clone());
        let relay_reservation = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay address")
            .with(Protocol::P2pCircuit);
        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });
        let config = config_with_relay_reservation(&relay_reservation);

        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(5),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: false,
                dcutr_ready: true,
                dcutr_success: false,
                relayed_peer_circuits: false,
                membership_records: false,
            },
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.configured_bootstrap_peers, 0);
        assert_eq!(report.configured_relay_reservations, 1);
        assert_eq!(report.accepted_relay_reservations, 1);
        assert_eq!(report.relayed_listen_addresses, 1);
        assert!(report.dcutr.enabled);
        assert!(report.dcutr.ready);
        assert!(report.lines().contains(&"bootstrap check: ok".to_owned()));
    }

    #[tokio::test]
    async fn bootstrap_check_can_probe_relayed_peer_circuit() {
        let discovery = relay_test_discovery();
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node).await;
        relay_node.swarm.add_external_address(relay_address.clone());
        let relay_reservation = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay address")
            .with(Protocol::P2pCircuit);

        let mut listener_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation.clone()],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery,
        })
        .expect("listener node");
        let listener_peer = listener_node.local_peer_id;
        let relayed_target_address = relay_reservation.clone().with(Protocol::P2p(listener_peer));

        tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_relay_reservation(
                &mut relay_node,
                &mut listener_node,
                relayed_target_address.clone(),
                relay_peer,
            ),
        )
        .await
        .expect("relay reservation timed out");

        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });
        let _listener_task = tokio::spawn(async move {
            loop {
                let _ = listener_node.swarm.select_next_some().await;
            }
        });

        let config = config_with_relayed_peer(listener_peer, &relayed_target_address);
        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(10),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: true,
                membership_records: false,
            },
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.configured_relayed_peer_circuits, 1);
        assert_eq!(report.connected_relayed_peer_circuits, 1);
        assert_eq!(report.relayed_peer_results.len(), 1);
        assert!(report.relayed_peer_results[0].connected);
        assert!(report.relayed_peer_results[0].outbound_circuit);
    }

    #[tokio::test]
    async fn public_relay_probe_can_validate_local_relay_candidate() {
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: relay_test_discovery(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node)
            .await
            .with_p2p(relay_peer)
            .expect("relay address");
        relay_node.swarm.add_external_address(relay_address.clone());
        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });

        let report = check_public_relay_candidates(
            &[relay_address],
            PublicRelayProbeMode::RelayedPeerCircuit,
            Duration::from_secs(10),
        )
        .await;

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].succeeded);
        assert!(
            report
                .lines()
                .contains(&"public relay probe: ok".to_owned())
        );
    }

    #[test]
    fn public_relay_probe_lines_include_failed_candidate_bootstrap_detail() {
        let relay = peer_id();
        let report = PublicRelayProbeReport {
            mode: PublicRelayProbeMode::DcutrSuccess,
            candidates: vec![PublicRelayCandidateReport {
                address: format!("/ip4/203.0.113.10/tcp/4001/p2p/{relay}"),
                succeeded: false,
                failure_stage: PublicRelayCandidateFailureStage::DcutrSuccess,
                error: Some("dcutr success check did not meet success threshold".to_owned()),
                bootstrap: Some(dcutr_success_report(
                    true,
                    0,
                    0,
                    1,
                    Some("NoDirectConnection".to_owned()),
                )),
                elapsed_millis: 45_000,
            }],
        };

        let lines = report.lines();

        assert!(!report.succeeded());
        assert!(lines.contains(&"public relay probe: failed".to_owned()));
        assert!(lines.contains(
            &"public relay candidate failure stages: candidate_setup 0 relay_reservation 0 relayed_peer_circuit 0 dcutr_success 1".to_owned()
        ));
        assert!(lines.iter().any(|line| {
            line.contains("failure_stage dcutr_success")
                && line.contains("diagnosis dcutr_no_hole_punch_success")
                && line.contains("dcutr success check did not meet success threshold")
        }));
        assert!(lines.iter().any(|line| line
            .contains("public relay candidate detail: bootstrap check: failed")));
        assert!(lines.iter().any(|line| {
            line.contains("public relay candidate detail: require dcutr success: true")
        }));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("public relay candidate detail: dcutr successes: 0"))
        );
        assert!(lines.iter().any(|line| {
            line.contains(
                "public relay candidate detail: dcutr success_reason: no_hole_punch_success",
            )
        }));
        assert!(lines.iter().any(|line| {
            line.contains("public relay candidate detail: dcutr last_error: NoDirectConnection")
        }));
    }

    #[test]
    fn public_relay_probe_lines_include_successful_candidate_config_hint() {
        let relay = peer_id();
        let address = format!("/ip4/203.0.113.10/tcp/4001/p2p/{relay}");
        let report = PublicRelayProbeReport {
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            candidates: vec![PublicRelayCandidateReport {
                address: address.clone(),
                succeeded: true,
                failure_stage: PublicRelayCandidateFailureStage::None,
                error: None,
                bootstrap: Some(relayed_peer_report(1, 1)),
                elapsed_millis: 1_250,
            }],
        };

        let lines = report.lines();

        assert!(report.succeeded());
        assert!(lines.contains(
            &"public relay candidate failure stages: candidate_setup 0 relay_reservation 0 relayed_peer_circuit 0 dcutr_success 0".to_owned()
        ));
        assert!(lines.contains(&format!(
            "public relay candidate config: relay_peer {relay}={address} relay_reservation {address}/p2p-circuit"
        )));
    }

    #[test]
    fn public_relay_candidate_diagnosis_classifies_bootstrap_failures() {
        let relay = peer_id();
        let address = format!("/ip4/203.0.113.10/tcp/4001/p2p/{relay}");
        let failed_candidate = |failure_stage, bootstrap| PublicRelayCandidateReport {
            address: address.clone(),
            succeeded: false,
            failure_stage,
            error: Some("probe failed".to_owned()),
            bootstrap: Some(bootstrap),
            elapsed_millis: 1000,
        };

        assert_eq!(
            failed_candidate(
                PublicRelayCandidateFailureStage::RelayReservation,
                relay_report(vec![RelayReservationCheck {
                    relay_peer_id: relay,
                    address: address.clone(),
                    accepted: false,
                    relayed_listen_address: false,
                }]),
            )
            .diagnosis(),
            PublicRelayCandidateDiagnosis::RelayReservationNotAccepted
        );
        assert_eq!(
            failed_candidate(
                PublicRelayCandidateFailureStage::RelayedPeerCircuit,
                relayed_peer_report(1, 0),
            )
            .diagnosis(),
            PublicRelayCandidateDiagnosis::RelayedPeerCircuitNotConnected
        );
        assert_eq!(
            failed_candidate(
                PublicRelayCandidateFailureStage::DcutrSuccess,
                dcutr_success_report(true, 0, 0, 1, Some("NoDirectConnection".to_owned())),
            )
            .diagnosis(),
            PublicRelayCandidateDiagnosis::DcutrNoHolePunchSuccess
        );
        assert_eq!(
            failed_candidate(
                PublicRelayCandidateFailureStage::DcutrSuccess,
                dcutr_success_report(true, 1, 0, 0, None),
            )
            .diagnosis(),
            PublicRelayCandidateDiagnosis::DcutrMissingDirectConnection
        );
    }

    #[test]
    fn public_relay_candidate_remaining_reports_stage_timeout() {
        let remaining = public_relay_candidate_remaining(Instant::now());
        let error = PublicRelayProbeFailure::candidate_timeout(
            PublicRelayCandidateFailureStage::DcutrSuccess,
        );

        assert_eq!(remaining, None);
        assert_eq!(error.stage, PublicRelayCandidateFailureStage::DcutrSuccess);
        assert_eq!(
            error.message,
            "candidate timeout exhausted before dcutr success check"
        );
    }

    #[test]
    fn public_relay_candidate_remaining_returns_remaining_budget() {
        let remaining = public_relay_candidate_remaining(Instant::now() + Duration::from_secs(1))
            .expect("future deadline should have budget");

        assert!(remaining > Duration::ZERO);
        assert!(remaining <= Duration::from_secs(1));
    }

    #[test]
    fn public_relay_candidate_config_hint_requires_direct_peer_address() {
        let relay = peer_id();
        let address = format!("/ip4/203.0.113.10/tcp/4001/p2p/{relay}");

        let hint = public_relay_candidate_config_hint(&address).expect("config hint");

        assert_eq!(
            hint,
            PublicRelayCandidateConfigHint {
                relay_peer_arg: format!("{relay}={address}"),
                relay_reservation: format!("{address}/p2p-circuit"),
            }
        );
        assert!(public_relay_candidate_config_hint("/ip4/203.0.113.10/tcp/4001").is_none());
        assert!(public_relay_candidate_config_hint("not a multiaddr").is_none());
    }

    #[tokio::test]
    async fn external_relay_reservation_timeout_reports_missing_steps() {
        let relay = peer_id();
        let mut listener_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: relay_test_discovery(),
        })
        .expect("listener node");
        let relayed_address: Multiaddr =
            format!("/memory/9/p2p/{relay}/p2p-circuit/p2p/{}", peer_id())
                .parse()
                .expect("relayed address");

        let error = wait_for_external_relay_reservation(
            &mut listener_node,
            relayed_address,
            relay,
            Duration::from_millis(1),
        )
        .await
        .expect_err("reservation should time out");

        assert!(!error.connected_to_relay);
        assert!(!error.reservation_accepted);
        assert!(!error.relayed_listen_address_observed);
        assert_eq!(error.last_error, None);
        assert_eq!(
            error.error_message(),
            "relay reservation timed out connected false accepted false relayed_listen_address false last_error none"
        );
    }

    #[tokio::test]
    async fn public_relay_scan_discovers_local_relay_candidate() {
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: relay_test_discovery(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node)
            .await
            .with_p2p(relay_peer)
            .expect("relay address");
        relay_node.swarm.add_external_address(relay_address.clone());
        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });

        let config = config_with_bootstrap_peer(relay_peer, &relay_address);
        let report = scan_public_relay_candidates(&config, Duration::from_secs(10), 4)
            .await
            .expect("relay scan");

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.scanned_bootstrap_peers, 1);
        assert_eq!(report.relay_capable_peers, 1);
        assert!(
            report
                .candidates
                .iter()
                .any(|candidate| candidate.address == relay_address.to_string())
        );
        assert!(report.lines().contains(&"public relay scan: ok".to_owned()));
    }

    #[test]
    fn public_relay_scan_report_includes_kademlia_routing_peers() {
        let bootstrap = peer_id();
        let routing_relay = peer_id();
        let bootstrap_address: Multiaddr = format!("/ip4/203.0.113.10/tcp/4001/p2p/{bootstrap}")
            .parse()
            .expect("bootstrap address");
        let routing_address: Multiaddr = format!("/ip4/203.0.113.20/tcp/4001/p2p/{routing_relay}")
            .parse()
            .expect("routing address");
        let bootstrap_peers = vec![(bootstrap, bootstrap_address.clone())];
        let mut identified_peers = HashMap::new();
        identified_peers.insert(
            routing_relay,
            PublicRelayIdentifyResult {
                relay_hop: true,
                candidate_addresses: 1,
            },
        );
        let poll = PublicRelayScanPollResult {
            scan_peers: vec![
                (bootstrap, bootstrap_address),
                (routing_relay, routing_address.clone()),
            ],
            connected_bootstrap_peers: HashSet::from([bootstrap]),
            identified_peers,
            dial_failures: Vec::new(),
            candidates: vec![(routing_relay, routing_address.clone())],
            discovered_routing_peers: 1,
            dialed_routing_peers: 1,
            closest_peer_lookup_started: true,
            closest_peer_lookup_finished: true,
            closest_peer_results: 2,
            closest_peer_errors: 0,
        };

        let report = public_relay_scan_report(&bootstrap_peers, &poll);
        let lines = report.lines();

        assert!(report.succeeded());
        assert_eq!(report.scanned_bootstrap_peers, 1);
        assert_eq!(report.scanned_peers, 2);
        assert_eq!(report.discovered_routing_peers, 1);
        assert_eq!(report.dialed_routing_peers, 1);
        assert!(report.closest_peer_lookup_started);
        assert!(report.closest_peer_lookup_finished);
        assert_eq!(report.closest_peer_results, 2);
        assert_eq!(report.closest_peer_errors, 0);
        assert_eq!(report.identified_peers, 1);
        assert_eq!(report.relay_capable_peers, 1);
        assert!(lines.contains(&"public relay scan total_peers: 2".to_owned()));
        assert!(lines.contains(&"public relay scan routing_peers: 1 dialed 1".to_owned()));
        assert!(lines.contains(
            &"public relay scan closest_peer_lookup: started true finished true results 2 errors 0"
                .to_owned()
        ));
        assert!(lines.iter().any(|line| {
            line.contains(&routing_relay.to_string())
                && line.contains("relay_hop true")
                && line.contains(&routing_address.to_string())
        }));
    }

    #[test]
    fn public_relay_scan_waits_for_active_closest_peer_lookup_at_candidate_limit() {
        let waiting_scan = relay_scan_poll_at_candidate_limit(false);
        let finished_scan = relay_scan_poll_at_candidate_limit(true);

        assert!(should_continue_public_relay_scan(
            5,
            8,
            public_relay_kademlia_lookup_state(true, &waiting_scan),
            true,
            &waiting_scan,
        ));
        assert!(!should_continue_public_relay_scan(
            5,
            8,
            public_relay_kademlia_lookup_state(true, &finished_scan),
            true,
            &finished_scan,
        ));
    }

    #[test]
    fn public_relay_candidates_replace_duplicate_peer_when_full() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let relay_c = peer_id();
        let first_relay_addresses = [
            public_relay_test_address(relay_a, 10),
            public_relay_test_address(relay_a, 11),
        ];
        let second_relay_address = public_relay_test_address(relay_b, 20);
        let new_relay_address = public_relay_test_address(relay_c, 30);
        let mut candidates = vec![
            (relay_a, first_relay_addresses[0].clone()),
            (relay_a, first_relay_addresses[1].clone()),
            (relay_b, second_relay_address.clone()),
        ];

        assert!(add_public_relay_candidate(
            &mut candidates,
            3,
            relay_c,
            new_relay_address.clone()
        ));

        assert_eq!(candidates.len(), 3);
        assert!(candidates.contains(&(relay_a, first_relay_addresses[0].clone())));
        assert!(candidates.contains(&(relay_b, second_relay_address)));
        assert!(candidates.contains(&(relay_c, new_relay_address)));
        assert!(!candidates.contains(&(relay_a, first_relay_addresses[1].clone())));
    }

    #[test]
    fn public_relay_candidates_keep_distinct_peers_when_full() {
        let relays = [peer_id(), peer_id(), peer_id(), peer_id()];
        let mut candidates = vec![
            (relays[0], public_relay_test_address(relays[0], 10)),
            (relays[1], public_relay_test_address(relays[1], 20)),
            (relays[2], public_relay_test_address(relays[2], 30)),
        ];
        let original_candidates = candidates.clone();

        assert!(!add_public_relay_candidate(
            &mut candidates,
            3,
            relays[3],
            public_relay_test_address(relays[3], 40)
        ));

        assert_eq!(candidates, original_candidates);
    }

    #[tokio::test]
    #[ignore = "requires P2P_VPN_LIVE_RELAY_MULTIADDRS or P2P_VPN_LIVE_RELAY_MULTIADDR for a reachable public libp2p relay"]
    async fn bootstrap_check_can_probe_live_public_relayed_peer_circuit() {
        let relay_addresses = live_relay_addresses();
        if relay_addresses.is_empty() {
            eprintln!(
                "skipping live public relay smoke: P2P_VPN_LIVE_RELAY_MULTIADDRS and P2P_VPN_LIVE_RELAY_MULTIADDR are not set"
            );
            return;
        }

        let timeout = live_relay_timeout();
        let mut failures = Vec::new();
        for relay_address in relay_addresses {
            let report = check_public_relay_candidates(
                std::slice::from_ref(&relay_address),
                PublicRelayProbeMode::RelayedPeerCircuit,
                timeout,
            )
            .await;
            if report.succeeded() {
                eprintln!("live public relay circuit smoke passed through {relay_address}");
                return;
            }
            failures.push(report.lines().join("\n"));
        }

        panic!(
            "no live public relay candidate completed relayed peer circuit smoke:\n{}",
            failures.join("\n")
        );
    }

    #[tokio::test]
    #[ignore = "requires P2P_VPN_LIVE_RELAY_MULTIADDRS or P2P_VPN_LIVE_RELAY_MULTIADDR and a network path where DCUtR can complete"]
    async fn bootstrap_check_can_probe_live_public_dcutr_success() {
        let relay_addresses = live_relay_addresses();
        if relay_addresses.is_empty() {
            eprintln!(
                "skipping live public DCUtR smoke: P2P_VPN_LIVE_RELAY_MULTIADDRS and P2P_VPN_LIVE_RELAY_MULTIADDR are not set"
            );
            return;
        }

        let timeout = live_relay_timeout();
        let mut failures = Vec::new();
        for relay_address in relay_addresses {
            let report = check_public_relay_candidates(
                std::slice::from_ref(&relay_address),
                PublicRelayProbeMode::DcutrSuccess,
                timeout,
            )
            .await;
            if report.succeeded() {
                eprintln!("live public DCUtR smoke passed through {relay_address}");
                return;
            }
            failures.push(report.lines().join("\n"));
        }

        panic!(
            "no live public relay candidate completed DCUtR smoke:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn bootstrap_check_lines_report_ipfs_compatible_thresholds() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let report = BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::All,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
                dcutr_success: true,
                relayed_peer_circuits: true,
                membership_records: false,
            },
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ipfs_compatible: true,
            dcutr: BootstrapDcutrCheck {
                enabled: true,
                ready: false,
                successes: 0,
                direct_connections: 0,
                failures: 1,
                last_error: Some("HandshakeTimedOut".to_owned()),
            },
            configured_bootstrap_peers: 2,
            connected_bootstrap_peers: 1,
            dial_failures: 1,
            configured_relay_reservations: 1,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 1,
            connected_relayed_peer_circuits: 0,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered: 2,
            autonat_status: BootstrapAutoNatStatus::Private,
            kademlia: BootstrapKademliaCheck {
                bootstrap_started: true,
                rendezvous_lookup_started: true,
                rendezvous_advertise_started: true,
            },
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: vec![BootstrapPeerCheck {
                peer_id: peer,
                address: "/dnsaddr/bootstrap.libp2p.io".to_owned(),
                connected: true,
                dial_failures: 0,
                last_error: None,
            }],
            relay_results: vec![RelayReservationCheck {
                relay_peer_id: peer,
                address: "/dns4/relay.example.net/tcp/4001".to_owned(),
                accepted: false,
                relayed_listen_address: false,
            }],
            relayed_peer_results: vec![RelayedPeerCircuitCheck {
                peer_id: peer,
                address: "/dns4/relay.example.net/tcp/4001/p2p/example/p2p-circuit".to_owned(),
                connected: false,
                outbound_circuit: false,
                dial_failures: 1,
                last_error: Some("dial failed".to_owned()),
            }],
        };

        let lines = report.lines();

        assert!(!report.succeeded());
        assert!(lines.contains(&"bootstrap check: failed".to_owned()));
        assert!(lines.contains(&"success threshold: all".to_owned()));
        assert!(lines.contains(&"require relay reservations: true".to_owned()));
        assert!(lines.contains(&"require autonat status: true".to_owned()));
        assert!(lines.contains(&"require dcutr ready: true".to_owned()));
        assert!(lines.contains(&"require dcutr success: true".to_owned()));
        assert!(lines.contains(&"require relayed peer circuits: true".to_owned()));
        assert!(lines.contains(&"require membership records: false".to_owned()));
        assert!(lines.contains(&"ipfs compatible: true".to_owned()));
        assert!(lines.contains(&"dcutr enabled: true".to_owned()));
        assert!(lines.contains(&"dcutr ready: false".to_owned()));
        assert!(lines.contains(&"dcutr readiness_reason: missing_relay_reservation".to_owned()));
        assert!(lines.contains(&"dcutr successes: 0".to_owned()));
        assert!(lines.contains(&"dcutr direct_connections: 0".to_owned()));
        assert!(lines.contains(&"dcutr success_reason: no_hole_punch_success".to_owned()));
        assert!(lines.contains(&"dcutr failures: 1".to_owned()));
        assert!(lines.contains(&"dcutr last_error: HandshakeTimedOut".to_owned()));
        assert!(
            lines.contains(
                &"relay reservations: 1 accepted 0 relayed_listen_addresses 0".to_owned()
            )
        );
        assert_default_membership_record_lines(&lines);
        assert!(lines.contains(&"autonat probe servers registered: 2".to_owned()));
        assert!(lines.contains(&"autonat status: private".to_owned()));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("accepted false relayed_listen_address false"))
        );
        assert!(lines.iter().any(|line| line.contains("last_error none")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("relayed peer circuit:")
                    && line.contains("connected false")
                    && line.contains("dial_failures 1"))
        );
    }

    fn assert_default_membership_record_lines(lines: &[String]) {
        assert!(lines.contains(&"kademlia membership records configured: 0".to_owned()));
        assert!(lines.contains(&"kademlia membership records publish started: false".to_owned()));
        assert!(lines.contains(&"kademlia membership records publish succeeded: false".to_owned()));
        assert!(lines.contains(&"kademlia membership records lookup started: false".to_owned()));
        assert!(lines.contains(&"kademlia membership records found: 0".to_owned()));
        assert!(lines.contains(&"kademlia membership records verified: 0".to_owned()));
        assert!(lines.contains(&"kademlia membership records accepted: 0".to_owned()));
    }

    #[test]
    fn bootstrap_membership_record_value_accepts_trusted_dht_bundle() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let trust_root = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: issuer.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("trust root");
        let member_record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("member record");
        let mut config = config_with_bootstrap_peer(peer_id(), &"/memory/9".parse().expect("addr"));
        config.network.member_records = vec![trust_root];
        let value = encode_bootstrap_membership_records("lab", None, vec![member_record])
            .expect("encode bundle");

        let (verified, accepted) =
            validate_bootstrap_membership_record_value(&config, None, &[], &value).expect("value");

        assert_eq!(verified, 1);
        assert_eq!(accepted, 1);
    }

    #[test]
    fn live_relay_candidate_parser_accepts_multiple_addresses() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let raw = format!(
            "/dns4/relay-a.example.net/tcp/4001/p2p/{relay_a},\n/ip4/203.0.113.10/tcp/4001/p2p/{relay_b}"
        );

        let relays = parse_public_relay_addresses(&raw).expect("relay candidates");

        assert_eq!(relays.len(), 2);
        assert_eq!(address_peer(&relays[0]), Some(relay_a));
        assert_eq!(address_peer(&relays[1]), Some(relay_b));
        assert!(
            parse_public_relay_addresses("/dns4/relay.example.net/tcp/4001")
                .expect_err("missing peer id should fail")
                .contains("missing /p2p/RELAY")
        );
        assert!(
            parse_public_relay_addresses(&format!(
                "/dns4/relay.example.net/tcp/4001/p2p/{relay_a}/p2p-circuit"
            ))
            .expect_err("relayed address should fail")
            .contains("without /p2p-circuit")
        );

        let too_many = (0..=PUBLIC_RELAY_CANDIDATE_LIMIT)
            .map(|_| format!("/dns4/relay.example.net/tcp/4001/p2p/{}", peer_id()))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            parse_public_relay_addresses(&too_many)
                .expect_err("candidate limit should fail")
                .contains("too many public relay candidates")
        );
    }

    #[test]
    fn live_relay_timeout_defaults_and_clamps() {
        assert_eq!(
            live_relay_timeout_from_env_value(None),
            Duration::from_secs(45)
        );
        assert_eq!(
            live_relay_timeout_from_env_value(Some("not-a-number")),
            Duration::from_secs(45)
        );
        assert_eq!(
            live_relay_timeout_from_env_value(Some("0")),
            Duration::from_secs(1)
        );
        assert_eq!(
            live_relay_timeout_from_env_value(Some("120")),
            Duration::from_mins(2)
        );
    }

    #[test]
    fn public_relay_scan_candidate_filter_keeps_supported_transports() {
        let relay = peer_id();

        assert!(
            public_relay_candidate_address(
                relay,
                &format!("/ip4/203.0.113.10/tcp/4001/p2p/{relay}")
                    .parse()
                    .expect("tcp address"),
            )
            .is_some()
        );
        assert!(
            public_relay_candidate_address(
                relay,
                &format!("/ip4/203.0.113.10/udp/4001/quic-v1/p2p/{relay}")
                    .parse()
                    .expect("quic address"),
            )
            .is_some()
        );
        assert!(
            public_relay_candidate_address(
                relay,
                &format!(
                    "/ip4/203.0.113.10/udp/4001/quic-v1/webtransport/certhash/uEiC_ejWKWaaGWEHa5vt56TbLG694aLAFAI85cE8dQZz5yg/p2p/{relay}"
                )
                .parse()
                .expect("webtransport address"),
            )
            .is_none()
        );
        assert!(
            public_relay_candidate_address(
                relay,
                &format!("/dns/relay.example.net/tcp/443/wss/p2p/{relay}")
                    .parse()
                    .expect("wss address"),
            )
            .is_none()
        );
        assert!(
            public_relay_candidate_address(
                relay,
                &format!(
                    "/ip4/203.0.113.10/udp/4001/webrtc-direct/certhash/uEiBrt91en4fdNjkn9hpSIADo7_4_-q5r_SbCVEYsf7zo3w/p2p/{relay}"
                )
                .parse()
                .expect("webrtc address"),
            )
            .is_none()
        );
    }

    #[test]
    fn bootstrap_check_can_require_observed_autonat_status() {
        assert!(autonat_report(1, BootstrapAutoNatStatus::Private).succeeded());
        assert!(autonat_report(1, BootstrapAutoNatStatus::Public).succeeded());
        assert!(!autonat_report(1, BootstrapAutoNatStatus::Unknown).succeeded());
        assert!(!autonat_report(0, BootstrapAutoNatStatus::Private).succeeded());
    }

    #[test]
    fn bootstrap_check_can_require_dcutr_ready_state() {
        assert!(dcutr_report(true, 1, 1, 1).succeeded());
        assert!(!dcutr_report(false, 1, 1, 1).succeeded());
        assert!(!dcutr_report(true, 1, 0, 1).succeeded());
        assert!(!dcutr_report(true, 1, 1, 0).succeeded());
        assert!(!dcutr_report(true, 0, 0, 0).succeeded());
    }

    #[test]
    fn bootstrap_check_can_require_dcutr_success_event() {
        let success = dcutr_success_report(true, 1, 1, 0, None);
        assert!(success.succeeded());
        assert!(success.lines().iter().any(|line| {
            line.starts_with("direct peer connection address: ") && line.contains("/memory/30")
        }));
        let missing_direct_evidence = dcutr_success_report(true, 1, 0, 0, None);
        assert!(!missing_direct_evidence.succeeded());
        assert!(
            missing_direct_evidence
                .lines()
                .contains(&"dcutr success_reason: missing_direct_connection_evidence".to_owned())
        );
        assert!(!dcutr_success_report(true, 0, 1, 0, None).succeeded());
        assert!(
            !dcutr_success_report(true, 0, 0, 1, Some("NoDirectConnection".to_owned())).succeeded()
        );
        assert!(!dcutr_success_report(false, 1, 1, 0, None).succeeded());
    }

    #[test]
    fn bootstrap_check_lines_report_dcutr_readiness_reason() {
        assert!(
            dcutr_report(true, 1, 1, 1)
                .lines()
                .contains(&"dcutr readiness_reason: ready".to_owned())
        );
        assert!(
            dcutr_report(false, 1, 1, 1)
                .lines()
                .contains(&"dcutr readiness_reason: disabled".to_owned())
        );
        assert!(
            dcutr_report(true, 0, 0, 0)
                .lines()
                .contains(&"dcutr readiness_reason: no_relay_reservations_configured".to_owned())
        );
        assert!(
            dcutr_report(true, 2, 1, 1)
                .lines()
                .contains(&"dcutr readiness_reason: missing_relay_reservation".to_owned())
        );
        assert!(
            dcutr_report(true, 2, 2, 1)
                .lines()
                .contains(&"dcutr readiness_reason: missing_relayed_listen_address".to_owned())
        );
        let mut incomplete = dcutr_report(true, 1, 1, 1);
        incomplete.dcutr.ready = false;
        assert!(
            incomplete
                .lines()
                .contains(&"dcutr readiness_reason: incomplete_readiness_evidence".to_owned())
        );
    }

    #[test]
    fn dcutr_public_probe_listens_on_tcp_and_quic() {
        let addresses = dcutr_probe_listen_address_strings();

        assert_eq!(
            addresses,
            vec![
                "/ip4/0.0.0.0/tcp/0".to_owned(),
                "/ip4/0.0.0.0/udp/0/quic-v1".to_owned(),
            ]
        );
        assert_eq!(dcutr_probe_listen_addresses().len(), addresses.len());
    }

    #[test]
    fn dcutr_success_polling_waits_for_direct_connection_evidence() {
        let now = Instant::now();
        let mut status = PollingStatus {
            threshold: BootstrapCheckThreshold::Any,
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            requirements: BootstrapCheckRequirements {
                dcutr_success: true,
                ..BootstrapCheckRequirements::default()
            },
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 1,
            connected_relayed_peer_circuits: 1,
            direct_connected_relayed_peer_circuits: 0,
            dcutr_successes: 1,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            membership_records: &BootstrapMembershipRecordDhtCheck::default(),
            now,
            deadline: now + Duration::from_secs(1),
        };

        assert!(should_continue_polling(&status));
        status.direct_connected_relayed_peer_circuits = 1;
        assert!(!should_continue_polling(&status));
    }

    #[test]
    fn dcutr_last_error_can_derive_from_connected_peer_direct_dial_failure() {
        let connected_peer = peer_id();
        let other_peer = peer_id();
        let mut poll = BootstrapPollResult::default();
        poll.connected_relayed_peers
            .insert(connected_peer, "/memory/42".parse().expect("multiaddr"));
        poll.relayed_peer_dial_failures
            .push((other_peer, "ignored".to_owned()));
        poll.relayed_peer_dial_failures
            .push((connected_peer, "HandshakeTimedOut".to_owned()));

        let error = dcutr_direct_dial_last_error(
            BootstrapCheckRequirements {
                dcutr_success: true,
                ..BootstrapCheckRequirements::default()
            },
            &poll,
        );

        assert_eq!(error.as_deref(), Some("direct_dial: HandshakeTimedOut"));
    }

    #[test]
    fn bootstrap_check_can_require_relayed_peer_circuits() {
        assert!(relayed_peer_report(1, 1).succeeded());
        assert!(!relayed_peer_report(1, 0).succeeded());
        assert!(!relayed_peer_report(0, 0).succeeded());
    }

    #[test]
    fn relay_readiness_requires_each_relay_to_have_listen_address() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let report = relay_report(vec![
            RelayReservationCheck {
                relay_peer_id: relay_a,
                address: format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay_a}/p2p-circuit"),
                accepted: true,
                relayed_listen_address: true,
            },
            RelayReservationCheck {
                relay_peer_id: relay_b,
                address: format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay_b}/p2p-circuit"),
                accepted: true,
                relayed_listen_address: false,
            },
        ]);

        assert_eq!(report.accepted_relay_reservations, 2);
        assert_eq!(report.relayed_listen_addresses, 2);
        assert!(!report.succeeded());
    }

    async fn next_listen_address(node: &mut crate::runtime::p2p::P2pNode) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = node.swarm.select_next_some().await {
                return address;
            }
        }
    }

    async fn wait_for_relay_reservation(
        relay: &mut crate::runtime::p2p::P2pNode,
        listener: &mut crate::runtime::p2p::P2pNode,
        relayed_address: Multiaddr,
        relay_peer: Libp2pPeerId,
    ) {
        let mut listen_addr_reported = false;
        let mut reservation_accepted = false;

        loop {
            tokio::select! {
                event = relay.swarm.select_next_some() => {
                    let _ = event;
                }
                event = listener.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Relay(
                            relay::client::Event::ReservationReqAccepted {
                                relay_peer_id,
                                renewal,
                                ..
                            },
                        )) if relay_peer_id == relay_peer && !renewal => {
                            reservation_accepted = true;
                        }
                        SwarmEvent::NewListenAddr { address, .. } if address == relayed_address => {
                            listen_addr_reported = true;
                        }
                        _ => {}
                    }
                }
            }

            if listen_addr_reported && reservation_accepted {
                return;
            }
        }
    }

    fn live_relay_addresses() -> Vec<Multiaddr> {
        let raw = env::var(LIVE_RELAY_MULTIADDRS_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var(LIVE_RELAY_MULTIADDR_ENV).ok());
        let Some(raw) = raw else {
            return Vec::new();
        };

        parse_public_relay_addresses(&raw)
            .expect("live relay multiaddr candidates must parse and include /p2p/RELAY")
    }

    fn live_relay_timeout() -> Duration {
        live_relay_timeout_from_env_value(env::var(LIVE_RELAY_TIMEOUT_SECONDS_ENV).ok().as_deref())
    }

    fn live_relay_timeout_from_env_value(raw: Option<&str>) -> Duration {
        let seconds = raw
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(45)
            .max(1);
        Duration::from_secs(seconds)
    }

    fn relay_test_discovery() -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        }
    }

    fn config_with_bootstrap_peer(peer: Libp2pPeerId, address: &Multiaddr) -> Config {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: peer.to_string(),
                    address: address.to_string(),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    fn config_with_relay_reservation(reservation: &Multiaddr) -> Config {
        let mut config = config_with_bootstrap_peer(peer_id(), &"/memory/9".parse().expect("addr"));
        config.network.bootstrap_peers = Vec::new();
        config.network.relay.reservations = vec![reservation.to_string()];
        config
    }

    fn config_with_relayed_peer(peer: Libp2pPeerId, address: &Multiaddr) -> Config {
        config_with_relayed_peer_discovery(peer, address, relay_test_discovery())
    }

    fn config_with_relayed_peer_discovery(
        peer: Libp2pPeerId,
        address: &Multiaddr,
        discovery: DiscoveryConfig,
    ) -> Config {
        let mut config = config_with_bootstrap_peer(peer_id(), &"/memory/9".parse().expect("addr"));
        config.network.bootstrap_peers = Vec::new();
        config.network.discovery = discovery;
        config.peers = vec![PeerConfig {
            id: peer.to_string(),
            name: Some("listener".to_owned()),
            addresses: vec![address.to_string()],
            routes: Vec::new(),
        }];
        config
    }

    fn peer_id() -> Libp2pPeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn public_relay_test_address(peer: Libp2pPeerId, host_octet: u8) -> Multiaddr {
        format!("/ip4/203.0.113.{host_octet}/tcp/4001/p2p/{peer}")
            .parse()
            .expect("public relay test address")
    }

    fn relay_scan_poll_at_candidate_limit(
        closest_peer_lookup_finished: bool,
    ) -> PublicRelayScanPollResult {
        let scan_peers = (0..PUBLIC_RELAY_SCAN_LIMIT - 1)
            .map(|index| {
                (
                    peer_id(),
                    format!("/memory/{}", index + 1)
                        .parse()
                        .expect("scan peer address"),
                )
            })
            .collect::<Vec<_>>();
        let identified_peers = scan_peers
            .iter()
            .map(|(peer, _)| {
                (
                    *peer,
                    PublicRelayIdentifyResult {
                        relay_hop: true,
                        candidate_addresses: 1,
                    },
                )
            })
            .collect();
        let candidates = scan_peers
            .iter()
            .take(8)
            .map(|(peer, _)| (*peer, public_relay_test_address(*peer, 10)))
            .collect();

        PublicRelayScanPollResult {
            scan_peers,
            identified_peers,
            candidates,
            closest_peer_lookup_started: true,
            closest_peer_lookup_finished,
            ..PublicRelayScanPollResult::default()
        }
    }

    fn autonat_report(
        autonat_probe_servers_registered: usize,
        autonat_status: BootstrapAutoNatStatus,
    ) -> BootstrapCheckReport {
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: true,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: false,
                membership_records: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: false,
                ready: false,
                successes: 0,
                direct_connections: 0,
                failures: 0,
                last_error: None,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered,
            autonat_status,
            kademlia: BootstrapKademliaCheck::default(),
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results: Vec::new(),
        }
    }

    fn relay_report(relay_results: Vec<RelayReservationCheck>) -> BootstrapCheckReport {
        let configured_relay_reservations = relay_results.len();
        let accepted_relay_reservations =
            relay_results.iter().filter(|relay| relay.accepted).count();
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: false,
                membership_records: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck::default(),
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations,
            accepted_relay_reservations,
            relayed_listen_addresses: configured_relay_reservations,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: Vec::new(),
            relay_results,
            relayed_peer_results: Vec::new(),
        }
    }

    fn dcutr_report(
        dcutr_enabled: bool,
        configured_relay_reservations: usize,
        accepted_relay_reservations: usize,
        relayed_listen_addresses: usize,
    ) -> BootstrapCheckReport {
        let relay_results: Vec<_> = (0..configured_relay_reservations)
            .map(|index| {
                let relay_peer_id = peer_id();
                RelayReservationCheck {
                    relay_peer_id,
                    address: format!("/memory/{}/p2p/{relay_peer_id}/p2p-circuit", index + 10),
                    accepted: index < accepted_relay_reservations,
                    relayed_listen_address: index < relayed_listen_addresses,
                }
            })
            .collect();
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: true,
                dcutr_success: false,
                relayed_peer_circuits: false,
                membership_records: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: dcutr_enabled,
                ready: dcutr_enabled
                    && relay_reservations_ready(
                        configured_relay_reservations,
                        accepted_relay_reservations,
                        relayed_listen_addresses,
                    ),
                successes: 0,
                direct_connections: 0,
                failures: 0,
                last_error: None,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations,
            accepted_relay_reservations,
            relayed_listen_addresses,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: Vec::new(),
            relay_results,
            relayed_peer_results: Vec::new(),
        }
    }

    fn dcutr_success_report(
        dcutr_enabled: bool,
        dcutr_successes: usize,
        direct_connections: usize,
        dcutr_failures: usize,
        dcutr_last_error: Option<String>,
    ) -> BootstrapCheckReport {
        let direct_connection_addresses = (0..direct_connections)
            .map(|index| format!("{} /memory/{}", peer_id(), index + 30))
            .collect();
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: true,
                relayed_peer_circuits: false,
                membership_records: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: dcutr_enabled,
                ready: false,
                successes: dcutr_successes,
                direct_connections,
                failures: dcutr_failures,
                last_error: dcutr_last_error,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results: Vec::new(),
        }
    }

    fn relayed_peer_report(
        configured_relayed_peer_circuits: usize,
        connected_relayed_peer_circuits: usize,
    ) -> BootstrapCheckReport {
        let relayed_peer_results: Vec<_> = (0..configured_relayed_peer_circuits)
            .map(|index| {
                let target_peer = peer_id();
                let relay_peer = peer_id();
                RelayedPeerCircuitCheck {
                    peer_id: target_peer,
                    address: format!(
                        "/memory/{}/p2p/{relay_peer}/p2p-circuit/p2p/{target_peer}",
                        index + 20
                    ),
                    connected: index < connected_relayed_peer_circuits,
                    outbound_circuit: index < connected_relayed_peer_circuits,
                    dial_failures: usize::from(index >= connected_relayed_peer_circuits),
                    last_error: (index >= connected_relayed_peer_circuits)
                        .then(|| "dial failed".to_owned()),
                }
            })
            .collect();

        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: true,
                membership_records: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck::default(),
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits,
            connected_relayed_peer_circuits,
            relayed_connection_addresses: Vec::new(),
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            membership_records: BootstrapMembershipRecordDhtCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results,
        }
    }

    fn public_dcutr_listener_descriptor() -> PublicDcutrListenerDescriptor {
        let relay_peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let listener_peer = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        PublicDcutrListenerDescriptor {
            schema_version: PublicDcutrListenerDescriptor::SCHEMA_VERSION,
            relay_candidate: format!("/dns4/relay.example.net/tcp/4001/p2p/{relay_peer}"),
            relay_peer: relay_peer.to_owned(),
            listener_peer: listener_peer.to_owned(),
            relayed_address: format!(
                "/dns4/relay.example.net/tcp/4001/p2p/{relay_peer}/p2p-circuit/p2p/{listener_peer}"
            ),
            listen_addresses: vec!["/ip4/0.0.0.0/tcp/0".to_owned()],
            created_unix_seconds: 1_786_230_000,
        }
    }
}
