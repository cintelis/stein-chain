#!/bin/sh
# Starts a Stein node in the role given by STEIN_ROLE.
#
#   validator  creates a devnet (validator key, faucet key, genesis) in
#              $STEIN_DATA_DIR on first start, then produces blocks. With
#              STEIN_ENABLE_FN_DSA=1 (FN-DSA image only) it creates the FN-DSA
#              test network instead.
#   follower   follows $STEIN_LEADER, accepting only $STEIN_GENESIS_HASH.
#
# Everything else comes from STEIN_* variables read by stein-node itself.
set -eu

case "${STEIN_ROLE}" in
  validator)
    if [ ! -f "${STEIN_DATA_DIR}/genesis.json" ]; then
      if [ "${STEIN_ENABLE_FN_DSA:-0}" = 1 ]; then
        stein-node init --enable-fn-dsa --scheme "${STEIN_SCHEME:-fn-dsa-512-draft}"
      else
        stein-node init --chain-id "${STEIN_CHAIN_ID:-31337}" --scheme "${STEIN_SCHEME:-ml-dsa-44}"
      fi
    fi
    exec stein-node run
    ;;
  follower)
    : "${STEIN_LEADER:?STEIN_LEADER must be the validator's URL}"
    : "${STEIN_GENESIS_HASH:?STEIN_GENESIS_HASH must be the validator's genesis hash}"
    exec stein-node follow
    ;;
  *)
    echo "STEIN_ROLE must be validator or follower, not '${STEIN_ROLE}'" >&2
    exit 64
    ;;
esac
