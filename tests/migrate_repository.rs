//! Perform functional tests on a Krill instance, using the API
//!
use std::{fs, str::FromStr, time::Duration};

use tokio::time::sleep;

use krill::{
    commons::api::{
        ObjectName, RepositoryContact, ResourceClassName, ResourceSet, RoaDefinition, RoaDefinitionUpdates,
    },
    daemon::ca::ta_handle,
    test::*,
};

#[tokio::test]
async fn migrate_repository() {
    init_logging();

    info("##################################################################");
    info("#                                                                #");
    info("#                --= Test Migrating a Repository  =--            #");
    info("#                                                                #");
    info("##################################################################");

    info("##################################################################");
    info("#                                                                #");
    info("#                      Start Krill                               #");
    info("#                                                                #");
    info("##################################################################");
    info("");
    let krill_dir = start_krill_with_default_test_config(true, false, false).await;

    info("##################################################################");
    info("#                                                                #");
    info("#               Start Secondary Publication Server               #");
    info("#                                                                #");
    info("##################################################################");
    info("");
    let pubd_dir = start_krill_pubd().await;

    let ta = ta_handle();
    let testbed = handle("testbed");

    let ca1 = handle("CA1");
    let ca1_res = ipv4_resources("10.0.0.0/16");
    let ca1_route_definition = RoaDefinition::from_str("10.0.0.0/16-16 => 65000").unwrap();

    let rcn_0 = ResourceClassName::from(0);

    info("##################################################################");
    info("#                                                                #");
    info("# Wait for the *testbed* CA to get its certificate, this means   #");
    info("# that all CAs which are set up as part of krill_start under the #");
    info("# testbed config have been set up.                               #");
    info("#                                                                #");
    info("##################################################################");
    info("");
    assert!(ca_contains_resources(&testbed, &ResourceSet::all_resources()).await);

    // Verify that the TA published expected objects
    {
        let mut expected_files = expected_mft_and_crl(&ta, &rcn_0).await;
        expected_files.push(expected_issued_cer(&testbed, &rcn_0).await);
        assert!(
            will_publish_embedded(
                "TA should have manifest, crl and cert for testbed",
                &ta,
                &expected_files
            )
            .await
        );
    }

    {
        info("##################################################################");
        info("#                                                                #");
        info("#                      Set up CA1 under testbed                  #");
        info("#                                                                #");
        info("##################################################################");
        info("");
        set_up_ca_with_repo(&ca1).await;
        set_up_ca_under_parent_with_resources(&ca1, &testbed, &ca1_res).await;
    }

    {
        info("##################################################################");
        info("#                                                                #");
        info("#                      Create a ROA for CA1                      #");
        info("#                                                                #");
        info("##################################################################");
        info("");
        let mut updates = RoaDefinitionUpdates::empty();
        updates.add(ca1_route_definition);
        ca_route_authorizations_update(&ca1, updates).await;
    }

    {
        info("##################################################################");
        info("#                                                                #");
        info("#    Verify that the testbed published the expected objects      #");
        info("#                                                                #");
        info("##################################################################");
        info("");
        let mut expected_files = expected_mft_and_crl(&testbed, &rcn_0).await;
        expected_files.push(expected_issued_cer(&ca1, &rcn_0).await);
        assert!(
            will_publish_embedded(
                "testbed CA should have mft, crl and certs for CA1 and CA2",
                &testbed,
                &expected_files
            )
            .await
        );
    }

    {
        info("##################################################################");
        info("#                                                                #");
        info("#       Expect that CA1 publishes in the embedded repo           #");
        info("#                                                                #");
        info("##################################################################");
        info("");
        let mut expected_files = expected_mft_and_crl(&ca1, &rcn_0).await;
        expected_files.push(ObjectName::from(&ca1_route_definition).to_string());

        assert!(will_publish_embedded("CA1 should publish the certificate for CA3", &ca1, &expected_files).await);
    }

    {
        info("##################################################################");
        info("#                                                                #");
        info("# Migrate a Repository for CA1 (using a keyroll)                 #");
        info("#                                                                #");
        info("# CA1 currently uses the embedded publication server. In order   #");
        info("# to migrate it, we will need to do the following:               #");
        info("#                                                                #");
        info("# - get the RFC 8183 publisher request from CA1                  #");
        info("# - add CA1 as a publisher under the dedicated (separate) pubd,  #");
        info("# - get the response                                             #");
        info("# - update the repo config for CA1 using the 8183 response       #");
        info("#    -- this should initiate a key roll                          #");
        info("#    -- the new key publishes in the new repo                    #");
        info("# - complete the key roll                                        #");
        info("#    -- the old key should be cleaned up,                        #");
        info("#    -- nothing published for CA1 in the embedded repo           #");
        info("#                                                                #");
        info("##################################################################");
        info("");

        // Add CA1 to dedicated repo
        let publisher_request = publisher_request(&ca1).await;
        dedicated_repo_add_publisher(publisher_request).await;
        let response = dedicated_repository_response(&ca1).await;

        // Wait a tiny bit.. when we add a new repo we check that it's available or
        // it will be rejected.
        sleep(Duration::from_secs(1)).await;

        // Update CA1 to use dedicated repo
        let contact = RepositoryContact::new(response);
        repo_update(&ca1, contact).await;

        // This should result in a key roll and content published in both repos
        assert!(state_becomes_new_key(&ca1).await);

        // Expect that CA1 still publishes two current keys in the embedded repo
        {
            let mut expected_files = expected_mft_and_crl(&ca1, &rcn_0).await;
            expected_files.push(ObjectName::from(&ca1_route_definition).to_string());

            assert!(
                will_publish_embedded(
                    "CA1 should publish the MFT and CRL for both current keys in the embedded repo",
                    &ca1,
                    &expected_files
                )
                .await
            );
        }

        // Expect that CA1 publishes two new keys in the dedicated repo
        {
            let expected_files = expected_new_key_mft_and_crl(&ca1, &rcn_0).await;
            assert!(
                will_publish_dedicated(
                    "CA1 should publish the MFT and CRL for both new keys in the dedicated repo",
                    &ca1,
                    &expected_files
                )
                .await
            );
        }

        // Complete the keyroll, this should remove the content in the embedded repo
        ca_roll_activate(&ca1).await;
        assert!(state_becomes_active(&ca1).await);

        // Expect that CA1 publishes two current keys in the dedicated repo
        {
            let mut expected_files = expected_mft_and_crl(&ca1, &rcn_0).await;
            expected_files.push(ObjectName::from(&ca1_route_definition).to_string());

            assert!(
                will_publish_dedicated(
                    "CA1 should publish the MFT and CRL for both current keys in the dedicated repo",
                    &ca1,
                    &expected_files
                )
                .await
            );
        }

        // Expect that CA1 publishes nothing in the embedded repo
        {
            assert!(
                will_publish_embedded("CA1 should no longer publish anything in the embedded repo", &ca1, &[]).await
            );
        }
    }

    let _ = fs::remove_dir_all(krill_dir);
    let _ = fs::remove_dir_all(pubd_dir);
}
