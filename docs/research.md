# Research: building a job escrow on LEZ testnet v0.3

Checked 2026-11-08 against official sources only (logos-blockchain, logos-co, docs.logos.co).

## What the public testnet runs

- `https://testnet.lez.logos.co/` runs LEZ **v0.3.1** ([testnet endpoints](https://docs.logos.co/get-started/testnet-endpoints.md); the wallet guide says a v0.3.2 wallet also works, and v0.3.1→v0.3.2 touches no `lee/` code).
- Probed live: `getFeeState` and `getCrossZoneDeadLetters` answer, `getPublications` (v0.4) does not. `getProgramIds` lists only `privacy_preserving_circuit`: in v0.3 programs live at accounts, not in a built-in table.
- The testnet was reset since August: the lez-payment-streams deploy transaction (`0c04d5eb…`, block 15855) is gone from both testnet and testnet-legacy.

## The `#[lez_program]` toolchain does not reach v0.3

- SPEL (`logos-co/spel`, which provides `#[lez_program]`) pins LEZ `v0.2.5-rc2` on `main` (c6f73ce, 2026-10-13). No branch targets v0.3.
- lez-payment-streams pins LEZ `v0.2.0`; lez-multisig pins `v0.2.0-rc3`; lez-programs `main` pins `v0.2.5-rc2` and its v0.3 branch is `WIP`.
- [docs.logos.co](https://docs.logos.co/lez/programs/write-and-deploy-lez-program-with-scaffold.md#deploy-to-the-testnet): scaffold cannot deploy to the v0.3 testnet; use a standalone wallet built from the release the testnet runs, and a guest built against the same release.
- So this program is a raw v0.3 guest using `lee_core::program`, built with `cargo risczero build` (risc0 3.0.5, Docker) and deployed with `wallet deploy-program`.

## The v0.3 program model (LEZ v0.3.2 source)

- An account is a nonce plus **shards**: one byte blob per program, keyed by the program's account id. The native balance is the shard of `NATIVE_TOKEN_PROGRAM_ID` (`[0; 32]`), 16 bytes little-endian, empty for zero (`lee/state_machine/core/src/native_token.rs`).
- A program is called twice ([guide](https://docs.logos.co/lez/programs/write-lez-program-with-plan-and-apply.md)):
  - `plan` sees the instruction and account handles (`account_id`, `is_authorized`, which shard) but no data. It emits effects, chained calls, events and validity windows.
  - `apply` gets one effect and the current bytes of one shard; it keeps, replaces or panics. A panic rolls back the whole transaction.
- A program can only write its own shard (`ForeignShardWrite`), on any account it is handed. `Plan::inspect` puts a read-only guard on another program's shard, applied by the planning program.
- Authorization: a public account is authorized when it signed the transaction, and stays authorized through chained calls. A chained call's `pda_seeds` authorize `AccountId::for_public_pda(caller, seed)` in the callee. `native_token::custody_transfer` builds exactly that payout call.
- Chained calls may only name accounts that are inputs of the root transaction. A transaction runs at most 10 calls, the root call included.
- Time: a plan may set a `timestamp_validity_window` (milliseconds since the Unix epoch, `[from, to)`), checked against the block's timestamp (`validated_state_diff/public_backend.rs`). Alternatively a plan can `inspect` a Clock account (`/LEZ/ClockProgramAccount/0000001`, borsh `{block_id: u64, timestamp: u64}`), as LEZ's own `test_programs/guest/src/bin/time_locked_transfer.rs` does; Clock is written as the last transaction of each block, so it lags one block.
- Deploying (`program_loader`): one header account plus 96 KiB segment accounts, at addresses the deployer picks. The program's id for callers is the **header account id**, not the image id. A header can be made `immutable`; until then the deployer can replace the code. Deploying an example program costs about 3.2M lepta, and the payer must hold at least 17M per loader transaction as fee reserve.

## How logos-mcp will call it

`lez_core` (logos-execution-zone-module fdd6a75, built on LEZ v0.3.1):

```cpp
std::string send_generic_public_transaction(
    const std::vector<std::string>& account_ids,
    const std::vector<bool>& signing_requirements,
    const std::vector<uint8_t>& instruction,          // borsh bytes, passed untouched
    const std::string& program_id_hex,                 // the program's header account id
    const std::string& payer_account_id_hex,           // fee payer, "" = first funded signer
    const std::vector<std::string>& shard_program_account_ids_hex); // per account, "" = called program's shard
```

State can be read with the sequencer's `getAccount(account_id)`, which returns every shard.

## Funding

No faucet command in the v0.3 wallet. Testnet funds come from a channel deposit bridged from the Logos Blockchain, or a transfer from another testnet user ([wallet guide](https://docs.logos.co/lez/get-started/run-lez-wallet-via-cli.md)).
