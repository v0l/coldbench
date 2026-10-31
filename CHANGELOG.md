# Changelog

## [0.2.2] - 2026-10-31

- egui and eframe 0.37, which keeps the window running on Wayland when its workspace is hidden.

## [0.2.1] - 2026-10-27

- Fixes signing and unlocking with a Jade, which hung on "checking" in 0.2.0: the PIN server
  request panicked because no TLS provider was chosen. One is now chosen at startup, and a device
  job that fails inside a library reports an error and closes the prompt, so Cancel works.
- Regtest: `scripts/regtest.sh up` starts a bitcoind and an Esplora server in containers, on
  127.0.0.1:3002 where a regtest wallet looks by default. `fund`, `mine` and `down` drive it, and
  `cargo test --test regtest -- --ignored` runs the receive, send, confirm and fee bump flow
  against it. `cargo run --example regtest_demo DIR` makes a regtest wallet of the test seed.
- The bands of the flow diagram have soft edges instead of stair steps.

## [0.2.0] - 2026-10-23

- Wallet encryption at rest, as Sparrow has: set a password in Settings, or when adding a
  wallet, and the file is sealed with ChaCha20-Poly1305 under an Argon2id key (64 MiB). The
  xpub, labels and history are unreadable without it. Locked wallets show in the side list and
  ask for the password on opening; it can be changed or removed in Settings. There is no
  recovery: a forgotten password means adding the wallet again from the device.
- Fiat prices: pick a currency in Settings (USD, EUR, GBP, CAD, CHF, AUD, JPY) and the wallet
  loads the price from mempool.space-style servers, racing a few at once, every ten minutes.
  The last price is kept between runs. Off by default, so nothing asks for a price until you
  choose one; mainnet only.
- Fiat shows with its symbol: small and gray under the balance, in its own column on the
  transactions, addresses and UTXOs pages, and beside amounts on the transaction and review
  pages. Values use today's price, not the price on the day.
- Send: a SATS | USD tab on the amount row lets you type the amount in your currency, and shows
  the other beside it.
- The transactions page shows an "unconfirmed" figure only while money is arriving, in place of
  the always-present confirmed and pending.
- The Jade's PIN prompt says to type the BIP39 passphrase on the device when that is switched on.
- Dependencies: `dirs` 7, `minreq` 3, `egui_bench` 0.5.6, and the rest of the lockfile.

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
