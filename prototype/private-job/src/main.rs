//! PROTOTYPE, throwaway. Does a fully private escrow job work on LEZ v0.3.1?
//!
//! The job account is a private PDA of the escrow whose keys the buyer and the provider share
//! through one group master secret. Two separate wallets, one per party, drive a job from open to
//! claim, then race each other on a second job. After every step it prints what each wallet sees
//! and what the chain shows anyone.
//!
//! Run against the local standalone sequencer with `RISC0_DEV_MODE=1`; see `run.sh`.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, bail};
use common::transaction::LeeTransaction;
use key_protocol::key_management::group_key_holder::GroupKeyHolder;
use lee::{privacy_preserving_transaction::circuit::ProgramWithDependencies, program::Program};
use lee_core::{
    Identifier, NullifierPublicKey,
    account::{AccountId, Balance},
    encryption::ViewingPublicKey,
    program::{PdaSeed, PrivateAccountKind},
};
use logos_escrow_core::{Hash32, Instruction, Job, job_seed};
use tokio::sync::Mutex;
use wallet::{
    AccountIdentity, AccountMention, WalletCore, account::Label,
    storage::key_chain::SharedAccountEntry,
};

const AMOUNT: Balance = 400_000;

struct Party {
    name: &'static str,
    wallet: WalletCore,
    /// The party's own private account, which signs.
    id: AccountId,
}

/// The job account both parties hold the keys to.
#[derive(Clone)]
struct SharedJob {
    id: AccountId,
    seed: PdaSeed,
    identity: AccountIdentity,
}

fn open_wallet(dir: &str) -> Result<WalletCore> {
    let dir = PathBuf::from(dir);
    WalletCore::new_update_chain(
        dir.join("wallet_config.json"),
        dir.join("storage.json"),
        None,
    )
}

fn arg(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("set {name}"))
}

fn random32() -> [u8; 32] {
    *wallet::cli::identifier_or_random(None).value()
}

/// The job account a group master secret gives for `buyer` and `job_ref`. The wallet's own
/// `create_shared_pda_account` assumes a built-in program and would derive the wrong id for a
/// deployed one, so this builds the identity with the escrow's header as the authority.
fn shared_job(
    gms: [u8; 32],
    program: AccountId,
    image_id: lee_core::program::ProgramId,
    buyer: AccountId,
    job_ref: Hash32,
) -> SharedJob {
    let seed = job_seed(&buyer, &job_ref);
    let keys = GroupKeyHolder::from_gms(gms).derive_keys_for_pda(&image_id, &seed);
    let npk = keys.generate_nullifier_public_key();
    let vpk = keys.generate_viewing_public_key();
    let identifier = Identifier::new(job_ref);
    let id = AccountId::for_private_pda(&program, &seed, &npk, &vpk, identifier);
    let identity = AccountIdentity::PrivatePdaShared {
        authority: program,
        seed,
        nsk: keys.nullifier_secret_key(),
        vpk,
        identifier,
    };
    SharedJob { id, seed, identity }
}

/// Lets a wallet follow the shared job: it learns the group secret and the job's derivation.
fn register(
    party: &mut Party,
    gms: [u8; 32],
    image_id: lee_core::program::ProgramId,
    job: &SharedJob,
    label: &str,
) -> Result<()> {
    party
        .wallet
        .insert_group_key_holder(Label::new(label), GroupKeyHolder::from_gms(gms));
    let identifier = match &job.identity {
        AccountIdentity::PrivatePdaShared { identifier, .. } => *identifier,
        _ => unreachable!(),
    };
    party
        .wallet
        .storage_mut()
        .key_chain_mut()
        .insert_shared_private_account(
            job.id,
            SharedAccountEntry {
                group_label: Label::new(label),
                identifier,
                pda_seed: Some(job.seed),
                authority_program_id: Some(image_id),
                account: lee_core::account::Account::default(),
            },
        );
    party.wallet.store_persistent_data()?;
    Ok(())
}

/// A fresh private payout address under `owner`'s keys, as the payer passes it.
fn fresh_payout(owner: &Party) -> Result<(AccountId, AccountIdentity)> {
    let found = owner
        .wallet
        .storage()
        .key_chain()
        .private_account(owner.id)
        .context("party has no private account")?;
    let npk: NullifierPublicKey = found.key_chain.nullifier_public_key;
    let vpk: ViewingPublicKey = found.key_chain.viewing_public_key.clone();
    let identifier = Identifier::new(random32());
    let id = AccountId::for_regular_private_account(&npk, &vpk, identifier);
    Ok((
        id,
        AccountIdentity::PrivateForeign {
            npk,
            vpk,
            kind: PrivateAccountKind::Regular(identifier),
        },
    ))
}

struct Escrow {
    program: AccountId,
    binary: ProgramWithDependencies,
}

impl Escrow {
    fn mention(&self, identity: AccountIdentity) -> AccountMention {
        AccountMention {
            identity,
            program_account_id: self.program,
        }
    }

    async fn send(
        &self,
        party: &Party,
        accounts: Vec<AccountIdentity>,
        instruction: &Instruction,
    ) -> Result<String> {
        let mentions = accounts.into_iter().map(|a| self.mention(a)).collect();
        let data = borsh::to_vec(instruction)?;
        let (hash, _) = party
            .wallet
            .send_privacy_preserving_tx(mentions, data, &self.binary)
            .await
            .map_err(|error| anyhow::anyhow!("{} send failed: {error:?}", party.name))?;
        let (_, block) = party
            .wallet
            .poll_transaction(hash)
            .await
            .with_context(|| format!("{} transaction {hash} not included", party.name))?;
        Ok(format!("{hash} in block {block}"))
    }
}

/// What a wallet knows about the job right now.
fn view(party: &Party, program: AccountId, job: &SharedJob) -> String {
    let Some(entry) = party
        .wallet
        .storage()
        .key_chain()
        .shared_private_account(job.id)
    else {
        return "not tracked".into();
    };
    let balance = entry.account.data.native_balance().unwrap_or_default();
    let shard = entry.account.data.shard(program);
    let record = if shard.is_empty() {
        "no record".to_string()
    } else {
        match borsh::from_slice::<Job>(shard) {
            Ok(job) => format!("{:?}, result {}", job.state, job.result.is_some()),
            Err(_) => "undecodable record".into(),
        }
    };
    format!("{record}, holds {balance}")
}

fn own_balance(party: &Party, id: AccountId) -> String {
    party
        .wallet
        .storage()
        .key_chain()
        .private_account(id)
        .map(|found| {
            found
                .account
                .data
                .native_balance()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| "unknown".into())
}

async fn sync(parties: &mut [&mut Party]) -> Result<()> {
    for party in parties.iter_mut() {
        party.wallet.sync_to_latest_block().await?;
        party.wallet.store_persistent_data()?;
    }
    Ok(())
}

/// What a public observer can read from a transaction.
async fn observer(
    party: &Party,
    hash_line: &str,
    job: &SharedJob,
    parties: &[AccountId],
) -> Result<String> {
    let hash = hash_line.split_whitespace().next().unwrap_or_default();
    let Some((tx, _)) = party.wallet.get_transaction(hash.parse()?).await? else {
        bail!("transaction {hash} not found");
    };
    let bytes = borsh::to_vec(&tx)?;
    let mentions = |id: &AccountId| bytes.windows(32).any(|w| w == id.value());
    if !matches!(tx, LeeTransaction::PrivacyPreserving(_)) {
        return Ok("a public transaction".into());
    }
    Ok(format!(
        "private transaction, job id visible: {}, a party's id visible: {}",
        mentions(&job.id),
        parties.iter().any(mentions)
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let program: AccountId = arg("PROGRAM")?.parse()?;
    let elf = std::fs::read(arg("ELF")?)?;
    let image_id = Program::new(elf.clone().into())?.id();
    let escrow = Escrow {
        program,
        binary: ProgramWithDependencies::new(Program::new(elf.into())?, program, HashMap::new()),
    };
    let mut buyer = Party {
        name: "buyer",
        wallet: open_wallet(&arg("BUYER_WALLET")?)?,
        id: arg("BUYER")?.parse()?,
    };
    let mut provider = Party {
        name: "provider",
        wallet: open_wallet(&arg("PROVIDER_WALLET")?)?,
        id: arg("PROVIDER")?.parse()?,
    };
    // Shared off-chain between the two parties, for example sealed in the chat.
    let gms = random32();

    println!("--- job 1: open, deliver, claim on a private job account");
    let job_ref = random32();
    let job = shared_job(gms, program, image_id, buyer.id, job_ref);
    register(&mut buyer, gms, image_id, &job, "job-1")?;
    register(&mut provider, gms, image_id, &job, "job-1")?;
    let (pay_to, pay_to_identity) = fresh_payout(&provider)?;
    let (refund_to, _) = fresh_payout(&buyer)?;
    println!("job account {}, pay_to {pay_to}", job.id);

    let deadline = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )? + 3_600_000;
    let open = Instruction::Open {
        job_ref,
        provider: provider.id,
        pay_to,
        refund_to,
        amount: AMOUNT,
        deadline,
    };
    let tx = escrow
        .send(
            &buyer,
            vec![
                AccountIdentity::PrivateOwned(buyer.id),
                job.identity.clone(),
            ],
            &open,
        )
        .await?;
    sync(&mut [&mut buyer, &mut provider]).await?;
    println!("open      {tx}");
    println!("  buyer sees    {}", view(&buyer, program, &job));
    println!("  provider sees {}", view(&provider, program, &job));
    println!(
        "  observer sees {}",
        observer(&buyer, &tx, &job, &[buyer.id, provider.id]).await?
    );

    let deliver = Instruction::Deliver {
        result: random32(),
        deadline,
    };
    let tx = escrow
        .send(
            &provider,
            vec![
                AccountIdentity::PrivateOwned(provider.id),
                job.identity.clone(),
            ],
            &deliver,
        )
        .await?;
    sync(&mut [&mut buyer, &mut provider]).await?;
    println!("deliver   {tx}");
    println!("  buyer sees    {}", view(&buyer, program, &job));
    println!("  provider sees {}", view(&provider, program, &job));

    let claim = Instruction::Claim {
        buyer: buyer.id,
        job_ref,
        amount: AMOUNT,
    };
    let tx = escrow
        .send(
            &provider,
            vec![
                AccountIdentity::PrivateOwned(provider.id),
                job.identity.clone(),
                pay_to_identity,
            ],
            &claim,
        )
        .await?;
    sync(&mut [&mut buyer, &mut provider]).await?;
    println!("claim     {tx}");
    println!("  buyer sees    {}", view(&buyer, program, &job));
    println!("  provider sees {}", view(&provider, program, &job));
    println!("  buyer balance {}", own_balance(&buyer, buyer.id));
    let paid = provider
        .wallet
        .storage()
        .key_chain()
        .private_account(pay_to)
        .map(|f| f.account.data.native_balance().unwrap_or_default());
    println!("  pay_to holds  {paid:?}");

    println!("--- job 2: both parties act on the same job state at once");
    let job_ref = random32();
    let job = shared_job(gms, program, image_id, buyer.id, job_ref);
    register(&mut buyer, gms, image_id, &job, "job-2")?;
    register(&mut provider, gms, image_id, &job, "job-2")?;
    let (pay_to, _) = fresh_payout(&provider)?;
    let (refund_to, _) = fresh_payout(&buyer)?;
    let open = Instruction::Open {
        job_ref,
        provider: provider.id,
        pay_to,
        refund_to,
        amount: AMOUNT,
        deadline,
    };
    let tx = escrow
        .send(
            &buyer,
            vec![
                AccountIdentity::PrivateOwned(buyer.id),
                job.identity.clone(),
            ],
            &open,
        )
        .await?;
    sync(&mut [&mut buyer, &mut provider]).await?;
    println!("open      {tx}");

    let escrow = Arc::new(escrow);
    let buyer = Arc::new(Mutex::new(buyer));
    let provider = Arc::new(Mutex::new(provider));
    let release = {
        let (escrow, buyer, job) = (escrow.clone(), buyer.clone(), job.clone());
        tokio::spawn(async move {
            let buyer = buyer.lock().await;
            escrow
                .send(
                    &buyer,
                    vec![
                        AccountIdentity::PrivateOwned(buyer.id),
                        job.identity.clone(),
                    ],
                    &Instruction::Release { job_ref },
                )
                .await
        })
    };
    let give_back = {
        let (escrow, provider, job) = (escrow.clone(), provider.clone(), job.clone());
        tokio::spawn(async move {
            let provider = provider.lock().await;
            escrow
                .send(
                    &provider,
                    vec![
                        AccountIdentity::PrivateOwned(provider.id),
                        job.identity.clone(),
                    ],
                    &Instruction::Return,
                )
                .await
        })
    };
    let (release, give_back) = (release.await?, give_back.await?);
    println!(
        "release   {}",
        release.map_or_else(|e| format!("FAILED: {e}"), |t| t)
    );
    println!(
        "return    {}",
        give_back.map_or_else(|e| format!("FAILED: {e}"), |t| t)
    );
    let mut buyer = Arc::try_unwrap(buyer)
        .ok()
        .context("buyer still shared")?
        .into_inner();
    let mut provider = Arc::try_unwrap(provider)
        .ok()
        .context("provider still shared")?
        .into_inner();
    sync(&mut [&mut buyer, &mut provider]).await?;
    println!("  buyer sees    {}", view(&buyer, program, &job));
    println!("  provider sees {}", view(&provider, program, &job));
    Ok(())
}
