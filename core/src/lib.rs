//! Job escrow for the Logos Execution Zone (LEZ v0.3).
//!
//! A buyer locks a payment for one job in a job account derived from the buyer and the job's
//! reference. The provider claims it once it has delivered, or once the buyer releases it early;
//! the buyer refunds it when the provider hasn't delivered by the deadline, or has returned it.
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
/// let the other side pull early. So nobody pays into someone else's address, which in LEZ can
/// burn a private one or be blocked.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Instruction {
    /// Locks `amount` from the buyer for `provider` until `deadline`. The provider is paid to
    /// `pay_to` and the buyer gets the deposit back to `refund_to`.
    /// Accounts: `[buyer (signs), job]`.
    Open {
        job_ref: Hash32,
        provider: AccountId,
        pay_to: AccountId,
        refund_to: AccountId,
        amount: Balance,
        deadline: Timestamp,
    },
    /// Records the provider's result hash. Only before the job's `deadline`.
    /// Accounts: `[provider (signs), job]`.
    Deliver { result: Hash32, deadline: Timestamp },
    /// Pays a delivered or released job's `amount` to its `pay_to`.
    /// Accounts: `[provider (signs), job, pay_to]`, or `[provider (signs), job]` when `pay_to` is
    /// the provider.
    Claim {
        buyer: AccountId,
        job_ref: Hash32,
        amount: Balance,
    },
    /// Lets the provider claim an open or delivered job at once. Moves no money.
    /// Accounts: `[buyer (signs), job]`.
    Release { job_ref: Hash32 },
    /// Pays an open job's `amount` back to its `refund_to` from the job's `deadline` on, or a
    /// returned job's at any time, when `deadline` is ignored.
    /// Accounts: `[buyer (signs), job, refund_to]`, or `[buyer (signs), job]` when `refund_to` is
    /// the buyer.
    Refund {
        job_ref: Hash32,
        amount: Balance,
        deadline: Timestamp,
    },
    /// The provider gives up an open job, so the buyer can refund it at once. Moves no money.
    /// Accounts: `[provider (signs), job]`.
    Return,
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum JobState {
    Open,
    Delivered,
    Claimed,
    /// The buyer released the job; the provider can claim it.
    Released,
    Refunded,
    /// The provider returned the job; the buyer can refund it.
    Returned,
}

/// The job record, stored in the escrow program's shard of the job account.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Job {
    pub buyer: AccountId,
    pub provider: AccountId,
    /// Where `Claim` pays.
    pub pay_to: AccountId,
    /// Where `Refund` pays.
    pub refund_to: AccountId,
    pub amount: Balance,
    /// Milliseconds since the Unix epoch.
    pub deadline: Timestamp,
    pub result: Option<Hash32>,
    pub state: JobState,
}

/// Why the escrow program refused a call. The guest exits with [`EscrowError::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowError {
    /// The instruction data or the account list does not decode.
    InvalidInstruction,
    /// The account that must sign this instruction did not.
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
    /// The instruction's `amount` is not the job's.
    WrongAmount,
    /// Another program called the escrow. Only direct calls are taken, because LEZ carries a
    /// signer's authorization into every chained call.
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

/// What `apply` checks against, and writes to, the job record.
#[derive(BorshSerialize, BorshDeserialize)]
enum Effect {
    Open(Job),
    Deliver {
        provider: AccountId,
        deadline: Timestamp,
        result: Hash32,
    },
    Claim {
        provider: AccountId,
        payee: AccountId,
        amount: Balance,
    },
    Release,
    Refund {
        payee: AccountId,
        amount: Balance,
        deadline: Timestamp,
    },
    Return {
        provider: AccountId,
    },
}

/// Turns an instruction into the effect on the job record and the native transfer it implies.
pub fn plan(input: &PlanInput) -> Result<Plan, EscrowError> {
    let instruction: Instruction =
        borsh::from_slice(&input.instruction_data).map_err(|_| EscrowError::InvalidInstruction)?;
    if input.caller_account_id.is_some() {
        return Err(EscrowError::NotDirect);
    }
    let mut plan = Plan::new(input);
    match instruction {
        Instruction::Open {
            job_ref,
            provider,
            pay_to,
            refund_to,
            amount,
            deadline,
        } => {
            let [buyer, job] = accounts(input)?;
            signed(buyer)?;
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
                &Effect::Open(Job {
                    buyer: buyer.account_id,
                    provider,
                    pay_to,
                    refund_to,
                    amount,
                    deadline,
                    result: None,
                    state: JobState::Open,
                }),
            );
            plan.call(native_transfer(buyer.account_id, job.account_id, amount));
            plan.timestamp_window(..deadline);
        }
        Instruction::Deliver { result, deadline } => {
            let [provider, job] = accounts(input)?;
            signed(provider)?;
            own_shard(input, job)?;
            plan.effect(
                job,
                &Effect::Deliver {
                    provider: provider.account_id,
                    deadline,
                    result,
                },
            );
            plan.timestamp_window(..deadline);
        }
        Instruction::Claim {
            buyer,
            job_ref,
            amount,
        } => {
            let (provider, job, payee) = payout_accounts(input)?;
            signed(provider)?;
            let seed = job_account(input, job, buyer, &job_ref)?;
            plan.effect(
                job,
                &Effect::Claim {
                    provider: provider.account_id,
                    payee,
                    amount,
                },
            );
            plan.call(custody_transfer(job.account_id, seed, payee, amount));
        }
        Instruction::Release { job_ref } => {
            let [buyer, job] = accounts(input)?;
            signed(buyer)?;
            job_account(input, job, buyer.account_id, &job_ref)?;
            plan.effect(job, &Effect::Release);
        }
        Instruction::Refund {
            job_ref,
            amount,
            deadline,
        } => {
            let (buyer, job, payee) = payout_accounts(input)?;
            signed(buyer)?;
            let seed = job_account(input, job, buyer.account_id, &job_ref)?;
            plan.effect(
                job,
                &Effect::Refund {
                    payee,
                    amount,
                    deadline,
                },
            );
            plan.call(custody_transfer(job.account_id, seed, payee, amount));
            plan.timestamp_window(deadline..);
        }
        Instruction::Return => {
            let [provider, job] = accounts(input)?;
            signed(provider)?;
            own_shard(input, job)?;
            plan.effect(
                job,
                &Effect::Return {
                    provider: provider.account_id,
                },
            );
        }
    }
    Ok(plan)
}

/// Checks an effect against the job record and returns the record to store.
pub fn apply(input: &ApplyInput) -> Result<ShardData, EscrowError> {
    let effect: Effect =
        borsh::from_slice(&input.effect_data).map_err(|_| EscrowError::InvalidInstruction)?;
    let next = match effect {
        Effect::Open(job) => {
            if !input.pre_data.is_empty() {
                return Err(EscrowError::JobExists);
            }
            job
        }
        Effect::Deliver {
            provider,
            deadline,
            result,
        } => {
            let mut job = load(&input.pre_data)?;
            if job.provider != provider {
                return Err(EscrowError::WrongParty);
            }
            if job.deadline != deadline {
                return Err(EscrowError::WrongDeadline);
            }
            if job.state != JobState::Open {
                return Err(EscrowError::WrongState);
            }
            job.result = Some(result);
            job.state = JobState::Delivered;
            job
        }
        Effect::Claim {
            provider,
            payee,
            amount,
        } => {
            let mut job = load(&input.pre_data)?;
            if job.provider != provider || job.pay_to != payee {
                return Err(EscrowError::WrongParty);
            }
            if job.amount != amount {
                return Err(EscrowError::WrongAmount);
            }
            if !matches!(job.state, JobState::Delivered | JobState::Released) {
                return Err(EscrowError::WrongState);
            }
            job.state = JobState::Claimed;
            job
        }
        Effect::Release => {
            let mut job = load(&input.pre_data)?;
            if !matches!(job.state, JobState::Open | JobState::Delivered) {
                return Err(EscrowError::WrongState);
            }
            job.state = JobState::Released;
            job
        }
        Effect::Refund {
            payee,
            amount,
            deadline,
        } => {
            let mut job = load(&input.pre_data)?;
            if job.refund_to != payee {
                return Err(EscrowError::WrongParty);
            }
            if job.amount != amount {
                return Err(EscrowError::WrongAmount);
            }
            match job.state {
                // The window `deadline..` only holds the refund back if it is the job's deadline.
                JobState::Open if job.deadline != deadline => {
                    return Err(EscrowError::WrongDeadline);
                }
                JobState::Open | JobState::Returned => {}
                _ => return Err(EscrowError::WrongState),
            }
            job.state = JobState::Refunded;
            job
        }
        Effect::Return { provider } => {
            let mut job = load(&input.pre_data)?;
            if job.provider != provider {
                return Err(EscrowError::WrongParty);
            }
            if job.state != JobState::Open {
                return Err(EscrowError::WrongState);
            }
            job.state = JobState::Returned;
            job
        }
    };
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

fn load(record: &[u8]) -> Result<Job, EscrowError> {
    if record.is_empty() {
        return Err(EscrowError::NoJob);
    }
    // Only this program writes its shard, so a record that does not decode cannot happen.
    borsh::from_slice(record).map_err(|_| EscrowError::NoJob)
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

fn signed(account: &AccountMeta) -> Result<(), EscrowError> {
    if account.is_authorized {
        Ok(())
    } else {
        Err(EscrowError::Unauthorized)
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
