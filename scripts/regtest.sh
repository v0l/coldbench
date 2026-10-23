#!/usr/bin/env bash
# A regtest chain for trying coldbench's send, confirm and fee bump flow: bitcoind plus an Esplora
# server (electrs) on 127.0.0.1:3002, which is where a regtest wallet looks by default.
#
#   scripts/regtest.sh up        start both, mine 101 blocks so there are coins to spend
#   scripts/regtest.sh fund ADDR [BTC]   pay an address from the miner wallet (default 1)
#   scripts/regtest.sh mine [N]  mine N blocks (default 1), which confirms what is pending
#   scripts/regtest.sh down      remove the containers
#
# Esplora indexes the mempool a few seconds after the node sees a transaction.
set -euo pipefail
NET=coldbench-regtest
NODE=$NET-bitcoind
ESPLORA=$NET-esplora
cli() { docker exec "$NODE" bitcoin-cli -regtest -rpcuser=u -rpcpassword=p -rpcwallet=miner "$@"; }

case "${1:-}" in
up)
	docker network create $NET >/dev/null 2>&1 || true
	docker rm -f $NODE $ESPLORA >/dev/null 2>&1 || true
	docker run -d --name $NODE --network $NET -p 127.0.0.1:18443:18443 --entrypoint bitcoind \
		polarlightning/bitcoind:30.0 -regtest -server -txindex=1 -fallbackfee=0.0001 \
		-rpcuser=u -rpcpassword=p -rpcbind=0.0.0.0 -rpcallowip=0.0.0.0/0 -rpcport=18443 >/dev/null
	until docker exec $NODE bitcoin-cli -regtest -rpcuser=u -rpcpassword=p getblockcount >/dev/null 2>&1; do sleep 1; done
	docker exec $NODE bitcoin-cli -regtest -rpcuser=u -rpcpassword=p createwallet miner >/dev/null
	# Block 0 alone counts as initial sync to electrs, so there have to be blocks first.
	cli generatetoaddress 101 "$(cli getnewaddress)" >/dev/null
	docker run -d --name $ESPLORA --network $NET -p 127.0.0.1:3002:3002 -p 127.0.0.1:60401:60401 \
		ghcr.io/vulpemventures/electrs:latest -v --network regtest --jsonrpc-import \
		--daemon-rpc-addr $NODE:18443 --cookie u:p --http-addr 0.0.0.0:3002 \
		--electrum-rpc-addr 0.0.0.0:60401 --cors '*' >/dev/null
	until [ "$(curl -s -m 2 localhost:3002/blocks/tip/height)" = "$(cli getblockcount)" ]; do sleep 1; done
	echo "regtest is up: Esplora on http://127.0.0.1:3002, node RPC on 127.0.0.1:18443 (u:p)"
	;;
fund)
	cli sendtoaddress "${2:?an address}" "${3:-1}"
	;;
mine)
	cli generatetoaddress "${2:-1}" "$(cli getnewaddress)" >/dev/null
	cli getblockcount
	;;
down)
	docker rm -f $NODE $ESPLORA >/dev/null 2>&1 || true
	docker network rm $NET >/dev/null 2>&1 || true
	;;
*)
	sed -n '2,10p' "$0"
	exit 1
	;;
esac
