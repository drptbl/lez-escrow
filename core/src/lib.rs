//! Job escrow for the Logos Execution Zone (LEZ v0.3).
//!
//! A buyer locks a payment for one job in a job account derived from the buyer and the job's
//! reference. The buyer releases it to the provider in parts, or the provider's delivery
//! releases all of it; the provider claims what is released. The buyer refunds what was never
//! released once the deadline passes, or once the provider returns the job.
//!
//! A job may name a controller program, such as a marketplace, a DAO or an arbiter. It may
//! release and return the job without the parties, and it decides what a delivery is worth: on a
//! controlled job, delivering releases nothing.
//!
//! LEZ calls a program twice. `plan` sees the instruction and the account handles but no account
//! data, so every instruction carries the values the job record must match. `apply` sees the job
//! record and checks those values before writing the next state.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::{
    Timestamp,
    account::{AccountId, Balance, ProgramShardSelector, ShardData},
    native_token::{Instruction as NativeInstruction, NATIVE_TOKEN_PROGRAM_ID, custody_transfer},
    program::{AccountMeta, ApplyInput, ChainedCall, PdaSeed, Plan, PlanInput},
};
use risc0_zkvm::sha::{Impl, Sha256 as _};

mod rules;

use rules::{Action, Origin, step};
pub use rules::{Job, JobState};

/// Domain separator for the job account's PDA seed.
const JOB_SEED_DOMAIN: &[u8; 32] = b"/lez-escrow/v1/JobSeed/000000000";

/// A 32-byte hash, such as a job reference or a result hash.
pub type Hash32 = [u8; 32];

/// The PDA seed of the job account for `buyer` and `job_ref`.
#[must_use]
pub fn job_seed(buyer: &AccountId, job_ref: &Hash32) -> PdaSeed {
    let mut bytes = [0_u8; 96];
    bytes[..32].copy_from_slice(JOB_SEED_DOMAIN);
    bytes[32..64].copy_from_slice(buyer.value());
    bytes[64..].copy_from_slice(job_ref);
    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("sha256 is 32 bytes"),
    )
}

/// The job account for `buyer` and `job_ref` under the escrow program deployed at `program`.
#[must_use]
pub fn job_account_id(program: &AccountId, buyer: &AccountId, job_ref: &Hash32) -> AccountId {
    AccountId::for_public_pda(program, &job_seed(buyer, job_ref))
}

/// The escrow program's instructions, borsh-encoded as the transaction's instruction data.
///
/// Money only ever moves to the addresses fixed at `Open`, and only when the side being paid
/// asks: the provider pulls with `Claim`, the buyer with `Refund`. `Release` and `Return` just
/// let the other side pull. So nobody pays into someone else's address, which in LEZ can burn a
/// private one or be blocked.
///
/// The parties sign their own calls. A program may call only for a job it controls: it may
/// `Release` and `Return` on its own, and every other instruction still needs the party it names
/// to be authorized, as the controller's own PDA would be.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Instruction {
    /// Locks `amount` from the buyer for `provider` until `deadline`. The provider is paid to
    /// `pay_to` and the buyer gets the deposit back to `refund_to`. `controller` names the
    /// program that may act on the job besides the parties; a program opening a job must name
    /// itself.
    /// Accounts: `[buyer (signs), job]`.
    Open {
        job_ref: Hash32,
        provider: AccountId,
        pay_to: AccountId,
        refund_to: AccountId,
        amount: Balance,
        deadline: Timestamp,
        controller: Option<AccountId>,
    },
    /// Records the provider's result hash, once, before the job's `deadline`. On a job without
    /// a controller it also releases everything left.
    /// Accounts: `[provider (signs), job]`.
    Deliver { result: Hash32, deadline: Timestamp },
    /// Pays `amount` to the job's `pay_to`, up to what is released and not yet claimed.
    /// Accounts: `[provider (signs), job, pay_to]`, or `[provider (signs), job]` when `pay_to` is
    /// the provider.
    Claim {
        buyer: AccountId,
        job_ref: Hash32,
        amount: Balance,
    },
    /// Lets the provider claim `amount` more of an open job's deposit, up to all of it that is
    /// left. Moves no money. The controller may call it without the buyer.
    /// Accounts: `[buyer (signs), job]`.
    Release { job_ref: Hash32, amount: Balance },
    /// Pays the part of the deposit never released, which must be `amount`, back to the job's
    /// `refund_to`: from an open job's `deadline` on, or from a returned job at any time, when
    /// `deadline` is 0.
    /// Accounts: `[buyer (signs), job, refund_to]`, or `[buyer (signs), job]` when `refund_to` is
    /// the buyer.
    Refund {
        job_ref: Hash32,
        amount: Balance,
        deadline: Timestamp,
    },
    /// The provider gives up an open job, so the buyer can refund what was never released at
    /// once. Moves no money. The controller may call it without the provider.
    /// Accounts: `[provider (signs), job]`.
    Return,
}

/// Why the escrow program refused a call. The guest exits with [`EscrowError::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowError {
    /// The instruction data or the account list does not decode.
    InvalidInstruction,
    /// The party this instruction names did not authorize it.
    Unauthorized,
    /// The job account is not the one derived from the buyer and the job reference, or does not
    /// select this program's shard.
    WrongJobAccount,
    /// `Open` found a job record already in the job account.
    JobExists,
    /// `Open` with a zero amount, the buyer as its own provider, or the job account as a payout
    /// address.
    InvalidTerms,
    /// The signer is not the job's buyer or provider as this instruction requires, or the payee
    /// is not the job's payout address.
    WrongParty,
    /// The instruction's `deadline` is not the job's.
    WrongDeadline,
    /// The job account holds no job record.
    NoJob,
    /// The job is not in a state this instruction applies to.
    WrongState,
    /// The instruction's `amount` is zero, or more than it may release or claim, or not what a
    /// refund returns.
    WrongAmount,
    /// A program other than the job's controller called the escrow, or a program opened a job
    /// without naming itself. LEZ carries a signer's authorization into every chained call, so
    /// a program the parties sign for could otherwise act on their jobs.
    NotDirect,
}

impl EscrowError {
    /// The guest's exit code for this error.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::InvalidInstruction => 1,
            Self::Unauthorized => 2,
            Self::WrongJobAccount => 3,
            Self::JobExists => 4,
            Self::InvalidTerms => 5,
            Self::WrongParty => 6,
            Self::WrongDeadline => 7,
            Self::NoJob => 8,
            Self::WrongState => 9,
            Self::WrongAmount => 10,
            Self::NotDirect => 11,
        }
    }
}

/// Turns an instruction into the effect on the job record and the native transfer it implies.
pub fn plan(input: &PlanInput) -> Result<Plan, EscrowError> {
    let instruction: Instruction =
        borsh::from_slice(&input.instruction_data).map_err(|_| EscrowError::InvalidInstruction)?;
    let mut plan = Plan::new(input);
    match instruction {
        Instruction::Open {
            job_ref,
            provider,
            pay_to,
            refund_to,
            amount,
            deadline,
            controller,
        } => {
            let [buyer, job] = accounts(input)?;
            job_account(input, job, buyer.account_id, &job_ref)?;
            if amount == 0
                || provider == buyer.account_id
                || pay_to == job.account_id
                || refund_to == job.account_id
            {
                return Err(EscrowError::InvalidTerms);
            }
            plan.effect(
                job,
                &effect(
                    input,
                    buyer,
                    Action::Open(Job {
                        buyer: buyer.account_id,
                        provider,
                        pay_to,
                        refund_to,
                        amount,
                        released: 0,
                        claimed: 0,
                        deadline,
                        state: JobState::Open,
                        result: None,
                        controller,
                    }),
                ),
            );
            plan.call(native_transfer(buyer.account_id, job.account_id, amount));
            plan.timestamp_window(..deadline);
        }
        Instruction::Deliver { result, deadline } => {
            let [provider, job] = accounts(input)?;
            own_shard(input, job)?;
            plan.effect(
                job,
                &effect(
                    input,
                    provider,
                    Action::Deliver {
                        provider: provider.account_id,
                        deadline,
                        result,
                    },
                ),
            );
            plan.timestamp_window(..deadline);
        }
        Instruction::Claim {
            buyer,
            job_ref,
            amount,
        } => {
            let (provider, job, payee) = payout_accounts(input)?;
            let seed = job_account(input, job, buyer, &job_ref)?;
            plan.effect(
                job,
                &effect(
                    input,
                    provider,
                    Action::Claim {
                        provider: provider.account_id,
                        payee,
                        amount,
                    },
                ),
            );
            plan.call(custody_transfer(job.account_id, seed, payee, amount));
        }
        Instruction::Release { job_ref, amount } => {
            let [buyer, job] = accounts(input)?;
            job_account(input, job, buyer.account_id, &job_ref)?;
            plan.effect(job, &effect(input, buyer, Action::Release { amount }));
        }
        Instruction::Refund {
            job_ref,
            amount,
            deadline,
        } => {
            let (buyer, job, payee) = payout_accounts(input)?;
            let seed = job_account(input, job, buyer.account_id, &job_ref)?;
            plan.effect(
                job,
                &effect(
                    input,
                    buyer,
                    Action::Refund {
                        payee,
                        amount,
                        deadline,
                    },
                ),
            );
            plan.call(custody_transfer(job.account_id, seed, payee, amount));
            plan.timestamp_window(deadline..);
        }
        Instruction::Return => {
            let [provider, job] = accounts(input)?;
            own_shard(input, job)?;
            plan.effect(
                job,
                &effect(
                    input,
                    provider,
                    Action::Return {
                        provider: provider.account_id,
                    },
                ),
            );
        }
    }
    Ok(plan)
}

/// Checks an effect against the job record and returns the record to store.
pub fn apply(input: &ApplyInput) -> Result<ShardData, EscrowError> {
    let effect: Effect =
        borsh::from_slice(&input.effect_data).map_err(|_| EscrowError::InvalidInstruction)?;
    let pre = if input.pre_data.is_empty() {
        None
    } else {
        // Only this program writes its shard, so a record that does not decode cannot happen.
        Some(borsh::from_slice(&input.pre_data).map_err(|_| EscrowError::NoJob)?)
    };
    let next = step(pre, effect.origin, effect.action)?;
    Ok(
        ShardData::try_from(borsh::to_vec(&next).expect("a job record serializes"))
            .expect("a job record fits in a shard"),
    )
}

/// The signer, the job account and the payee of a payout. The payee is left out when it is the
/// signer, because LEZ refuses a call that names the same account twice.
fn payout_accounts(
    input: &PlanInput,
) -> Result<(&AccountMeta, &AccountMeta, AccountId), EscrowError> {
    match input.accounts.as_slice() {
        [signer, job] => Ok((signer, job, signer.account_id)),
        [signer, job, payee] => Ok((signer, job, payee.account_id)),
        _ => Err(EscrowError::InvalidInstruction),
    }
}

fn accounts<const N: usize>(input: &PlanInput) -> Result<&[AccountMeta; N], EscrowError> {
    <&[AccountMeta; N]>::try_from(input.accounts.as_slice())
        .map_err(|_| EscrowError::InvalidInstruction)
}

/// Checks that `job` selects this program's shard, where job records live.
fn own_shard(input: &PlanInput, job: &AccountMeta) -> Result<(), EscrowError> {
    if job.program_account_id == input.self_account_id {
        Ok(())
    } else {
        Err(EscrowError::WrongJobAccount)
    }
}

/// Checks that `job` is the job account for `buyer` and `job_ref`, selecting this program's
/// shard, and returns its PDA seed.
fn job_account(
    input: &PlanInput,
    job: &AccountMeta,
    buyer: AccountId,
    job_ref: &Hash32,
) -> Result<PdaSeed, EscrowError> {
    own_shard(input, job)?;
    let seed = job_seed(&buyer, job_ref);
    if job.account_id == AccountId::for_public_pda(&input.self_account_id, &seed) {
        Ok(seed)
    } else {
        Err(EscrowError::WrongJobAccount)
    }
}

/// What `apply` checks against, and writes to, the job record.
#[derive(BorshSerialize, BorshDeserialize)]
struct Effect {
    origin: Origin,
    action: Action,
}

/// The effect of `action`, asked for by this call's caller on behalf of `party`.
fn effect(input: &PlanInput, party: &AccountMeta, action: Action) -> Effect {
    Effect {
        origin: Origin {
            caller: input.caller_account_id,
            party_authorized: party.is_authorized,
        },
        action,
    }
}

fn native_transfer(from: AccountId, to: AccountId, amount: Balance) -> ChainedCall {
    ChainedCall::new(
        NATIVE_TOKEN_PROGRAM_ID,
        vec![
            ProgramShardSelector::native_balance(from),
            ProgramShardSelector::native_balance(to),
        ],
        &NativeInstruction::Transfer { amount },
    )
}
