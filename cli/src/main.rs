//! Calls the escrow program on a LEZ sequencer from a LEZ wallet (`LEE_WALLET_HOME_DIR`).
//!
//! A call goes public through `WalletCore::send_pub_tx_paid_by`, which `lez_core`'s
//! `send_generic_public_transaction` calls, or private through
//! `WalletCore::send_privacy_preserving_tx` when the signer is one of the wallet's private
//! accounts or the payee is a private payout address. The job record is read back with
//! `getAccount`.

use std::{
    collections::HashMap,
    io::Read as _,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lee::{privacy_preserving_transaction::circuit::ProgramWithDependencies, program::Program};
use lee_core::{
    Identifier, NullifierPublicKey,
    account::{AccountId, Balance},
    encryption::ViewingPublicKey,
    program::PrivateAccountKind,
};
use logos_escrow_core::{Hash32, Instruction, Job, job_account_id};
use wallet::{AccountIdentity, AccountMention, WalletCore};

#[derive(Parser)]
#[command(about = "Drive the logos-escrow program from a LEZ wallet")]
struct Args {
    /// The escrow program's header account id (base58).
    #[arg(long, env = "LOGOS_ESCROW_PROGRAM", global = true)]
    program: Option<AccountId>,
    /// A wallet account that pays a public transaction's fee; by default the signer pays.
    /// Private transactions are fee-exempt.
    #[arg(long, global = true)]
    payer: Option<AccountId>,
    /// The escrow guest binary, which a private call proves locally.
    #[arg(
        long,
        env = "LOGOS_ESCROW_ELF",
        global = true,
        default_value = "methods/guest/target/riscv32im-risc0-zkvm-elf/docker/logos_escrow.bin"
    )]
    elf: PathBuf,
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

/// A payout from a job, to the payout address the job's record names.
#[derive(clap::Args)]
struct PayoutArgs {
    #[command(flatten)]
    job: JobArgs,
    /// The payout address file, when that address is a private account (see `payout-address`).
    #[arg(long)]
    payee_address: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the job account for a buyer and job reference.
    JobAccount(JobArgs),
    /// Print a job's record and the balance it holds.
    Show(JobArgs),
    /// Make a fresh private payout address under one of the wallet's private accounts' keys.
    /// Prints the account id to use in `open`, and writes the file a payer needs to `--out`.
    PayoutAddress {
        /// The wallet's private account whose keys own the address.
        #[arg(long)]
        owner: AccountId,
        #[arg(long)]
        out: PathBuf,
    },
    /// Buyer: lock `amount` for `provider` until `deadline_ms` (or `in_secs` from now).
    Open {
        #[command(flatten)]
        job: JobArgs,
        #[arg(long)]
        provider: AccountId,
        /// Where the provider is paid.
        #[arg(long)]
        pay_to: AccountId,
        /// Where the buyer gets the deposit back.
        #[arg(long)]
        refund_to: AccountId,
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
    /// Provider: collect a delivered job to its `pay_to`.
    Claim(PayoutArgs),
    /// Buyer: pay the job's `pay_to` now.
    Release(PayoutArgs),
    /// Buyer: take back an undelivered job to its `refund_to`, after its deadline.
    Refund(PayoutArgs),
    /// Provider: give an open job's deposit back to its `refund_to`.
    Return(PayoutArgs),
}

fn parse_hash(text: &str) -> Result<Hash32> {
    let bytes = hex::decode(text).context("not hex")?;
    <Hash32>::try_from(bytes).map_err(|_| anyhow::anyhow!("expected 32 bytes"))
}

/// A private account that only its owner's keys can spend, given out so another party can pay
/// into it once.
struct PayoutAddress {
    npk: NullifierPublicKey,
    vpk: ViewingPublicKey,
    identifier: Identifier,
}

impl PayoutAddress {
    fn account_id(&self) -> AccountId {
        AccountId::for_regular_private_account(&self.npk, &self.vpk, self.identifier)
    }

    /// Three hex lines: the nullifier public key, the viewing public key, the identifier.
    fn to_file(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            hex::encode(self.npk.0),
            hex::encode(self.vpk.to_bytes()),
            hex::encode(self.identifier.value())
        )
    }

    fn from_file(path: &PathBuf) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read payout address {}", path.display()))?;
        let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
        let mut next = |what: &str| {
            lines
                .next()
                .with_context(|| format!("payout address file is missing the {what}"))
        };
        let npk = NullifierPublicKey(
            hex::decode(next("nullifier public key")?)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("nullifier public key must be 32 bytes"))?,
        );
        let vpk = ViewingPublicKey::from_bytes(hex::decode(next("viewing public key")?)?)
            .map_err(|error| anyhow::anyhow!("viewing public key: {error}"))?;
        let identifier = Identifier::new(
            hex::decode(next("identifier")?)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("identifier must be 32 bytes"))?,
        );
        Ok(Self {
            npk,
            vpk,
            identifier,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let program = args
        .program
        .context("set --program or LOGOS_ESCROW_PROGRAM")?;
    let wallet = WalletCore::from_env().context("open the wallet at LEE_WALLET_HOME_DIR")?;
    let mut escrow = Escrow {
        wallet,
        program,
        payer: args.payer,
        elf: args.elf,
    };

    match args.command {
        Command::JobAccount(job) => println!("{}", escrow.job_account(&job)),
        Command::Show(job) => escrow.show(&job).await?,
        Command::PayoutAddress { owner, out } => {
            let found = escrow
                .wallet
                .storage()
                .key_chain()
                .private_account(owner)
                .with_context(|| format!("{owner} is not one of this wallet's private accounts"))?;
            let mut identifier = [0_u8; 32];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut identifier)?;
            let address = PayoutAddress {
                npk: found.key_chain.nullifier_public_key,
                vpk: found.key_chain.viewing_public_key.clone(),
                identifier: Identifier::new(identifier),
            };
            std::fs::write(&out, address.to_file())?;
            println!("{}", address.account_id());
        }
        Command::Open {
            job,
            provider,
            pay_to,
            refund_to,
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
                pay_to,
                refund_to,
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
        Command::Claim(payout) => {
            let record = escrow.record(&payout.job).await?;
            let instruction = Instruction::Claim {
                buyer: payout.job.buyer,
                job_ref: payout.job.job_ref,
                amount: record.amount,
            };
            let accounts = vec![
                escrow.signs(record.provider),
                escrow.mention(escrow.job_account(&payout.job)),
                escrow.payee(record.pay_to, payout.payee_address.as_ref())?,
            ];
            escrow.send(&payout.job, accounts, &instruction).await?;
        }
        Command::Release(payout) => {
            let record = escrow.record(&payout.job).await?;
            let instruction = Instruction::Release {
                job_ref: payout.job.job_ref,
                amount: record.amount,
            };
            let accounts = vec![
                escrow.signs(payout.job.buyer),
                escrow.mention(escrow.job_account(&payout.job)),
                escrow.payee(record.pay_to, payout.payee_address.as_ref())?,
            ];
            escrow.send(&payout.job, accounts, &instruction).await?;
        }
        Command::Refund(payout) => {
            let record = escrow.record(&payout.job).await?;
            let instruction = Instruction::Refund {
                job_ref: payout.job.job_ref,
                amount: record.amount,
                deadline: record.deadline,
            };
            let accounts = vec![
                escrow.signs(payout.job.buyer),
                escrow.mention(escrow.job_account(&payout.job)),
                escrow.payee(record.refund_to, payout.payee_address.as_ref())?,
            ];
            escrow.send(&payout.job, accounts, &instruction).await?;
        }
        Command::Return(payout) => {
            let record = escrow.record(&payout.job).await?;
            let instruction = Instruction::Return {
                buyer: payout.job.buyer,
                job_ref: payout.job.job_ref,
                amount: record.amount,
            };
            let accounts = vec![
                escrow.signs(record.provider),
                escrow.mention(escrow.job_account(&payout.job)),
                escrow.payee(record.refund_to, payout.payee_address.as_ref())?,
            ];
            escrow.send(&payout.job, accounts, &instruction).await?;
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
    elf: PathBuf,
}

impl Escrow {
    fn job_account(&self, job: &JobArgs) -> AccountId {
        job_account_id(&self.program, &job.buyer, &job.job_ref)
    }

    /// The signer: one of the wallet's private accounts if it holds one with this id, otherwise a
    /// public account. Every account selects the escrow program's shard; the program builds the
    /// native-balance selectors for the transfers it chains.
    fn signs(&self, account_id: AccountId) -> AccountMention {
        let private = self
            .wallet
            .storage()
            .key_chain()
            .private_account(account_id)
            .is_some();
        let identity = if private {
            AccountIdentity::PrivateOwned(account_id)
        } else {
            AccountIdentity::Public(account_id)
        };
        AccountMention {
            identity,
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

    /// The payout address the record names: a fresh private account from its address file, or a
    /// public account.
    fn payee(&self, expected: AccountId, address: Option<&PathBuf>) -> Result<AccountMention> {
        let Some(path) = address else {
            return Ok(self.mention(expected));
        };
        let address = PayoutAddress::from_file(path)?;
        if address.account_id() != expected {
            bail!(
                "{} is the address {}, but the job pays {expected}",
                path.display(),
                address.account_id()
            );
        }
        Ok(AccountMention {
            identity: AccountIdentity::PrivateForeign {
                npk: address.npk,
                vpk: address.vpk,
                kind: PrivateAccountKind::Regular(address.identifier),
            },
            program_account_id: self.program,
        })
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
        &mut self,
        job: &JobArgs,
        accounts: Vec<AccountMention>,
        instruction: &Instruction,
    ) -> Result<()> {
        let data = borsh::to_vec(instruction)?;
        println!("instruction: {}", hex::encode(&data));
        let private = accounts.iter().any(|mention| mention.identity.is_private());
        let hash = if private {
            // The wallet proves against the private accounts' latest notes.
            self.wallet.sync_to_latest_block().await?;
            let elf = std::fs::read(&self.elf)
                .with_context(|| format!("read the escrow binary at {}", self.elf.display()))?;
            let program = ProgramWithDependencies::new(
                Program::new(elf.into())?,
                self.program,
                HashMap::new(),
            );
            println!("proving locally...");
            let (hash, _) = self
                .wallet
                .send_privacy_preserving_tx(accounts, data, &program)
                .await
                .map_err(|error| anyhow::anyhow!("send failed: {error:?}"))?;
            hash
        } else {
            self.wallet
                .send_pub_tx_paid_by(accounts, data, self.program, self.payer)
                .await
                .map_err(|error| anyhow::anyhow!("send failed: {error:?}"))?
        };
        println!("tx:          {hash}");
        let (_, block) = self
            .wallet
            .poll_transaction(hash)
            .await
            .context("transaction not included")?;
        println!("block:       {block}");
        if private {
            self.wallet.sync_to_latest_block().await?;
            self.wallet.store_persistent_data()?;
        }
        // A public call that was refused is still included and charged, so read the record back.
        self.show(job).await
    }
}
