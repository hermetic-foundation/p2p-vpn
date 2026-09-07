use std::mem::size_of;

use libp2p_core::Multiaddr;
use libp2p_identity::PeerId;

use crate::{
    handler::HandlerIn, protocol::KadPeer, record, Event, GetProvidersOk, GetRecordOk,
    InboundRequest, ProviderRecord, QueryResult, Record,
};

fn record_bytes(record: &Record) -> usize {
    record
        .key
        .as_ref()
        .len()
        .saturating_add(record.value.capacity())
}

fn address_bytes(addresses: &Vec<Multiaddr>) -> usize {
    addresses.iter().fold(
        addresses.capacity().saturating_mul(size_of::<Multiaddr>()),
        |bytes, address| bytes.saturating_add(address.len()),
    )
}

fn peers_bytes(peers: &Vec<KadPeer>) -> usize {
    peers.iter().fold(
        peers.capacity().saturating_mul(size_of::<KadPeer>()),
        |bytes, peer| bytes.saturating_add(address_bytes(&peer.multiaddrs)),
    )
}

fn provider_bytes(record: &ProviderRecord) -> usize {
    record
        .key
        .as_ref()
        .len()
        .saturating_add(address_bytes(&record.addresses))
}

pub(super) fn event_bytes(event: &Event) -> Option<usize> {
    Some(match event {
        Event::OutboundQueryProgressed { result, .. } => match result {
            QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders { key, providers })) => key
                .as_ref()
                .len()
                .saturating_add(providers.capacity().saturating_mul(size_of::<PeerId>())),
            QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(record))) => {
                record_bytes(&record.record)
            }
            // Terminal results must be handed off directly, never dropped at queue capacity.
            _ => return None,
        },
        Event::InboundRequest { request } => match request {
            InboundRequest::PutRecord { record, .. } => record.as_ref().map_or(0, record_bytes),
            InboundRequest::AddProvider { record } => record.as_ref().map_or(0, provider_bytes),
            InboundRequest::FindNode { .. }
            | InboundRequest::GetProvider { .. }
            | InboundRequest::GetRecord { .. } => 0,
        },
        // These snapshots already retain aggregate routing reservations.
        Event::RoutingUpdated { .. } | Event::UnroutablePeer { .. } | Event::ModeChanged { .. } => {
            0
        }
        Event::RoutablePeer { address, .. } | Event::PendingRoutablePeer { address, .. } => {
            address.len()
        }
    })
}

pub(super) fn handler_bytes(event: &HandlerIn) -> usize {
    match event {
        HandlerIn::Reset(_) | HandlerIn::ReconfigureMode { .. } => 0,
        HandlerIn::FindNodeReq { key, .. } => key.capacity(),
        HandlerIn::GetProvidersReq { key, .. } | HandlerIn::GetRecord { key, .. } => {
            key.as_ref().len()
        }
        HandlerIn::PutRecord { record, .. } => record_bytes(record),
        HandlerIn::AddProvider { key, provider, .. } => key
            .as_ref()
            .len()
            .saturating_add(address_bytes(&provider.multiaddrs)),
        HandlerIn::FindNodeRes { closer_peers, .. } => peers_bytes(closer_peers),
        HandlerIn::GetProvidersRes {
            closer_peers,
            provider_peers,
            ..
        } => peers_bytes(closer_peers).saturating_add(peers_bytes(provider_peers)),
        HandlerIn::GetRecordRes {
            record,
            closer_peers,
            ..
        } => record
            .as_ref()
            .map_or(0, record_bytes)
            .saturating_add(peers_bytes(closer_peers)),
        HandlerIn::PutRecordRes { key, value, .. } => {
            key.as_ref().len().saturating_add(value.capacity())
        }
    }
}

fn normalize_key(key: &mut record::Key) {
    *key = record::Key::new(key);
}

fn normalize_addresses(addresses: &mut [Multiaddr]) {
    for address in addresses {
        *address = crate::addresses::normalized_address(address);
    }
}

fn normalize_record(record: &mut Record) {
    normalize_key(&mut record.key);
}

fn normalize_peers(peers: &mut [KadPeer]) {
    for peer in peers {
        normalize_addresses(&mut peer.multiaddrs);
    }
}

pub(super) fn normalize_handler(event: &mut HandlerIn) {
    match event {
        HandlerIn::GetProvidersReq { key, .. }
        | HandlerIn::GetRecord { key, .. }
        | HandlerIn::PutRecordRes { key, .. } => normalize_key(key),
        HandlerIn::AddProvider { key, provider, .. } => {
            normalize_key(key);
            normalize_addresses(&mut provider.multiaddrs);
        }
        HandlerIn::PutRecord { record, .. } => normalize_record(record),
        HandlerIn::FindNodeRes { closer_peers, .. } => normalize_peers(closer_peers),
        HandlerIn::GetProvidersRes {
            closer_peers,
            provider_peers,
            ..
        } => {
            normalize_peers(closer_peers);
            normalize_peers(provider_peers);
        }
        HandlerIn::GetRecordRes {
            record,
            closer_peers,
            ..
        } => {
            if let Some(record) = record {
                normalize_record(record);
            }
            normalize_peers(closer_peers);
        }
        HandlerIn::Reset(_) | HandlerIn::ReconfigureMode { .. } | HandlerIn::FindNodeReq { .. } => {
        }
    }
}

pub(super) fn normalize_event(event: &mut Event) {
    match event {
        Event::OutboundQueryProgressed { result, .. } => match result {
            QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(record))) => {
                normalize_record(&mut record.record)
            }
            QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders { key, .. })) => {
                normalize_key(key)
            }
            // Only intermediate record/provider results enter this queue. Terminal
            // results are returned directly by Behaviour::poll, not stored here.
            _ => {}
        },
        Event::InboundRequest {
            request:
                InboundRequest::PutRecord {
                    record: Some(record),
                    ..
                },
        } => normalize_record(record),
        Event::InboundRequest {
            request:
                InboundRequest::AddProvider {
                    record: Some(record),
                },
        } => {
            normalize_key(&mut record.key);
            normalize_addresses(&mut record.addresses);
        }
        Event::RoutablePeer { address, .. } | Event::PendingRoutablePeer { address, .. } => {
            normalize_addresses(std::slice::from_mut(address))
        }
        _ => {}
    }
}
