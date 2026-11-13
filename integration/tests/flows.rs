//! Runs the built escrow guest through LEZ v0.3.1's state machine, as the sequencer does: real
//! signatures, chained native transfers, PDA authorization and validity windows.
//!
//! Needs the guest binaries: `make build`.

use std::borrow::Cow;

use lee::{
    PrivateKey, PublicKey, PublicTransaction, V03State,
    error::{InvalidProgramBehaviorError, LeeError},
    program::Program,
    public_transaction::{Message, WitnessSet},
};
use lee_core::{
    account::{AccountId, Balance, ProgramShardSelector},
    native_token::{Instruction as NativeInstruction, NATIVE_TOKEN_PROGRAM_ID, TransferError},
    program::PdaSeed,
};
use logos_escrow_core::{Instruction, Job, JobState, job_account_id};

const GUEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../methods/guest/target/riscv32im-risc0-zkvm-elf/docker/logos_escrow.bin"
);
/// The test-only controller, which relays whatever escrow call it is given.
const CONTROLLER_GUEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../methods/guest/target/riscv32im-risc0-zkvm-elf/docker/test_controller.bin"
);
/// Where the test deploys the escrow program; on chain this is the header account.
const PROGRAM: AccountId = AccountId::new([0xE5; 32]);
const CONTROLLER: AccountId = AccountId::new([0xC0; 32]);
const JOB_REF: [u8; 32] = [9; 32];
const AMOUNT: Balance = 500;
const START: Balance = 10_000;
const DEADLINE: u64 = 1_800_000_000_000;
const BEFORE: u64 = DEADLINE - 1;
const RESULT: [u8; 32] = [4; 32];
/// Where the provider is paid and the buyer refunded: empty public accounts, apart from the parties.
const PAY_TO: AccountId = AccountId::new([0x33; 32]);
const REFUND_TO: AccountId = AccountId::new([0x66; 32]);

struct Chain {
    state: V03State,
    block: u64,
}

struct Party {
    key: PrivateKey,
    id: AccountId,
}

impl Party {
    fn new(seed: u8) -> Self {
        let key = PrivateKey::try_new([seed; 32]).expect("valid key");
        let id = AccountId::from(&PublicKey::new_from_private_key(&key));
        Self { key, id }
    }
}

fn buyer() -> Party {
    Party::new(1)
}

fn provider() -> Party {
    Party::new(2)
}

/// Signs calls to the controller; it is neither party.
fn operator() -> Party {
    Party::new(3)
}

fn job() -> AccountId {
    job_account_id(&PROGRAM, &buyer().id, &JOB_REF)
}

fn load(path: &str) -> Program {
    let elf = std::fs::read(path).unwrap_or_else(|error| {
        panic!("guest binary missing at {path} ({error}); build it with make build")
    });
    Program::new(Cow::Owned(elf)).expect("valid guest")
}

impl Chain {
    fn new() -> Self {
        let state = V03State::new()
            .with_public_account_balances([
                (buyer().id, START),
                (provider().id, START),
                (operator().id, START),
            ])
            .with_named_programs([(PROGRAM, load(GUEST)), (CONTROLLER, load(CONTROLLER_GUEST))]);
        Self { state, block: 1 }
    }

    /// Sends one escrow call signed by `signer`, naming `accounts` with the escrow shard.
    fn call(
        &mut self,
        signer: &Party,
        accounts: &[AccountId],
        instruction: &Instruction,
        timestamp: u64,
    ) -> Result<(), LeeError> {
        let selectors = accounts
            .iter()
            .map(|id| ProgramShardSelector::new(*id, PROGRAM))
            .collect();
        self.send(PROGRAM, signer, selectors, instruction, timestamp)
    }

    /// Has the controller make one escrow call, with `seeds` authorizing its PDAs. The operator
    /// signs, so no party does.
    fn relay(
        &mut self,
        accounts: &[AccountId],
        seeds: Vec<[u8; 32]>,
        instruction: &Instruction,
        timestamp: u64,
    ) -> Result<(), LeeError> {
        self.relay_signed_by(&operator(), accounts, seeds, instruction, timestamp)
    }

    fn relay_signed_by(
        &mut self,
        signer: &Party,
        accounts: &[AccountId],
        seeds: Vec<[u8; 32]>,
        instruction: &Instruction,
        timestamp: u64,
    ) -> Result<(), LeeError> {
        let selectors = accounts
            .iter()
            .map(|id| ProgramShardSelector::new(*id, CONTROLLER))
            .collect();
        let relay = (
            PROGRAM,
            seeds,
            borsh::to_vec(instruction).expect("instruction serializes"),
        );
        self.send(CONTROLLER, signer, selectors, &relay, timestamp)
    }

    fn send<I: borsh::BorshSerialize>(
        &mut self,
        program: AccountId,
        signer: &Party,
        selectors: Vec<ProgramShardSelector>,
        instruction: &I,
        timestamp: u64,
    ) -> Result<(), LeeError> {
        let nonce = self.state.get_account_by_id(signer.id).nonce;
        let message = Message::try_new(program, selectors, vec![nonce], instruction)?;
        let witness_set = WitnessSet::for_message(&message, &[&signer.key]);
        self.block += 1;
        self.state
            .transition_from_public_transaction(
                &PublicTransaction::new(message, witness_set),
                self.block,
                timestamp,
            )
            .map(drop)
    }

    fn balance(&self, account_id: AccountId) -> Balance {
        self.state
            .get_account_by_id(account_id)
            .data
            .native_balance()
            .expect("canonical balance")
    }

    fn job(&self) -> Job {
        self.job_at(job())
    }

    fn job_at(&self, job: AccountId) -> Job {
        let account = self.state.get_account_by_id(job);
        borsh::from_slice(account.data.shard(PROGRAM)).expect("job record decodes")
    }

    fn open(&mut self) -> Result<(), LeeError> {
        self.open_controlled_by(None)
    }

    fn open_controlled_by(&mut self, controller: Option<AccountId>) -> Result<(), LeeError> {
        self.call(
            &buyer(),
            &[buyer().id, job()],
            &open_instruction(controller),
            BEFORE,
        )
    }

    fn deliver(&mut self, timestamp: u64) -> Result<(), LeeError> {
        let deliver = Instruction::Deliver {
            result: RESULT,
            deadline: DEADLINE,
        };
        self.call(&provider(), &[provider().id, job()], &deliver, timestamp)
    }

    fn claim(&mut self) -> Result<(), LeeError> {
        self.claim_of(AMOUNT)
    }

    fn claim_of(&mut self, amount: Balance) -> Result<(), LeeError> {
        let claim = Instruction::Claim {
            buyer: buyer().id,
            job_ref: JOB_REF,
            amount,
        };
        self.call(
            &provider(),
            &[provider().id, job(), PAY_TO],
            &claim,
            DEADLINE + 10,
        )
    }

    fn refund(&mut self, timestamp: u64) -> Result<(), LeeError> {
        let refund = Instruction::Refund {
            job_ref: JOB_REF,
            amount: AMOUNT,
            deadline: DEADLINE,
        };
        self.call(
            &buyer(),
            &[buyer().id, job(), REFUND_TO],
            &refund,
            timestamp,
        )
    }
}

fn open_instruction(controller: Option<AccountId>) -> Instruction {
    Instruction::Open {
        job_ref: JOB_REF,
        provider: provider().id,
        pay_to: PAY_TO,
        refund_to: REFUND_TO,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller,
    }
}

fn release_of(amount: Balance) -> Instruction {
    Instruction::Release {
        job_ref: JOB_REF,
        amount,
    }
}

fn refused_with(code: u8) -> impl Fn(&Result<(), LeeError>) -> bool {
    move |result| matches!(result, Err(LeeError::ProgramExitedWithCode { code: c, .. }) if *c == u32::from(code))
}

#[test]
fn open_deliver_claim_pays_the_providers_pay_to() {
    let mut chain = Chain::new();

    chain.open().expect("open");
    assert_eq!(chain.balance(buyer().id), START - AMOUNT);
    assert_eq!(chain.balance(job()), AMOUNT);
    assert_eq!(chain.job().state, JobState::Open);

    chain.deliver(BEFORE).expect("deliver");
    assert_eq!(chain.job().state, JobState::Delivered);
    assert_eq!(chain.job().result, Some(RESULT));

    chain.claim().expect("claim");
    assert_eq!(chain.job().claimed, AMOUNT);
    assert_eq!(chain.balance(job()), 0);
    assert_eq!(chain.balance(PAY_TO), AMOUNT);
    assert_eq!(chain.balance(provider().id), START);
    assert_eq!(chain.balance(buyer().id), START - AMOUNT);
}

#[test]
fn an_undelivered_job_refunds_the_buyer_only_from_the_deadline() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    assert!(matches!(
        chain.refund(BEFORE),
        Err(LeeError::OutOfValidityWindow)
    ));
    assert_eq!(chain.balance(job()), AMOUNT);

    chain.refund(DEADLINE).expect("refund at the deadline");
    assert_eq!(chain.job().state, JobState::Refunded);
    assert_eq!(chain.balance(job()), 0);
    assert_eq!(chain.balance(REFUND_TO), AMOUNT);
}

#[test]
fn a_late_delivery_is_refused_and_the_buyer_can_refund() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    assert!(matches!(
        chain.deliver(DEADLINE),
        Err(LeeError::OutOfValidityWindow)
    ));
    chain.refund(DEADLINE).expect("refund");
    assert_eq!(chain.balance(REFUND_TO), AMOUNT);
}

#[test]
fn opening_after_the_deadline_is_refused() {
    let mut chain = Chain::new();
    let late = chain.call(
        &buyer(),
        &[buyer().id, job()],
        &open_instruction(None),
        DEADLINE,
    );
    assert!(matches!(late, Err(LeeError::OutOfValidityWindow)));
    assert_eq!(chain.balance(buyer().id), START);
}

#[test]
fn released_parts_are_claimed_and_the_rest_refunded_after_the_deadline() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    chain
        .call(&buyer(), &[buyer().id, job()], &release_of(200), BEFORE)
        .expect("release part");
    assert_eq!(chain.job().released, 200);
    assert_eq!(chain.balance(job()), AMOUNT);

    chain.claim_of(200).expect("claim the released part");
    assert_eq!(chain.balance(PAY_TO), 200);

    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT - 200,
        deadline: DEADLINE,
    };
    chain
        .call(&buyer(), &[buyer().id, job(), REFUND_TO], &refund, DEADLINE)
        .expect("refund the rest");
    assert_eq!(chain.job().state, JobState::Refunded);
    assert_eq!(chain.balance(REFUND_TO), AMOUNT - 200);
    assert_eq!(chain.balance(job()), 0);
}

#[test]
fn a_refused_call_exits_with_its_code_and_moves_nothing() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    // Claiming before anything is released.
    assert!(refused_with(
        logos_escrow_core::EscrowError::WrongAmount.code()
    )(&chain.claim()));
    assert_eq!(chain.balance(job()), AMOUNT);
    assert_eq!(chain.balance(PAY_TO), 0);
    assert_eq!(chain.job().state, JobState::Open);
}

#[test]
fn nobody_can_move_the_deposit_without_the_escrow_program() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    // The buyer signs a plain native transfer out of the job account.
    let drain = chain.send(
        NATIVE_TOKEN_PROGRAM_ID,
        &buyer(),
        vec![
            ProgramShardSelector::native_balance(job()),
            ProgramShardSelector::native_balance(buyer().id),
        ],
        &NativeInstruction::Transfer { amount: AMOUNT },
        BEFORE,
    );
    assert!(matches!(
        drain,
        Err(LeeError::InvalidProgramBehavior(InvalidProgramBehaviorError::NativeTransferFailed(
            TransferError::UnauthorizedSender { account_id }
        ))) if account_id == job()
    ));
    assert_eq!(chain.balance(job()), AMOUNT);
}

#[test]
fn a_returned_job_is_refunded_before_the_deadline() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    chain
        .call(
            &provider(),
            &[provider().id, job()],
            &Instruction::Return,
            BEFORE,
        )
        .expect("return");
    assert_eq!(chain.job().state, JobState::Returned);
    assert_eq!(chain.balance(job()), AMOUNT);

    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: 0,
    };
    chain
        .call(&buyer(), &[buyer().id, job(), REFUND_TO], &refund, BEFORE)
        .expect("refund before the deadline");
    assert_eq!(chain.job().state, JobState::Refunded);
    assert_eq!(chain.balance(REFUND_TO), AMOUNT);
    assert_eq!(chain.balance(provider().id), START);
}

#[test]
fn a_job_paying_the_buyer_itself_refunds_with_two_accounts() {
    let mut chain = Chain::new();
    let open = Instruction::Open {
        job_ref: JOB_REF,
        provider: provider().id,
        pay_to: provider().id,
        refund_to: buyer().id,
        amount: AMOUNT,
        deadline: DEADLINE,
        controller: None,
    };
    chain
        .call(&buyer(), &[buyer().id, job()], &open, BEFORE)
        .expect("open");

    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: DEADLINE,
    };
    chain
        .call(&buyer(), &[buyer().id, job()], &refund, DEADLINE)
        .expect("refund to the buyer itself");
    assert_eq!(chain.job().state, JobState::Refunded);
    assert_eq!(chain.balance(buyer().id), START);
}

#[test]
fn a_controller_releases_part_and_returns_the_rest_without_the_parties() {
    let mut chain = Chain::new();
    chain.open_controlled_by(Some(CONTROLLER)).expect("open");

    // On a controlled job, delivering releases nothing: the controller judges it.
    chain.deliver(BEFORE).expect("deliver");
    assert_eq!(chain.job().released, 0);
    assert!(refused_with(
        logos_escrow_core::EscrowError::WrongAmount.code()
    )(&chain.claim()));

    chain
        .relay(&[buyer().id, job()], vec![], &release_of(200), BEFORE)
        .expect("the controller releases part");
    chain
        .relay(
            &[provider().id, job()],
            vec![],
            &Instruction::Return,
            BEFORE,
        )
        .expect("the controller returns the rest");
    assert_eq!(chain.job().state, JobState::Returned);

    chain.claim_of(200).expect("the provider claims its part");
    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT - 200,
        deadline: 0,
    };
    chain
        .call(&buyer(), &[buyer().id, job(), REFUND_TO], &refund, BEFORE)
        .expect("the buyer refunds the rest");
    assert_eq!(chain.balance(PAY_TO), 200);
    assert_eq!(chain.balance(REFUND_TO), AMOUNT - 200);
    assert_eq!(chain.balance(job()), 0);
}

#[test]
fn a_program_cannot_act_on_a_job_it_does_not_control_even_when_the_buyer_signs() {
    let mut chain = Chain::new();
    chain.open().expect("open");

    // The buyer signs a call to some program, and LEZ carries that signature into the
    // program's call to the escrow.
    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: 0,
    };
    let calls = [
        (vec![buyer().id, job()], release_of(AMOUNT)),
        (vec![buyer().id, job(), REFUND_TO], refund),
    ];
    for (accounts, instruction) in calls {
        let result = chain.relay_signed_by(&buyer(), &accounts, vec![], &instruction, BEFORE);
        assert!(
            refused_with(logos_escrow_core::EscrowError::NotDirect.code())(&result),
            "{instruction:?}: {result:?}"
        );
    }
    assert_eq!(chain.job().released, 0);
    assert_eq!(chain.balance(job()), AMOUNT);
}

#[test]
fn a_program_opens_a_job_only_for_itself_and_cannot_claim_for_the_provider() {
    let mut chain = Chain::new();
    // The buyer signs, but the controller names no controller, or another program.
    for named in [None, Some(PROGRAM)] {
        let result = chain.relay_signed_by(
            &buyer(),
            &[buyer().id, job()],
            vec![],
            &open_instruction(named),
            BEFORE,
        );
        assert!(
            refused_with(logos_escrow_core::EscrowError::NotDirect.code())(&result),
            "{named:?}: {result:?}"
        );
    }
    assert_eq!(chain.balance(buyer().id), START);

    chain.open_controlled_by(Some(CONTROLLER)).expect("open");
    chain
        .relay(&[buyer().id, job()], vec![], &release_of(AMOUNT), BEFORE)
        .expect("the controller releases everything");
    let claim = Instruction::Claim {
        buyer: buyer().id,
        job_ref: JOB_REF,
        amount: AMOUNT,
    };
    assert!(refused_with(
        logos_escrow_core::EscrowError::Unauthorized.code()
    )(&chain.relay(
        &[provider().id, job(), PAY_TO],
        vec![],
        &claim,
        BEFORE
    )));
    assert_eq!(chain.balance(PAY_TO), 0);
    assert_eq!(chain.balance(job()), AMOUNT);
}

#[test]
fn a_controllers_own_account_opens_and_refunds_a_job_through_it() {
    let mut chain = Chain::new();
    let seed = [0x5E; 32];
    let treasury = AccountId::for_public_pda(&CONTROLLER, &PdaSeed::new(seed));
    let job = job_account_id(&PROGRAM, &treasury, &JOB_REF);
    chain
        .send(
            NATIVE_TOKEN_PROGRAM_ID,
            &buyer(),
            vec![
                ProgramShardSelector::native_balance(buyer().id),
                ProgramShardSelector::native_balance(treasury),
            ],
            &NativeInstruction::Transfer { amount: AMOUNT },
            BEFORE,
        )
        .expect("fund the treasury");

    chain
        .relay(
            &[treasury, job],
            vec![seed],
            &open_instruction(Some(CONTROLLER)),
            BEFORE,
        )
        .expect("the treasury opens a job through the controller");
    assert_eq!(chain.balance(treasury), 0);
    assert_eq!(chain.balance(job), AMOUNT);
    assert_eq!(chain.job_at(job).controller, Some(CONTROLLER));

    // Without the seed the treasury hasn't authorized the refund.
    let refund = Instruction::Refund {
        job_ref: JOB_REF,
        amount: AMOUNT,
        deadline: DEADLINE,
    };
    assert!(refused_with(
        logos_escrow_core::EscrowError::Unauthorized.code()
    )(&chain.relay(
        &[treasury, job, REFUND_TO],
        vec![],
        &refund,
        DEADLINE
    )));
    chain
        .relay(&[treasury, job, REFUND_TO], vec![seed], &refund, DEADLINE)
        .expect("the treasury refunds through the controller");
    assert_eq!(chain.balance(REFUND_TO), AMOUNT);
    assert_eq!(chain.balance(job), 0);
}
