//! Every refusal of the example arbiter, through its `plan` and `apply` the way LEZ calls them.
//! The happy paths run on LEZ's state machine in `integration/tests/flows.rs`.

use lee_core::{
    account::{AccountId, ShardData},
    program::{AccountMeta, ApplyInput, PlanInput, PlanOutput},
};
use logos_escrow_arbiter::{
    ArbiterError, ArbiterInstruction, Case, Decision, apply, case_account_id, plan,
};
use logos_escrow_core::job_account_id;

const ARBITER: AccountId = AccountId::new([0xA7; 32]);
const ESCROW: AccountId = AccountId::new([0xE5; 32]);
const FAKE_ESCROW: AccountId = AccountId::new([0xFE; 32]);
const BUYER: AccountId = AccountId::new([1; 32]);
const PROVIDER: AccountId = AccountId::new([2; 32]);
const JUDGE: AccountId = AccountId::new([3; 32]);
const ATTACKER: AccountId = AccountId::new([4; 32]);
const JOB_REF: [u8; 32] = [9; 32];

fn job() -> AccountId {
    job_account_id(&ESCROW, &BUYER, &JOB_REF)
}

fn case() -> AccountId {
    case_account_id(&ARBITER, &job())
}

fn signer(id: AccountId) -> AccountMeta {
    AccountMeta::new(id, true, ARBITER)
}

fn other(id: AccountId) -> AccountMeta {
    AccountMeta::new(id, false, ARBITER)
}

fn plan_from(
    caller: Option<AccountId>,
    accounts: Vec<AccountMeta>,
    instruction: &ArbiterInstruction,
) -> Result<PlanOutput, ArbiterError> {
    let input = PlanInput {
        self_account_id: ARBITER,
        caller_account_id: caller,
        accounts,
        instruction_data: borsh::to_vec(instruction).expect("serializes"),
    };
    plan(&input).map(|plan| plan.output().clone())
}

fn apply_to(output: &PlanOutput, pre: &[u8]) -> Result<ShardData, ArbiterError> {
    let [effect] = output.effects.as_slice() else {
        panic!("expected one effect, got {:?}", output.effects);
    };
    apply(&ApplyInput {
        self_account_id: ARBITER,
        selector: effect.selector,
        pre_data: ShardData::try_from(pre.to_vec()).expect("fits"),
        effect_data: effect.data.clone(),
    })
}

fn open(escrow: AccountId) -> ArbiterInstruction {
    ArbiterInstruction::Open {
        escrow,
        judge: JUDGE,
        job_ref: JOB_REF,
        provider: PROVIDER,
        pay_to: PROVIDER,
        refund_to: BUYER,
        amount: 500,
        deadline: 1_800_000_000_000,
    }
}

/// The case right after the buyer opened the job.
fn opened() -> ShardData {
    let output = plan_from(
        None,
        vec![signer(BUYER), other(case()), other(job())],
        &open(ESCROW),
    )
    .expect("open plans");
    apply_to(&output, &[]).expect("open applies")
}

fn release(escrow: AccountId) -> ArbiterInstruction {
    ArbiterInstruction::Decide {
        escrow,
        decision: Decision::Release {
            job_ref: JOB_REF,
            amount: 100,
        },
    }
}

fn decide_accounts(judge: AccountMeta) -> Vec<AccountMeta> {
    vec![judge, other(case()), other(BUYER), other(job())]
}

#[test]
fn open_records_the_judge_and_the_escrow() {
    let case: Case = borsh::from_slice(&opened()).expect("a case");
    assert_eq!(
        case,
        Case {
            escrow: ESCROW,
            judge: JUDGE
        }
    );
}

#[test]
fn open_needs_the_buyer_signature() {
    assert_eq!(
        plan_from(
            None,
            vec![other(BUYER), other(case()), other(job())],
            &open(ESCROW)
        )
        .map(|_| ()),
        Err(ArbiterError::Unauthorized)
    );
}

#[test]
fn nobody_takes_a_case_for_someone_elses_job() {
    // The attacker signs as the buyer of the victim's job, through an escrow of their own.
    let squat = plan_from(
        None,
        vec![signer(ATTACKER), other(case()), other(job())],
        &open(FAKE_ESCROW),
    );
    assert_eq!(squat.map(|_| ()), Err(ArbiterError::WrongCase));
}

#[test]
fn open_refuses_a_case_account_not_derived_from_the_job() {
    let elsewhere = case_account_id(&ARBITER, &job_account_id(&ESCROW, &BUYER, &[8; 32]));
    assert_eq!(
        plan_from(
            None,
            vec![signer(BUYER), other(elsewhere), other(job())],
            &open(ESCROW)
        )
        .map(|_| ()),
        Err(ArbiterError::WrongCase)
    );
}

#[test]
fn a_case_is_opened_once() {
    let output = plan_from(
        None,
        vec![signer(BUYER), other(case()), other(job())],
        &open(ESCROW),
    )
    .expect("plans");
    assert_eq!(apply_to(&output, &opened()), Err(ArbiterError::CaseExists));
}

#[test]
fn only_the_cases_judge_decides_and_only_for_its_escrow() {
    let by_attacker =
        plan_from(None, decide_accounts(signer(ATTACKER)), &release(ESCROW)).expect("plans");
    assert_eq!(
        apply_to(&by_attacker, &opened()),
        Err(ArbiterError::WrongJudge)
    );
    let elsewhere =
        plan_from(None, decide_accounts(signer(JUDGE)), &release(FAKE_ESCROW)).expect("plans");
    assert_eq!(
        apply_to(&elsewhere, &opened()),
        Err(ArbiterError::WrongEscrow)
    );
    let judged = plan_from(None, decide_accounts(signer(JUDGE)), &release(ESCROW)).expect("plans");
    assert!(apply_to(&judged, &opened()).is_ok());
}

#[test]
fn a_decision_needs_the_judge_to_sign_directly() {
    assert_eq!(
        plan_from(None, decide_accounts(other(JUDGE)), &release(ESCROW)).map(|_| ()),
        Err(ArbiterError::Unauthorized)
    );
    // LEZ carries the judge's signature into any program the judge signs for.
    assert_eq!(
        plan_from(
            Some(ATTACKER),
            decide_accounts(signer(JUDGE)),
            &release(ESCROW)
        )
        .map(|_| ()),
        Err(ArbiterError::NotDirect)
    );
}

#[test]
fn a_decision_needs_a_case() {
    let output = plan_from(None, decide_accounts(signer(JUDGE)), &release(ESCROW)).expect("plans");
    assert_eq!(apply_to(&output, &[]), Err(ArbiterError::NoCase));
}

/// Arbiter refusals use codes the escrow's errors (1 to 11) never do, so a refusal says which
/// program refused.
#[test]
fn arbiter_codes_do_not_overlap_the_escrows() {
    for error in [
        ArbiterError::InvalidInstruction,
        ArbiterError::Unauthorized,
        ArbiterError::WrongCase,
        ArbiterError::CaseExists,
        ArbiterError::WrongJudge,
        ArbiterError::WrongEscrow,
        ArbiterError::NoCase,
        ArbiterError::NotDirect,
    ] {
        assert!(error.code() > 100, "{error:?}");
    }
}
