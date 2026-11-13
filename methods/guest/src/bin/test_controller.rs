//! A test-only controller program. It relays whatever escrow call it is given, with whatever PDA
//! seeds it is given, and checks nothing, so it must never be deployed anywhere that matters. It
//! exists so tests can drive the escrow through a real chained call.
//!
//! Instruction: the borsh tuple `(escrow program id, PDA seeds, escrow instruction bytes)`.
//! Accounts: the escrow call's accounts, in its order; each selects the escrow's shard.

use lee_core::{
    account::{AccountId, ProgramShardSelector},
    program::{ChainedCall, PdaSeed, Plan, PlanInput, run_program},
};

type Relay = (AccountId, Vec<[u8; 32]>, Vec<u8>);

fn main() {
    run_program(plan, |(): (), _: &[u8]| -> Option<Vec<u8>> {
        unreachable!("the relay plans no effects")
    })
}

fn plan(input: &PlanInput, (escrow, seeds, instruction): Relay) -> Plan {
    let mut plan = Plan::new(input);
    plan.call(ChainedCall {
        program_account_id: escrow,
        shard_selectors: input
            .accounts
            .iter()
            .map(|account| ProgramShardSelector::new(account.account_id, escrow))
            .collect(),
        instruction_data: instruction,
        pda_seeds: seeds.into_iter().map(PdaSeed::new).collect(),
    });
    plan
}
