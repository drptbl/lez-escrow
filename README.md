# logos-escrow

Job escrow for the Logos Execution Zone. A buyer locks a payment for one job and releases it to the provider in parts, or all at once when the provider delivers. The buyer gets back what was never released when the provider doesn't deliver by the deadline.

Built for agent services in [logos-mcp](https://github.com/drptbl/logos-mcp), but any LEZ wallet can call it, and other programs can build on it: a job can name a controller program, such as a marketplace, a DAO or an arbiter, that decides what the provider gets. Testnet only: LEZ is not meant for real funds yet.

## Deployment

| Network | Value |
| --- | --- |
| Sequencer | `https://testnet.lez.logos.co/` (LEZ `v0.3.1`) |
| Program id (header account) | `HpavNGFYxjyhLsy171aXsvv146yUeXmRWiho22uvT43d` |
| Program id, hex | `f9eba477278c918fadafbcb4a6463e947d0fd8de344b5431ee0aa3cd41d7e3a4` |
| Image id | `1d6461792a0ca794bb2e6d76460237acef8559e24d9e3d44570662362801a789` |
| Immutable | yes, checked on chain |
| Deployed | 2026-11-09, blocks 79270 to 79276, header tx `b538ab5e3a3da9f5d0685145af496b69b921dea30165c9e2c3e317bd74b1fcb7` |

The program id is the account the program was deployed at, not the image id. Pass it as `program_id_hex` to `lez_core`.

The deployed binary is immutable. A fix means a new deploy at a new program id, and jobs already open finish on the old one.

Each release on GitHub carries this binary, `logos_escrow.bin`, and its sha256, built from the release tag in CI. A release only publishes when the build's image id is the one above, so you can deploy the same program to a local chain.

It replaces three earlier deploys, which still run, so jobs already open there finish there. Don't open new jobs on them:

- `9jbUSu3t3rdZbmekKJ73DCwaFULWUWhPVVUbbgzdUgNL` has no payout addresses and pays by push, so it can't serve private parties safely.
- `CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G` has no `Return`.
- `4gCJP2bc5YBCh3saEzudfTmNYZ4uZNDj8fUea4wsHrEM` also takes calls from other programs. LEZ `v0.3.1` carries a signer's authorization into chained calls, so any program a buyer or provider signs for could drive it.

## How it works

Each job lives in its own job account. The account holds the locked lepta and, in the escrow program's shard, the job record.

The buyer names two payout addresses at `Open`: `pay_to`, where the provider is paid, and `refund_to`, where the deposit goes back. Money only ever moves there, and only when the side being paid asks for it: the provider pulls with `Claim`, the buyer with `Refund`. `Release` and `Return` move nothing. They only let the other side pull early. So nobody ever pays into someone else's address, which is what keeps private payout addresses safe, see [Private accounts](#private-accounts).

| State | Reached by | Next |
| --- | --- | --- |
| `Open` | `Open` | `Delivered`, `Returned`, `Refunded` |
| `Delivered` | `Deliver`, on a job without a controller | final |
| `Returned` | `Return` | `Refunded` |
| `Refunded` | `Refund` | final |

The record also counts lepta: `released` is how much of `amount` the provider may take, and `claimed` how much it took. `Claim` works in any state, so whatever was released stays the provider's after a `Return` or a `Refund`.

- The provider delivers by recording a 32-byte result hash before the deadline, for example the SHA-256 of the result it sent. The chain can't check the result itself.
- On a job without a controller there are no disputes. Delivering releases everything left, and the buyer can no longer refund. On a controlled job, delivering releases nothing, see [Controllers](#controllers).
- The buyer can `Release` any part of what's left of an open job, as many times as it likes, and the provider can claim it without delivering.
- A provider that can't do an open job can `Return` it, and the buyer can then refund what was never released at once instead of waiting for the deadline.
- The buyer picks both payout addresses, so a provider should read the record and check `pay_to`, `amount`, `deadline` and `controller` before it works on a job.
- Deadlines are milliseconds since the Unix epoch, checked against the block's timestamp.
- The record stays after the job ends, so a job reference can't be used twice.
- There is no escrow fee. Whoever pays for a transaction must hold LEZ's whole fee reserve up front, about 16M lepta at the wallet's default gas limit, and gets back what it didn't use. A buyer paying for its own `Open` needs `amount` plus that reserve. On a local LEZ `v0.3.1` sequencer at base fee `8`, `Open`, `Deliver` and `Claim` each charged 210,000 to 250,000 lepta, about 30,000 gas. A gas limit of 200,000 is plenty and cuts the reserve to about 1.6M.
- The job itself is public: anyone can read its record, amount and deadline. Buyer and provider can still act from private accounts.
- Payouts never move more than `amount` in total. Lepta that someone sends to a job account on top of it stay there for good.

## Controllers

A job can name a controller at `Open`, a program that acts on it besides the buyer and provider. It calls the escrow in a chained call with the same accounts and instruction data a party would send, with the party's account unsigned.

- The controller can `Release` any part of what's left and `Return` the job without the parties. Those are an arbiter's two decisions: pay the provider, or give the rest back.
- `Deliver`, `Claim` and `Refund` still need the party they name, even from the controller. So only the side being paid pulls a payout, and a controller can't pull one publicly to a private payout address, where it would be lost. A party that is the controller's own PDA signs through it, with `pda_seeds` in the chained call.
- On a controlled job, delivering only records the result. The provider gets paid when the buyer or the controller releases.
- The parties keep their direct calls. The deadline still lets the buyer refund what was never released, so a stuck controller can't lock the money.
- A program can only act on jobs that name it. A program that opens a job has to name itself, and a call from any other program is refused with `NotDirect`.
- A party that is a program's PDA can only act through that program, so that program has to be the job's controller. Make a PDA provider for a job controlled by another program, or by none, and it can never deliver or claim: whatever was released to it stays in the job account.

The controller decides who gets the money, never where it goes: payouts only reach `pay_to` and `refund_to`. Read what the controller does, and whether it can be updated, before you open or take a controlled job.

LEZ `v0.3.1` carries a signer's authorization into every chained call, so a program you sign a transaction for can open a job with your lepta, just as it could transfer them. The controller rule protects your existing jobs, not a signature you give a hostile program.

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
- `payer_account_id_hex` may be empty, in which case the signer pays. It may also be any other funded account the wallet holds, which then co-signs. The wallet refuses a payer that is in `account_ids` without signing, such as the `pay_to` on a `Claim` or the `refund_to` on a `Refund`.
- `shard_program_account_ids_hex` is empty, which selects the escrow program's shard of every account.

| Instruction | `account_ids` | Signs | Allowed when | Moves |
| --- | --- | --- | --- | --- |
| `Open` | `[buyer, job]` | buyer | before `deadline`, job account empty | `amount` from buyer to job |
| `Deliver` | `[provider, job]` | provider | before `deadline`, `Open`, once | nothing |
| `Claim` | `[provider, job, pay_to]` | provider | any state, `amount` up to `released - claimed` | `amount` from job to `pay_to` |
| `Release` | `[buyer, job]` | buyer, or the controller | `Open`, `amount` up to `amount - released` | nothing |
| `Refund` | `[buyer, job, refund_to]` | buyer | `Open` from `deadline` on, or `Returned` | `amount - released` from job to `refund_to` |
| `Return` | `[provider, job]` | provider, or the controller | `Open` | nothing |

When the payout address is the signer itself, leave it out: `Claim` takes `[provider, job]` and `Refund` takes `[buyer, job]`. LEZ refuses a call that names the same account twice. A `Refund` after a `Return` passes `deadline` 0, because the deadline no longer applies.

The program can't read the job record while it plans a call. So every instruction carries the values the record must match, and the call fails when they don't. Read the record first: `Deliver` quotes its `deadline`, a `Claim` takes at most `released - claimed`, and a `Refund` exactly `amount - released`.

### Instruction data

Borsh: one variant byte, then the fields in order. Integers are little-endian, an account id or hash is 32 raw bytes, and an `Option` is `0`, or `1` and the value.

| Byte 0 | Instruction | Fields | Length |
| --- | --- | --- | --- |
| `0` | `Open` | `job_ref: [u8; 32]`, `provider: [u8; 32]`, `pay_to: [u8; 32]`, `refund_to: [u8; 32]`, `amount: u128`, `deadline: u64`, `controller: Option<[u8; 32]>` | 154, or 186 with a controller |
| `1` | `Deliver` | `result: [u8; 32]`, `deadline: u64` | 41 |
| `2` | `Claim` | `buyer: [u8; 32]`, `job_ref: [u8; 32]`, `amount: u128` | 81 |
| `3` | `Release` | `job_ref: [u8; 32]`, `amount: u128` | 49 |
| `4` | `Refund` | `job_ref: [u8; 32]`, `amount: u128`, `deadline: u64` | 57 |
| `5` | `Return` | none | 1 |

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
| `released` | `u128` |
| `claimed` | `u128` |
| `deadline` | `u64`, milliseconds |
| `state` | `u8`: `0` Open, `1` Delivered, `2` Returned, `3` Refunded |
| `result` | `Option<[u8; 32]>` |
| `controller` | `Option<[u8; 32]>` |

`core/tests/escrow.rs` (`the_job_record_and_controlled_open_layout_are_stable`) pins a whole record against a literal.

## Private accounts

Buyer and provider can act from private accounts. The job stays public: anyone can read its record, amount, deadline and payout addresses, and the buyer and provider show up there as private account ids. What's hidden is which funds paid in and where the payouts end up, since nothing links those private accounts to a wallet.

Make each payout address a fresh private account for that one job, under the keys of whoever gets paid. The provider sends its `pay_to` with the quote, and the buyer picks its own `refund_to`. A private account id is:

```text
SHA-256("/LEE/v0.3/AccountId/Private/" || 4 zero bytes || npk || vpk || identifier)
```

`npk` is the 32-byte nullifier public key, `vpk` the 1,184-byte viewing public key, and `identifier` 32 bytes of your choice. Since only the side being paid moves money to its address, it always holds those keys:

- If its wallet hasn't seen the address yet, pass it as a new account under its own keys.
- If someone already paid into it and its wallet has synced, pass it as one of its own accounts. That's why paying into a payout address first can't block it.

Never pull to a private address in a public call. The program can't tell the two apart, so the money would land in a public account with that id, and nobody can sign for that.

Call it with `lez_core`'s `send_generic_private_transaction_with_accounts`:

| Argument | Value |
| --- | --- |
| `accounts_json` | the accounts in the order above. A private signer is `{"kind": "private_owned", "account_id": hex}` and a public one `{"kind": "public", "account_id": hex}`. The job is `{"kind": "public_no_sign", "account_id": hex}`. A payout address the wallet already holds is `private_owned`, and a fresh one is `{"kind": "private_foreign", "nullifier_public_key": hex, "viewing_public_key": hex, "identifier": hex}`. |
| `instruction` | the Borsh bytes above |
| `program_elf` | the escrow binary, `logos_escrow.bin`, which must build the deployed image id |
| `program_kind_json` | `{"kind": "disclosed", "account_id": "<program id, hex>"}` |
| `program_dependencies`, `dependency_kinds_json`, `shard_program_account_ids_hex` | empty |

Always pass `identifier` for a `private_foreign` payout address. Without it `lez_core` picks a random one, the id no longer matches the job, and the call is refused.

- The wallet proves every private call locally, which takes about 30 to 60 seconds. Start a `Deliver` well before the deadline.
- In a private call, the checks against the job record run at the sequencer, after the proof. A call that fails them costs the proving time and never appears on chain.
- Private transactions are fee-exempt in LEZ `v0.3.1`.
- The deadline is public in every call, because LEZ checks it against the block's timestamp.

## Errors

`poll_transaction_status` only says a transaction was included. A refused public call is included too and still pays its fee, so read the record again to see whether it worked. A refused private call is never included: it fails while proving, or the sequencer leaves it out.

The program refuses a call by exiting with one of these codes. LEZ then charges only the cycles used.

| Code | Error | Means |
| --- | --- | --- |
| 1 | `InvalidInstruction` | instruction data or account list doesn't decode |
| 2 | `Unauthorized` | the party the instruction names didn't authorize it |
| 3 | `WrongJobAccount` | job account isn't the one derived from buyer and `job_ref`. `Deliver` and `Return` only check that the account selects the escrow shard, so there any other wrong account shows up as `WrongParty`, `WrongDeadline` or `NoJob` |
| 4 | `JobExists` | `Open` on a job account that already has a record |
| 5 | `InvalidTerms` | `Open` with a zero amount, the buyer as provider, or the job account as a payout address |
| 6 | `WrongParty` | the signer isn't the job's buyer or provider as the instruction needs, or the payee isn't its `pay_to` or `refund_to` |
| 7 | `WrongDeadline` | `deadline` isn't the job's |
| 8 | `NoJob` | no record in the job account |
| 9 | `WrongState` | the job isn't in a state this instruction takes |
| 10 | `WrongAmount` | `amount` is zero, more than a `Release` or `Claim` may move, or not what a `Refund` returns |
| 11 | `NotDirect` | a program other than the job's controller called the escrow, or a program opened a job without naming itself |

LEZ itself refuses the rest. In a public call those cost more: the transaction is still included, changes nothing and pays its whole gas limit. In a private call they are dropped like any other refusal.

- `OutOfValidityWindow`: an `Open` or `Deliver` at or after the deadline, or a `Refund` before it on a job that wasn't returned
- `NativeTransferFailed`: an `Open` the buyer can't fund

Check the deadline and the buyer's balance before you send.

## Build and test

You need Rust, Docker and the RISC Zero toolchain (`rzup install cargo-risczero 3.0.5`, `rzup install r0vm 3.0.5`, `rzup install rust`).

```bash
make build
make test
```

`make build` builds the guest in RISC Zero's Docker image `r0.1.91.1`, the one LEZ `v0.3.1` builds its own programs with, so the image id is reproducible. `methods/guest/Cargo.lock` is part of that identity too: change it and the image id changes. `make test` runs the program's rules as plain Rust, then the built binary through LEZ `v0.3.1`'s state machine with real signatures, transfers and deadlines. CI runs both, and fails when the build's image id isn't the deployed one above.

`make build` also builds `test_controller.bin`, a test-only program that relays whatever escrow call it gets, so the tests can call the escrow from another program. It checks nothing, so never deploy it anywhere that matters.

`cli/tests/devnet.rs` runs every flow on a local chain from [logos-devnet](https://github.com/drptbl/logos-devnet), through the CLI and the same LEZ wallet `lez_core` wraps, with public and private parties. CI runs it too:

```bash
make build && logos-devnet up
logos-devnet exec -- cargo test -p logos_escrow_cli --test devnet -- --ignored
```

`cli/` is a small client on the same wallet code `lez_core` uses. A call goes private when the signer is one of the wallet's private accounts or the payee is a private payout address:

```bash
export LEE_WALLET_HOME_DIR=~/.lez-escrow/wallet LOGOS_ESCROW_PROGRAM=<program id>
cargo run -p logos_escrow_cli -- payout-address --owner <private account id> --out provider.addr
cargo run -p logos_escrow_cli -- open --buyer <id> --job-ref <hex> --provider <id> --pay-to <id> --refund-to <id> --amount 1000000 --in-secs 3600
cargo run -p logos_escrow_cli -- release --buyer <id> --job-ref <hex> --amount 250000
cargo run -p logos_escrow_cli -- deliver --buyer <id> --job-ref <hex> --result <hex>
cargo run -p logos_escrow_cli -- claim --buyer <id> --job-ref <hex> --payee-address provider.addr
cargo run -p logos_escrow_cli -- show --buyer <id> --job-ref <hex>
```

`open --controller <program id>` names a controller. `release` without `--amount` releases everything left, `claim` takes everything released and not yet claimed, and `refund` everything never released.

`payout-address` makes a fresh private address under one of your private accounts' keys, prints its id for `open`, and writes the file you pass as `--payee-address` when you claim or refund to it. Keep that file: its identifier is the only way to reach a fresh address, so the command never overwrites one. Once your wallet holds the address, the CLI uses it directly. For someone else's public account, pass `--public-payee`. Otherwise the CLI refuses a payout it can't place.

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE)).
