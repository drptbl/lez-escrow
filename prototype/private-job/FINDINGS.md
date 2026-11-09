# PROTOTYPE: fully private jobs

Question: can an escrow job live in a private account, so the chain shows neither the parties nor the terms, on LEZ `v0.3.1`?

Verdict: yes. It takes one program change and a workaround for a wallet gap, and it brings a race neither party can avoid.

## What ran

`run.sh` against a local standalone LEZ `v0.3.1` sequencer in dev mode. There were two separate wallets, one for the buyer and one for the provider, and each party signed from its own private account. The job account is a private PDA of the escrow. Both wallets derive its keys from one group master secret, which in real use the parties would exchange off-chain.

| Step | Result |
| --- | --- |
| Buyer opens job 1 | Both wallets see `Open`, holding 400,000 |
| Provider delivers | The buyer's wallet sees `Delivered` after it syncs |
| Provider claims to a fresh private `pay_to` | `pay_to` holds 400,000, and the job is empty and `Claimed` |
| Both act on job 2 at once | The first to land wins and the other is dropped. One run kept `Return`, the other `Release`. |

What a public observer finds in the open transaction's bytes:

| Field | Visible |
| --- | --- |
| Escrow program id | yes (it runs as a disclosed program) |
| Job account, buyer, provider, `pay_to`, amount | no |
| Deadline | yes, as the transaction's time window (from the LEZ source, not measured here) |

## The program change

`Open` used to check the job account against the public PDA formula. A plan can't compute a private PDA's id, which also needs keys the plan never sees. So `Open` now calls the escrow itself with the job's PDA seed (`VerifyJob`). LEZ authorizes the job account in that call only when it is the escrow's PDA for that seed, public or private.

The other instructions drop the formula. Payouts already need that seed, and the record checks the signer. The public runtime tests pass, including a refused `Open` on someone else's job account.

## Gaps outside this repo

1. **The LEZ wallet derives the wrong id for a deployed program's shared PDA.** `create_shared_pda_account` uses `AccountId::from_builtin_program(image_id)` as the authority (`lez/wallet/src/lib.rs:468-469`). That only matches built-in programs, and it is unchanged in `v0.3.2` and `v0.4.0-rc1`. The prototype registers the job itself, with the escrow's header account as authority, and sync then follows it correctly.
2. **`lez_core` can't express any of this.** Its private calls take only `private_owned`, `public`, `public_no_sign` and `private_foreign`, and it has no group-key functions. logos-mcp can't drive private jobs until it does.

## The race

Any update to the job account spends its current note. If both parties prove against the same state, whichever transaction lands first wins, and the other is never included, so its sender has to sync and try again.

That also means either party can stall the other by touching the job account first, for free, since private transactions cost nothing in `v0.3.1`. Nobody can take the money that way: every change still goes through the escrow's rules. But each blocked attempt costs the other side a proof, about 30 to 60 seconds, and a stalled `Deliver` can run past the deadline. I inferred this from the race and didn't run a deliberate stalling attack.

## Not checked

- Real proving times and the public testnet. Everything ran in dev mode.
- How the two parties exchange the group master secret.
- What a party can do once it holds the job's full spending keys outside the escrow, beyond the stalling above.
