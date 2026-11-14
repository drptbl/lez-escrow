# Arbiter

An escrow controller you can start from. The buyer opens a job through the arbiter and names a judge, and only that judge can make the arbiter pay the provider or give the deposit back. The buyer and the provider still claim and refund on the escrow themselves, so the judge decides who gets the money but never where it goes.

## How it works

| Call | Who | What happens |
| --- | --- | --- |
| `Open` | the buyer | The arbiter records the judge in a case account derived from the job account, then opens the escrow job naming itself as the controller. |
| `Decide`, `Release` | the judge | The arbiter lets the provider claim part of the deposit. |
| `Decide`, `Return` | the judge | The arbiter gives what was never released back to the buyer. |

- The judge is checked in `apply`, against the case. A call from anyone else fails whole, escrow call included.
- `Open` only takes the escrow's own job account for that buyer and job reference, so nobody can take the case of someone else's job.
- The judge has to call directly. LEZ carries a signature into every program call, so a program the judge signs for can't decide in the judge's name.
- On a controlled job, delivering releases nothing: the provider's `Deliver` only records the result for the judge to look at.

The program is [`src/lib.rs`](src/lib.rs), its zkVM entry point is in [`guest`](guest), and [`client`](client) is a small CLI that sends its calls for the walkthrough below. [`tests/arbiter.rs`](tests/arbiter.rs) covers every refusal, and the end of [`integration/tests/flows.rs`](../../integration/tests/flows.rs) runs it on LEZ's state machine.

## What you need

- `logos-devnet` and Docker
- Rust, `jq`, and `cargo-risczero` 3.0 (`rzup install cargo-risczero 3.0.5`, `rzup install rust`)
- both programs built, from the repo root:

```bash
make build
```

## Run it

From this directory:

```bash
logos-devnet up
source .logos-devnet/env
export LEE_WALLET_HOME_DIR="$LOGOS_DEVNET_ACCOUNTS_PARTIES_WALLET" LOGOS_ESCROW_PROGRAM="$LOGOS_PROGRAM_ESCROW"
buyer=$(jq -r '.accounts[0].id' "$LOGOS_DEVNET_ACCOUNTS_PARTIES")
provider=$(jq -r '.accounts[1].id' "$LOGOS_DEVNET_ACCOUNTS_PARTIES")
judge=$(jq -r '.accounts[2].id' "$LOGOS_DEVNET_ACCOUNTS_PARTIES")
job=$(openssl rand -hex 32)
arbiter() { cargo run -q -p logos_escrow_arbiter_client -- "$@" --buyer "$buyer" --job-ref "$job"; }
escrow() { cargo run -q -p logos_escrow_cli -- "$@" --buyer "$buyer" --job-ref "$job"; }
```

`logos-devnet.toml` here gives the example a devnet of its own on a free port, deploys the escrow and the arbiter from your build, and funds three accounts in one wallet: the buyer, the provider and the judge. `arbiter` sends the arbiter's calls, and `escrow` is the escrow's own CLI.

The buyer locks 1M lepta for 10 minutes, with the judge deciding:

```bash
arbiter open --judge "$judge" --provider "$provider" --pay-to "$provider" --refund-to "$buyer" --amount 1000000 --in-secs 600
# tx <hash> in block <n>
# job <job account>: Open, released 0, claimed 0
```

The provider delivers. The record shows the result, still `Open` with `released: 0`:

```bash
escrow deliver --result "$(printf 'the work' | shasum -a 256 | cut -d' ' -f1)"
```

The provider tries to judge in its own favour. The call is included, as every refused public call is, and changes nothing:

```bash
arbiter release --judge "$provider" --amount 1000000
# job <job account>: Open, released 0, claimed 0
```

The judge pays the provider 400k, and the provider claims it. The job account then holds 600k:

```bash
arbiter release --judge "$judge" --amount 400000
# job <job account>: Open, released 400000, claimed 0
escrow claim
```

The judge gives the rest back, and the buyer refunds it. The job account ends empty and `Refunded`:

```bash
arbiter return --judge "$judge"
# job <job account>: Returned, released 400000, claimed 400000
escrow refund
```

`logos-devnet up` takes about 20 s and the calls another 20 s, once `cargo` has built the two CLIs. `logos-devnet down` stops the devnet, and deleting `.logos-devnet` removes it.

## Use it in your project

Copy `src/lib.rs` and `guest/` into your project, and depend on the escrow's rules from its release tag:

```toml
logos_escrow_core = { git = "https://github.com/drptbl/logos-escrow", tag = "v0.2.0" }
```

- Build your guest in a crate of its own, as `guest/` here, and deploy it yourself. The escrow is already on testnet, see its [Deployment](../../README.md#deployment).
- Bind whatever you record to the escrow's job, as `Open` does with `job_account_id`, and take decisions only from direct calls.
- Change who decides to fit your project: a committee, a deadline of your own, an oracle's account. The escrow only cares that the call comes from the controller its job names.

## Limits

- It has only run with public parties. Private parties through a controller haven't been tried on a chain.
- The judge is an account with a key. A program can't be the judge, since decisions have to be direct calls.
- A judge that never decides leaves the buyer to refund what was never released after the deadline. Until that refund lands, the judge can still release.
