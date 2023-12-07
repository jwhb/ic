use candid::{Nat, Principal};
use ic_agent::identity::BasicIdentity;
use ic_agent::Identity;
use ic_canister_client_sender::Ed25519KeyPair;
use ic_icrc1::{Block, Operation, Transaction};
use ic_ledger_core::block::BlockType;
use ic_ledger_core::tokens::TokensType;
use ic_ledger_core::Tokens;
use icrc_ledger_types::icrc::generic_metadata_value::MetadataValue;
use icrc_ledger_types::icrc1::account::{Account, Subaccount};
use icrc_ledger_types::icrc1::transfer::{Memo, TransferArg};
use icrc_ledger_types::icrc2::approve::ApproveArgs;
use num_traits::cast::ToPrimitive;
use proptest::prelude::*;
use proptest::sample::select;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde_bytes::ByteBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const E8: u64 = 100_000_000;
pub const DEFAULT_TRANSFER_FEE: u64 = 10_000;
pub const IDENTITY_PEM:&str = "-----BEGIN PRIVATE KEY-----\nMFMCAQEwBQYDK2VwBCIEILhMGpmYuJ0JEhDwocj6pxxOmIpGAXZd40AjkNhuae6q\noSMDIQBeXC6ae2dkJ8QC50bBjlyLqsFQFsMsIThWB21H6t6JRA==\n-----END PRIVATE KEY-----";

pub fn minter_identity() -> BasicIdentity {
    let rng = ring::rand::SystemRandom::new();
    let key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .expect("Could not generate a key pair.");

    BasicIdentity::from_key_pair(
        ring::signature::Ed25519KeyPair::from_pkcs8(key_pair.as_ref())
            .expect("Could not read the key pair."),
    )
}

pub fn principal_strategy() -> impl Strategy<Value = Principal> {
    let bytes_strategy = prop::collection::vec(0..=255u8, 29);
    bytes_strategy.prop_map(|bytes| Principal::from_slice(bytes.as_slice()))
}

pub fn account_strategy() -> impl Strategy<Value = Account> {
    let bytes_strategy = prop::option::of(prop::collection::vec(0..=255u8, 32));
    let principal_strategy = principal_strategy();
    (bytes_strategy, principal_strategy).prop_map(|(bytes, principal)| Account {
        owner: principal,
        subaccount: bytes.map(|x| x.as_slice().try_into().unwrap()),
    })
}

fn token_amount<Tokens: TokensType>(n: u64) -> Tokens {
    Tokens::try_from(candid::Nat::from(n))
        .unwrap_or_else(|e| panic!("failed to convert {n} to tokens: {e}"))
}

pub fn arb_small_amount<Tokens: TokensType>() -> impl Strategy<Value = Tokens> {
    any::<u16>().prop_map(|v| token_amount(v as u64))
}

pub fn arb_amount<Tokens: TokensType>() -> impl Strategy<Value = Tokens> {
    any::<u64>().prop_map(|v| token_amount(v))
}

fn arb_memo() -> impl Strategy<Value = Option<Memo>> {
    prop::option::of(prop::collection::vec(0..=255u8, 32).prop_map(|x| Memo(ByteBuf::from(x))))
}

fn operation_strategy<Tokens: TokensType>(
    amount_strategy: impl Strategy<Value = Tokens>,
) -> impl Strategy<Value = Operation<Tokens>> {
    amount_strategy.prop_flat_map(|amount| {
        prop_oneof![
            account_strategy().prop_map(move |to| Operation::Mint { to, amount }),
            account_strategy().prop_map(move |from| {
                Operation::Burn {
                    from,
                    spender: None,
                    amount,
                }
            }),
            (
                account_strategy(),
                account_strategy(),
                prop::option::of(Just(token_amount(DEFAULT_TRANSFER_FEE)))
            )
                .prop_map(move |(to, from, fee)| Operation::Transfer {
                    from,
                    to,
                    spender: None,
                    amount,
                    fee
                }),
            (
                account_strategy(),
                account_strategy(),
                prop::option::of(Just(token_amount(DEFAULT_TRANSFER_FEE))),
                prop::option::of(Just({
                    (SystemTime::now()
                        + Duration::from_secs(rand::thread_rng().gen_range(0..=u32::MAX as u64)))
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64
                }))
            )
                .prop_map(move |(spender, from, fee, expires_at)| Operation::Approve {
                    from,
                    spender,
                    amount,
                    expected_allowance: Some(amount),
                    expires_at,
                    fee
                }),
        ]
    })
}

fn valid_created_at_time_strategy(now: SystemTime) -> impl Strategy<Value = Option<u64>> {
    let day_in_sec = 24 * 60 * 60 - 60 * 5;
    prop::option::of((0..=day_in_sec).prop_map(move |duration| {
        let start = now - Duration::from_secs(day_in_sec);
        // Ledger takes transactions that were created in the last 24 hours (5 minute window to submit valid transactions)
        let random_time = start + Duration::from_secs(duration); // calculate the random time
        random_time.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }))
}

fn valid_expires_at_strategy(now: SystemTime) -> impl Strategy<Value = Option<u64>> {
    prop::option::of((0..=u32::MAX as u64).prop_map(move |duration| {
        let random_time = now + Duration::from_secs(duration);
        random_time.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }))
}

pub fn transaction_strategy<Tokens: TokensType>(
    amount_strategy: impl Strategy<Value = Tokens>,
) -> impl Strategy<Value = Transaction<Tokens>> {
    let operation_strategy = operation_strategy(amount_strategy);
    let created_at_time_strategy = prop::option::of(Just({
        let end = SystemTime::now();
        // Ledger takes transactions that were created in the last 24 hours (5 minute window to submit valid transactions)
        let day_in_sec = 24 * 60 * 60 - 60 * 5;
        let start = end - Duration::from_secs(day_in_sec);
        let mut rng = rand::thread_rng(); // initialize random number generator
        let random_duration = Duration::from_secs(rng.gen_range(0..=day_in_sec));
        let random_time = start + random_duration; // calculate the random time
        random_time.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }));
    (operation_strategy, arb_memo(), created_at_time_strategy).prop_map(
        |(operation, memo, created_at_time)| Transaction {
            operation,
            created_at_time,
            memo,
        },
    )
}

pub fn blocks_strategy<Tokens: TokensType>(
    amount_strategy: impl Strategy<Value = Tokens>,
) -> impl Strategy<Value = Block<Tokens>> {
    let transaction_strategy = transaction_strategy(amount_strategy);
    let fee_collector_strategy = prop::option::of(account_strategy());
    let fee_collector_block_index_strategy = prop::option::of(prop::num::u64::ANY);
    let fee_strategy = prop::option::of(arb_small_amount());
    let timestamp_strategy = Just({
        let end = SystemTime::now();
        // Ledger takes transactions that were created in the last 24 hours (5 minute window to submit valid transactions)
        let day_in_sec = 24 * 60 * 60 - 60 * 5;
        let start = end - Duration::from_secs(day_in_sec);
        let mut rng = rand::thread_rng(); // initialize random number generator
        let random_duration = Duration::from_secs(rng.gen_range(0..=day_in_sec));
        let random_time = start + random_duration; // calculate the random time
        random_time.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    });
    (
        transaction_strategy,
        fee_strategy,
        timestamp_strategy,
        fee_collector_strategy,
        fee_collector_block_index_strategy,
    )
        .prop_map(
            |(transaction, fee, timestamp, fee_collector, fee_collector_block_index)| {
                let transaction_fee = match transaction.operation {
                    Operation::Transfer { fee, .. } => fee,
                    Operation::Approve { fee, .. } => fee,
                    Operation::Burn { .. } => None,
                    Operation::Mint { .. } => None,
                };
                let effective_fee = transaction_fee
                    .is_none()
                    .then(|| fee.unwrap_or(token_amount(DEFAULT_TRANSFER_FEE)));
                assert!(effective_fee.is_some() || transaction_fee.is_some());
                Block {
                    parent_hash: Some(Block::<Tokens>::block_hash(
                        &Block {
                            parent_hash: None,
                            transaction: transaction.clone(),
                            effective_fee,
                            timestamp,
                            fee_collector,
                            fee_collector_block_index,
                        }
                        .encode(),
                    )),
                    transaction,
                    effective_fee,
                    timestamp,
                    fee_collector,
                    fee_collector_block_index,
                }
            },
        )
}

// Construct a valid blockchain strategy
pub fn valid_blockchain_strategy<Tokens: TokensType>(
    size: usize,
) -> impl Strategy<Value = Vec<Block<Tokens>>> {
    let blocks = prop::collection::vec(blocks_strategy(arb_amount()), 0..size);
    blocks.prop_map(|mut blocks| {
        let mut parent_hash = None;
        for block in blocks.iter_mut() {
            block.parent_hash = parent_hash;
            parent_hash = Some(Block::<Tokens>::block_hash(&(block.clone().encode())));
        }
        blocks
    })
}

pub fn valid_blockchain_with_gaps_strategy<Tokens: TokensType>(
    size: usize,
) -> impl Strategy<Value = (Vec<Block<Tokens>>, Vec<usize>)> {
    let blockchain_strategy = valid_blockchain_strategy(size);
    let gaps = prop::collection::vec(0..5usize, size);
    (blockchain_strategy, gaps).prop_map(|(blockchain, gaps)| {
        let block_indices = gaps
            .into_iter()
            .enumerate()
            .scan(0, |acc, (index, gap)| {
                *acc += gap;
                Some(index + *acc)
            })
            .collect();
        (blockchain, block_indices)
    })
}

pub fn transfer_arg(sender: Account) -> impl Strategy<Value = TransferArg> {
    (any::<u16>(), arb_memo(), account_strategy()).prop_map(move |(amount, memo, to)| TransferArg {
        from_subaccount: sender.subaccount,
        to,
        amount: candid::Nat::from(amount),
        created_at_time: None,
        fee: None,
        memo,
    })
}

pub fn transfer_args_with_sender(
    num: usize,
    sender: Account,
) -> impl Strategy<Value = Vec<TransferArg>> {
    prop::collection::vec(transfer_arg(sender), 0..num)
}

#[derive(Clone, Debug)]
pub enum LedgerEndpointArg {
    ApproveArg(ApproveArgs),
    TransferArg(TransferArg),
}

impl LedgerEndpointArg {
    fn subaccount_from(&self) -> Option<Subaccount> {
        match self {
            Self::ApproveArg(arg) => arg.from_subaccount,
            Self::TransferArg(arg) => arg.from_subaccount,
        }
    }
}

#[derive(Clone)]
pub struct ArgWithCaller {
    pub caller: Arc<BasicIdentity>,
    pub principal_to_basic_identity: HashMap<Principal, Arc<BasicIdentity>>,
    pub arg: LedgerEndpointArg,
}

impl fmt::Debug for ArgWithCaller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArgWithCaller")
            .field(
                "account_to_basic_identity",
                &self.principal_to_basic_identity,
            )
            .field("arg", &self.principal_to_basic_identity)
            .field("caller", &self.caller.sender().unwrap())
            .finish_non_exhaustive()
    }
}

impl ArgWithCaller {
    pub fn from(&self) -> Account {
        Account {
            owner: self.caller.sender().unwrap(),
            subaccount: self.arg.subaccount_from(),
        }
    }

    pub fn accounts(&self) -> Vec<Account> {
        let mut res = vec![self.from()];
        if let LedgerEndpointArg::TransferArg(arg) = &self.arg {
            res.push(arg.to)
        }
        res
    }

    pub fn fee(&self) -> Option<u64> {
        let fee = match &self.arg {
            LedgerEndpointArg::ApproveArg(arg) => arg.fee.as_ref(),
            LedgerEndpointArg::TransferArg(arg) => arg.fee.as_ref(),
        };
        fee.as_ref().map(|fee| fee.0.to_u64().unwrap())
    }
    pub fn to_transaction(&self, minter: Account) -> Transaction<Tokens> {
        let from = self.from();
        let (operation, created_at_time, memo) = match self.arg.clone() {
            LedgerEndpointArg::ApproveArg(approve_arg) => {
                let operation = Operation::<Tokens>::Approve {
                    amount: Tokens::try_from(approve_arg.amount.clone()).unwrap(),
                    expires_at: approve_arg.expires_at,
                    fee: approve_arg
                        .fee
                        .clone()
                        .map(|f| Tokens::try_from(f.clone()).unwrap()),
                    expected_allowance: approve_arg
                        .expected_allowance
                        .clone()
                        .map(|a| Tokens::try_from(a.clone()).unwrap()),
                    spender: approve_arg.spender,
                    from,
                };
                (operation, approve_arg.created_at_time, approve_arg.memo)
            }
            LedgerEndpointArg::TransferArg(transfer_arg) => {
                let burn_operation = transfer_arg.to == minter;
                let mint_operation = from == minter;
                let operation = if mint_operation {
                    Operation::Mint {
                        amount: Tokens::try_from(transfer_arg.amount.clone()).unwrap(),
                        to: transfer_arg.to,
                    }
                } else if burn_operation {
                    Operation::Burn {
                        amount: Tokens::try_from(transfer_arg.amount.clone()).unwrap(),
                        from,
                        spender: None,
                    }
                } else {
                    Operation::Transfer {
                        amount: Tokens::try_from(transfer_arg.amount.clone()).unwrap(),
                        to: transfer_arg.to,
                        from,
                        spender: None,
                        fee: transfer_arg
                            .fee
                            .clone()
                            .map(|f| Tokens::try_from(f).unwrap()),
                    }
                };

                (operation, transfer_arg.created_at_time, transfer_arg.memo)
            }
        };
        Transaction::<Tokens> {
            operation,
            created_at_time,
            memo,
        }
    }
}

#[derive(Clone, Default, Debug)]
struct TransactionsAndBalances {
    transactions: Vec<ArgWithCaller>,
    balances: HashMap<Account, u64>,
    txs: HashSet<Transaction<Tokens>>,
    principal_to_basic_identity: HashMap<Principal, Arc<BasicIdentity>>,
    allowances: HashMap<(Account, Account), Tokens>,
}

impl TransactionsAndBalances {
    pub fn apply(
        &mut self,
        minter_identity: Arc<BasicIdentity>,
        default_fee: u64,
        tx: ArgWithCaller,
    ) {
        let fee = tx.fee().unwrap_or(default_fee);
        let minter: Account = minter_identity.sender().unwrap().into();
        let transaction = tx.to_transaction(minter);
        self.principal_to_basic_identity
            .extend(tx.principal_to_basic_identity.clone());
        self.principal_to_basic_identity
            .entry(tx.from().owner)
            .or_insert(tx.caller.clone());
        if self.duplicate(
            transaction.operation.clone(),
            transaction.created_at_time,
            transaction.memo.clone(),
        ) {
            return;
        };
        match transaction.operation {
            Operation::Mint { to, amount, .. } => {
                self.credit(to, amount.get_e8s());
            }
            Operation::Burn { from, amount, .. } => {
                assert_eq!(tx.from(), from);
                self.debit(from, amount.get_e8s());
            }
            Operation::Transfer {
                from, to, amount, ..
            } => {
                self.credit(to, amount.get_e8s());
                assert_eq!(tx.from(), from);
                self.debit(from, amount.get_e8s() + fee);
            }
            Operation::Approve {
                from,
                spender,
                amount,
                ..
            } => {
                assert_eq!(tx.from(), from);
                self.allowances
                    .entry((from, spender))
                    .and_modify(|current_allowance| {
                        *current_allowance =
                            Tokens::from_e8s((*current_allowance).get_e8s() + amount.get_e8s())
                    })
                    .or_insert(amount);
                self.debit(from, fee);
            }
        };
        self.transactions.push(tx);
    }

    fn credit(&mut self, account: Account, amount: u64) {
        *self.balances.entry(account).or_insert(0) += amount;
    }

    fn debit(&mut self, account: Account, amount: u64) {
        use std::collections::hash_map::Entry;

        match self.balances.entry(account) {
            Entry::Occupied(e) if e.get() <= &amount => {
                e.remove();
            }
            Entry::Occupied(mut e) => {
                *e.get_mut() -= amount;
            }
            _ => {}
        }
    }

    pub fn non_dust_balances(&self, threshold: u64) -> Vec<(Account, u64)> {
        self.balances
            .iter()
            .filter(|(_, balance)| balance > &&(threshold + 1))
            .map(|(account, balance)| (*account, *balance))
            .collect()
    }

    fn duplicate(
        &mut self,
        operation: Operation<Tokens>,
        created_at_time: Option<u64>,
        memo: Option<Memo>,
    ) -> bool {
        if created_at_time.is_some() {
            let tx = Transaction {
                operation,
                created_at_time,
                memo,
            };
            if self.txs.contains(&tx) {
                return true;
            }
            self.txs.insert(tx);
        }
        false
    }
}

fn amount_strategy() -> impl Strategy<Value = u64> {
    0..100_000_000_000u64 // max is 1M ICP
}

fn basic_identity_strategy() -> impl Strategy<Value = BasicIdentity> {
    prop::array::uniform32(0u8..).prop_map(|ran| {
        let rng = ChaCha20Rng::from_seed(ran);
        let signing_key = ed25519_consensus::SigningKey::new(rng);
        let keypair = Ed25519KeyPair {
            secret_key: signing_key.to_bytes(),
            public_key: signing_key.verification_key().to_bytes(),
        };
        BasicIdentity::from_pem(keypair.to_pem().as_bytes()).unwrap()
    })
}

#[derive(Debug)]
struct SigningAccount {
    identity: BasicIdentity,
    subaccount: Option<Subaccount>,
}

impl SigningAccount {
    fn account(&self) -> Account {
        Account {
            owner: self.identity.sender().unwrap(),
            subaccount: self.subaccount,
        }
    }
}

fn basic_identity_and_account_strategy() -> impl Strategy<Value = SigningAccount> {
    let bytes_strategy = prop::option::of(prop::collection::vec(0..=255u8, 32));
    let identity_strategy = basic_identity_strategy();
    (bytes_strategy, identity_strategy).prop_map(|(bytes, identity)| SigningAccount {
        identity,
        subaccount: bytes.map(|x| x.as_slice().try_into().unwrap()),
    })
}

/// Generates a list of valid transaction args with the caller, i.e.
/// transaction args that the Ledger will accept and that have the
/// Principal that should send them.
///
/// TODO: replace amount generation with something that makes sense,
///       e.g. exponential distribution
/// TODO: allow to pass the account distribution
pub fn valid_transactions_strategy(
    minter_identity: BasicIdentity,
    default_fee: u64,
    length: usize,
    now: SystemTime,
) -> impl Strategy<Value = Vec<ArgWithCaller>> {
    fn mint_strategy(
        minter_identity: Arc<BasicIdentity>,
        now: SystemTime,
        tx_hash_set_pointer: Arc<HashSet<Transaction<Tokens>>>,
    ) -> impl Strategy<Value = ArgWithCaller> {
        let minter: Account = minter_identity.sender().unwrap().into();
        (
            basic_identity_and_account_strategy(),
            amount_strategy(),
            valid_created_at_time_strategy(now),
            arb_memo(),
        )
            .prop_filter_map(
                "The minting account is set as to account or tx is a duplicate",
                move |(to_signer, amount, created_at_time, memo)| {
                    let to = to_signer.account();
                    let tx = Transaction {
                        operation: Operation::Mint::<Tokens> {
                            amount: Tokens::from_e8s(amount),
                            to,
                        },
                        created_at_time,
                        memo: memo.clone(),
                    };
                    if to == minter || tx_hash_set_pointer.contains(&tx) {
                        None
                    } else {
                        assert_eq!(minter_identity.sender().unwrap(), minter.owner);
                        Some(ArgWithCaller {
                            caller: minter_identity.clone(),
                            arg: LedgerEndpointArg::TransferArg(TransferArg {
                                from_subaccount: minter.subaccount,
                                to,
                                amount: amount.into(),
                                created_at_time,
                                fee: None,
                                memo,
                            }),
                            principal_to_basic_identity: HashMap::from([(
                                to.owner,
                                Arc::new(to_signer.identity),
                            )]),
                        })
                    }
                },
            )
    }

    fn burn_strategy(
        account_balance: impl Strategy<Value = (Account, u64)>,
        minter_identity: Arc<BasicIdentity>,
        default_fee: u64,
        now: SystemTime,
        tx_hash_set_pointer: Arc<HashSet<Transaction<Tokens>>>,
        account_to_basic_identity_pointer: Arc<HashMap<Principal, Arc<BasicIdentity>>>,
    ) -> impl Strategy<Value = ArgWithCaller> {
        let minter: Account = minter_identity.sender().unwrap().into();
        account_balance.prop_flat_map(move |(from, balance)| {
            let tx_hash_set = tx_hash_set_pointer.clone();
            let account_to_basic_identity = account_to_basic_identity_pointer.clone();
            (
                default_fee..=balance,
                valid_created_at_time_strategy(now),
                arb_memo(),
            )
                .prop_filter_map(
                    "Tx hash already exists",
                    move |(amount, created_at_time, memo)| {
                        let tx = Transaction {
                            operation: Operation::Burn::<Tokens> {
                                amount: Tokens::from_e8s(amount),
                                from,
                                spender: None,
                            },
                            created_at_time,
                            memo: memo.clone(),
                        };

                        if tx_hash_set.contains(&tx) {
                            None
                        } else {
                            let caller =
                                account_to_basic_identity.get(&from.owner).unwrap().clone();
                            assert_eq!(caller.sender().unwrap(), from.owner);
                            Some(ArgWithCaller {
                                caller,
                                arg: LedgerEndpointArg::TransferArg(TransferArg {
                                    from_subaccount: from.subaccount,
                                    to: minter,
                                    amount: amount.into(),
                                    created_at_time,
                                    fee: None,
                                    memo,
                                }),
                                principal_to_basic_identity: HashMap::new(),
                            })
                        }
                    },
                )
        })
    }

    fn transfer_strategy(
        account_balance: impl Strategy<Value = (Account, u64)>,
        minter_identity: Arc<BasicIdentity>,
        default_fee: u64,
        now: SystemTime,
        tx_hash_set_pointer: Arc<HashSet<Transaction<Tokens>>>,
        account_to_basic_identity_pointer: Arc<HashMap<Principal, Arc<BasicIdentity>>>,
    ) -> impl Strategy<Value = ArgWithCaller> {
        let minter: Account = minter_identity.sender().unwrap().into();
        account_balance.prop_flat_map(move |(from, balance)| {
            let tx_hash_set = tx_hash_set_pointer.clone();
            let account_to_basic_identity = account_to_basic_identity_pointer.clone();
            (
                basic_identity_and_account_strategy(),
                0..=(balance - default_fee),
                valid_created_at_time_strategy(now),
                arb_memo(),
                prop::option::of(Just(default_fee)),
            )
                .prop_filter_map(
                    "Tx is a self transfer or duplicate",
                    move |(to_signer, amount, created_at_time, memo, fee)| {
                        let to = to_signer.account();
                        let tx = Transaction {
                            operation: Operation::Transfer::<Tokens> {
                                amount: Tokens::from_e8s(amount),
                                from,
                                fee: fee.map(Tokens::from_e8s),
                                spender: None,
                                to,
                            },
                            created_at_time,
                            memo: memo.clone(),
                        };

                        if to == from || to == minter || from == minter || tx_hash_set.contains(&tx)
                        {
                            None
                        } else {
                            let caller =
                                account_to_basic_identity.get(&from.owner).unwrap().clone();
                            assert_eq!(caller.sender().unwrap(), from.owner);
                            Some(ArgWithCaller {
                                caller,
                                arg: LedgerEndpointArg::TransferArg(TransferArg {
                                    from_subaccount: from.subaccount,
                                    to,
                                    amount: amount.into(),
                                    created_at_time,
                                    fee: fee.map(Nat::from),
                                    memo,
                                }),
                                principal_to_basic_identity: HashMap::from([(
                                    to.owner,
                                    Arc::new(to_signer.identity),
                                )]),
                            })
                        }
                    },
                )
        })
    }

    fn approve_strategy(
        account_balance: impl Strategy<Value = (Account, u64)>,
        minter_identity: Arc<BasicIdentity>,
        default_fee: u64,
        now: SystemTime,
        tx_hash_set_pointer: Arc<HashSet<Transaction<Tokens>>>,
        account_to_basic_identity_pointer: Arc<HashMap<Principal, Arc<BasicIdentity>>>,
        allowance_map_pointer: Arc<HashMap<(Account, Account), Tokens>>,
    ) -> impl Strategy<Value = ArgWithCaller> {
        let minter: Account = minter_identity.sender().unwrap().into();
        account_balance.prop_flat_map(move |(from, balance)| {
            let tx_hash_set = tx_hash_set_pointer.clone();
            let account_to_basic_identity = account_to_basic_identity_pointer.clone();
            let allowance_map = allowance_map_pointer.clone();
            (
                basic_identity_and_account_strategy(),
                0..=(balance - default_fee),
                valid_created_at_time_strategy(now),
                arb_memo(),
                prop::option::of(Just(default_fee)),
                valid_expires_at_strategy(now),
                proptest::bool::ANY,
            )
                .prop_filter_map(
                    "Tx is a duplicate or self approve",
                    move |(
                        spender_signer,
                        amount,
                        created_at_time,
                        memo,
                        fee,
                        expires_at,
                        expect_allowance,
                    )| {
                        let spender = spender_signer.account();
                        let expected_allowance = allowance_map.get(&(from, spender)).copied();
                        let tx = Transaction {
                            operation: Operation::Approve::<Tokens> {
                                from,
                                spender,
                                fee: fee.map(Tokens::from_e8s),
                                amount: Tokens::from_e8s(amount),
                                expected_allowance: if expect_allowance {
                                    expected_allowance
                                } else {
                                    None
                                },
                                expires_at,
                            },
                            created_at_time,
                            memo: memo.clone(),
                        };
                        if spender == from
                            || spender == minter
                            || from == minter
                            || tx_hash_set.contains(&tx)
                        {
                            None
                        } else {
                            let caller =
                                account_to_basic_identity.get(&from.owner).unwrap().clone();
                            assert_eq!(caller.sender().unwrap(), from.owner);
                            Some(ArgWithCaller {
                                caller,
                                arg: LedgerEndpointArg::ApproveArg(ApproveArgs {
                                    from_subaccount: from.subaccount,
                                    spender,
                                    amount: amount.into(),
                                    created_at_time,
                                    fee: fee.map(Nat::from),
                                    memo,
                                    expected_allowance: expected_allowance.map(Nat::from),
                                    expires_at,
                                }),
                                principal_to_basic_identity: HashMap::from([(
                                    spender.owner,
                                    Arc::new(spender_signer.identity),
                                )]),
                            })
                        }
                    },
                )
        })
    }

    fn generate_strategy(
        state: TransactionsAndBalances,
        minter_identity: Arc<BasicIdentity>,
        default_fee: u64,
        additional_length: usize,
        now: SystemTime,
    ) -> BoxedStrategy<TransactionsAndBalances> {
        if additional_length == 0 {
            return Just(state).boxed();
        }

        // The next transaction is based on the non-dust balances in the state.
        // If there are no balances bigger than default_fees then the only next
        // transaction possible is minting, otherwise we can also burn or transfer.
        let balances = state.non_dust_balances(default_fee);
        let tx_hashes_pointer = Arc::new(state.txs.clone());
        let account_to_basic_identity_pointer = Arc::new(state.principal_to_basic_identity.clone());
        let allowance_map_pointer = Arc::new(state.allowances.clone());
        let mint_strategy =
            mint_strategy(minter_identity.clone(), now, tx_hashes_pointer.clone()).boxed();
        let arb_tx = if balances.is_empty() {
            mint_strategy
        } else {
            let account_balance = Rc::new(select(balances));
            let approve_strategy = approve_strategy(
                account_balance.clone(),
                minter_identity.clone(),
                default_fee,
                now,
                tx_hashes_pointer.clone(),
                account_to_basic_identity_pointer.clone(),
                allowance_map_pointer.clone(),
            )
            .boxed();
            let burn_strategy = burn_strategy(
                account_balance.clone(),
                minter_identity.clone(),
                default_fee,
                now,
                tx_hashes_pointer.clone(),
                account_to_basic_identity_pointer.clone(),
            )
            .boxed();
            let transfer_strategy = transfer_strategy(
                account_balance,
                minter_identity.clone(),
                default_fee,
                now,
                tx_hashes_pointer.clone(),
                account_to_basic_identity_pointer.clone(),
            )
            .boxed();
            proptest::strategy::Union::new_weighted(vec![
                (10, approve_strategy),
                (1, burn_strategy),
                (1, mint_strategy),
                (1000, transfer_strategy),
            ])
            .boxed()
        };

        (Just(state), arb_tx)
            .prop_flat_map(move |(mut state, tx)| {
                state.apply(minter_identity.clone(), default_fee, tx);
                generate_strategy(
                    state,
                    minter_identity.clone(),
                    default_fee,
                    additional_length - 1,
                    now,
                )
            })
            .boxed()
    }

    generate_strategy(
        TransactionsAndBalances::default(),
        Arc::new(minter_identity),
        default_fee,
        length,
        now,
    )
    .prop_map(|res| res.transactions.clone())
}

pub fn decimals_strategy() -> impl Strategy<Value = u8> {
    0..u8::MAX
}

pub fn symbol_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Za-z0-9]{1,5}").expect("failed to make generator")
}

pub fn metadata_strategy() -> impl Strategy<Value = Vec<(String, MetadataValue)>> {
    (symbol_strategy(), decimals_strategy()).prop_map(|(symbol, decimals)| {
        vec![
            ("icrc1:symbol".to_string(), MetadataValue::Text(symbol)),
            (
                "icrc1:decimals".to_string(),
                MetadataValue::Nat(candid::Nat::from(decimals)),
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use crate::{minter_identity, valid_transactions_strategy};
    use proptest::{
        strategy::{Strategy, ValueTree},
        test_runner::TestRunner,
    };
    use std::time::SystemTime;

    #[test]
    fn test_valid_transactions_strategy_generates_transaction() {
        let size = 10;
        let strategy =
            valid_transactions_strategy(minter_identity(), 10_000, size, SystemTime::now());
        let tree = strategy
            .new_tree(&mut TestRunner::default())
            .expect("Unable to run valid_transactions_strategy");
        assert_eq!(tree.current().len(), size)
    }
}
