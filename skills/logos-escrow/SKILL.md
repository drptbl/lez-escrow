---
name: logos-escrow
description: Integrate the logos-escrow job escrow on the Logos Execution Zone (LEZ). Use when paying or getting paid for a job through escrow from a client (lez_core or the LEZ Rust wallet), building a controller program (marketplace, DAO, arbiter) that settles escrow jobs, or testing against the escrow on a local logos-devnet.
---

# logos-escrow

[README.md](../../README.md) is the interface reference: program ids, account orders, instruction and record layouts, error codes. Read the section a step names before you encode anything. This skill holds the order of work and the rules integrations get wrong.

## Settle a job from a client

1. Take the program id from the README's Deployment table, or from `LOGOS_PROGRAM_ESCROW` on a devnet.
2. Derive the job account from the buyer and a 32-byte `job_ref` (README, Job account). A `job_ref` is spent for that buyer once opened, so make each one fresh.
3. Before every call after `Open`, read the job record (README, Reading a job) and quote it: `Deliver` takes the record's `deadline`, `Claim` at most `released - claimed`, and `Refund` exactly `amount - released`, with `deadline` 0 after a `Return`.
4. Send the call (README, Calling it with `lez_core`), signed by the party the instruction names, with the accounts in the README's order.
5. Read the record again to learn the outcome. A refused public call is still included and charged, so inclusion alone proves nothing. Done when the record shows the state and counters you expected.

Rules every call keeps:

- The side being paid pulls its own payout. When `pay_to` or `refund_to` is the signer itself, send two accounts and leave the payee out.
- Pull to a private payout address only from a private call (README, Private accounts). A public pull lands the money in a public account nobody can sign for.
- Size the fee reserve: the payer must hold `gas_limit` × base fee up front (base fee 8 on LEZ v0.3.1), and a failure that isn't the program's exit code charges all of it. A `gas_limit` of 200,000 is plenty for any escrow call.
- Deadlines are milliseconds since the Unix epoch, checked against the block's timestamp: `Open` and `Deliver` land before it, a deadline `Refund` at or after it.
- As a provider, read `pay_to`, `amount`, `deadline` and `controller` from the record before you start the work.

## Build a controller program

Start from [examples/arbiter](../../examples/arbiter/README.md): what it does, a walkthrough on a devnet, and how to copy it into your project. Then hold to these, which the README's Controllers section explains:

- Your program opens the job itself and names its own program id as `controller`. The escrow refuses a program that names anything else.
- Your program alone may `Release` and `Return` without the parties. `Deliver`, `Claim` and `Refund` stay with the party they name, or with your program's PDA when you pass its `pda_seeds`.
- Decide who may act in your program's `apply`, against state your program stored: `plan` sees no account data. A failed `apply` fails the whole transaction, escrow call included.
- Take a decision only from a direct call (`caller_account_id` is `None`). LEZ carries a signature into every chained call, so otherwise any program your decider signs for could decide in their name.
- Bind your own state to the escrow's job: check the job account against `job_account_id(escrow, buyer, job_ref)` before you record anything for it, so nobody records first for someone else's job through an escrow of their own.
- A party that is a program's PDA acts only through that program, so that program must be the job's controller.
- Build your guest in its own crate. A guest sharing the escrow's crate changes the escrow's image id, and the build no longer matches the deployed program.

## Test against a devnet

1. Add the released binary to your `logos-devnet.toml` (README, On your own devnet) and run `logos-devnet up`.
2. Run your tests under `logos-devnet exec -- <command>`, which loads `LOGOS_PROGRAM_ESCROW` and the chain's other settings.
3. A devnet keeps its state between runs, so give every run fresh `job_ref`s. Run `logos-devnet reset` before a test that recreates the same private accounts.
