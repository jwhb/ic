use ic_nervous_system_integration_tests::pocket_ic_helpers::sns;
use ic_nervous_system_integration_tests::{
    create_service_nervous_system_builder::CreateServiceNervousSystemBuilder,
    pocket_ic_helpers::{
        add_wasm_via_nns_proposal, add_wasms_to_sns_wasm, install_nns_canisters, nns,
    },
};
use ic_nns_test_utils::sns_wasm::create_modified_sns_wasm;
use ic_sns_governance::{
    governance::UPGRADE_STEPS_INTERVAL_REFRESH_BACKOFF_SECONDS, pb::v1 as sns_pb,
};
use ic_sns_swap::pb::v1::Lifecycle;
use ic_sns_wasm::pb::v1::SnsCanisterType;
use pocket_ic::nonblocking::PocketIc;
use pocket_ic::PocketIcBuilder;
use std::time::Duration;

/// Advances time by up to `timeout_seconds` seconds and `timeout_seconds` tickets (1 tick = 1 second).
/// Each tick, it observes the state using the provided `observe` function.
/// If the observed state matches the `expected` state, it returns `Ok(())`.
/// If the timeout is reached, it returns an error.
async fn await_with_timeout<'a, T, F, Fut>(
    pocket_ic: &'a PocketIc,
    timeout_seconds: u64,
    observe: F,
    expected: &T,
) -> Result<(), String>
where
    T: std::cmp::PartialEq + std::fmt::Debug,
    F: Fn(&'a PocketIc) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut counter = 0;
    loop {
        pocket_ic.advance_time(Duration::from_secs(1)).await;
        pocket_ic.tick().await;

        let observed = observe(pocket_ic).await;
        if observed == *expected {
            return Ok(());
        }
        if counter == timeout_seconds {
            return Err(format!(
                "Observed state: {:?}\n!= Expected state {:?}\nafter {} seconds / rounds",
                observed, expected, timeout_seconds,
            ));
        }
        counter += 1;
    }
}

#[tokio::test]
async fn test_get_upgrade_journal() {
    let pocket_ic = PocketIcBuilder::new()
        .with_nns_subnet()
        .with_sns_subnet()
        .build_async()
        .await;

    // Install the (master) NNS canisters.
    let with_mainnet_nns_canisters = false;
    install_nns_canisters(&pocket_ic, vec![], with_mainnet_nns_canisters, None, vec![]).await;

    // Publish (master) SNS Wasms to SNS-W.
    let with_mainnet_sns_canisters = false;
    let deployed_sns_starting_info = add_wasms_to_sns_wasm(&pocket_ic, with_mainnet_sns_canisters)
        .await
        .unwrap();
    let initial_sns_version = nns::sns_wasm::get_latest_sns_version(&pocket_ic).await;

    // Deploy an SNS instance via proposal.
    let sns = {
        let create_service_nervous_system = CreateServiceNervousSystemBuilder::default().build();
        let swap_parameters = create_service_nervous_system
            .swap_parameters
            .clone()
            .unwrap();

        let sns_instance_label = "1";
        let (sns, _) = nns::governance::propose_to_deploy_sns_and_wait(
            &pocket_ic,
            create_service_nervous_system,
            sns_instance_label,
        )
        .await;

        sns::swap::await_swap_lifecycle(&pocket_ic, sns.swap.canister_id, Lifecycle::Open)
            .await
            .unwrap();
        sns::swap::smoke_test_participate_and_finalize(
            &pocket_ic,
            sns.swap.canister_id,
            swap_parameters,
        )
        .await;
        sns
    };

    // State A: right after SNS creation.
    {
        let sns_pb::GetUpgradeJournalResponse {
            upgrade_steps,
            response_timestamp_seconds,
            ..
        } = sns::governance::get_upgrade_journal(&pocket_ic, sns.governance.canister_id).await;
        let upgrade_steps = upgrade_steps
            .expect("upgrade_steps should be Some")
            .versions;
        assert_eq!(upgrade_steps, vec![initial_sns_version.clone()]);
        assert_eq!(response_timestamp_seconds, Some(1620501459));
    }

    await_with_timeout(
        &pocket_ic,
        UPGRADE_STEPS_INTERVAL_REFRESH_BACKOFF_SECONDS,
        |pocket_ic| async {
            sns::governance::get_upgrade_journal(pocket_ic, sns.governance.canister_id)
                .await
                .upgrade_steps
                .unwrap()
                .versions
        },
        &vec![initial_sns_version.clone()],
    )
    .await
    .unwrap();

    // Publish a new SNS version.
    let (new_sns_version_1, new_sns_version_2) = {
        let (_, original_root_wasm) = deployed_sns_starting_info
            .get(&SnsCanisterType::Root)
            .unwrap();

        let new_sns_version_1 = {
            let root_wasm = create_modified_sns_wasm(original_root_wasm, Some(1));
            add_wasm_via_nns_proposal(&pocket_ic, root_wasm.clone())
                .await
                .unwrap();
            let root_wasm_hash = root_wasm.sha256_hash().to_vec();
            sns_pb::governance::Version {
                root_wasm_hash,
                ..initial_sns_version.clone()
            }
        };

        let new_sns_version_2 = {
            let root_wasm = create_modified_sns_wasm(original_root_wasm, Some(2));
            add_wasm_via_nns_proposal(&pocket_ic, root_wasm.clone())
                .await
                .unwrap();
            let root_wasm_hash = root_wasm.sha256_hash().to_vec();
            sns_pb::governance::Version {
                root_wasm_hash,
                ..new_sns_version_1.clone()
            }
        };

        let sns_version = nns::sns_wasm::get_latest_sns_version(&pocket_ic).await;
        assert_ne!(sns_version, initial_sns_version);

        (new_sns_version_1, new_sns_version_2)
    };

    await_with_timeout(
        &pocket_ic,
        UPGRADE_STEPS_INTERVAL_REFRESH_BACKOFF_SECONDS,
        |pocket_ic| async {
            sns::governance::get_upgrade_journal(pocket_ic, sns.governance.canister_id)
                .await
                .upgrade_steps
                .unwrap()
                .versions
        },
        &vec![
            initial_sns_version,
            new_sns_version_1,
            new_sns_version_2.clone(),
        ],
    )
    .await
    .unwrap();

    // Advance the target version.
    {
        sns::governance::advance_target_version(
            &pocket_ic,
            sns.governance.canister_id,
            new_sns_version_2.clone(),
        )
        .await;

        // Check that the target version is set to the new version.
        let sns_pb::GetUpgradeJournalResponse { target_version, .. } =
            sns::governance::get_upgrade_journal(&pocket_ic, sns.governance.canister_id).await;

        assert_eq!(target_version, Some(new_sns_version_2.clone()));
    }
}
