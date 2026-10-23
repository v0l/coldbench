# wallet

A Bitcoin wallet for hardware signers, in the [bench suite](https://github.com/v0l/bench-suite)
look. It keeps only public keys; every payment is checked and signed on the device.

Pages follow Sparrow's: transactions, send, receive, addresses, UTXOs and settings.

## Devices

| device | connection | tested |
|---|---|---|
| Blockstream Jade | USB serial, PIN through Blockstream's blind pin server | found and unlocked |
| Coldcard Mk3, Mk4, Q | USB HID | builds, not yet on hardware |
| Trezor Model T, Safe 3, One | WebUSB | builds, not yet on hardware |

The devices are driven by `crates/hww`, written here. It does not use Python HWI: each call
reports what the person must do on the device (`PinOnDevice`, `Confirm(..)`) and can be cancelled
while it waits.

On Linux each device needs its udev rules (HWI ships them in `hwilib/udev`) and your user in the
`plugdev` and `dialout` groups.

## Use

```sh
wallet                 # open the window
wallet --devices       # list plugged in devices and exit
wallet --add           # open straight into adding a wallet
wallet --dir DIR       # keep wallets in DIR instead of the data folder
```

1. Plug in and unlock the device, press **add wallet**, pick the script type and read the keys.
2. The wallet scans its history. By default each address is asked of one public server
   operator only (Esplora and Electrum, about ten operators on mainnet), picked by a secret salt
   the wallet keeps, so no single server sees the whole wallet. Up to 8 requests run at once,
   4 per server. Confirmations are checked with merkle proofs against headers with real proof of
   work. Every server still sees your IP; list your own servers in settings to use those instead.
3. **send**: fill in the payment, create it, check it, press **sign on device** and approve on the
   device, then **broadcast**. A Coldcard used air-gapped works through **save psbt** and dropping
   the signed file back on the window.

Wallets are JSON files in the data folder (`~/.local/share/wallet/wallets` on Linux), one per
account, with labels inside.

## Without a device

```sh
cargo run --example seed_demo /tmp/wdemo
wallet --dir /tmp/wdemo
```

writes watch-only wallets of the public BIP84 test seed, one with real history and one with made
up coins.

## Build

```sh
cargo test --workspace
cargo run --release
```

Linux needs `libudev-dev` and `libusb-1.0-0-dev`.
