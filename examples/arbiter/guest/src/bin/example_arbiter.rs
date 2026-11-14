//! The example arbiter's zkVM entry point. Its rules live in `examples/arbiter`; this only reads
//! the runtime's call, runs `plan` or `apply`, and writes the result. A refused call exits with
//! the error's code, which LEZ charges only for the cycles used.

use lee_core::program::{ApplyInput, CallKind, PlanInput, apply_write, read_input_frame};
use logos_escrow_arbiter::ArbiterError;
use risc0_zkvm::guest::env;

fn main() {
    let call_kind: CallKind =
        borsh::from_slice(&read_input_frame()).expect("the runtime writes a call kind");
    let payload = read_input_frame();
    match call_kind {
        CallKind::Plan => {
            let input: PlanInput =
                borsh::from_slice(&payload).expect("the runtime writes a plan input");
            match logos_escrow_arbiter::plan(&input) {
                Ok(plan) => plan.write(),
                Err(error) => refuse(error),
            }
        }
        CallKind::Apply => {
            let input: ApplyInput =
                borsh::from_slice(&payload).expect("the runtime writes an apply input");
            match logos_escrow_arbiter::apply(&input) {
                Ok(record) => apply_write(input, record),
                Err(error) => refuse(error),
            }
        }
    }
}

fn refuse(error: ArbiterError) -> ! {
    env::exit(error.code())
}
