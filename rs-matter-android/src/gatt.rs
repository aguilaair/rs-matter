//! The Matter GATT peripheral on Android: the BTP session driven over the Kotlin side's
//! `BluetoothGattServer` (C1 writes in, C2 indications out). Modelled on the Win32 peripheral in
//! `rs-matter-transport` (`btp/gatt/win32.rs`): one client at a time, the one that subscribes to C2.

use std::sync::Arc;

use log::{info, warn};

use rs_matter::error::Error;
use rs_matter::stack::ble::GattPeripheral;
use rs_matter::transport::network::btp::{AdvData, Btp};
use rs_matter::transport::network::BtAddr;
use rs_matter::utils::futures::{select, select3, Coalesce, Either};

use crate::bridge::Shared;

/// Runs its closure when dropped, also when the future is cancelled.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// [`GattPeripheral`] over Android's `BluetoothGattServer`, through the Kotlin `MatterNode`.
pub struct AndroidGattPeripheral {
    shared: Arc<Shared>,
}

impl AndroidGattPeripheral {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl GattPeripheral for AndroidGattPeripheral {
    async fn run(&mut self, btp: &Btp, _service_name: &str, service_adv: &AdvData) -> Result<(), Error> {
        let shared = self.shared.clone();
        // Android takes the service data without the UUID; the Kotlin side adds the 0xFFF6 UUID.
        let data: Vec<u8> = service_adv.service_payload_iter().collect();

        // Old events from a previous run mean nothing to this one.
        while shared.c1.1.try_recv().is_ok() {}
        while shared.c2.1.try_recv().is_ok() {}
        while shared.indicated.1.try_recv().is_ok() {}

        shared.ble_start(&data)?;
        let _stop = OnDrop(|| shared.ble_stop());
        info!("Advertising the Matter GATT BTP service");

        serve_sessions(btp, &shared).await
    }
}

/// Serve the clients that subscribe to C2, one at a time.
async fn serve_sessions(btp: &Btp, shared: &Shared) -> Result<(), Error> {
    loop {
        let peer = wait_subscribed(shared).await?;
        info!("Peer {} subscribed to C2", peer);

        btp.reset();

        select3(
            wait_complete(btp, shared, peer),
            process_writes(btp, shared, peer),
            process_indications(btp, shared, peer, &mut [0; 512]),
        )
        .coalesce()
        .await?;

        shared.ble_disconnect(peer);
        while shared.c1.1.try_recv().is_ok() {}
    }
}

async fn wait_subscribed(shared: &Shared) -> Result<BtAddr, Error> {
    loop {
        let (addr, subscribed) = shared.c2.1.recv().await.map_err(|_| closed())?;
        if subscribed {
            return Ok(addr);
        }
    }
}

/// Until `peer` unsubscribes from C2 (or disconnects), or the BTP session times out.
async fn wait_complete(btp: &Btp, shared: &Shared, peer: BtAddr) -> Result<(), Error> {
    let unsubscribed = async {
        loop {
            let (addr, subscribed) = shared.c2.1.recv().await.map_err(|_| closed())?;
            if addr == peer && !subscribed {
                return Ok(());
            }
            if subscribed && addr != peer {
                warn!("Ignoring {} subscribing to C2: {} is in session", addr, peer);
            }
        }
    };
    match select(unsubscribed, btp.wait_timeout()).await {
        Either::First(r) => {
            info!("Peer unsubscribed");
            r
        }
        Either::Second(_) => {
            info!("Timeout while waiting for data from the peer");
            Ok(())
        }
    }
}

/// Pass the writes of `peer` to C1 to the BTP session; other clients are ignored.
async fn process_writes(btp: &Btp, shared: &Shared, peer: BtAddr) -> Result<(), Error> {
    loop {
        let w = shared.c1.1.recv().await.map_err(|_| closed())?;
        if w.addr == peer {
            btp.process_incoming(Some(w.mtu), w.addr, &w.value)?;
        } else {
            warn!("Ignoring a write to C1 from {}, not in session", w.addr);
        }
    }
}

/// Indicate new BTP data on C2 to `peer`, one indication at a time (Android allows one in
/// flight). A failed indication ends the session.
async fn process_indications(btp: &Btp, shared: &Shared, peer: BtAddr, buf: &mut [u8]) -> Result<(), Error> {
    loop {
        let len = btp.process_outgoing(None, buf)?;
        if len == 0 {
            btp.wait_outgoing().await;
            continue;
        }
        if !shared.ble_indicate(peer, &buf[..len])? {
            warn!("Bluetooth: cannot indicate on C2 to {}", peer);
            return Ok(());
        }
        if !shared.indicated.1.recv().await.map_err(|_| closed())? {
            warn!("Bluetooth: the indication on C2 to {} failed", peer);
            return Ok(());
        }
    }
}

fn closed() -> Error {
    rs_matter::error::ErrorCode::BtpError.into()
}
