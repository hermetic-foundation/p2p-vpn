use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Extract only explicitly selected numeric fields, never raw control payloads.
pub fn parse(
    lines: Option<&Value>,
    selected: &[&str],
) -> Result<BTreeMap<String, Option<u64>>, &'static str> {
    let mut result: BTreeMap<_, _> = selected
        .iter()
        .map(|name| ((*name).to_owned(), None))
        .collect();
    let Some(lines) = lines.filter(|value| !value.is_null()) else {
        return Ok(result);
    };
    let lines = lines.as_array().ok_or("control capture is not an array")?;
    if lines.len() > 16384 {
        return Err("too many control lines");
    }
    let mut seen = BTreeSet::new();
    let mut total = 0_usize;
    for line in lines {
        let line = line.as_str().ok_or("control line is not a string")?;
        total = total
            .checked_add(line.len())
            .ok_or("control size overflow")?;
        if total > 1024 * 1024 {
            return Err("control capture exceeds one MiB");
        }
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(slot) = result.get_mut(name) else {
            continue;
        };
        if !seen.insert(name) {
            return Err("duplicate selected metric");
        }
        let value = fields.next().ok_or("selected metric has no value")?;
        if fields.next().is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err("selected metric is not an unsigned integer");
        }
        *slot = Some(value.parse().map_err(|_| "selected metric overflows u64")?);
    }
    Ok(result)
}

#[test]
fn missing_and_zero_are_distinct_and_unselected_data_is_discarded() {
    let selected = ["attempts", "current_only"];
    let baseline = serde_json::json!(["attempts 0", "peer private-key-sensitive-payload"]);
    let parsed = parse(Some(&baseline), &selected).unwrap();
    assert_eq!(parsed["attempts"], Some(0));
    assert_eq!(parsed["current_only"], None);
    assert_eq!(parsed.len(), 2);
    assert!(
        parse(None, &selected)
            .unwrap()
            .values()
            .all(Option::is_none)
    );
    let current = serde_json::json!(["attempts 9", "current_only 3"]);
    assert_eq!(
        parse(Some(&current), &selected).unwrap()["current_only"],
        Some(3)
    );
}

#[test]
fn duplicate_malformed_and_overflowing_selected_fields_fail_closed() {
    for lines in [
        serde_json::json!(["n 1", "n 1"]),
        serde_json::json!(["n -1"]),
        serde_json::json!(["n +1"]),
        serde_json::json!(["n 1 extra"]),
        serde_json::json!(["n 18446744073709551616"]),
        serde_json::json!(["n"]),
        serde_json::json!([false]),
        serde_json::json!({"n":1}),
    ] {
        assert!(parse(Some(&lines), &["n"]).is_err());
    }
    assert_eq!(
        parse(Some(&serde_json::json!(["n 18446744073709551615"])), &["n"]).unwrap()["n"],
        Some(u64::MAX)
    );
}

#[test]
fn control_capture_bounds_apply_even_to_unselected_fields() {
    let oversized = serde_json::json!(["x".repeat(1024 * 1024 + 1)]);
    assert!(parse(Some(&oversized), &["n"]).is_err());
    let many = serde_json::json!(vec![""; 16385]);
    assert!(parse(Some(&many), &["n"]).is_err());
}
