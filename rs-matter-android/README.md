# rs-matter-android

Run a Matter device on Android, with [rs-matter](https://github.com/project-chip/rs-matter): a phone
commissions it over Bluetooth (scan its QR code in Apple Home, Google Home, Home Assistant…), sends it
the home's Thread network or Wi-Fi credentials, and then controls it over the IP network the Android
device is already on.

Status: experimental. Tested on an Android 10 (armeabi-v7a) wall panel with an iPhone (Apple Home,
then shared to Home Assistant): BLE commissioning, the Thread dataset handed to the app, the On/Off
endpoint switched from Home Assistant.

## What you get

- **BLE commissioning** through Android's `BluetoothGattServer` (the Matter BTP service `0xFFF6`), the
  transport itself in Rust.
- **The network credentials** a commissioner sends (`AddOrUpdateThreadNetwork` / `…WiFiNetwork`) in a
  Kotlin callback: a Thread operational dataset, or an SSID and password. What to do with them is the
  app's business: hand the dataset to a Thread border router or radio, join the Wi-Fi network…
- **A Thread scan without a Thread radio**: `ThreadBorderRouters` reports the Thread networks the LAN's
  border routers advertise (`_meshcop._udp`). Apple's commissioner scans before it sends a dataset and
  waits until its network shows up.
- **Endpoints chosen at runtime**: On/Off lights, occupancy, light, temperature and humidity sensors.
- Fabrics, ACLs and attributes persisted under a directory of the app's.

## Use

```kotlin
val node = MatterNode(
    context,
    MatterNode.Config(
        storageDir = File(filesDir, "matter"),
        discriminator = 1234,            // 12 bits; make it per device
        passcode = 31415926,             // per device; not 00000000, 11111111…, 12345678 or 87654321 (Matter Core 5.1.7.1)
        deviceName = "My device",
        endpoints = listOf(
            MatterNode.Endpoint.OnOff(1),
            MatterNode.Endpoint.OccupancySensor(2),
            MatterNode.Endpoint.LightSensor(3),
        ),
    ),
    object : MatterNode.Listener {
        override fun onThreadDataset(dataset: ByteArray): Boolean = myRadio.join(dataset)
        override fun onThreadScan() = borderRouters.networks()
        override fun onOnOff(endpoint: Int, on: Boolean) { /* switch something */ }
        override fun onEvent(kind: String, detail: String) { /* "qr_code", "pairing_code", "state" … */ }
    },
)
node.start()                                  // shows "qr_code" (MT:…) and "pairing_code" events
node.setOccupied(2, true)
node.setIlluminance(3, lux = 140.0)
node.openCommissioningWindow()                // pair again / with another controller
```

The app needs the Bluetooth permissions (`BLUETOOTH_CONNECT` and `BLUETOOTH_ADVERTISE` from Android 12
on, granted before `start()`); the library's manifest declares them, plus `INTERNET` and
`CHANGE_WIFI_MULTICAST_STATE` (take a `WifiManager.MulticastLock` for mDNS).

By default the node is commissionable **over BLE only** while its window is open, like a Thread device
that isn't on any network yet: commissioners only send network credentials to devices they commission
over BLE. `Config.onNetworkCommissioning = true` also advertises it on the LAN.

## Certificates

The node identifies itself with the Matter SDK's **test** device attestation certificate (vendor
`0xFFF1`). Apple Home accepts it with a warning; Home Assistant's Matter Server only with *Enable
test-net DCL usage* on. A product needs its own vendor ID and certificates (a PAA/PAI/DAC chain from
the CSA's Distributed Compliance Ledger); plugging in your own attestation isn't exposed yet.

## Build

`android/` is a Gradle project with the library (`:matter`) and a sample app (`:sample`). The native
library is built by `android/build-rust.sh` (called by Gradle): Rust 1.92+ with the Android targets
(`rustup target add armv7-linux-androideabi aarch64-linux-android x86_64-linux-android
i686-linux-android`) and the Android NDK (`ANDROID_NDK_HOME`, or the newest under the SDK).

```sh
cd android
./gradlew :sample:assembleDebug -PrustAbis=arm64-v8a      # or armeabi-v7a, x86_64, x86 (default: all)
```

Another Gradle project can include `android/matter` as a module (it finds `build-rust.sh` next to it).

## How it fits rs-matter

- `src/gatt.rs`: `GattPeripheral` over the Kotlin `BluetoothGattServer` (JNI), modelled on the Win32
  peripheral: one session at a time, the client that subscribes to C2.
- `src/netctl.rs`: `NetCtl` that hands credentials to the app and reports itself connected once the app
  takes them (rs-matter's connection manager otherwise reconnects in a loop).
- `src/scan.rs`: answers `ScanNetworks` during BLE commissioning through the stack's
  `CommissioningScanner` hook (added in this fork, see [UPSTREAM.md](UPSTREAM.md)).
- `src/endpoints.rs`: the app's endpoints, built at runtime, one handler routing to their clusters.
- `src/node.rs`: the stack (`MatterStack::run_thread_no_coex` over `PreexistingWireless`).

Changes to rs-matter itself, and what upstreaming them involves: [UPSTREAM.md](UPSTREAM.md).

## Licence

Apache-2.0, like rs-matter.
