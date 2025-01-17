use crate::common::utils::update_neuron;
use crate::common::utils::wait_for_rosetta_to_catch_up_with_icp_ledger;
use crate::common::{
    system_test_environment::RosettaTestingEnvironment,
    utils::{get_test_agent, list_neurons, test_identity},
};
use ic_agent::{identity::BasicIdentity, Identity};
use ic_icp_rosetta_client::RosettaChangeAutoStakeMaturityArgs;
use ic_icp_rosetta_client::RosettaIncreaseNeuronStakeArgs;
use ic_icp_rosetta_client::{
    RosettaCreateNeuronArgs, RosettaDisburseNeuronArgs, RosettaSetNeuronDissolveDelayArgs,
};
use ic_nns_governance::pb::v1::neuron::DissolveState;
use ic_nns_governance::pb::v1::KnownNeuronData;
use ic_rosetta_api::ledger_client::list_known_neurons_response::ListKnownNeuronsResponse;
use ic_rosetta_api::{
    models::AccountBalanceRequest,
    request::transaction_operation_results::TransactionOperationResults,
};
use ic_types::PrincipalId;
use icp_ledger::{AccountIdentifier, DEFAULT_TRANSFER_FEE};
use lazy_static::lazy_static;
use rosetta_core::objects::ObjectMap;
use rosetta_core::request_types::CallRequest;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;

lazy_static! {
    pub static ref TEST_IDENTITY: Arc<BasicIdentity> = Arc::new(test_identity());
}

#[test]
fn test_create_neuron() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_tokens(100_000_000).unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = 100_000_000u64;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let neurons = list_neurons(&agent).await;

        assert!(!neurons.full_neurons.is_empty());
        assert!(neurons.full_neurons.clone().into_iter().all(|n| {
            n.controller == Some(PrincipalId::from(TEST_IDENTITY.sender().unwrap()))
                && n.cached_neuron_stake_e8s == staked_amount
        }));
    });
}

#[test]
fn test_increase_neuron_stake() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let initial_balance = 100_000_000_000;
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_e8s(initial_balance),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = initial_balance / 10;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        // Try to stake more than the amount of ICP in the account
        match env
            .rosetta_client
            .increase_neuron_stake(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaIncreaseNeuronStakeArgs::builder(u64::MAX.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
        {
            Err(e)
                if e.to_string().contains(
                    "the debit account doesn't have enough funds to complete the transaction",
                ) => {}
            Err(e) => panic!("Unexpected error: {}", e),
            Ok(ok) => panic!("Expected an errorm but got: {:?}", ok),
        }

        // Now we try with a valid amount
        let additional_stake = initial_balance / 10;
        env.rosetta_client
            .increase_neuron_stake(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaIncreaseNeuronStakeArgs::builder(additional_stake.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        assert_eq!(
            neuron.cached_neuron_stake_e8s,
            staked_amount + additional_stake
        );

        wait_for_rosetta_to_catch_up_with_icp_ledger(
            &env.rosetta_client,
            env.network_identifier.clone(),
            &agent,
        )
        .await;

        let balance = env
            .rosetta_client
            .account_balance(
                AccountBalanceRequest::builder(
                    env.network_identifier.clone(),
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()).into(),
                )
                .build(),
            )
            .await
            .unwrap()
            .balances
            .first()
            .unwrap()
            .value
            .parse::<u64>()
            .unwrap();
        assert_eq!(
            balance,
            initial_balance - staked_amount - additional_stake - DEFAULT_TRANSFER_FEE.get_e8s() * 2
        );
    });
}

#[test]
fn test_set_neuron_dissolve_delay_timestamp() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_tokens(100_000_000).unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = 100_000_000u64;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();

        let dissolve_delay_timestamp = match neuron.dissolve_state.unwrap() {
            // When a neuron is created it has a one week dissolve delay
            DissolveState::DissolveDelaySeconds(dissolve_delay_timestamp) => {
                dissolve_delay_timestamp
            }
            k => panic!(
                "Neuron should be in WhenDissolvedTimestampSeconds state, but is instead: {:?}",
                k
            ),
        };

        let one_week = 24 * 60 * 60 * 7;
        assert_eq!(dissolve_delay_timestamp, one_week);

        let new_dissolve_delay = dissolve_delay_timestamp + 1000;
        let new_dissolve_delay_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + new_dissolve_delay;

        // To be able to set the dissolve delay timestamp we need to set the state machine to live again
        env.rosetta_client
            .set_neuron_dissolve_delay(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaSetNeuronDissolveDelayArgs::builder(new_dissolve_delay_timestamp)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();

        let dissolve_delay_timestamp = match neuron.dissolve_state.unwrap() {
            // The neuron now has a new dissolve delay timestamp and is in NOT DISSOLVING which corresponds to a dissolve delay that is greater than 0
            DissolveState::DissolveDelaySeconds(dissolve_delay_timestamp) => {
                dissolve_delay_timestamp
            }
            k => panic!(
                "Neuron should be in DissolveDelaySeconds state, but is instead: {:?}",
                k
            ),
        };
        // The Dissolve Delay Timestamp should be updated
        // Since the state machine is live we do not know exactly how much time will be left at the time of calling the governance canister.
        // It should be between dissolve_delay_timestamp and dissolve_delay_timestamp - X seconds depending on how long it takes to call the governance canister
        assert!(dissolve_delay_timestamp <= new_dissolve_delay);
        assert!(dissolve_delay_timestamp > new_dissolve_delay - 10);

        assert!(dissolve_delay_timestamp > 0);
    });
}

#[test]
fn test_start_and_stop_neuron_dissolve() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_tokens(100_000_000).unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = 100_000_000u64;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        let dissolve_delay_timestamp = match neuron.dissolve_state.unwrap() {
            // When a neuron is created its dissolve delay timestamp is set to two weeks from now and is in NOT DISSOLVING state
            DissolveState::DissolveDelaySeconds(dissolve_delay_timestamp) => {
                dissolve_delay_timestamp
            }
            k => panic!(
                "Neuron should be in DissolveDelaySeconds state, but is instead: {:?}",
                k
            ),
        };
        let start_dissolving_response = TransactionOperationResults::try_from(
            env.rosetta_client
                .start_dissolving_neuron(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    neuron_index,
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();

        // The neuron should now be in DISSOLVING state
        assert_eq!(
            start_dissolving_response.operations.first().unwrap().status,
            Some("COMPLETED".to_owned())
        );
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        match neuron.dissolve_state.unwrap() {
            DissolveState::WhenDissolvedTimestampSeconds(d) => {
                assert!(dissolve_delay_timestamp <= d);
            }
            k => panic!(
                "Neuron should be in DissolveDelaySeconds state, but is instead: {:?}",
                k
            ),
        };

        // When we try to dissolve an already dissolving neuron the response should succeed with no change to the neuron
        let start_dissolving_response = TransactionOperationResults::try_from(
            env.rosetta_client
                .start_dissolving_neuron(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    neuron_index,
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();
        assert_eq!(
            start_dissolving_response.operations.first().unwrap().status,
            Some("COMPLETED".to_owned())
        );
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        assert!(
            matches!(
                neuron.dissolve_state.unwrap(),
                DissolveState::WhenDissolvedTimestampSeconds(_)
            ),
            "Neuron should be in WhenDissolvedTimestampSeconds state, but is instead: {:?}",
            neuron.dissolve_state.unwrap()
        );

        // Stop dissolving the neuron
        let stop_dissolving_response = TransactionOperationResults::try_from(
            env.rosetta_client
                .stop_dissolving_neuron(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    neuron_index,
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();
        assert_eq!(
            stop_dissolving_response.operations.first().unwrap().status,
            Some("COMPLETED".to_owned())
        );
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        assert!(matches!(
            neuron.dissolve_state.unwrap(),
            DissolveState::DissolveDelaySeconds(_)
        ));
    });
}

#[test]
fn test_change_auto_stake_maturity() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_tokens(100_000_000).unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = 100_000_000u64;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();

        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        // The neuron should not have auto stake maturity set
        assert!(neuron.auto_stake_maturity.is_none());

        // Change the auto stake maturity to true
        let change_auto_stake_maturity_response = TransactionOperationResults::try_from(
            env.rosetta_client
                .change_auto_stake_maturity(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    RosettaChangeAutoStakeMaturityArgs::builder(true)
                        .with_neuron_index(neuron_index)
                        .build(),
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();

        assert_eq!(
            change_auto_stake_maturity_response
                .operations
                .first()
                .unwrap()
                .status,
            Some("COMPLETED".to_owned())
        );
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        assert!(neuron.auto_stake_maturity.unwrap());

        // Change the auto stake maturity to false
        let change_auto_stake_maturity_response = TransactionOperationResults::try_from(
            env.rosetta_client
                .change_auto_stake_maturity(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    RosettaChangeAutoStakeMaturityArgs::builder(false)
                        .with_neuron_index(neuron_index)
                        .build(),
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();

        assert_eq!(
            change_auto_stake_maturity_response
                .operations
                .first()
                .unwrap()
                .status,
            Some("COMPLETED".to_owned())
        );
        let neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        assert!(neuron.auto_stake_maturity.is_none());
    });
}

#[test]
fn test_disburse_neuron() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let initial_balance = 100_000_000_000;
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_e8s(initial_balance),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = initial_balance/10;
        let neuron_index = 0;
        let from_subaccount = [0; 32];

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into())
                    .with_from_subaccount(from_subaccount)
                    .with_neuron_index(neuron_index)
                    .build(),
            )
            .await
            .unwrap();
        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;

        TransactionOperationResults::try_from(
            env.rosetta_client
                .start_dissolving_neuron(
                    env.network_identifier.clone(),
                    &(*TEST_IDENTITY).clone(),
                    neuron_index,
                )
                .await
                .unwrap()
                .metadata,
        )
        .unwrap();

        let mut neuron = list_neurons(&agent).await.full_neurons[0].to_owned();
        // If we try to disburse the neuron when it is not yet DISSOLVED we expect an error
        match env
            .rosetta_client
            .disburse_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaDisburseNeuronArgs::builder(neuron_index)
                    .with_recipient(TEST_IDENTITY.sender().unwrap().into())
                    .build(),
            )
            .await
        {
            Err(e) if e.to_string().contains(&format!("Could not disburse: PreconditionFailed: Neuron {} has NOT been dissolved. It is in state Dissolving",neuron.id.unwrap().id)) => (),
            Err(e) => panic!("Unexpected error: {}", e),
            Ok(_) => panic!("Expected an error but got success"),
        }
        // Let rosetta catch up with the transfer that happended when creating the neuron
        wait_for_rosetta_to_catch_up_with_icp_ledger(
            &env.rosetta_client,
            env.network_identifier.clone(),
            &agent,
        ).await;
        let balance_before_disburse = env
        .rosetta_client
        .account_balance(
            AccountBalanceRequest::builder(
                env.network_identifier.clone(),
                AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()).into(),
            )
            .build(),
        )
        .await
        .unwrap()
        .balances
        .first()
        .unwrap()
        .clone()
        .value.parse::<u64>().unwrap();

        // We now update the neuron so it is in state DISSOLVED
        let now = env.pocket_ic.get_time().await.duration_since(UNIX_EPOCH).unwrap().as_secs();
        neuron.dissolve_state = Some(DissolveState::WhenDissolvedTimestampSeconds(now - 1));
        update_neuron(&agent, neuron.into()).await;

        match list_neurons(&agent).await.full_neurons[0].dissolve_state.unwrap() {
            DissolveState::WhenDissolvedTimestampSeconds (d) => {
                // The neuron should now be in DISSOLVED state
                assert!(d<now);
            }
            k => panic!(
                "Neuron should be in DissolveDelaySeconds state, but is instead: {:?}",
                k
            ),
        }

        // Now we should be able to disburse the neuron
        env.rosetta_client
            .disburse_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaDisburseNeuronArgs::builder(neuron_index)
                    .with_recipient(TEST_IDENTITY.sender().unwrap().into())
                    .build(),
            )
            .await
            .unwrap();

        // Wait for the ledger to sync up to the block where the disbursement happened
        wait_for_rosetta_to_catch_up_with_icp_ledger(
            &env.rosetta_client,
            env.network_identifier.clone(),
            &agent,
        )
        .await;

        // The recipient should have received the disbursed amount
        let balance_after_disburse = env
            .rosetta_client
            .account_balance(
                AccountBalanceRequest::builder(
                    env.network_identifier.clone(),
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()).into(),
                )
                .build(),
            )
            .await
            .unwrap()
            .balances
            .first()
            .unwrap().clone()
            .value.parse::<u64>().unwrap();
        // The balance should be the same as before the creation of the neuron minus the transfer fee
        assert_eq!(balance_after_disburse, balance_before_disburse + staked_amount - DEFAULT_TRANSFER_FEE.get_e8s());
    });
}

#[test]
fn test_list_known_neurons() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let env = RosettaTestingEnvironment::builder()
            .with_initial_balances(
                vec![(
                    AccountIdentifier::from(TEST_IDENTITY.sender().unwrap()),
                    // A hundred million ICP should be enough
                    icp_ledger::Tokens::from_tokens(100_000_000).unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .with_governance_canister()
            .build()
            .await;

        // Stake the minimum amount 100 million e8s
        let staked_amount = 100_000_000u64;

        env.rosetta_client
            .create_neuron(
                env.network_identifier.clone(),
                &(*TEST_IDENTITY).clone(),
                RosettaCreateNeuronArgs::builder(staked_amount.into()).build(),
            )
            .await
            .unwrap();

        // See if the neuron was created successfully
        let agent = get_test_agent(env.pocket_ic.url().unwrap().port().unwrap()).await;
        let mut neuron = list_neurons(&agent).await.full_neurons[0].to_owned();

        neuron.known_neuron_data = Some(KnownNeuronData {
            name: "KnownNeuron 0".to_owned(),
            description: Some("This is a known neuron".to_owned()),
        });
        update_neuron(&agent, neuron.into()).await;

        let known_neurons = ListKnownNeuronsResponse::try_from(Some(
            env.rosetta_client
                .call(CallRequest {
                    network_identifier: env.network_identifier.clone(),
                    method_name: "list_known_neurons".to_owned(),
                    parameters: ObjectMap::new(),
                })
                .await
                .unwrap()
                .result,
        ))
        .unwrap();

        assert_eq!(known_neurons.known_neurons.len(), 1);
        assert_eq!(
            known_neurons.known_neurons[0]
                .known_neuron_data
                .clone()
                .unwrap(),
            KnownNeuronData {
                name: "KnownNeuron 0".to_owned(),
                description: Some("This is a known neuron".to_owned())
            }
            .into()
        );
    });
}
