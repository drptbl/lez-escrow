//! The job rules, free of LEZ's plan/apply types, so a port to another LEZ program model only
//! rewrites the layer in `lib.rs` that feeds them.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::{
    Timestamp,
    account::{AccountId, Balance},
};

use crate::{EscrowError, Hash32};

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum JobState {
    /// The deposit is locked; the buyer may release parts of it.
    Open,
    /// The provider delivered, and everything is released.
    Delivered,
    /// The provider gave up the job; the buyer can refund what was never released.
    Returned,
    /// The buyer took back what was never released.
    Refunded,
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
    /// The deposit.
    pub amount: Balance,
    /// How much of the deposit the provider may claim.
    pub released: Balance,
    /// How much the provider has claimed.
    pub claimed: Balance,
    /// Milliseconds since the Unix epoch.
    pub deadline: Timestamp,
    pub state: JobState,
    pub result: Option<Hash32>,
    /// The program that may act on this job besides its parties.
    pub controller: Option<AccountId>,
}

impl Job {
    /// What the provider may claim now.
    #[must_use]
    pub const fn claimable(&self) -> Balance {
        self.released - self.claimed
    }

    /// What was never released: what `Release` may still release, and what `Refund` returns.
    #[must_use]
    pub const fn unreleased(&self) -> Balance {
        self.amount - self.released
    }
}

/// Who asked for an action.
#[derive(Debug, Clone, Copy, BorshSerialize, BorshDeserialize)]
pub struct Origin {
    /// The program that called the escrow, or `None` when the transaction called it directly.
    pub caller: Option<AccountId>,
    /// Whether the party the action names (the buyer or the provider) authorized it.
    pub party_authorized: bool,
}

/// One change to a job, with the values the caller quoted for the record to match.
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub enum Action {
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
    Release {
        amount: Balance,
    },
    Refund {
        payee: AccountId,
        amount: Balance,
        deadline: Timestamp,
    },
    Return {
        provider: AccountId,
    },
}

/// Applies `action`, asked for by `origin`, to the job record `pre`, or to no record, and
/// returns the next record.
///
/// A program may only act on a job it controls. The controller may `Release` and `Return` on its
/// own; every other action needs the party it names, so only the side being paid pulls a payout.
pub fn step(pre: Option<Job>, origin: Origin, action: Action) -> Result<Job, EscrowError> {
    // The new job's controller for `Open`, so a program opens a job only for itself.
    let controller = match (&action, &pre) {
        (Action::Open(job), _) | (_, Some(job)) => job.controller,
        (_, None) => return Err(EscrowError::NoJob),
    };
    if origin.caller.is_some() && origin.caller != controller {
        return Err(EscrowError::NotDirect);
    }
    let controller_acts_alone =
        origin.caller.is_some() && matches!(action, Action::Release { .. } | Action::Return { .. });
    if !controller_acts_alone && !origin.party_authorized {
        return Err(EscrowError::Unauthorized);
    }
    if let Action::Open(job) = action {
        return match pre {
            Some(_) => Err(EscrowError::JobExists),
            None => Ok(job),
        };
    }
    let Some(mut job) = pre else {
        return Err(EscrowError::NoJob);
    };
    match action {
        Action::Open(_) => unreachable!("handled above"),
        Action::Deliver {
            provider,
            deadline,
            result,
        } => {
            if job.provider != provider {
                return Err(EscrowError::WrongParty);
            }
            if job.deadline != deadline {
                return Err(EscrowError::WrongDeadline);
            }
            if job.state != JobState::Open {
                return Err(EscrowError::WrongState);
            }
            if job.result.is_some() {
                return Err(EscrowError::WrongState);
            }
            job.result = Some(result);
            // A controlled job's controller judges the result and releases what it decides.
            if job.controller.is_none() {
                job.released = job.amount;
                job.state = JobState::Delivered;
            }
        }
        Action::Claim {
            provider,
            payee,
            amount,
        } => {
            if job.provider != provider || job.pay_to != payee {
                return Err(EscrowError::WrongParty);
            }
            if amount == 0 || amount > job.claimable() {
                return Err(EscrowError::WrongAmount);
            }
            job.claimed += amount;
        }
        Action::Release { amount } => {
            if job.state != JobState::Open {
                return Err(EscrowError::WrongState);
            }
            // Bounded by what is left before adding, so it cannot overflow.
            if amount == 0 || amount > job.unreleased() {
                return Err(EscrowError::WrongAmount);
            }
            job.released += amount;
        }
        Action::Refund {
            payee,
            amount,
            deadline,
        } => {
            if job.refund_to != payee {
                return Err(EscrowError::WrongParty);
            }
            match job.state {
                // The window `deadline..` only holds the refund back if it is the job's deadline.
                JobState::Open if job.deadline != deadline => {
                    return Err(EscrowError::WrongDeadline);
                }
                JobState::Open | JobState::Returned => {}
                _ => return Err(EscrowError::WrongState),
            }
            if amount == 0 || amount != job.unreleased() {
                return Err(EscrowError::WrongAmount);
            }
            job.state = JobState::Refunded;
        }
        Action::Return { provider } => {
            if job.provider != provider {
                return Err(EscrowError::WrongParty);
            }
            if job.state != JobState::Open {
                return Err(EscrowError::WrongState);
            }
            job.state = JobState::Returned;
        }
    }
    Ok(job)
}
