//! Upload ordering from existing metadata. Relative labels are approximate;
//! missing/localized labels remain unknown, never fabricated dates.
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
pub fn metadata(v: &Value) -> Option<u64> {
    v["timestamp"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .map(|n| n as u64)
        .or_else(|| {
            let d = v["upload_date"].as_str()?;
            if d.len() != 8 || !d.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let year = d[..4].parse::<i64>().ok()?;
            let month = d[4..6].parse::<i64>().ok()?;
            let day = d[6..].parse::<i64>().ok()?;
            if !(1970..=9999).contains(&year)
                || !(1..=12).contains(&month)
                || !(1..=31).contains(&day)
            {
                return None;
            }
            // Gregorian civil date → days since Unix epoch, using 400-year eras.
            let y = year - i64::from(month <= 2);
            let era = y.div_euclid(400);
            let yoe = y - era * 400;
            let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
            let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
            u64::try_from(days * 86400).ok()
        })
}
pub fn relative(s: &str) -> Option<u64> {
    let lower = s.to_ascii_lowercase();
    if !lower.ends_with(" ago") {
        return None;
    }
    let fields: Vec<_> = lower.split_whitespace().collect();
    let i = fields.iter().position(|s| s.parse::<u64>().is_ok())?;
    let n = fields[i].parse::<u64>().ok()?;
    let unit = *fields.get(i + 1)?;
    let seconds = match unit.trim_end_matches('s') {
        "second" => 1,
        "minute" => 60,
        "hour" => 3600,
        "day" => 86400,
        "week" => 604800,
        "month" => 2629800,
        "year" => 31557600,
        _ => return None,
    };
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .checked_sub(n.checked_mul(seconds)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates_and_unknowns() {
        assert_eq!(
            metadata(&serde_json::json!({"upload_date":"19700101"})),
            Some(0)
        );
        assert_eq!(
            metadata(&serde_json::json!({"upload_date":"20000101"})),
            Some(946684800)
        );
        assert!(relative("2 years ago").is_some());
        assert!(relative("2M views").is_none());
        assert!(relative("LIVE").is_none());
    }
}
