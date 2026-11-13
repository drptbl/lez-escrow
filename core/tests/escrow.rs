//! Drives the escrow program's two entry points the way the LEZ runtime does: `plan` turns an
//! instruction into effects and chained calls, then `apply` runs each effect against the shard
//! it names.

use lee_core::{
    account::{AccountId, ProgramShardSelector, ShardData},
    native_token::{Instruction as NativeInstruction, NATIVE_TOKEN_PROGRAM_ID},
    program::{AccountMeta, ApplyInput, ChainedCall, PlanInput, PlanOutput, ShardEffect},
};
use logos_escrow_core::{EscrowError, Instruction, Job, JobState, job_account_id, plan};

const PROGRAM: AccountId = AccountId::new([7; 32]);
const BUYER: AccountId = AccountId::new([1; 32]);
const PROVIDER: AccountId = AccountId::new([2; 32]);
/// Where the provider is paid, and where the buyer gets the deposit back.
const PAY_TO: AccountId = AccountId::new([3; 32]);
const REFUND_TO: AccountId = AccountId::new([6; 32]);
const JOB_REF: [u8; 32] = [9; 32];
const AMOUNT: u128 = 500;
const DEADLINE: u64 = 1_800_000_000_000;

fn job() -> AccountId {
    job_account_id(&PROGRAM, &BUYER, &JOB_REF)
}

fn signer(account_id: AccountId) -> AccountMeta {
    AccountMeta::new(account_id, true, PROGRAM)
}

fn other(account_id: AccountId) -> AccountMeta {
    AccountMeta::new(account_id, false, PROGRAM)
}

fn plan_with(
    accounts: Vec<AccountMeta>,
    instruction: &Instruction,
) -> Result<PlanOutput, EscrowError> {
    let input = PlanInput {
        self_account_id: PROGRAM,
        caller_account_id: None,
        accounts,
        instruction_data: borsh::to_vec(instruction).expect("instruction serializes"),
    };
    plan(&input).map(|plan| plan.output().clone())
}

/// Applies the plan's single effect to the job record `pre`, as the runtime would.
fn apply_to(output: &PlanOutput, pre: &[u8]) -> Result<ShardData, EscrowError> {
    let [effect] = output.effects.as_slice() else {
        panic!("expected exactly one effect, got {:?}", output.effects);
    };
    apply_effect(effect, pre)
}

fn apply_effect(effect: &ShardEffect, pre: &[u8]) -> Result<ShardData, EscrowError> {
    assert_eq!(effect.selector, ProgramShardSelector::new(job(), PROGRAM));
    logos_escrow_core::apply(&ApplyInput {
        self_account_id: PROGRAM,
        selector: effect.selector,
        pre_data: ShardData::try_from(pre.to_vec()).expect("fits"),
        effect_data: effect.data.clone(),
    })
}

fn native_transfer(from: AccountId, to: AccountId, amount: u128) -> ChainedCall {
    ChainedCall::new(
        NATIVE_TOKEN_PROGRAM_ID,
        vec![
            ProgramShardSelector::native_balance(from),
            ProgramShardSelector::native_balance(to),
        ],
        &NativeInstruction::Transfer { amount },
    )
}

fn open() -> Instruction {
    Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PAY_TO,
        refund_to: REFUND_TO,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    }
}

fn decode(record: &[u8]) -> Job {
    borsh::from_slice(record).expect("record decodes")
}

#[test]
fn open_locks_the_deposit_in_the_job_account_until_the_deadline() {
    let output = plan_with(vec![signer(BUYER), other(job())], &open()).expect("open plans");

    assert_eq!(
        output.chained_calls,
        vec![native_transfer(BUYER, job(), AMOUNT)]
    );
    assert_eq!(output.timestamp_validity_window.start(), None);
    assert_eq!(output.timestamp_validity_window.end(), Some(DEADLINE));

    let record = apply_to(&output, &[]).expect("open applies to an empty job account");
    assert_eq!(
        decode(&record),
        Job {
            buyer: BUYER,
            provider: PROVIDER,
            pay_to: PAY_TO,
            refund_to: REFUND_TO,
            amount: AMOUNT,
            released: 0,
            claimed: 0,
            deadline: DEADLINE,
            state: JobState::Open,
            result: None,
            controller: None,
        }
    );
}

#[test]
fn open_needs_the_buyer_signature() {
    assert_eq!(
        apply_to(
            &plan_with(vec![other(BUYER), other(job())], &open()).expect("plans"),
            &[]
        ),
        Err(EscrowError::Unauthorized)
    );
}

#[test]
fn open_refuses_a_job_account_not_derived_from_the_buyer_and_job_ref() {
    let someone_elses = job_account_id(&PROGRAM, &PROVIDER, &JOB_REF);
    assert_eq!(
        plan_with(vec![signer(BUYER), other(someone_elses)], &open()).map(|_| ()),
        Err(EscrowError::WrongJobAccount)
    );
}

#[test]
fn open_refuses_a_job_that_already_exists() {
    let output = plan_with(vec![signer(BUYER), other(job())], &open()).expect("open plans");
    let existing = apply_to(&output, &[]).expect("first open applies");
    assert_eq!(apply_to(&output, &existing), Err(EscrowError::JobExists));
}

#[test]
fn open_refuses_terms_that_cannot_settle() {
    let accounts = || vec![signer(BUYER), other(job())];
    let zero = Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PAY_TO,
        refund_to: REFUND_TO,
        amount: 0,
        deadline: DEADLINE,
        controller: None,
    };
    let self_dealing = Instruction::Open {
        job_ref: JOB_REF,
        provider: BUYER,
        pay_to: PAY_TO,
        refund_to: REFUND_TO,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    };
    assert_eq!(
        plan_with(accounts(), &zero).map(|_| ()),
        Err(EscrowError::InvalidTerms)
    );
    assert_eq!(
        plan_with(accounts(), &self_dealing).map(|_| ()),
        Err(EscrowError::InvalidTerms)
    );
}

const RESULT: [u8; 32] = [4; 32];

/// The job record right after a successful `Open`.
fn opened() -> ShardData {
    let output = plan_with(vec![signer(BUYER), other(job())], &open()).expect("open plans");
    apply_to(&output, &[]).expect("open applies")
}

fn deliver() -> Instruction {
    Instruction::Deliver {
        result: RESULT,
        deadline: DEADLINE,
    }
}

#[test]
fn deliver_records_the_result_hash_before_the_deadline() {
    let output =
        plan_with(vec![signer(PROVIDER), other(job())], &deliver()).expect("deliver plans");

    assert!(output.chained_calls.is_empty());
    assert_eq!(output.timestamp_validity_window.start(), None);
    assert_eq!(output.timestamp_validity_window.end(), Some(DEADLINE));

    let record = apply_to(&output, &opened()).expect("deliver applies to an open job");
    assert_eq!(
        decode(&record),
        Job {
            buyer: BUYER,
            provider: PROVIDER,
            pay_to: PAY_TO,
            refund_to: REFUND_TO,
            amount: AMOUNT,
            released: AMOUNT,
            claimed: 0,
            deadline: DEADLINE,
            state: JobState::Delivered,
            result: Some(RESULT),
            controller: None,
        }
    );
}

#[test]
fn only_the_named_provider_can_deliver() {
    assert_eq!(
        apply_to(
            &plan_with(vec![other(PROVIDER), other(job())], &deliver()).expect("plans"),
            &opened()
        ),
        Err(EscrowError::Unauthorized)
    );
    let stranger = AccountId::new([5; 32]);
    let output = plan_with(vec![signer(stranger), other(job())], &deliver()).expect("plans");
    assert_eq!(apply_to(&output, &opened()), Err(EscrowError::WrongParty));
}

#[test]
fn deliver_must_quote_the_job_deadline() {
    let later = Instruction::Deliver {
        result: RESULT,
        deadline: DEADLINE + 1,
    };
    let output = plan_with(vec![signer(PROVIDER), other(job())], &later).expect("plans");
    assert_eq!(
        apply_to(&output, &opened()),
        Err(EscrowError::WrongDeadline)
    );
}

fn delivered() -> ShardData {
    let output = plan_with(vec![signer(PROVIDER), other(job())], &deliver()).expect("plans");
    apply_to(&output, &opened()).expect("deliver applies")
}

#[test]
fn deliver_needs_an_open_job() {
    let output = plan_with(vec![signer(PROVIDER), other(job())], &deliver()).expect("plans");
    assert_eq!(
        apply_to(&output, &delivered()),
        Err(EscrowError::WrongState)
    );
    assert_eq!(apply_to(&output, &[]), Err(EscrowError::NoJob));
}

fn claim() -> Instruction {
    Instruction::Claim {
        buyer: BUYER,
        job_ref: JOB_REF,
        amount: AMOUNT,
    }
}

/// A payout from the job account, authorized by the job account's PDA seed.
fn payout(to: AccountId) -> ChainedCall {
    native_transfer(job(), to, AMOUNT)
        .with_pda_seeds(vec![logos_escrow_core::job_seed(&BUYER, &JOB_REF)])
}

#[test]
fn the_provider_claims_a_delivered_job() {
    let output = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("claim plans");

    assert_eq!(output.chained_calls, vec![payout(PAY_TO)]);
    assert_eq!(output.timestamp_validity_window.end(), None);

    let record = apply_to(&output, &delivered()).expect("claim applies to a delivered job");
    let job = decode(&record);
    assert_eq!((job.state, job.claimed), (JobState::Delivered, AMOUNT));
    assert_eq!(job.result, Some(RESULT));
}

#[test]
fn only_the_named_provider_can_claim() {
    assert_eq!(
        apply_to(
            &plan_with(vec![other(PROVIDER), other(job()), other(PAY_TO)], &claim())
                .expect("plans"),
            &delivered()
        ),
        Err(EscrowError::Unauthorized)
    );
    let stranger = AccountId::new([5; 32]);
    let output = plan_with(
        vec![signer(stranger), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("plans");
    assert_eq!(
        apply_to(&output, &delivered()),
        Err(EscrowError::WrongParty)
    );
}

fn claim_of(amount: u128) -> Instruction {
    Instruction::Claim {
        buyer: BUYER,
        job_ref: JOB_REF,
        amount,
    }
}

#[test]
fn claim_pays_any_part_of_what_is_released_and_unclaimed() {
    let release = plan_with(release_accounts(), &release_of(200)).expect("plans");
    let after_release = apply_to(&release, &opened()).expect("release applies");
    let claim = |amount: u128| {
        plan_with(
            vec![signer(PROVIDER), other(job()), other(PAY_TO)],
            &claim_of(amount),
        )
        .expect("claim plans")
    };

    let part = claim(150);
    assert_eq!(
        part.chained_calls,
        vec![
            native_transfer(job(), PAY_TO, 150)
                .with_pda_seeds(vec![logos_escrow_core::job_seed(&BUYER, &JOB_REF)])
        ]
    );
    let after_part = apply_to(&part, &after_release).expect("a partial claim applies");
    assert_eq!(decode(&after_part).claimed, 150);

    for too_much in [0, 51, u128::MAX] {
        assert_eq!(
            apply_to(&claim(too_much), &after_part),
            Err(EscrowError::WrongAmount),
            "{too_much}"
        );
    }
    let rest = decode(&apply_to(&claim(50), &after_part).expect("the rest applies"));
    assert_eq!(
        (rest.state, rest.released, rest.claimed),
        (JobState::Open, 200, 200)
    );
}

#[test]
fn claim_needs_something_released() {
    let output = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("plans");
    let claimed = apply_to(&output, &delivered()).expect("first claim applies");
    assert_eq!(apply_to(&output, &opened()), Err(EscrowError::WrongAmount));
    assert_eq!(apply_to(&output, &claimed), Err(EscrowError::WrongAmount));
}

fn release_of(amount: u128) -> Instruction {
    Instruction::Release {
        job_ref: JOB_REF,
        amount,
    }
}

/// Releases everything.
fn release() -> Instruction {
    release_of(AMOUNT)
}

fn release_accounts() -> Vec<AccountMeta> {
    vec![signer(BUYER), other(job())]
}

#[test]
fn release_lets_the_provider_claim_part_of_an_open_job_without_moving_money() {
    let output = plan_with(release_accounts(), &release_of(200)).expect("release plans");

    assert!(output.chained_calls.is_empty());
    assert_eq!(output.timestamp_validity_window.end(), None);

    let once = apply_to(&output, &opened()).expect("applies");
    let job = decode(&once);
    assert_eq!(
        (job.state, job.released, job.claimed),
        (JobState::Open, 200, 0)
    );
    let twice = decode(&apply_to(&output, &once).expect("applies again"));
    assert_eq!((twice.state, twice.released), (JobState::Open, 400));
}

#[test]
fn release_cannot_exceed_what_is_left() {
    let two_hundred = plan_with(release_accounts(), &release_of(200)).expect("plans");
    let after_400 = apply_to(
        &two_hundred,
        &apply_to(&two_hundred, &opened()).expect("applies"),
    )
    .expect("applies");
    for too_much in [0, 101, u128::MAX] {
        let output = plan_with(release_accounts(), &release_of(too_much)).expect("plans");
        assert_eq!(
            apply_to(&output, &after_400),
            Err(EscrowError::WrongAmount),
            "{too_much}"
        );
    }
    let the_rest = plan_with(release_accounts(), &release_of(100)).expect("plans");
    assert_eq!(
        decode(&apply_to(&the_rest, &after_400).expect("applies")).released,
        AMOUNT
    );
}

#[test]
fn a_delivered_job_has_nothing_left_to_release() {
    let output = plan_with(release_accounts(), &release_of(1)).expect("plans");
    assert_eq!(
        apply_to(&output, &delivered()),
        Err(EscrowError::WrongState)
    );
}

#[test]
fn release_needs_the_buyer_signature() {
    assert_eq!(
        apply_to(
            &plan_with(vec![other(BUYER), other(job())], &release()).expect("plans"),
            &opened()
        ),
        Err(EscrowError::Unauthorized)
    );
}

/// The job record after the buyer released it.
fn released() -> ShardData {
    let output = plan_with(release_accounts(), &release()).expect("plans");
    apply_to(&output, &opened()).expect("release applies")
}

#[test]
fn the_provider_claims_a_released_job_to_its_pay_to() {
    let output = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("claim plans");
    assert_eq!(output.chained_calls, vec![payout(PAY_TO)]);
    let job = decode(&apply_to(&output, &released()).expect("applies"));
    assert_eq!((job.state, job.claimed), (JobState::Open, AMOUNT));
}

fn refund() -> Instruction {
    Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: DEADLINE,
    }
}

#[test]
fn the_buyer_takes_back_an_undelivered_job_from_the_deadline_on() {
    let output = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("refund plans");

    assert_eq!(output.chained_calls, vec![payout(REFUND_TO)]);
    assert_eq!(output.timestamp_validity_window.start(), Some(DEADLINE));
    assert_eq!(output.timestamp_validity_window.end(), None);

    assert_eq!(
        decode(&apply_to(&output, &opened()).expect("applies")).state,
        JobState::Refunded
    );
}

#[test]
fn refund_needs_the_buyer_signature() {
    let unsigned = plan_with(
        vec![other(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("plans");
    assert_eq!(
        apply_to(&unsigned, &opened()),
        Err(EscrowError::Unauthorized)
    );
}

#[test]
fn refund_must_quote_the_job_deadline_and_amount() {
    let early = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: DEADLINE - 1,
    };
    let output =
        plan_with(vec![signer(BUYER), other(job()), other(REFUND_TO)], &early).expect("plans");
    assert_eq!(
        apply_to(&output, &opened()),
        Err(EscrowError::WrongDeadline)
    );

    let more = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT + 1,
        deadline: DEADLINE,
    };
    let output =
        plan_with(vec![signer(BUYER), other(job()), other(REFUND_TO)], &more).expect("plans");
    assert_eq!(apply_to(&output, &opened()), Err(EscrowError::WrongAmount));
}

#[test]
fn a_delivered_job_cannot_be_refunded() {
    let output = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("plans");
    assert_eq!(
        apply_to(&output, &delivered()),
        Err(EscrowError::WrongState)
    );
}

#[test]
fn malformed_calls_are_refused() {
    let input = PlanInput {
        self_account_id: PROGRAM,
        caller_account_id: None,
        accounts: vec![signer(BUYER), other(job())],
        instruction_data: vec![0xff],
    };
    assert_eq!(
        plan(&input).map(|_| ()),
        Err(EscrowError::InvalidInstruction)
    );
    assert_eq!(
        plan_with(vec![signer(BUYER)], &open()).map(|_| ()),
        Err(EscrowError::InvalidInstruction)
    );
}

/// Literals computed independently (Python `hashlib` and the borsh spec), so a caller that
/// reimplements the derivation and the encoding can check itself against these.
#[test]
fn the_job_account_and_instruction_layout_are_stable() {
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(logos_escrow_core::job_seed(&BUYER, &JOB_REF).as_bytes()),
        "82f93aaaff0ed39ce06c69791bd64f06a3c4e8d97e1e44f2857a71bfbca0a504"
    );
    assert_eq!(
        hex(job().value()),
        "1fa6999db67e6acc47b26a123f282e33e37ad257f071ce550c6ab33c1879cecc"
    );
    assert_eq!(
        hex(&borsh::to_vec(&deliver()).expect("serializes")),
        "01040404040404040404040404040404040404040404040404040404040404040400505c18a3010000"
    );
    assert_eq!(
        hex(&borsh::to_vec(&open()).expect("serializes")),
        concat!(
            "00",
            "0909090909090909090909090909090909090909090909090909090909090909",
            "0202020202020202020202020202020202020202020202020202020202020202",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "0606060606060606060606060606060606060606060606060606060606060606",
            "f4010000000000000000000000000000",
            "00505c18a3010000",
            "00"
        )
    );
    assert_eq!(
        hex(&borsh::to_vec(&release()).expect("serializes")),
        concat!(
            "03",
            "0909090909090909090909090909090909090909090909090909090909090909",
            "f4010000000000000000000000000000"
        )
    );
    assert_eq!(hex(&borsh::to_vec(&give_back()).expect("serializes")), "05");
}

#[test]
fn every_payout_needs_the_job_account_of_the_buyer_and_job_ref() {
    let someone_elses = job_account_id(&PROGRAM, &PROVIDER, &JOB_REF);
    let calls = [
        (
            vec![signer(PROVIDER), other(someone_elses), other(PAY_TO)],
            claim(),
        ),
        (vec![signer(BUYER), other(someone_elses)], release()),
        (
            vec![signer(BUYER), other(someone_elses), other(REFUND_TO)],
            refund(),
        ),
    ];
    for (accounts, instruction) in calls {
        assert_eq!(
            plan_with(accounts, &instruction).map(|_| ()),
            Err(EscrowError::WrongJobAccount),
            "{instruction:?}"
        );
    }
}

/// The program a controlled job names, and one it doesn't.
const CONTROLLER: AccountId = AccountId::new([8; 32]);
const OTHER_PROGRAM: AccountId = AccountId::new([10; 32]);

/// Plans `instruction` as a chained call from `caller`.
fn plan_from(
    caller: AccountId,
    accounts: Vec<AccountMeta>,
    instruction: &Instruction,
) -> Result<PlanOutput, EscrowError> {
    let input = PlanInput {
        self_account_id: PROGRAM,
        caller_account_id: Some(caller),
        accounts,
        instruction_data: borsh::to_vec(instruction).expect("instruction serializes"),
    };
    plan(&input).map(|plan| plan.output().clone())
}

fn open_controlled_by(controller: Option<AccountId>) -> Instruction {
    Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PAY_TO,
        refund_to: REFUND_TO,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller,
    }
}

/// The job record right after the buyer opened it naming `CONTROLLER`.
fn opened_controlled() -> ShardData {
    let output = plan_with(
        vec![signer(BUYER), other(job())],
        &open_controlled_by(Some(CONTROLLER)),
    )
    .expect("open plans");
    apply_to(&output, &[]).expect("open applies")
}

#[test]
fn a_direct_open_may_name_any_controller() {
    assert_eq!(decode(&opened_controlled()).controller, Some(CONTROLLER));
}

/// Literals from the borsh spec, for callers that decode a job record or encode `Open` with a
/// controller themselves.
#[test]
fn the_job_record_and_controlled_open_layout_are_stable() {
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(&opened_controlled()),
        concat!(
            "0101010101010101010101010101010101010101010101010101010101010101",
            "0202020202020202020202020202020202020202020202020202020202020202",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "0606060606060606060606060606060606060606060606060606060606060606",
            "f4010000000000000000000000000000",
            "00000000000000000000000000000000",
            "00000000000000000000000000000000",
            "00505c18a3010000",
            "00",
            "00",
            "01",
            "0808080808080808080808080808080808080808080808080808080808080808"
        )
    );
    assert!(
        hex(&borsh::to_vec(&open_controlled_by(Some(CONTROLLER))).expect("serializes")).ends_with(
            "00505c18a3010000010808080808080808080808080808080808080808080808080808080808080808"
        )
    );
}

#[test]
fn a_program_may_only_open_a_job_it_controls() {
    let accounts = || vec![signer(BUYER), other(job())];
    let itself = plan_from(
        CONTROLLER,
        accounts(),
        &open_controlled_by(Some(CONTROLLER)),
    )
    .expect("plans");
    assert_eq!(
        itself.chained_calls,
        vec![native_transfer(BUYER, job(), AMOUNT)]
    );
    assert_eq!(
        decode(&apply_to(&itself, &[]).expect("applies")).controller,
        Some(CONTROLLER)
    );
    for named in [None, Some(OTHER_PROGRAM)] {
        let output = plan_from(CONTROLLER, accounts(), &open_controlled_by(named)).expect("plans");
        assert_eq!(
            apply_to(&output, &[]),
            Err(EscrowError::NotDirect),
            "{named:?}"
        );
    }
}

#[test]
fn the_controller_releases_and_returns_without_the_parties() {
    let release = plan_from(
        CONTROLLER,
        vec![other(BUYER), other(job())],
        &release_of(200),
    )
    .expect("plans");
    assert_eq!(
        decode(&apply_to(&release, &opened_controlled()).expect("applies")).released,
        200
    );
    let give_back = plan_from(
        CONTROLLER,
        vec![other(PROVIDER), other(job())],
        &give_back(),
    )
    .expect("plans");
    assert_eq!(
        decode(&apply_to(&give_back, &opened_controlled()).expect("applies")).state,
        JobState::Returned
    );
}

#[test]
fn a_program_cannot_act_on_a_job_it_does_not_control() {
    let calls = [
        (vec![other(BUYER), other(job())], release()),
        (vec![other(PROVIDER), other(job())], give_back()),
        (vec![signer(PROVIDER), other(job())], deliver()),
        (vec![signer(PROVIDER), other(job()), other(PAY_TO)], claim()),
        (
            vec![signer(BUYER), other(job()), other(REFUND_TO)],
            refund(),
        ),
    ];
    for (accounts, instruction) in calls {
        // Another program, on a controlled job and on one with no controller.
        let output = plan_from(OTHER_PROGRAM, accounts.clone(), &instruction).expect("plans");
        assert_eq!(
            apply_to(&output, &opened_controlled()),
            Err(EscrowError::NotDirect),
            "{instruction:?}"
        );
        let output = plan_from(CONTROLLER, accounts, &instruction).expect("plans");
        assert_eq!(
            apply_to(&output, &opened()),
            Err(EscrowError::NotDirect),
            "{instruction:?}"
        );
    }
}

/// The controlled job after the controller released all of it.
fn released_controlled() -> ShardData {
    let output =
        plan_from(CONTROLLER, vec![other(BUYER), other(job())], &release()).expect("plans");
    apply_to(&output, &opened_controlled()).expect("release applies")
}

#[test]
fn the_controller_cannot_deliver_or_pull_payouts_for_the_parties() {
    // A payout pulled in a public call to a private address would be lost, so only the side
    // being paid pulls, even when the controller calls.
    let calls = [
        (
            vec![other(PROVIDER), other(job())],
            deliver(),
            opened_controlled(),
        ),
        (
            vec![other(PROVIDER), other(job()), other(PAY_TO)],
            claim(),
            released_controlled(),
        ),
    ];
    for (accounts, instruction, record) in calls {
        let output = plan_from(CONTROLLER, accounts, &instruction).expect("plans");
        assert_eq!(
            apply_to(&output, &record),
            Err(EscrowError::Unauthorized),
            "{instruction:?}"
        );
    }
    let returned = apply_to(
        &plan_from(
            CONTROLLER,
            vec![other(PROVIDER), other(job())],
            &give_back(),
        )
        .expect("plans"),
        &opened_controlled(),
    )
    .expect("return applies");
    let at_once = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: 0,
    };
    let unsigned = plan_from(
        CONTROLLER,
        vec![other(BUYER), other(job()), other(REFUND_TO)],
        &at_once,
    )
    .expect("plans");
    assert_eq!(
        apply_to(&unsigned, &returned),
        Err(EscrowError::Unauthorized)
    );

    // A party the controller authorizes, such as its own PDA, still pulls through it.
    let signed = plan_from(
        CONTROLLER,
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &at_once,
    )
    .expect("plans");
    assert_eq!(
        decode(&apply_to(&signed, &returned).expect("applies")).state,
        JobState::Refunded
    );
}

#[test]
fn the_parties_still_act_directly_on_a_controlled_job() {
    let release = plan_with(release_accounts(), &release_of(200)).expect("plans");
    let after_release = apply_to(&release, &opened_controlled()).expect("release applies");
    let give_back = plan_with(return_accounts(), &give_back()).expect("plans");
    assert_eq!(
        decode(&apply_to(&give_back, &after_release).expect("return applies")).state,
        JobState::Returned
    );
}

#[test]
fn delivery_on_a_controlled_job_releases_nothing() {
    let output =
        plan_with(vec![signer(PROVIDER), other(job())], &deliver()).expect("deliver plans");
    let after = apply_to(&output, &opened_controlled()).expect("deliver applies");
    let record = decode(&after);
    assert_eq!(
        (record.state, record.released, record.result),
        (JobState::Open, 0, Some(RESULT))
    );
    // Once only.
    assert_eq!(apply_to(&output, &after), Err(EscrowError::WrongState));
    // The deadline still lets the buyer take everything back.
    let refund = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("plans");
    assert_eq!(
        decode(&apply_to(&refund, &after).expect("refund applies")).state,
        JobState::Refunded
    );
}

fn give_back() -> Instruction {
    Instruction::Return
}

fn return_accounts() -> Vec<AccountMeta> {
    vec![signer(PROVIDER), other(job())]
}

#[test]
fn return_marks_an_open_job_refundable_without_moving_money() {
    let output = plan_with(return_accounts(), &give_back()).expect("return plans");

    assert!(output.chained_calls.is_empty());
    assert_eq!(output.timestamp_validity_window.start(), None);
    assert_eq!(output.timestamp_validity_window.end(), None);

    assert_eq!(
        decode(&apply_to(&output, &opened()).expect("applies")).state,
        JobState::Returned
    );
}

#[test]
fn only_the_named_provider_can_return_a_job() {
    assert_eq!(
        apply_to(
            &plan_with(vec![other(PROVIDER), other(job())], &give_back()).expect("plans"),
            &opened()
        ),
        Err(EscrowError::Unauthorized)
    );

    let stranger = AccountId::new([5; 32]);
    let output = plan_with(vec![signer(stranger), other(job())], &give_back()).expect("plans");
    assert_eq!(apply_to(&output, &opened()), Err(EscrowError::WrongParty));
}

/// The job record after the provider returned it.
fn returned() -> ShardData {
    let output = plan_with(return_accounts(), &give_back()).expect("plans");
    apply_to(&output, &opened()).expect("return applies")
}

#[test]
fn only_an_open_job_can_be_returned() {
    let output = plan_with(return_accounts(), &give_back()).expect("plans");
    for record in [delivered(), returned()] {
        assert_eq!(apply_to(&output, &record), Err(EscrowError::WrongState));
    }
}

#[test]
fn the_buyer_refunds_a_returned_job_at_once_to_its_refund_to() {
    // After a return the deadline no longer applies, so the buyer passes 0.
    let at_once = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: 0,
    };
    let output = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &at_once,
    )
    .expect("refund plans");
    assert_eq!(output.chained_calls, vec![payout(REFUND_TO)]);
    assert_eq!(output.timestamp_validity_window.start(), Some(0));
    assert_eq!(
        decode(&apply_to(&output, &returned()).expect("applies")).state,
        JobState::Refunded
    );
    // On a job that wasn't returned, a refund still has to quote the deadline.
    assert_eq!(
        apply_to(&output, &opened()),
        Err(EscrowError::WrongDeadline)
    );
}

#[test]
fn refund_returns_only_what_was_never_released() {
    let release = plan_with(release_accounts(), &release_of(200)).expect("plans");
    let after_release = apply_to(&release, &opened()).expect("release applies");
    let refund_of = |amount: u128| {
        plan_with(
            vec![signer(BUYER), other(job()), other(REFUND_TO)],
            &Instruction::Refund {
                job_ref: JOB_REF,
                amount,
                deadline: DEADLINE,
            },
        )
        .expect("refund plans")
    };
    for wrong in [0, 299, AMOUNT] {
        assert_eq!(
            apply_to(&refund_of(wrong), &after_release),
            Err(EscrowError::WrongAmount),
            "{wrong}"
        );
    }
    let rest = refund_of(300);
    assert_eq!(
        rest.chained_calls,
        vec![
            native_transfer(job(), REFUND_TO, 300)
                .with_pda_seeds(vec![logos_escrow_core::job_seed(&BUYER, &JOB_REF)])
        ]
    );
    let after_refund = apply_to(&rest, &after_release).expect("the refund applies");
    assert_eq!(decode(&after_refund).state, JobState::Refunded);

    // What was released before the refund is still the provider's to claim.
    let claim = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim_of(200),
    )
    .expect("plans");
    assert_eq!(
        decode(&apply_to(&claim, &after_refund).expect("claim applies")).claimed,
        200
    );
}

#[test]
fn nothing_released_cannot_be_claimed_and_nothing_left_cannot_be_refunded() {
    let claim = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("plans");
    assert_eq!(apply_to(&claim, &returned()), Err(EscrowError::WrongAmount));
    let refund = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("plans");
    assert_eq!(
        apply_to(&refund, &released()),
        Err(EscrowError::WrongAmount)
    );
}

#[test]
fn a_payout_goes_only_to_the_jobs_payout_address() {
    let stranger = AccountId::new([5; 32]);
    let claim = plan_with(
        vec![signer(PROVIDER), other(job()), other(stranger)],
        &claim(),
    )
    .expect("plans");
    assert_eq!(apply_to(&claim, &delivered()), Err(EscrowError::WrongParty));
    let refund = plan_with(
        vec![signer(BUYER), other(job()), other(stranger)],
        &refund(),
    )
    .expect("plans");
    assert_eq!(apply_to(&refund, &opened()), Err(EscrowError::WrongParty));
}

/// A job whose payout addresses are the parties themselves.
fn opened_paying_the_parties() -> ShardData {
    let open = Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PROVIDER,
        refund_to: BUYER,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    };
    let output = plan_with(vec![signer(BUYER), other(job())], &open).expect("open plans");
    apply_to(&output, &[]).expect("open applies")
}

#[test]
fn a_payout_to_the_signer_itself_leaves_out_the_payee_account() {
    // LEZ refuses a call that names the same account twice, so the signer stands in for it.
    let refund = plan_with(vec![signer(BUYER), other(job())], &refund()).expect("plans");
    assert_eq!(refund.chained_calls, vec![payout(BUYER)]);
    assert_eq!(
        decode(&apply_to(&refund, &opened_paying_the_parties()).expect("applies")).state,
        JobState::Refunded
    );
    // That only works when the job pays the signer.
    assert_eq!(apply_to(&refund, &opened()), Err(EscrowError::WrongParty));
}

#[test]
fn open_refuses_the_job_account_as_a_payout_address() {
    let accounts = || vec![signer(BUYER), other(job())];
    let pays_itself = Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: job(),
        refund_to: REFUND_TO,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    };
    let refunds_itself = Instruction::Open {
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PAY_TO,
        refund_to: job(),
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    };
    for instruction in [pays_itself, refunds_itself] {
        assert_eq!(
            plan_with(accounts(), &instruction).map(|_| ()),
            Err(EscrowError::InvalidTerms)
        );
    }
}

/// The job record after the provider claimed it.
fn claimed() -> ShardData {
    let output = plan_with(
        vec![signer(PROVIDER), other(job()), other(PAY_TO)],
        &claim(),
    )
    .expect("plans");
    apply_to(&output, &delivered()).expect("claim applies")
}

/// The job record after the buyer refunded it.
fn refunded() -> ShardData {
    let at_once = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: 0,
    };
    let output = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &at_once,
    )
    .expect("plans");
    apply_to(&output, &returned()).expect("refund applies")
}

#[test]
fn a_finished_or_returned_job_cannot_be_released() {
    let output = plan_with(release_accounts(), &release()).expect("plans");
    for record in [claimed(), refunded(), returned()] {
        assert_eq!(apply_to(&output, &record), Err(EscrowError::WrongState));
    }
}

#[test]
fn a_finished_job_cannot_be_refunded_again() {
    let output = plan_with(
        vec![signer(BUYER), other(job()), other(REFUND_TO)],
        &refund(),
    )
    .expect("plans");
    for record in [claimed(), refunded()] {
        assert_eq!(apply_to(&output, &record), Err(EscrowError::WrongState));
    }
}

#[test]
fn a_two_account_claim_pays_only_a_provider_that_is_its_own_pay_to() {
    let claim = plan_with(vec![signer(PROVIDER), other(job())], &claim()).expect("plans");
    assert_eq!(claim.chained_calls, vec![payout(PROVIDER)]);
    assert_eq!(apply_to(&claim, &delivered()), Err(EscrowError::WrongParty));
}
