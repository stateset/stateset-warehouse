//! Offline capture helper: canonicalize one original REST command envelope.
//! No network, credentials, database, or warehouse mutation is involved.
use sha2::{Digest, Sha256};
use stateset_warehouse::store::Request;
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    io::stdin().take(256 * 1024 + 1).read_to_end(&mut raw)?;
    if raw.len() > 256 * 1024 {
        return Err("Warehouse command envelope exceeds 256 KiB".into());
    }
    let request: Request =
        serde_json::from_slice(&raw).map_err(|_| "Invalid warehouse command envelope")?;
    let canonical = serde_json::to_vec(&request)?;
    let fingerprint = format!("{:x}", Sha256::digest(&canonical));
    println!(
        "{}",
        serde_json::json!({"canonical_json":String::from_utf8(canonical)?,"digest":fingerprint})
    );
    Ok(())
}
