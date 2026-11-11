//! The network controller of an Android node: the device is already on an IP network (Wi-Fi or
//! Ethernet, Android's), so it doesn't connect anywhere itself. The credentials a commissioner sends
//! (a Thread operational dataset, or a Wi-Fi SSID and password) go to the app's Kotlin callback,
//! which decides what to do with them: hand a dataset to a Thread border router or radio, keep it,
//! join a Wi-Fi network…

use std::sync::Arc;

use log::{info, warn};

use rs_matter::dm::clusters::net_comm::{NetCtl, NetCtlError, NetworkScanInfo, NetworkType, WirelessCreds};
use rs_matter::dm::clusters::thread_diag::ThreadDiag;
use rs_matter::dm::clusters::wifi_diag::{WifiDiag, WirelessDiag};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::error::Error;

use crate::bridge::Shared;

/// [`NetCtl`] that hands credentials to the Kotlin `MatterNode` (`onThreadDataset`,
/// `onWifiCredentials`). Scanning reports no networks.
pub struct AndroidNetCtl {
    net_type: NetworkType,
    shared: Arc<Shared>,
}

impl AndroidNetCtl {
    pub fn new(net_type: NetworkType, shared: Arc<Shared>) -> Self {
        Self { net_type, shared }
    }
}

impl NetCtl for AndroidNetCtl {
    fn net_type(&self) -> NetworkType {
        self.net_type
    }

    fn set_enabled(&self, _enabled: bool) -> Result<(), Error> {
        Ok(())
    }

    async fn scan<F>(&self, _network: Option<&[u8]>, _f: F) -> Result<(), NetCtlError>
    where
        F: FnMut(&NetworkScanInfo) -> Result<(), Error>,
    {
        // The node can't scan Thread or Wi-Fi from here; commissioners that already have the
        // credentials (Home Assistant, Apple, Google with Thread) don't need it.
        Ok(())
    }

    async fn connect(&self, creds: &WirelessCreds<'_>) -> Result<(), NetCtlError> {
        let accepted = match creds {
            WirelessCreds::Thread { dataset_tlv } => {
                info!("Thread operational dataset received ({} bytes)", dataset_tlv.len());
                self.shared.thread_dataset(dataset_tlv)?
            }
            WirelessCreds::Wifi { ssid, pass } => {
                info!("Wi-Fi credentials received (SSID {} bytes)", ssid.len());
                self.shared.wifi_credentials(ssid, pass)?
            }
        };
        if accepted {
            Ok(())
        } else {
            warn!("The app refused the network credentials");
            Err(NetCtlError::OtherConnectionFailure)
        }
    }
}

impl NetChangeNotif for AndroidNetCtl {
    async fn wait_changed(&self) {
        core::future::pending().await
    }
}

impl WirelessDiag for AndroidNetCtl {}
impl WifiDiag for AndroidNetCtl {}
impl ThreadDiag for AndroidNetCtl {}
