//! Every escrow flow against a running devnet, through the real CLI and the LEZ wallet it uses,
//! with public and private parties.
//!
//! ```bash
//! make build && logos-devnet up
//! logos-devnet exec -- cargo test -p logos_escrow_cli --test devnet -- --ignored
//! ```

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lee_core::{
    Identifier, NullifierPublicKey,
    account::{AccountId, Balance},
    encryption::ViewingPublicKey,
};
use wallet::{
    AccountIdentity, WalletCore, program_facades::native_token_transfer::NativeTokenTransfer,
};

const AMOUNT: Balance = 1_000_000;

struct Devnet {
    program: String,
    home: PathBuf,
    payer: AccountId,
    buyer: AccountId,
    provider: AccountId,
    scratch: PathBuf,
}

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is unset: run `logos-devnet up` and source its env"))
}

impl Devnet {
    /// A copy of the devnet's funded `parties` wallet, so a run leaves the original untouched.
    fn new() -> Self {
        let scratch =
            std::env::temp_dir().join(format!("logos-escrow-devnet-{}", std::process::id()));
        let home = scratch.join("wallet");
        std::fs::create_dir_all(&home).expect("make the wallet copy");
        let group = PathBuf::from(env("LOGOS_DEVNET_ACCOUNTS_PARTIES_WALLET"));
        for file in ["wallet_config.json", "storage.json"] {
            std::fs::copy(group.join(file), home.join(file)).expect("copy the group wallet");
        }
        let accounts: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(env("LOGOS_DEVNET_ACCOUNTS_PARTIES"))
                .expect("read the account group"),
        )
        .expect("the account group is JSON");
        let ids: Vec<AccountId> = accounts["accounts"]
            .as_array()
            .expect("an accounts list")
            .iter()
            .map(|account| {
                account["id"]
                    .as_str()
                    .expect("an id")
                    .parse()
                    .expect("a base58 id")
            })
            .collect();
        let [payer, buyer, provider] = ids[..] else {
            panic!("the parties group has {} accounts, not 3", ids.len());
        };
        Self {
            program: env("LOGOS_PROGRAM_ESCROW"),
            home,
            payer,
            buyer,
            provider,
            scratch,
        }
    }

    fn wallet(&self) -> WalletCore {
        WalletCore::new_update_chain(
            self.home.join("wallet_config.json"),
            self.home.join("storage.json"),
            None,
        )
        .expect("open the wallet")
    }

    /// Runs the CLI and returns its output, failing the test if it fails.
    fn cli(&self, args: &[&str]) -> String {
        let elf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../methods/guest/target/riscv32im-risc0-zkvm-elf/docker/logos_escrow.bin");
        let output = Command::new(env!("CARGO_BIN_EXE_logos-escrow"))
            .args(args)
            .env("LEE_WALLET_HOME_DIR", &self.home)
            .env("LOGOS_ESCROW_PROGRAM", &self.program)
            .env("LOGOS_ESCROW_ELF", elf)
            .output()
            .expect("run the CLI");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "logos-escrow {args:?} failed:\n{text}"
        );
        text
    }

    /// Runs one escrow call and returns the job's state afterwards.
    fn call(&self, args: &[&str]) -> String {
        let text = self.cli(args);
        text.lines()
            .rev()
            .find_map(|line| line.split("state: ").nth(1))
            .map(|rest| {
                rest.split(|c: char| !c.is_alphanumeric())
                    .next()
                    .unwrap_or("")
                    .to_owned()
            })
            .unwrap_or_else(|| panic!("no job state in:\n{text}"))
    }

    fn payout_address(&self, owner: AccountId, name: &str) -> (String, PathBuf) {
        let path = self.scratch.join(format!("{name}.addr"));
        let owner = owner.to_string();
        let id = self.cli(&[
            "payout-address",
            "--owner",
            &owner,
            "--out",
            path.to_str().expect("utf-8"),
        ]);
        (id.trim().to_owned(), path)
    }

    async fn public_balance(&self, id: AccountId) -> Balance {
        self.wallet()
            .get_account_balance(id)
            .await
            .expect("read a balance")
    }

    /// A private account's balance, as the wallet holding its keys sees it after a sync.
    async fn private_balance(&self, id: &str) -> Balance {
        let mut wallet = self.wallet();
        wallet.sync_to_latest_block().await.expect("sync");
        wallet.store_persistent_data().expect("save the wallet");
        let id: AccountId = id.parse().expect("an account id");
        wallet
            .storage()
            .key_chain()
            .private_account(id)
            .map_or(0, |found| {
                found.account.data.native_balance().expect("a balance")
            })
    }
}

fn job_ref() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    format!("{nanos:064x}")
}

#[tokio::test]
#[ignore = "needs a running devnet: logos-devnet up, then source its env"]
async fn every_flow_settles_on_a_devnet() {
    let devnet = Devnet::new();
    let payer = devnet.payer.to_string();
    let (buyer, provider) = (devnet.buyer.to_string(), devnet.provider.to_string());
    let amount = AMOUNT.to_string();

    // Public parties, paid to themselves: open, deliver, claim.
    let job = job_ref();
    let before = devnet.public_balance(devnet.provider).await;
    let open = [
        "--payer",
        &payer,
        "open",
        "--buyer",
        &buyer,
        "--job-ref",
        &job,
        "--provider",
        &provider,
        "--pay-to",
        &provider,
        "--refund-to",
        &buyer,
        "--amount",
        &amount,
        "--in-secs",
        "600",
    ];
    assert_eq!(devnet.call(&open), "Open");
    // A claim before delivery is refused and moves nothing.
    assert_eq!(
        devnet.call(&[
            "--payer",
            &payer,
            "claim",
            "--buyer",
            &buyer,
            "--job-ref",
            &job
        ]),
        "Open"
    );
    assert_eq!(
        devnet.call(&[
            "--payer",
            &payer,
            "deliver",
            "--buyer",
            &buyer,
            "--job-ref",
            &job,
            "--result",
            &job
        ]),
        "Delivered"
    );
    assert_eq!(
        devnet.call(&[
            "--payer",
            &payer,
            "claim",
            "--buyer",
            &buyer,
            "--job-ref",
            &job
        ]),
        "Claimed"
    );
    assert_eq!(
        devnet.public_balance(devnet.provider).await,
        before + AMOUNT
    );

    // Public parties: open, the deadline passes, refund.
    let job = job_ref();
    let before = devnet.public_balance(devnet.buyer).await;
    let open = [
        "--payer",
        &payer,
        "open",
        "--buyer",
        &buyer,
        "--job-ref",
        &job,
        "--provider",
        &provider,
        "--pay-to",
        &provider,
        "--refund-to",
        &buyer,
        "--amount",
        &amount,
        "--in-secs",
        "4",
    ];
    assert_eq!(devnet.call(&open), "Open");
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(
        devnet.call(&[
            "--payer",
            &payer,
            "refund",
            "--buyer",
            &buyer,
            "--job-ref",
            &job
        ]),
        "Refunded"
    );
    assert_eq!(devnet.public_balance(devnet.buyer).await, before);

    // Private parties, funded by a shielded transfer.
    let mut wallet = devnet.wallet();
    let (private_buyer, _) = wallet.create_new_account_private(None);
    let (private_provider, _) = wallet.create_new_account_private(None);
    wallet.store_persistent_data().expect("save the wallet");
    let (hash, _) = NativeTokenTransfer(&wallet)
        .send_shielded_transfer(
            AccountIdentity::Public(devnet.payer),
            private_buyer,
            10 * AMOUNT,
        )
        .await
        .expect("shield funds to the private buyer");
    wallet
        .poll_transaction(hash)
        .await
        .expect("the shielded transfer lands");
    drop(wallet);
    let (pbuyer, pprovider) = (private_buyer.to_string(), private_provider.to_string());

    for flow in ["claim", "return", "release", "pre-funded"] {
        let job = job_ref();
        let (pay_to, pay_file) = devnet.payout_address(private_provider, &format!("{flow}-pay"));
        let (refund_to, refund_file) =
            devnet.payout_address(private_buyer, &format!("{flow}-refund"));
        let open = [
            "open",
            "--buyer",
            &pbuyer,
            "--job-ref",
            &job,
            "--provider",
            &pprovider,
            "--pay-to",
            &pay_to,
            "--refund-to",
            &refund_to,
            "--amount",
            &amount,
            "--in-secs",
            "600",
        ];
        assert_eq!(devnet.call(&open), "Open", "{flow}");
        let job_args = ["--buyer", pbuyer.as_str(), "--job-ref", job.as_str()];
        let with = |command: &str, extra: &[&str]| -> Vec<String> {
            std::iter::once(command)
                .chain(job_args)
                .chain(extra.iter().copied())
                .map(str::to_owned)
                .collect()
        };
        let call =
            |args: Vec<String>| devnet.call(&args.iter().map(String::as_str).collect::<Vec<_>>());
        let pay = ["--payee-address", pay_file.to_str().expect("utf-8")];
        let refund = ["--payee-address", refund_file.to_str().expect("utf-8")];
        match flow {
            "claim" => {
                assert_eq!(call(with("deliver", &["--result", &job])), "Delivered");
                assert_eq!(call(with("claim", &pay)), "Claimed");
                assert_eq!(devnet.private_balance(&pay_to).await, AMOUNT);
            }
            "return" => {
                assert_eq!(call(with("return", &[])), "Returned");
                assert_eq!(call(with("refund", &refund)), "Refunded");
                assert_eq!(devnet.private_balance(&refund_to).await, AMOUNT);
            }
            "release" => {
                assert_eq!(call(with("release", &[])), "Released");
                assert_eq!(call(with("claim", &pay)), "Claimed");
                assert_eq!(devnet.private_balance(&pay_to).await, AMOUNT);
            }
            _ => {
                // Someone pays into the fresh pay_to first, so it is no longer fresh.
                let text = std::fs::read_to_string(&pay_file).expect("read the address");
                let lines: Vec<&str> = text.lines().collect();
                let wallet = devnet.wallet();
                let (hash, _) = NativeTokenTransfer(&wallet)
                    .send_shielded_transfer_to_outer_account(
                        AccountIdentity::Public(devnet.payer),
                        NullifierPublicKey(
                            hex::decode(lines[0])
                                .expect("hex")
                                .try_into()
                                .expect("32 bytes"),
                        ),
                        ViewingPublicKey::from_bytes(hex::decode(lines[1]).expect("hex"))
                            .expect("a key"),
                        Identifier::new(
                            hex::decode(lines[2])
                                .expect("hex")
                                .try_into()
                                .expect("32 bytes"),
                        ),
                        1,
                    )
                    .await
                    .expect("pay into the pay_to first");
                wallet
                    .poll_transaction(hash)
                    .await
                    .expect("the transfer lands");
                drop(wallet);
                assert_eq!(call(with("deliver", &["--result", &job])), "Delivered");
                assert_eq!(call(with("claim", &pay)), "Claimed");
                assert_eq!(devnet.private_balance(&pay_to).await, AMOUNT + 1);
            }
        }
    }
    std::fs::remove_dir_all(&devnet.scratch).ok();
}
