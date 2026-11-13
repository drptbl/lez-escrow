//! An example escrow controller: an arbiter that lets one named judge decide a job.
//!
//! The buyer opens a job through the arbiter and names a judge. The arbiter records the judge in
//! a case account derived from the job account, and opens the escrow job naming itself as the
//! controller. From then on the judge, and only the judge, can make the arbiter release part of
//! the deposit to the provider or return the job to the buyer. The parties still claim and refund
//! directly on the escrow.
//!
//! LEZ calls a program twice: `plan` sees the accounts but no data, `apply` sees the case record.
//! So `plan` checks the signatures and builds the escrow call, and `apply` checks the case.
//! A failed `apply` fails the whole transaction, escrow call included.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::{
    Timestamp,
    account::{AccountId, Balance, ProgramShardSelector, ShardData},
    program::{AccountMeta, ApplyInput, ChainedCall, PdaSeed, Plan, PlanInput},
};
use logos_escrow_core::{Hash32, Instruction as EscrowInstruction};
use risc0_zkvm::sha::{Impl, Sha256 as _};

/// Domain separator for the case account's PDA seed.
const CASE_SEED_DOMAIN: &[u8; 32] = b"/logos-escrow/example/ArbiterCa/";

/// The case account of the escrow job account `job`, under the arbiter deployed at `arbiter`.
#[must_use]
pub fn case_account_id(arbiter: &AccountId, job: &AccountId) -> AccountId {
    AccountId::for_public_pda(arbiter, &case_seed(job))
}

fn case_seed(job: &AccountId) -> PdaSeed {
    let mut bytes = [0_u8; 64];
    bytes[..32].copy_from_slice(CASE_SEED_DOMAIN);
    bytes[32..].copy_from_slice(job.value());
    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("sha256 is 32 bytes"),
    )
}

/// The arbiter's instructions, borsh-encoded.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ArbiterInstruction {
    /// Opens an escrow job with this arbiter as its controller and `judge` deciding it. The other
    /// fields are the escrow's `Open`.
    /// Accounts: `[buyer (signs), case, job]`.
    Open {
        escrow: AccountId,
        judge: AccountId,
        job_ref: Hash32,
        provider: AccountId,
        pay_to: AccountId,
        refund_to: AccountId,
        amount: Balance,
        deadline: Timestamp,
    },
    /// The judge's decision on a job. `job_ref` is the job's, which the escrow's `Release` needs.
    /// Accounts: `[judge (signs), case, party, job]`, where the party is the buyer for a
    /// release and the provider for a return.
    Decide {
        escrow: AccountId,
        job_ref: Hash32,
        decision: Decision,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Decision {
    /// Lets the provider claim this much more.
    Release(Balance),
    /// Gives what was never released back to the buyer.
    Return,
}

/// What the case account holds.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Case {
    pub escrow: AccountId,
    pub judge: AccountId,
}

/// Why the arbiter refused a call. The guest exits with [`ArbiterError::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbiterError {
    /// The instruction data or the account list does not decode.
    InvalidInstruction,
    /// The buyer or the judge did not sign.
    Unauthorized,
    /// The case account is not the one derived from the job account.
    WrongCase,
    /// `Open` found a case already.
    CaseExists,
    /// The signer is not the case's judge, or the escrow is not the case's.
    WrongJudge,
    /// The case account holds no case.
    NoCase,
}

impl ArbiterError {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::InvalidInstruction => 1,
            Self::Unauthorized => 2,
            Self::WrongCase => 3,
            Self::CaseExists => 4,
            Self::WrongJudge => 5,
            Self::NoCase => 6,
        }
    }
}

/// What `apply` checks against, or writes to, the case record.
#[derive(BorshSerialize, BorshDeserialize)]
enum Effect {
    Open(Case),
    Decide(Case),
}

/// Checks the signer and the case account, and builds the escrow call.
pub fn plan(input: &PlanInput) -> Result<Plan, ArbiterError> {
    let instruction: ArbiterInstruction =
        borsh::from_slice(&input.instruction_data).map_err(|_| ArbiterError::InvalidInstruction)?;
    let mut plan = Plan::new(input);
    match instruction {
        ArbiterInstruction::Open {
            escrow,
            judge,
            job_ref,
            provider,
            pay_to,
            refund_to,
            amount,
            deadline,
        } => {
            let [buyer, case, job] = accounts(input)?;
            signed(buyer)?;
            own_case(input, case, job)?;
            plan.effect(case, &Effect::Open(Case { escrow, judge }));
            // The buyer's signature carries into this call, and the escrow takes it only because
            // the job names this program, the caller, as its controller.
            plan.call(escrow_call(
                escrow,
                &[buyer, job],
                &EscrowInstruction::Open {
                    job_ref,
                    provider,
                    pay_to,
                    refund_to,
                    amount,
                    deadline,
                    controller: Some(input.self_account_id),
                },
            ));
        }
        ArbiterInstruction::Decide {
            escrow,
            job_ref,
            decision,
        } => {
            let [judge, case, party, job] = accounts(input)?;
            signed(judge)?;
            own_case(input, case, job)?;
            plan.effect(
                case,
                &Effect::Decide(Case {
                    escrow,
                    judge: judge.account_id,
                }),
            );
            let instruction = match decision {
                Decision::Release(amount) => EscrowInstruction::Release { job_ref, amount },
                Decision::Return => EscrowInstruction::Return,
            };
            plan.call(escrow_call(escrow, &[party, job], &instruction));
        }
    }
    Ok(plan)
}

/// Writes a new case, or checks a decision against the stored one and keeps it.
pub fn apply(input: &ApplyInput) -> Result<ShardData, ArbiterError> {
    let effect: Effect =
        borsh::from_slice(&input.effect_data).map_err(|_| ArbiterError::InvalidInstruction)?;
    match effect {
        Effect::Open(case) => {
            if !input.pre_data.is_empty() {
                return Err(ArbiterError::CaseExists);
            }
            Ok(
                ShardData::try_from(borsh::to_vec(&case).expect("a case serializes"))
                    .expect("a case fits in a shard"),
            )
        }
        Effect::Decide(asked) => {
            if input.pre_data.is_empty() {
                return Err(ArbiterError::NoCase);
            }
            let stored: Case =
                borsh::from_slice(&input.pre_data).map_err(|_| ArbiterError::NoCase)?;
            if stored != asked {
                return Err(ArbiterError::WrongJudge);
            }
            Ok(input.pre_data.clone())
        }
    }
}

fn accounts<const N: usize>(input: &PlanInput) -> Result<&[AccountMeta; N], ArbiterError> {
    <&[AccountMeta; N]>::try_from(input.accounts.as_slice())
        .map_err(|_| ArbiterError::InvalidInstruction)
}

fn signed(account: &AccountMeta) -> Result<(), ArbiterError> {
    if account.is_authorized {
        Ok(())
    } else {
        Err(ArbiterError::Unauthorized)
    }
}

/// Checks that `case` is the case account of `job`, selecting this program's shard.
fn own_case(input: &PlanInput, case: &AccountMeta, job: &AccountMeta) -> Result<(), ArbiterError> {
    if case.program_account_id == input.self_account_id
        && case.account_id == case_account_id(&input.self_account_id, &job.account_id)
    {
        Ok(())
    } else {
        Err(ArbiterError::WrongCase)
    }
}

/// A call to the escrow naming `accounts`, each selecting the escrow's shard.
fn escrow_call(
    escrow: AccountId,
    accounts: &[&AccountMeta],
    instruction: &EscrowInstruction,
) -> ChainedCall {
    ChainedCall::new(
        escrow,
        accounts
            .iter()
            .map(|account| ProgramShardSelector::new(account.account_id, escrow))
            .collect(),
        instruction,
    )
}
