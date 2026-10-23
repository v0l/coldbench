# Changelog

## [0.1.0] - 2026-10-23

First release. Used with a Jade. Coldcard and Trezor support is written and builds, but has not
been tried on those devices yet.

- A watch-only wallet for Blockstream Jade, Coldcard and Trezor, with Sparrow's pages:
  transactions, send, receive, addresses, UTXOs and settings.
- Talks to each device directly through its own `hww` crate, with no Python HWI. A device that
  is waiting for its PIN says so in the window, and the wait can be cancelled.
- Coin control, freezing, labels, send all, fee picks from the server's estimates, and fee bumps.
- Stonewall privacy spends, picked as "privacy" on the send page: two groups of your coins each
  pay the amount, one to the recipient and one back to you, so it reads as a two person coinjoin.
- Every transaction drawn as a flow of coins, as mempool.space does, on the review page and in
  each transaction's details.
- Sign on the device, or save, copy, load, drop or paste a PSBT for an SD card or another signer.
- Import and export labels and frozen coins in BIP329, the format Sparrow uses, so a wallet
  moves over from Sparrow with its notes.
- Show an address on the device to check it, and sign or verify messages (BIP137).
- Syncs over every public Esplora and Electrum server at once, each address asked of one
  operator only, in parallel, with merkle proofs checked against headers. Or your own servers.

[0.1.0]: https://github.com/v0l/coldbench/releases/tag/v0.1.0
