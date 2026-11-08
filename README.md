# logos-escrow

Job escrow for the Logos Execution Zone. A buyer locks a payment for one job, the provider gets it when it delivers, and the buyer gets it back when the provider doesn't deliver by the deadline.

Built for agent services in [logos-mcp](https://github.com/drptbl/logos-mcp), but any LEZ wallet can call it. Testnet only: LEZ is not meant for real funds yet.

## Deployment

| Network | Value |
| --- | --- |
| Sequencer | `https://testnet.lez.logos.co/` (LEZ `v0.3.1`) |
| Program id (header account) | `9jbUSu3t3rdZbmekKJ73DCwaFULWUWhPVVUbbgzdUgNL` |
| Program id, hex | `81c6de4e3a4ba0d4694d8c225782952e012ccf8746ac77fe1535ec948928d729` |
| Image id | `83b695c12eb5204777a1638b9506330e28264769545e9b0c2ebab35192473dfa` |
| Immutable | yes, checked on chain |
| Deployed | 2026-11-08, blocks 79211 to 79214, header tx `f048321589a015d4d629328f1d183c0a860a77ad53358f4936f9736a9fca246d` |

The program id is the account the program was deployed at, not the image id. Pass it as `program_id_hex` to `lez_core`.

The deployed binary is immutable. A fix means a new deploy at a new program id, and jobs already open finish on the old one.

It replaces two earlier deploys, which still run, so jobs already open there finish there. Don't open new jobs on them:

- `CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G` has no `Return`.
- `4gCJP2bc5YBCh3saEzudfTmNYZ4uZNDj8fUea4wsHrEM` also takes calls from other programs. LEZ `v0.3.1` carries a signer's authorization into chained calls, so any program a buyer or provider signs for could drive it.

## How it works

Each job lives in its own job account. The account holds the locked lepta and, in the escrow program's shard, the job record.

The buyer names two payout addresses at `Open`: `pay_to`, where the provider is paid, and `refund_to`, where the deposit goes back. Payouts go only there, not to whoever signs. That's what lets a payee be a fresh private account, see [Private accounts](#private-accounts).

| State | Reached by | Next |
| --- | --- | --- |
| `Open` | `Open` | `Delivered`, `Released`, `Refunded` |
| `Delivered` | `Deliver` | `Claimed`, `Released` |
| `Claimed` | `Claim` | final |
| `Released` | `Release` | final |
| `Refunded` | `Refund` or `Return` | final |

- The provider delivers by recording a 32-byte result hash before the deadline, for example the SHA-256 of the result it sent. The chain can't check the result itself.
- There are no disputes. Once delivered, the provider can claim at any time and the buyer can no longer refund.
- A provider that can't do an open job can `Return` the deposit to the buyer at any time, so the buyer doesn't wait for the deadline.
- The buyer can release to the provider at any time before the job is final.
- Deadlines are milliseconds since the Unix epoch, checked against the block's timestamp.
- The record stays after the job ends, so a job reference can't be used twice.
- There is no escrow fee. Whoever pays for a transaction must hold LEZ's whole fee reserve up front, about 16M lepta at the wallet's default gas limit, and gets back what it didn't use. A buyer paying for its own `Open` needs `amount` plus that reserve. On a local LEZ `v0.3.1` sequencer at base fee `8`, `Open`, `Deliver` and `Claim` each charged 210,000 to 250,000 lepta, about 30,000 gas. A gas limit of 200,000 is plenty and cuts the reserve to about 1.6M.
- The job itself is public: anyone can read its record, amount and deadline. Buyer and provider can still act from private accounts.
- Payouts move exactly `amount`. Lepta that someone sends to a job account on top of it stay there for good.

## Job account

The job account is a public PDA of the escrow program:

```text
seed = SHA-256("/lez-escrow/v1/JobSeed/000000000" || buyer || job_ref)
job  = SHA-256("/LEE/v0.2/AccountId/PDA/" || 8 zero bytes || program_id || seed)
```

`buyer`, `job_ref` and `program_id` are 32 bytes each, and both prefixes are 32 bytes. The seed domain keeps the project's former name, lez-escrow, so callers' derivation code didn't change with the rename. Deriving from the buyer too means nobody who learns a `job_ref` can open that job first. In the services protocol `job_ref` would be the SHA-256 of the accepted quote.

`core/tests/escrow.rs` (`the_job_account_and_instruction_layout_are_stable`) pins these against literals computed with Python's `hashlib`, so you can check your own derivation against it.

## Calling it with `lez_core`

```cpp
send_generic_public_transaction(account_ids, signing_requirements, instruction, program_id_hex, payer_account_id_hex, shard_program_account_ids_hex)
```

- `account_ids` are 32-byte hex, in the order below.
- `signing_requirements` is `true` only for the signer, whose key the wallet must hold.
- `instruction` is the Borsh bytes below.
- `program_id_hex` is the program id.
- `payer_account_id_hex` may be empty, in which case the signer pays. It may also be any other funded account the wallet holds, which then co-signs. The wallet refuses a payer that is in `account_ids` without signing, such as the `pay_to` on a `Release`.
- `shard_program_account_ids_hex` is empty, which selects the escrow program's shard of every account.

| Instruction | `account_ids` | Signs | Allowed when | Moves |
| --- | --- | --- | --- | --- |
| `Open` | `[buyer, job]` | buyer | before `deadline`, job account empty | `amount` from buyer to job |
| `Deliver` | `[provider, job]` | provider | before `deadline`, `Open` | nothing |
| `Claim` | `[provider, job, pay_to]` | provider | `Delivered` | `amount` from job to `pay_to` |
| `Release` | `[buyer, job, pay_to]` | buyer | `Open` or `Delivered` | `amount` from job to `pay_to` |
| `Refund` | `[buyer, job, refund_to]` | buyer | from `deadline` on, `Open` | `amount` from job to `refund_to` |
| `Return` | `[provider, job, refund_to]` | provider | `Open`, any time | `amount` from job to `refund_to` |

The program can't read the job record while it plans a call. So every instruction carries the values the record must match, and the call fails when they don't. Read the record first and copy `amount` and `deadline` from it.

### Instruction data

Borsh: one variant byte, then the fields in order. Integers are little-endian, and an account id or hash is 32 raw bytes.

| Byte 0 | Instruction | Fields | Length |
| --- | --- | --- | --- |
| `0` | `Open` | `job_ref: [u8; 32]`, `provider: [u8; 32]`, `pay_to: [u8; 32]`, `refund_to: [u8; 32]`, `amount: u128`, `deadline: u64` | 153 |
| `1` | `Deliver` | `result: [u8; 32]`, `deadline: u64` | 41 |
| `2` | `Claim` | `buyer: [u8; 32]`, `job_ref: [u8; 32]`, `amount: u128` | 81 |
| `3` | `Release` | `job_ref: [u8; 32]`, `amount: u128` | 49 |
| `4` | `Refund` | `job_ref: [u8; 32]`, `amount: u128`, `deadline: u64` | 57 |
| `5` | `Return` | `buyer: [u8; 32]`, `job_ref: [u8; 32]`, `amount: u128` | 81 |

`Deliver { result: [4; 32], deadline: 1800000000000 }` encodes to:

```text
01 0404040404040404040404040404040404040404040404040404040404040404 00505c18a3010000
```

## Reading a job

Ask the sequencer for the job account:

```bash
curl -s https://testnet.lez.logos.co/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getAccount","params":["<job account, base58>"]}'
```

The result is `{"nonce": .., "data": {"shards": {"<base58 id>": [bytes], ..}}}`. The record sits under the program id, and there is no job when that key is missing. An account that never existed comes back as `{"nonce": 0, "data": {"shards": {}}}`, not as an error. The locked lepta sit under `11111111111111111111111111111111`, the native token, as a 16-byte little-endian `u128`, and that shard is missing at zero.

The record is Borsh:

| Field | Type |
| --- | --- |
| `buyer` | `[u8; 32]` |
| `provider` | `[u8; 32]` |
| `pay_to` | `[u8; 32]` |
| `refund_to` | `[u8; 32]` |
| `amount` | `u128` |
| `deadline` | `u64`, milliseconds |
| `result` | `Option<[u8; 32]>`: `0`, or `1` and the hash |
| `state` | `u8`: `0` Open, `1` Delivered, `2` Claimed, `3` Released, `4` Refunded |

## Private accounts

Buyer and provider can act from private accounts. The job stays public: anyone can read its record, amount, deadline and payout addresses, and the buyer and provider show up there as private account ids. What's hidden is which funds paid in and where the payouts end up, since nothing links those private accounts to a wallet.

Paying into someone else's private account only works while that account has never been used. So make each payout address a fresh private account for that one job, under the keys of whoever gets paid: the provider sends its `pay_to` with the quote, and the buyer picks its own `refund_to`. Whoever sends a payout needs the payee's nullifier public key, viewing public key and identifier. The payee's wallet finds the money on its next sync.

Call it with `lez_core`'s `send_generic_private_transaction_with_accounts`:

| Argument | Value |
| --- | --- |
| `accounts_json` | the accounts in the order above. The signer is `{"kind": "private_owned", "account_id": hex}`, the job `{"kind": "public_no_sign", "account_id": hex}`, and a private payee `{"kind": "private_foreign", "nullifier_public_key": hex, "viewing_public_key": hex, "identifier": hex}` |
| `instruction` | the Borsh bytes above |
| `program_elf` | the escrow binary, `logos_escrow.bin`, which must build the deployed image id |
| `program_kind_json` | `{"kind": "disclosed", "account_id": "<program id, hex>"}` |
| `program_dependencies`, `dependency_kinds_json`, `shard_program_account_ids_hex` | empty |

Public and private mix freely: a public signer can pay a private payee, and a private signer a public one.

- The wallet proves every private call locally, which takes about 30 to 60 seconds. Start a `Deliver` well before the deadline.
- Private transactions are fee-exempt in LEZ `v0.3.1`.
- The deadline is public in every call, because LEZ checks it against the block's timestamp.

## Errors

`poll_transaction_status` only says a transaction was included. A refused public call is included too and still pays its fee, so read the record again to see whether it worked. A refused private call is never included: it fails while proving, or the sequencer leaves it out.

The program refuses a call by exiting with one of these codes. LEZ then charges only the cycles used.

| Code | Error | Means |
| --- | --- | --- |
| 1 | `InvalidInstruction` | instruction data or account list doesn't decode |
| 2 | `Unauthorized` | the account that must sign didn't |
| 3 | `WrongJobAccount` | job account isn't the one derived from buyer and `job_ref`. `Deliver` only checks that the account selects the escrow shard. Any other wrong account shows up as `WrongParty`, `WrongDeadline` or `NoJob` |
| 4 | `JobExists` | `Open` on a job account that already has a record |
| 5 | `InvalidTerms` | `Open` with a zero amount, the buyer as provider, or the job account as a payout address |
| 6 | `WrongParty` | the signer isn't the job's buyer or provider as the instruction needs, or the payee isn't its `pay_to` or `refund_to` |
| 7 | `WrongDeadline` | `deadline` isn't the job's |
| 8 | `NoJob` | no record in the job account |
| 9 | `WrongState` | the job isn't in a state this instruction takes |
| 10 | `WrongAmount` | `amount` isn't the job's |
| 11 | `NotDirect` | another program called the escrow. Only direct calls are taken |

LEZ itself refuses the rest, and those cost more. The transaction is still included, changes nothing and pays its whole gas limit:

- `OutOfValidityWindow`: an `Open` or `Deliver` at or after the deadline, or a `Refund` before it
- `NativeTransferFailed`: an `Open` the buyer can't fund

Check the deadline and the buyer's balance before you send.

## Build and test

You need Rust, Docker and the RISC Zero toolchain (`rzup install cargo-risczero 3.0.5`, `rzup install r0vm 3.0.5`, `rzup install rust`).

```bash
make build
make test
```

`make build` builds the guest in RISC Zero's Docker image `r0.1.91.1`, the one LEZ `v0.3.1` builds its own programs with, so the image id is reproducible. `methods/guest/Cargo.lock` is part of that identity too: change it and the image id changes. `make test` runs the program's rules as plain Rust, then the built binary through LEZ `v0.3.1`'s state machine with real signatures, transfers and deadlines. CI runs both, and fails when the build's image id isn't the deployed one above.

`cli/` is a small client on the same wallet code `lez_core` uses. A call goes private when the signer is one of the wallet's private accounts or the payee is a private payout address:

```bash
export LEE_WALLET_HOME_DIR=~/.lez-escrow/wallet LOGOS_ESCROW_PROGRAM=<program id>
cargo run -p logos_escrow_cli -- payout-address --owner <private account id> --out provider.addr
cargo run -p logos_escrow_cli -- open --buyer <id> --job-ref <hex> --provider <id> --pay-to <id> --refund-to <id> --amount 1000000 --in-secs 3600
cargo run -p logos_escrow_cli -- deliver --buyer <id> --job-ref <hex> --result <hex>
cargo run -p logos_escrow_cli -- claim --buyer <id> --job-ref <hex> --payee-address provider.addr
cargo run -p logos_escrow_cli -- show --buyer <id> --job-ref <hex>
```

`payout-address` makes a fresh private address under one of your private accounts' keys, prints its id for `open`, and writes the file the paying side passes as `--payee-address`.

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE)).
