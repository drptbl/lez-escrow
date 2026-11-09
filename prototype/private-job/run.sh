#!/usr/bin/env bash
# PROTOTYPE, throwaway. Sets up two wallets on a running local standalone LEZ v0.3.1 sequencer
# (dev mode, port 3040), deploys the escrow guest and runs the private-job prototype.
#
#   LEZ=<logos-execution-zone checkout at v0.3.1, with target/release/wallet built>
#   RISC0_DEV_MODE=1 prototype/private-job/run.sh
set -euo pipefail
: "${LEZ:?set LEZ to a logos-execution-zone v0.3.1 checkout}"
W=$LEZ/target/release/wallet
WORK=${WORK:-$(mktemp -d -t private-job-prototype)}
ELF=methods/guest/target/riscv32im-risc0-zkvm-elf/docker/logos_escrow.bin
# Debug genesis keys from LEZ's own Justfile (`wallet-import-test-accounts`).
GENESIS_KEY=7f273098f25b71e6c005a9519f2678da8d1c7f01f6a27778e2d9948abdf901fb
export RISC0_DEV_MODE=${RISC0_DEV_MODE:-1}

wallet() { local home=$1; shift; LEE_WALLET_HOME_DIR=$home "$W" "$@"; }
new_wallet() {
  mkdir -p "$1"
  echo prototype | wallet "$1" change-network local >/dev/null 2>&1
  wallet "$1" account new private --label me 2>&1 | grep -o 'Private/[A-Za-z0-9]*' | cut -d/ -f2
}

echo "work dir $WORK"
BUYER=$(new_wallet "$WORK/buyer")
PROVIDER=$(new_wallet "$WORK/provider")
wallet "$WORK/buyer" account import public --private-key $GENESIS_KEY >/dev/null 2>&1
FUNDER=$(wallet "$WORK/buyer" account list 2>&1 | grep -o 'Public/[A-Za-z0-9]*' | head -1 | cut -d/ -f2)
echo "buyer $BUYER, provider $PROVIDER, funder $FUNDER"

wallet "$WORK/buyer" auth-transfer send --from Public/$FUNDER --to Private/$BUYER --amount 2000000 >/dev/null 2>&1
echo "buyer funded: $(wallet "$WORK/buyer" account get --account-id Private/$BUYER 2>&1 | grep -o 'Balance [0-9]*')"

header=$(wallet "$WORK/buyer" account new public --label header 2>&1 | grep -o 'Public/[A-Za-z0-9]*' | cut -d/ -f2)
segs=""; for i in 0 1 2; do segs="$segs Public/$(wallet "$WORK/buyer" account new public --label seg$i 2>&1 | grep -o 'Public/[A-Za-z0-9]*' | cut -d/ -f2)"; done
wallet "$WORK/buyer" program-loader deploy --elf $ELF --header Public/$header --segments $segs --immutable --payer Public/$FUNDER 2>&1 | grep -E "deployed|rror"

PROGRAM=$header ELF=$ELF BUYER=$BUYER PROVIDER=$PROVIDER \
  BUYER_WALLET=$WORK/buyer PROVIDER_WALLET=$WORK/provider \
  cargo run -q -p private_job_prototype 2>&1 | grep -v "WARNING: proving in dev mode"
