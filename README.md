# lez-escrow

Job escrow for the Logos Execution Zone. A buyer locks a payment for one job, the provider gets it when it delivers, and the buyer gets it back when the provider doesn't deliver by the deadline.

Built for agent services in [logos-mcp](https://github.com/drptbl/logos-mcp), but any LEZ wallet can call it. Testnet only: LEZ is not meant for real funds yet.

## Deployment

| Network | Value |
| --- | --- |
| Sequencer | `https://testnet.lez.logos.co/` (LEZ `v0.3.1`) |
| Program id (header account) | `4gCJP2bc5YBCh3saEzudfTmNYZ4uZNDj8fUea4wsHrEM` |
| Program id, hex | `369d7d27dee17ad2f9d39e067ccaf3693c29bc4d432c4d8d6cb684e2ba85a68a` |
| Image id | `357fc2be58aaee6650f3203efe856c9f53bc2483e4f556fd4ab836f2379e1f79` |
| Immutable | yes, checked on chain |
| Deployed | 2026-11-08, blocks 79116 to 79128, header tx `fae91a3b02cc3a57571fa8e3503d57e4c2c79b2b9bb9dd1d1ab8f891d6ccfc6b` |

The program id is the account the program was deployed at, not the image id. Pass it as `program_id_hex` to `lez_core`.

The deployed binary is immutable. A fix means a new deploy at a new program id, and jobs already open finish on the old one.

## How it works

Each job lives in its own job account. The account holds the locked lepta and, in the escrow program's shard, the job record.

| State | Reached by | Next |
| --- | --- | --- |
| `Open` | `Open` | `Delivered`, `Released`, `Refunded` |
| `Delivered` | `Deliver` | `Claimed`, `Released` |
| `Claimed` | `Claim` | final |
| `Released` | `Release` | final |
| `Refunded` | `Refund` | final |

- The provider delivers by recording a 32-byte result hash before the deadline, for example the SHA-256 of the result it sent. The chain can't check the result itself.
- There are no disputes. Once delivered, the provider can claim at any time and the buyer can no longer refund.
- The buyer can release to the provider at any time before the job is final.
- Deadlines are milliseconds since the Unix epoch, checked against the block's timestamp.
- The record stays after the job ends, so a job reference can't be used twice.
- There is no escrow fee. Whoever pays for a transaction must hold LEZ's whole fee reserve up front, about 16M lepta at the wallet's default gas limit, and gets back what it didn't use. A buyer paying for its own `Open` needs `amount` plus that reserve. On a local LEZ `v0.3.1` sequencer at base fee `8`, `Open`, `Deliver` and `Claim` each charged 210,000 to 250,000 lepta, about 30,000 gas. A gas limit of 200,000 is plenty and cuts the reserve to about 1.6M.
- Built and tested for public accounts. Anyone can read a job's buyer, provider, amount and deadline.
- Payouts move exactly `amount`. Lepta that someone sends to a job account on top of it stay there for good.

## Job account

The job account is a public PDA of the escrow program:

```text
seed = SHA-256("/lez-escrow/v1/JobSeed/000000000" || buyer || job_ref)
job  = SHA-256("/LEE/v0.2/AccountId/PDA/" || 8 zero bytes || program_id || seed)
```

`buyer`, `job_ref` and `program_id` are 32 bytes each, and both prefixes are 32 bytes. Deriving from the buyer too means nobody who learns a `job_ref` can open that job first. In the services protocol `job_ref` would be the SHA-256 of the accepted quote.

`core/tests/escrow.rs` (`the_job_account_and_instruction_layout_are_stable`) pins these against literals computed with Python's `hashlib`, so you can check your own derivation against it.

## Calling it with `lez_core`

```cpp
send_generic_public_transaction(account_ids, signing_requirements, instruction, program_id_hex, payer_account_id_hex, shard_program_account_ids_hex)
```

- `account_ids` are 32-byte hex, in the order below.
- `signing_requirements` is `true` only for the signer, whose key the wallet must hold.
- `instruction` is the Borsh bytes below.
- `program_id_hex` is the program id.
- `payer_account_id_hex` may be empty, in which case the signer pays. It may also be any other funded account the wallet holds, which then co-signs. The wallet refuses a payer that is in `account_ids` without signing, such as the provider on a `Release`.
- `shard_program_account_ids_hex` is empty, which selects the escrow program's shard of every account.

| Instruction | `account_ids` | Signs | Allowed when | Moves |
| --- | --- | --- | --- | --- |
| `Open` | `[buyer, job]` | buyer | before `deadline`, job account empty | `amount` from buyer to job |
| `Deliver` | `[provider, job]` | provider | before `deadline`, `Open` | nothing |
| `Claim` | `[provider, job]` | provider | `Delivered` | `amount` from job to provider |
| `Release` | `[buyer, job, provider]` | buyer | `Open` or `Delivered` | `amount` from job to provider |
| `Refund` | `[buyer, job]` | buyer | from `deadline` on, `Open` | `amount` from job to buyer |

The program can't read the job record while it plans a call. So every instruction carries the values the record must match, and the call fails when they don't. Read the record first and copy `amount` and `deadline` from it.

### Instruction data

Borsh: one variant byte, then the fields in order. Integers are little-endian, and an account id or hash is 32 raw bytes.

| Byte 0 | Instruction | Fields | Length |
| --- | --- | --- | --- |
| `0` | `Open` | `job_ref: [u8; 32]`, `provider: [u8; 32]`, `amount: u128`, `deadline: u64` | 89 |
| `1` | `Deliver` | `result: [u8; 32]`, `deadline: u64` | 41 |
| `2` | `Claim` | `buyer: [u8; 32]`, `job_ref: [u8; 32]`, `amount: u128` | 81 |
| `3` | `Release` | `job_ref: [u8; 32]`, `amount: u128` | 49 |
| `4` | `Refund` | `job_ref: [u8; 32]`, `amount: u128`, `deadline: u64` | 57 |

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
| `amount` | `u128` |
| `deadline` | `u64`, milliseconds |
| `result` | `Option<[u8; 32]>`: `0`, or `1` and the hash |
| `state` | `u8`: `0` Open, `1` Delivered, `2` Claimed, `3` Released, `4` Refunded |

## Errors

`poll_transaction_status` only says a transaction was included. A refused call is included too and still pays its fee, so read the record again to see whether it worked.

The program refuses a call by exiting with one of these codes. LEZ then charges only the cycles used.

| Code | Error | Means |
| --- | --- | --- |
| 1 | `InvalidInstruction` | instruction data or account list doesn't decode |
| 2 | `Unauthorized` | the account that must sign didn't |
| 3 | `WrongJobAccount` | job account isn't the one derived from buyer and `job_ref`. `Deliver` only checks that the account selects the escrow shard. Any other wrong account shows up as `WrongParty`, `WrongDeadline` or `NoJob` |
| 4 | `JobExists` | `Open` on a job account that already has a record |
| 5 | `InvalidTerms` | `Open` with a zero amount, or the buyer as provider |
| 6 | `WrongParty` | signer, or payee, isn't the job's provider |
| 7 | `WrongDeadline` | `deadline` isn't the job's |
| 8 | `NoJob` | no record in the job account |
| 9 | `WrongState` | the job isn't in a state this instruction takes |
| 10 | `WrongAmount` | `amount` isn't the job's |

LEZ itself refuses the rest, and those cost more. The transaction is still included, changes nothing and pays its whole gas limit:

- `OutOfValidityWindow`: an `Open` or `Deliver` at or after the deadline, or a `Refund` before it
- `NativeTransferFailed`: an `Open` the buyer can't fund

Check the deadline and the buyer's balance before you send.

## Build and test

You need Rust, Docker and the RISC Zero toolchain (`rzup install cargo-risczero 3.0.5`, `rzup install r0vm 3.0.5`).

```bash
make build
make test
```

`make build` builds the guest in RISC Zero's Docker image `r0.1.91.1`, the one LEZ `v0.3.1` builds its own programs with, so the image id is reproducible. `methods/guest/Cargo.lock` is part of that identity too: change it and the image id changes. `make test` runs the program's rules as plain Rust, then the built binary through LEZ `v0.3.1`'s state machine with real signatures, transfers and deadlines.

`cli/` is a small client on the same wallet code `lez_core` uses:

```bash
export LEE_WALLET_HOME_DIR=~/.lez-escrow/wallet LEZ_ESCROW_PROGRAM=<program id>
cargo run -p lez_escrow_cli -- open --buyer <id> --job-ref <hex> --provider <id> --amount 1000000 --in-secs 600
cargo run -p lez_escrow_cli -- deliver --buyer <id> --job-ref <hex> --result <hex>
cargo run -p lez_escrow_cli -- claim --buyer <id> --job-ref <hex>
cargo run -p lez_escrow_cli -- show --buyer <id> --job-ref <hex>
```

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE)).
