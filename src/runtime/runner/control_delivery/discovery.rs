//! Native targeted-query ownership for finite, non-payload control recovery.

use super::*;
use crate::runtime::recovery_queries::ControlQueryScope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    Providers,
    FindPeer,
    SignedRecord,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) enum LookupState {
    #[default]
    New,
    Pending(Kind),
    NextPeerLookup,
    Finished,
}

impl LookupState {
    pub(super) fn waiting(self) -> bool {
        matches!(self, Self::Pending(_) | Self::NextPeerLookup)
    }
}

impl MembershipRecordSyncs {
    fn owns_discovery_scope(
        &self,
        scope: ControlQueryScope,
        forwarder: &Forwarder,
        now: Instant,
    ) -> bool {
        scope.generation == self.capabilities.generation
            && now < scope.deadline
            && forwarder.authorizes_control_capabilities(scope.peer)
            && self
                .capabilities
                .peers
                .get(&scope.peer)
                .is_some_and(|recipient| {
                    !recipient.exhausted && recipient.acknowledged != scope.generation
                })
            && self
                .capabilities
                .rounds
                .get(&scope.peer)
                .is_some_and(|round| {
                    round.deadline == scope.deadline
                        && matches!(round.lookup, LookupState::Pending(_))
                })
    }

    fn finish_discovery_stage(&mut self, scope: ControlQueryScope, found: bool) {
        if self.capabilities.generation != scope.generation {
            return;
        }
        if let Some(round) = self.capabilities.rounds.get_mut(&scope.peer)
            && round.deadline == scope.deadline
        {
            round.lookup = match round.lookup {
                LookupState::Pending(Kind::Providers) if !found => LookupState::NextPeerLookup,
                _ => LookupState::Finished,
            };
        }
    }
}

pub(crate) fn reconcile(
    syncs: &mut MembershipRecordSyncs,
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    discovered: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    now: Instant,
) {
    syncs.expire_control_work(now, metrics);
    let queries = discovered
        .recovery_queries
        .control_queries()
        .collect::<Vec<_>>();
    for (id, owner) in queries {
        if now >= owner.expires_at || !syncs.owns_discovery_scope(owner.scope, forwarder, now) {
            discovered.recovery_queries.finished(id, now);
            swarm.behaviour_mut().kad.cancel_query(&id);
            syncs.finish_discovery_stage(owner.scope, false);
            log_runtime_event(
                LogLevel::Info,
                "control_discovery_retired",
                &[
                    ("peer", &owner.scope.peer.to_string()),
                    ("generation", &owner.scope.generation.to_string()),
                    (
                        "reason",
                        if now >= owner.expires_at {
                            "deadline"
                        } else {
                            "scope_lost"
                        },
                    ),
                ],
            );
        }
    }
    // Other native cancellation paths (network reset, expiry, authority cleanup)
    // cannot leave a policy phase waiting on a query that no longer exists.
    for (peer, round) in &mut syncs.capabilities.rounds {
        if let LookupState::Pending(kind) = round.lookup
            && !discovered
                .recovery_queries
                .control_queries()
                .any(|(_, owner)| {
                    owner.scope.peer == *peer
                        && owner.scope.generation == syncs.capabilities.generation
                        && owner.scope.deadline == round.deadline
                })
        {
            round.lookup = if kind == Kind::Providers {
                LookupState::NextPeerLookup
            } else {
                LookupState::Finished
            };
        }
    }
}

pub(super) fn drive(
    syncs: &mut MembershipRecordSyncs,
    node: &mut P2pNode,
    forwarder: &Forwarder,
    discovered: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    now: Instant,
) {
    if !node.discovery.kademlia {
        return;
    }
    let public = node
        .swarm
        .behaviour()
        .kad
        .protocol_names()
        .iter()
        .any(|protocol| protocol.as_ref() == PUBLIC_IPFS_KADEMLIA_PROTOCOL);
    for (peer, round) in &mut syncs.capabilities.rounds {
        if now < round.lan_until
            || now >= round.deadline
            || node.swarm.behaviour().control.is_connected(peer)
            || !forwarder.authorizes_control_capabilities(*peer)
            || discovered.lan_first_recovery_active(*peer, now)
        {
            continue;
        }
        let kind = match round.lookup {
            LookupState::New => {
                if public {
                    Kind::Providers
                } else {
                    Kind::SignedRecord
                }
            }
            LookupState::NextPeerLookup => Kind::FindPeer,
            _ => continue,
        };
        let available = if kind == Kind::FindPeer {
            discovered.recovery_queries.can_continue_control(*peer)
        } else {
            discovered.recovery_queries.should_query(*peer, now)
        };
        if !available {
            continue;
        }
        let query = match kind {
            Kind::Providers => {
                let key = crate::runtime::p2p::kademlia_rendezvous_key(
                    &node.network_name,
                    node.membership_tag.as_deref(),
                );
                let key = crate::runtime::p2p::kademlia_provider_wire_key(
                    &node.swarm.behaviour().kad,
                    &key,
                );
                node.swarm.behaviour_mut().kad.try_get_providers(key)
            }
            Kind::FindPeer => node.swarm.behaviour_mut().kad.try_get_closest_peers(*peer),
            Kind::SignedRecord => node.swarm.behaviour_mut().kad.try_get_record(
                crate::runtime::p2p::kademlia_peer_addresses_key(
                    &node.network_name,
                    node.membership_tag.as_deref(),
                    *peer,
                ),
            ),
        };
        let Ok(id) = query else {
            continue;
        };
        round.lookup = LookupState::Pending(kind);
        discovered.recovery_queries.record_control(
            id,
            ControlQueryScope {
                peer: *peer,
                generation: syncs.capabilities.generation,
                deadline: round.deadline,
            },
            now,
        );
        metrics.record_kademlia_provider_lookup();
        log_runtime_event(
            LogLevel::Info,
            "control_discovery_started",
            &[
                ("peer", &peer.to_string()),
                ("generation", &syncs.capabilities.generation.to_string()),
                (
                    "kind",
                    match kind {
                        Kind::Providers => "providers",
                        Kind::FindPeer => "find_peer",
                        Kind::SignedRecord => "signed_record",
                    },
                ),
            ],
        );
    }
}

pub(crate) fn handle_result(
    swarm: &mut Swarm<Behaviour>,
    context: &mut KademliaEventContext<'_>,
    id: kad::QueryId,
    result: &kad::QueryResult,
    last: bool,
    now: Instant,
) -> bool {
    let Some(owner) = context
        .discovered_peer_addresses
        .recovery_queries
        .control_query(&id)
    else {
        return false;
    };
    let scope = owner.scope;
    let live = now < owner.expires_at
        && context
            .membership_record_syncs
            .owns_discovery_scope(scope, context.forwarder, now);
    let mut found = false;
    if live {
        let addresses = match (
            context.membership_record_syncs.capabilities.rounds[&scope.peer].lookup,
            result,
        ) {
            (
                LookupState::Pending(Kind::Providers),
                kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                    key,
                    providers,
                })),
            ) => {
                let expected = crate::runtime::p2p::kademlia_rendezvous_key(
                    &context.local_capabilities.network_name,
                    context.local_capabilities.membership_tag.as_deref(),
                );
                if *key
                    == crate::runtime::p2p::kademlia_provider_wire_key(
                        &swarm.behaviour().kad,
                        &expected,
                    )
                    && providers.contains(&scope.peer)
                {
                    swarm.behaviour_mut().kad.addresses_of_peer(scope.peer)
                } else {
                    Vec::new()
                }
            }
            (
                LookupState::Pending(Kind::FindPeer),
                kad::QueryResult::GetClosestPeers(
                    Ok(kad::GetClosestPeersOk { key, peers })
                    | Err(kad::GetClosestPeersError::Timeout { key, peers, .. }),
                ),
            ) if *key == scope.peer.to_bytes() => peers
                .iter()
                .filter(|peer| peer.peer_id == scope.peer)
                .flat_map(|peer| peer.addrs.iter().cloned())
                .collect(),
            (
                LookupState::Pending(Kind::SignedRecord),
                kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(record))),
            ) if record.record.key
                == crate::runtime::p2p::kademlia_peer_addresses_key(
                    &context.local_capabilities.network_name,
                    context.local_capabilities.membership_tag.as_deref(),
                    scope.peer,
                ) =>
            {
                decode_kademlia_peer_addresses(
                    &context.local_capabilities.network_name,
                    context.local_capabilities.membership_tag.as_deref(),
                    context.previous_membership_tags,
                    &record.record.value,
                    |peer| {
                        peer == scope.peer
                            && context.forwarder.authorizes_control_capabilities(peer)
                    },
                )
                .map_or_else(|_| Vec::new(), |(_, addresses)| addresses)
            }
            _ => Vec::new(),
        };
        let source = if matches!(
            context.membership_record_syncs.capabilities.rounds[&scope.peer].lookup,
            LookupState::Pending(Kind::SignedRecord)
        ) {
            DiscoveredPeerAddressSource::AuthenticatedPeerRecord
        } else {
            DiscoveredPeerAddressSource::PublicDiscovery
        };
        let addresses = accepted_discovery_addresses(
            context.metrics,
            scope.peer,
            addresses.iter(),
            source,
            "control_discovery",
        );
        for address in addresses {
            if address_targets_peer(scope.peer, address)
                && supports_control_address(address)
                && retain_discovered_address(
                    swarm,
                    context.discovered_peer_addresses,
                    context.metrics,
                    scope.peer,
                    address,
                    true,
                )
            {
                // This peer may be a DHT client. Cache its control destination,
                // not a claim that it is a public DHT routing server.
                #[allow(deprecated)]
                swarm
                    .behaviour_mut()
                    .control
                    .add_address(&scope.peer, address.clone());
                context
                    .discovered_peer_addresses
                    .insert(scope.peer, address.clone());
                context.metrics.record_discovered_address_accepted();
                found = true;
            }
        }
    }
    if found || last || !live {
        log_runtime_event(
            LogLevel::Info,
            "control_discovery_completed",
            &[
                ("peer", &scope.peer.to_string()),
                ("generation", &scope.generation.to_string()),
                ("target_found", &found.to_string()),
                ("scope_live", &live.to_string()),
            ],
        );
        context
            .discovered_peer_addresses
            .recovery_queries
            .finished(id, now);
        swarm.behaviour_mut().kad.cancel_query(&id);
        context
            .membership_record_syncs
            .finish_discovery_stage(scope, found);
        context.membership_record_syncs.drive_capabilities(
            swarm,
            context.forwarder,
            context.metrics,
            now,
        );
    }
    true
}

fn supports_control_address(address: &Multiaddr) -> bool {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
    {
        supports_relayed_peer_dial_transport(address)
    } else {
        supports_auto_relay_candidate_transport(address)
    }
}

#[cfg(test)]
mod tests;
