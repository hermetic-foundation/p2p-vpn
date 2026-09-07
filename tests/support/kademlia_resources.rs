use std::collections::BTreeMap;

// Frozen production ceilings from controlled_kademlia_config and the phase-1
// ownership audit. These are payload/slot bounds, not process RSS limits.
const BOUNDS: &[(&str, u64)] = &[
    ("routing_entries", 512),
    ("routing_address_bytes", 2 * 1024 * 1024),
    ("query_pool_retained", 32),
    ("query_candidates", 8192),
    ("query_address_bytes", 8 * 1024 * 1024),
    ("query_max_candidates", 256),
    ("query_max_address_bytes", 256 * 1024),
    ("query_payload_bytes", 12 * 1024 * 1024),
    ("query_result_peers", 8192),
    ("query_provider_addresses", 2048),
    ("query_bootstrap_target_slots", 8192),
    ("query_fixed_peer_slots", 8192),
    ("pending_rpc_requests", 256),
    ("pending_rpc_bytes", 1024 * 1024),
    ("background_bounded_jobs", 2),
    ("background_pending_keys", 128),
    ("background_pending_key_bytes", 2 * 1024 * 1024),
    ("background_cursor_bytes", 1024 * 1024),
    ("background_skipped_keys", 64),
    ("background_skipped_key_bytes", 1024 * 1024),
    ("events", 512),
    ("event_bytes", 4 * 1024 * 1024),
];

const HANDLER_BOUNDS: &[(&str, u64)] = &[
    ("pending_requests", 64),
    ("pending_bytes", 256 * 1024),
    ("pending_negotiations", 32),
    ("inbound_streams", 32),
    ("outbound_streams", 32),
    ("queued_rejections", 64),
];

pub fn validate(lines: &[String]) -> Result<(), String> {
    let mut fields = BTreeMap::new();
    for line in lines.iter().filter(|line| line.starts_with("kad_")) {
        let (key, value) = line
            .split_once(' ')
            .ok_or_else(|| format!("invalid resource: {line}"))?;
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("non-numeric resource: {line}"))?;
        if fields.insert(key, value).is_some() {
            return Err(format!("duplicate resource: {key}"));
        }
    }
    for prefix in ["kad_primary", "kad_pairing"] {
        let get = |suffix: &str| {
            fields
                .get(format!("{prefix}_{suffix}").as_str())
                .copied()
                .ok_or_else(|| format!("missing resource: {prefix}_{suffix}"))
        };
        match get("present")? {
            0 if prefix == "kad_pairing" => {
                if fields
                    .keys()
                    .filter(|key| key.starts_with("kad_pairing_"))
                    .count()
                    != 1
                {
                    return Err("absent pairing DHT reported resources".to_owned());
                }
                continue;
            }
            1 => {}
            _ => return Err(format!("invalid presence: {prefix}")),
        }
        for (field, expected) in [
            ("query_pool_limited", 1),
            ("query_pool_capacity", 32),
            ("event_count_limited", 1),
            ("event_limit", 512),
            ("event_bytes_limited", 1),
            ("event_byte_limit", 4 * 1024 * 1024),
        ] {
            if get(field)? != expected {
                return Err(format!("unexpected production limit: {prefix}_{field}"));
            }
        }
        for &(field, ceiling) in BOUNDS {
            if get(field)? > ceiling {
                return Err(format!("{prefix}_{field} exceeds {ceiling}"));
            }
        }
        let retained = get("query_pool_retained")?;
        let retired = get("query_phases_retired")?;
        let cancelled = get("query_phases_cancelled")?;
        if get("query_phases_admitted")?.checked_sub(retired) != Some(retained)
            || get("query_bounded_caches")? != retained
            || get("query_phases_completed")?
                .checked_add(get("query_phases_timed_out")?)
                .and_then(|sum| sum.checked_add(cancelled))
                != Some(retired)
        {
            return Err(format!("inconsistent query lifecycle: {prefix}"));
        }
        if get("query_candidates")? > retained * 256
            || get("query_address_bytes")? > retained * 256 * 1024
        {
            return Err(format!("query caches exceed retained owners: {prefix}"));
        }
        if retained == 0 {
            for field in [
                "query_payload_bytes",
                "query_result_peers",
                "query_provider_addresses",
                "query_bootstrap_target_slots",
                "query_fixed_peer_slots",
                "pending_rpc_requests",
                "pending_rpc_bytes",
                "query_max_candidates",
                "query_max_address_bytes",
                "query_retained_rejected_reports",
            ] {
                if get(field)? != 0 {
                    return Err(format!("{prefix}_{field} outlived query owners"));
                }
            }
        }
        let admitted = get("dial_intents_admitted")?;
        let events = get("events")?;
        let terminal = get("dial_intents_dispatched")?.checked_add(get("dial_intents_discarded")?);
        if get("dial_intents_attempted")? < admitted
            || terminal
                .and_then(|count| admitted.checked_sub(count))
                .is_none_or(|queued| queued > events)
        {
            return Err(format!("inconsistent dial intents: {prefix}"));
        }
        let handlers = get("handlers")?;
        for &(field, ceiling) in HANDLER_BOUNDS {
            let aggregate = get(&format!("handler_{field}"))?;
            if handlers
                .checked_mul(ceiling)
                .is_none_or(|limit| aggregate > limit)
                || get(&format!("handler_peak_{field}"))? > ceiling
            {
                return Err(format!("handler {field} exceeds owner budget: {prefix}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_report() -> Vec<String> {
        let mut fields = BTreeMap::new();
        for &(key, _) in BOUNDS {
            fields.insert(key.to_owned(), 0);
        }
        for key in [
            "handlers",
            "query_bounded_caches",
            "query_phases_admitted",
            "query_phases_retired",
            "query_phases_completed",
            "query_phases_timed_out",
            "query_phases_cancelled",
            "query_retained_rejected_reports",
            "dial_intents_attempted",
            "dial_intents_admitted",
            "dial_intents_dispatched",
            "dial_intents_discarded",
        ] {
            fields.insert(key.to_owned(), 0);
        }
        for &(key, _) in HANDLER_BOUNDS {
            fields.insert(format!("handler_{key}"), 0);
            fields.insert(format!("handler_peak_{key}"), 0);
        }
        for (key, value) in [
            ("present", 1),
            ("query_pool_limited", 1),
            ("query_pool_capacity", 32),
            ("event_count_limited", 1),
            ("event_limit", 512),
            ("event_bytes_limited", 1),
            ("event_byte_limit", 4 * 1024 * 1024),
        ] {
            fields.insert(key.to_owned(), value);
        }
        let mut lines = fields
            .into_iter()
            .map(|(key, value)| format!("kad_primary_{key} {value}"))
            .collect::<Vec<_>>();
        lines.push("kad_pairing_present 0".to_owned());
        lines
    }

    fn change(lines: &mut [String], suffix: &str, value: u64) {
        let key = format!("kad_primary_{suffix} ");
        *lines
            .iter_mut()
            .find(|line| line.starts_with(&key))
            .unwrap() = format!("{key}{value}");
    }

    #[test]
    fn validates_empty_and_independent_pairing_dhts() {
        let mut lines = empty_report();
        assert_eq!(validate(&lines), Ok(()));
        lines.pop();
        lines.extend(
            lines
                .clone()
                .into_iter()
                .map(|line| line.replacen("kad_primary_", "kad_pairing_", 1)),
        );
        assert_eq!(validate(&lines), Ok(()));
    }

    #[test]
    fn rejects_missing_duplicate_and_non_numeric_fields() {
        let mut lines = empty_report();
        lines.retain(|line| !line.starts_with("kad_primary_handlers "));
        assert!(validate(&lines).unwrap_err().contains("missing resource"));
        for malformed in ["kad_primary_handlers 0", "kad_primary_handlers unknown"] {
            let mut lines = empty_report();
            lines.push(malformed.to_owned());
            assert!(validate(&lines).is_err());
        }
    }

    #[test]
    fn rejects_over_budget_and_orphaned_query_state() {
        for (key, value) in [
            ("routing_entries", 513),
            ("query_pool_retained", 33),
            ("query_phases_admitted", 1),
            ("query_phases_retired", 1),
            ("query_payload_bytes", 1),
            ("pending_rpc_requests", 1),
            ("query_bounded_caches", 1),
            ("handler_pending_requests", 1),
            ("handler_peak_pending_requests", 65),
            ("dial_intents_admitted", 1),
        ] {
            let mut lines = empty_report();
            change(&mut lines, key, value);
            assert!(validate(&lines).is_err(), "accepted invalid {key}={value}");
        }
    }
}
