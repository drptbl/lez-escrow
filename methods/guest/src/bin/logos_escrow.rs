//! The escrow program's zkVM entry point. The rules live in `logos_escrow_core`; this only reads
//! the runtime's call, runs `plan` or `apply`, and writes the result.
//!
//! A refused call exits with the error's code instead of panicking: LEZ charges a non-zero exit
//! only for the cycles it used, but a panic for the transaction's whole gas limit.

use lee_core::program::{ApplyInput, CallKind, PlanInput, apply_write, read_input_frame};
use logos_escrow_core::EscrowError;
use risc0_zkvm::guest::env;

fn main() {
    // Both frames come from the LEZ runtime, not from the caller.
    let call_kind: CallKind =
        borsh::from_slice(&read_input_frame()).expect("the runtime writes a call kind");
    let payload = read_input_frame();
    match call_kind {
        CallKind::Plan => {
            let input: PlanInput =
                borsh::from_slice(&payload).expect("the runtime writes a plan input");
            match logos_escrow_core::plan(&input) {
                Ok(plan) => plan.write(),
                Err(error) => refuse(error),
            }
        }
        CallKind::Apply => {
            let input: ApplyInput =
                borsh::from_slice(&payload).expect("the runtime writes an apply input");
            match logos_escrow_core::apply(&input) {
                Ok(record) => apply_write(input, record),
                Err(error) => refuse(error),
            }
        }
    }
}

fn refuse(error: EscrowError) -> ! {
    env::exit(error.code())
}
