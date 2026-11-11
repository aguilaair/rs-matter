//! Network scans during BLE commissioning: rs-matter asks (`ScanNetworks`) before the node is on
//! any network, and some commissioners (Apple's) wait for the network they mean to provision to
//! show up. The app answers through `MatterNode.Listener.onThreadScan`.

use std::sync::{Arc, Mutex};

use log::{info, warn};

use rs_matter::dm::clusters::net_comm::{NetworkScanInfo, NetworkType};
use rs_matter::error::Error;

use crate::bridge::Shared;

static NODE: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

/// Route commissioning-time scans to `shared`'s Kotlin side (one node per process).
pub fn register(shared: Arc<Shared>) {
    *NODE.lock().unwrap() = Some(shared);
    rs_matter::stack::wireless::set_commissioning_scanner(Some(scan));
}

/// The [`rs_matter::stack::wireless::CommissioningScanner`] of this crate.
fn scan(
    net_type: NetworkType,
    f: &mut dyn FnMut(&NetworkScanInfo) -> Result<(), Error>,
) -> Result<(), Error> {
    if net_type != NetworkType::Thread {
        return Ok(());
    }
    let Some(shared) = NODE.lock().unwrap().clone() else { return Ok(()) };
    let packed = shared.thread_scan()?;
    let mut n = 0;
    for net in unpack(&packed) {
        f(&NetworkScanInfo::Thread {
            pan_id: net.pan_id,
            ext_pan_id: net.ext_pan_id,
            network_name: net.name,
            channel: net.channel,
            version: net.version,
            ext_addr: net.ext_addr,
            rssi: net.rssi,
            lqi: net.lqi,
        })?;
        n += 1;
    }
    info!("Thread scan during commissioning: {n} network(s) from the app");
    Ok(())
}

struct Net<'a> {
    pan_id: u16,
    ext_pan_id: u64,
    channel: u16,
    rssi: i8,
    lqi: u8,
    version: u8,
    ext_addr: &'a [u8],
    name: &'a str,
}

/// Each network: pan_id u16, ext_pan_id u64, channel u16 (big endian), rssi i8, lqi u8,
/// version u8, ext_addr [8], name_len u8, name (UTF-8). As `MatterNode.packThreadScan` writes.
fn unpack(b: &[u8]) -> Vec<Net<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 24 <= b.len() {
        let name_len = b[i + 23] as usize;
        let Some(name) = b.get(i + 24..i + 24 + name_len).and_then(|n| core::str::from_utf8(n).ok()) else {
            warn!("Malformed Thread scan result from the app");
            break;
        };
        out.push(Net {
            pan_id: u16::from_be_bytes([b[i], b[i + 1]]),
            ext_pan_id: u64::from_be_bytes(b[i + 2..i + 10].try_into().unwrap()),
            channel: u16::from_be_bytes([b[i + 10], b[i + 11]]),
            rssi: b[i + 12] as i8,
            lqi: b[i + 13],
            version: b[i + 14],
            ext_addr: &b[i + 15..i + 23],
            name,
        });
        i += 24 + name_len;
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn unpacks_what_kotlin_packs() {
        let mut b = vec![0x41, 0x08];
        b.extend_from_slice(&0x108e20b33659615bu64.to_be_bytes());
        b.extend_from_slice(&26u16.to_be_bytes());
        b.extend_from_slice(&[(-60i8) as u8, 200, 4]);
        b.extend_from_slice(&[0xba, 0x75, 0x4f, 0x15, 0x5a, 0x49, 0x8d, 0x30]);
        b.push(14);
        b.extend_from_slice(b"ha-thread-4108");
        let n = super::unpack(&b);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].pan_id, 0x4108);
        assert_eq!(n[0].ext_pan_id, 0x108e20b33659615b);
        assert_eq!(n[0].channel, 26);
        assert_eq!(n[0].rssi, -60);
        assert_eq!(n[0].name, "ha-thread-4108");
        assert!(super::unpack(&b[..20]).is_empty());
    }
}
