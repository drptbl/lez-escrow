//! Calls the escrow program on a LEZ sequencer from a LEZ wallet (`LEE_WALLET_HOME_DIR`).
//!
//! Each call goes through `WalletCore::send_pub_tx_paid_by`, which is what `lez_core`'s
//! `send_generic_public_transaction` calls, and reads the job record back with `getAccount`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lee_core::account::{AccountId, Balance};
use lez_escrow_core::{Hash32, Instruction, Job, job_account_id};
use wallet::{AccountIdentity, AccountMention, WalletCore};

#[derive(Parser)]
#[command(about = "Drive the lez-escrow program from a LEZ wallet")]
struct Args {
    /// The escrow program's header account id (base58).
    #[arg(long, env = "LEZ_ESCROW_PROGRAM", global = true)]
    program: Option<AccountId>,
    /// A wallet account that pays every transaction's fee; by default the signer pays.
    #[arg(long, global = true)]
    payer: Option<AccountId>,
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
    /// Print the job account for a buyer and job reference.
    JobAccount(JobArgs),
    /// Print a job's record and the balance it holds.
    Show(JobArgs),
    /// Buyer: lock `amount` for `provider` until `deadline_ms` (or `in_secs` from now).
    Open {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long)]
        provider: AccountId,
        #[arg(long)]
        amount: Balance,
        #[arg(long, conflicts_with = "in_secs")]
        deadline_ms: Option<u64>,
        #[arg(long)]
        in_secs: Option<u64>,
    },
    /// Provider: record the result hash before the deadline.
    Deliver {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long, value_parser = parse_hash)]
        result: Hash32,
    },
    /// Provider: collect a delivered job.
    Claim(JobArgs),
    /// Buyer: pay the provider now.
    Release(JobArgs),
    /// Buyer: take back an undelivered job after its deadline.
    Refund(JobArgs),
}

fn parse_hash(text: &str) -> Result<Hash32> {
    let bytes = hex::decode(text).context("not hex")?;
    <Hash32>::try_from(bytes).map_err(|_| anyhow::anyhow!("expected 32 bytes"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let program = args
        .program
        .context("set --program or LEZ_ESCROW_PROGRAM")?;
    let wallet = WalletCore::from_env().context("open the wallet at LEE_WALLET_HOME_DIR")?;
    let escrow = Escrow {
        wallet,
        program,
        payer: args.payer,
    };

    match args.command {
        Command::JobAccount(job) => println!("{}", escrow.job_account(&job)),
        Command::Show(job) => escrow.show(&job).await?,
        Command::Open {
            job,
            provider,
            amount,
            deadline_ms,
            in_secs,
        } => {
            let deadline = match (deadline_ms, in_secs) {
                (Some(deadline), _) => deadline,
                (None, Some(secs)) => secs
                    .checked_mul(1000)
                    .and_then(|ms| ms.checked_add(now_ms().ok()?))
                    .context("deadline overflows")?,
                (None, None) => bail!("set --deadline-ms or --in-secs"),
            };
            let instruction = Instruction::Open {
                job_ref: job.job_ref,
                provider,
                amount,
                deadline,
            };
            let accounts = vec![
                escrow.signs(job.buyer),
                escrow.mention(escrow.job_account(&job)),
            ];
            escrow.send(&job, accounts, &instruction).await?;
        }
        Command::Deliver { job, result } => {
            let record = escrow.record(&job).await?;
            let instruction = Instruction::Deliver {
                result,
                deadline: record.deadline,
            };
            let accounts = vec![
                escrow.signs(record.provider),
                escrow.mention(escrow.job_account(&job)),
            ];
            escrow.send(&job, accounts, &instruction).await?;
        }
        Command::Claim(job) => {
            let record = escrow.record(&job).await?;
            let instruction = Instruction::Claim {
                buyer: job.buyer,
                job_ref: job.job_ref,
                amount: record.amount,
            };
            let accounts = vec![
                escrow.signs(record.provider),
                escrow.mention(escrow.job_account(&job)),
            ];
            escrow.send(&job, accounts, &instruction).await?;
        }
        Command::Release(job) => {
            let record = escrow.record(&job).await?;
            let instruction = Instruction::Release {
                job_ref: job.job_ref,
                amount: record.amount,
            };
            let accounts = vec![
                escrow.signs(job.buyer),
                escrow.mention(escrow.job_account(&job)),
                escrow.mention(record.provider),
            ];
            escrow.send(&job, accounts, &instruction).await?;
        }
        Command::Refund(job) => {
            let record = escrow.record(&job).await?;
            let instruction = Instruction::Refund {
                job_ref: job.job_ref,
                amount: record.amount,
                deadline: record.deadline,
            };
            let accounts = vec![
                escrow.signs(job.buyer),
                escrow.mention(escrow.job_account(&job)),
            ];
            escrow.send(&job, accounts, &instruction).await?;
        }
    }
    Ok(())
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

struct Escrow {
    wallet: WalletCore,
    program: AccountId,
    payer: Option<AccountId>,
}

impl Escrow {
    fn job_account(&self, job: &JobArgs) -> AccountId {
        job_account_id(&self.program, &job.buyer, &job.job_ref)
    }

    /// A signing account. Every account selects the escrow program's shard; the program builds
    /// the native-balance selectors for the transfers it chains.
    fn signs(&self, account_id: AccountId) -> AccountMention {
        AccountMention {
            identity: AccountIdentity::Public(account_id),
            program_account_id: self.program,
        }
    }

    /// An account named without signing, selecting the escrow program's shard.
    fn mention(&self, account_id: AccountId) -> AccountMention {
        AccountMention {
            identity: AccountIdentity::PublicNoSign(account_id),
            program_account_id: self.program,
        }
    }

    async fn record(&self, job: &JobArgs) -> Result<Job> {
        let account = self
            .wallet
            .get_account_public(self.job_account(job))
            .await?;
        let shard = account.data.shard(self.program);
        if shard.is_empty() {
            bail!("no job at {}", self.job_account(job));
        }
        Ok(borsh::from_slice(shard)?)
    }

    async fn show(&self, job: &JobArgs) -> Result<()> {
        let job_account = self.job_account(job);
        let balance = self.wallet.get_account_balance(job_account).await?;
        println!("job account: {job_account}");
        println!("holds:       {balance} lepta");
        match self.record(job).await {
            Ok(record) => println!("record:      {record:?}"),
            Err(error) => println!("record:      none ({error})"),
        }
        Ok(())
    }

    async fn send(
        &self,
        job: &JobArgs,
        accounts: Vec<AccountMention>,
        instruction: &Instruction,
    ) -> Result<()> {
        let data = borsh::to_vec(instruction)?;
        println!("instruction: {}", hex::encode(&data));
        let hash = self
            .wallet
            .send_pub_tx_paid_by(accounts, data, self.program, self.payer)
            .await
            .map_err(|error| anyhow::anyhow!("send failed: {error:?}"))?;
        println!("tx:          {hash}");
        let (_, block) = self
            .wallet
            .poll_transaction(hash)
            .await
            .context("transaction not included")?;
        println!("block:       {block}");
        // Inclusion does not mean success: a refused call is included and charged. Read back.
        self.show(job).await
    }
}
