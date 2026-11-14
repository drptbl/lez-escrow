//! Sends the example arbiter's calls from a LEZ wallet (`LEE_WALLET_HOME_DIR`): open a job
//! through it, and the judge's release and return. The parties claim and refund with the escrow's
//! own CLI. Each call prints its transaction, then the job as the escrow holds it.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lee_core::account::{AccountId, Balance};
use logos_escrow_arbiter::{ArbiterInstruction, Decision, case_account_id};
use logos_escrow_core::{Hash32, Job, job_account_id};
use wallet::{AccountIdentity, AccountMention, WalletCore};

#[derive(Parser)]
#[command(about = "Drive the example arbiter from a LEZ wallet")]
struct Args {
    /// The arbiter's program id; logos-devnet exports it.
    #[arg(long, env = "LOGOS_PROGRAM_ARBITER", global = true)]
    arbiter: Option<AccountId>,
    /// The escrow's program id; logos-devnet exports it.
    #[arg(long, env = "LOGOS_PROGRAM_ESCROW", global = true)]
    escrow: Option<AccountId>,
    #[command(subcommand)]
    command: Command,
}

/// Names one job: the buyer and the job reference (64 hex characters).
#[derive(clap::Args)]
struct JobArgs {
    #[arg(long)]
    buyer: AccountId,
    #[arg(long, value_parser = parse_hash)]
    job_ref: Hash32,
}

#[derive(Subcommand)]
enum Command {
    /// Buyer: lock `amount` for `provider` for `in_secs`, with `judge` deciding.
    Open {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long)]
        judge: AccountId,
        #[arg(long)]
        provider: AccountId,
        #[arg(long)]
        pay_to: AccountId,
        #[arg(long)]
        refund_to: AccountId,
        #[arg(long)]
        amount: Balance,
        #[arg(long)]
        in_secs: u64,
    },
    /// Judge: let the provider claim `amount` more.
    Release {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long)]
        judge: AccountId,
        #[arg(long)]
        amount: Balance,
    },
    /// Judge: give what was never released back to the buyer.
    Return {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long)]
        judge: AccountId,
    },
}

fn parse_hash(text: &str) -> Result<Hash32> {
    let bytes = hex::decode(text).context("not hex")?;
    <Hash32>::try_from(bytes).map_err(|_| anyhow::anyhow!("expected 32 bytes"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let arbiter = args
        .arbiter
        .context("set --arbiter or LOGOS_PROGRAM_ARBITER")?;
    let escrow = args
        .escrow
        .context("set --escrow or LOGOS_PROGRAM_ESCROW")?;
    let wallet = WalletCore::from_env().context("open the wallet at LEE_WALLET_HOME_DIR")?;
    let client = Client {
        wallet,
        arbiter,
        escrow,
    };

    match args.command {
        Command::Open {
            job,
            judge,
            provider,
            pay_to,
            refund_to,
            amount,
            in_secs,
        } => {
            let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
            let open = ArbiterInstruction::Open {
                escrow,
                judge,
                job_ref: job.job_ref,
                provider,
                pay_to,
                refund_to,
                amount,
                deadline: now + in_secs * 1000,
            };
            client.send(&job, job.buyer, &[], &open).await
        }
        Command::Release { job, judge, amount } => {
            let decide = ArbiterInstruction::Decide {
                escrow,
                decision: Decision::Release {
                    job_ref: job.job_ref,
                    amount,
                },
            };
            client.send(&job, judge, &[job.buyer], &decide).await
        }
        Command::Return { job, judge } => {
            let provider = client.job(&job).await?.provider;
            let decide = ArbiterInstruction::Decide {
                escrow,
                decision: Decision::Return,
            };
            client.send(&job, judge, &[provider], &decide).await
        }
    }
}

struct Client {
    wallet: WalletCore,
    arbiter: AccountId,
    escrow: AccountId,
}

impl Client {
    fn job_account(&self, job: &JobArgs) -> AccountId {
        job_account_id(&self.escrow, &job.buyer, &job.job_ref)
    }

    /// Sends one arbiter call signed by `signer`. Its accounts are the signer, the case, `party`
    /// for a decision, then the job.
    async fn send(
        &self,
        job: &JobArgs,
        signer: AccountId,
        party: &[AccountId],
        instruction: &ArbiterInstruction,
    ) -> Result<()> {
        let job_account = self.job_account(job);
        let mention = |identity| AccountMention {
            identity,
            program_account_id: self.arbiter,
        };
        let mut accounts = vec![
            mention(AccountIdentity::Public(signer)),
            mention(AccountIdentity::PublicNoSign(case_account_id(
                &self.arbiter,
                &job_account,
            ))),
        ];
        accounts.extend(
            party
                .iter()
                .map(|id| mention(AccountIdentity::PublicNoSign(*id))),
        );
        accounts.push(mention(AccountIdentity::PublicNoSign(job_account)));
        let hash = self
            .wallet
            .send_pub_tx_paid_by(accounts, borsh::to_vec(instruction)?, self.arbiter, None)
            .await
            .map_err(|error| anyhow::anyhow!("send failed: {error:?}"))?;
        let (_, block) = self
            .wallet
            .poll_transaction(hash)
            .await
            .context("transaction not included")?;
        println!("tx {hash} in block {block}");
        // A refused public call is still included, so the job shows whether it worked.
        let record = self.job(job).await?;
        println!(
            "job {job_account}: {:?}, released {}, claimed {}",
            record.state, record.released, record.claimed
        );
        Ok(())
    }

    async fn job(&self, job: &JobArgs) -> Result<Job> {
        let account = self
            .wallet
            .get_account_public(self.job_account(job))
            .await?;
        let shard = account.data.shard(self.escrow);
        if shard.is_empty() {
            bail!("no job at {}", self.job_account(job));
        }
        Ok(borsh::from_slice(shard)?)
    }
}
