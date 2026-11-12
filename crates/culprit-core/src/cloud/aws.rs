//! Pure AWS endpoint and regional identity facts, shared by authentication and cloud adapters.
/// `shop.abc123.us-east-1.rds.amazonaws.com` → `us-east-1`.
pub fn region_from_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let rds = labels.iter().rposition(|l| *l == "rds")?;
    let region = labels.get(rds.checked_sub(1)?)?;
    valid_region(region).then(|| region.to_string())
}

pub use crate::aws_region_valid as valid_region;

/// `us-east-1` → `use1`, `eu-central-1` → `euc1`, `ap-southeast-2` → `apse2`, `us-gov-west-1` → `usgw1`.
pub fn region_short(region: &str) -> String {
    region
        .split('-')
        .map(|part| match part {
            "east" => "e".to_string(),
            "west" => "w".to_string(),
            "north" => "n".to_string(),
            "south" => "s".to_string(),
            "central" => "c".to_string(),
            "northeast" => "ne".to_string(),
            "northwest" => "nw".to_string(),
            "southeast" => "se".to_string(),
            "southwest" => "sw".to_string(),
            "gov" => "g".to_string(),
            other => other.to_string(),
        })
        .collect()
}
