# Changelog

## [0.1.0]

First release.

- A watch-only wallet for Blockstream Jade, Coldcard and Trezor, with Sparrow's pages:
  transactions, send, receive, addresses, UTXOs and settings.
- Talks to each device directly through its own `hww` crate, with no Python HWI. A device that
  is waiting for its PIN says so in the window, and the wait can be cancelled.
- Coin control, freezing, labels, send all, fee picks from the server's estimates, and fee bumps.
- Sign on the device, or save, copy, load, drop or paste a PSBT for an SD card or another signer.
- Show an address on the device to check it, and sign or verify messages (BIP137).
- Syncs over every public Esplora and Electrum server at once, each address asked of one
  operator only, in parallel, with merkle proofs checked against headers. Or your own servers.

[0.1.0]: https://github.com/v0l/coldbench/releases/tag/v0.1.0
