use super::*;
use common::cctp::*;

#[test]
fn cross_adapter_fill_waits_for_active_fill_actions() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let mut params = sample_create_route_params(mint, [0x83; 32]);
    params.allowed_adapters = vec![CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID];
    params.allowed_executors = vec![payer.pubkey()];
    params.action_config = vec![spl(owner.pubkey(), 10_000)];
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let route = derive_route_pda(&owner.pubkey(), &hash, &params.salt).0;
    send_ix(
        &mut svm,
        create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            hash,
            params,
            &[CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .unwrap();
    let route_vault = derive_ata(&route, &mint);
    let execution_vault = derive_ata(&derive_execution_authority_pda(&route).0, &mint);
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);

    let nonce = [0x84; 32];
    let (attester, attester_addr) = test_attester();
    plant_transmitter_state(&mut svm, 1, vec![attester_addr]);
    plant_used_nonce(&mut svm, nonce, true);
    mint_to(&mut svm, &payer, &payer, &mint, &route_vault, AMOUNT);
    let message = sample_cctp_message(nonce, &route, &mint, AMOUNT, 0, 0).encode();
    let attestation = sign_attestation(&message, &[&attester]);
    send_ix(
        &mut svm,
        verify_cctp_mint_ix(&payer.pubkey(), &route, nonce, message, attestation),
        &payer,
        &[],
    )
    .expect("CCTP evidence is verified");

    let second_amount = 6_000_000;
    let relay = sample_relay_data(route, mint, SOURCE_CHAIN_ID, second_amount);
    let second_id = relay_hash_for(&relay);
    let relayer = funded_keypair(&mut svm, 100 * SOL);
    let relayer_ata = create_ata(&mut svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(&mut svm, &payer, &payer, &mint, &relayer_ata, second_amount);
    send_ix(
        &mut svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &route_vault,
            second_id,
            second_amount,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .unwrap();
    send_ix(
        &mut svm,
        verify_across_fill_ix(&payer.pubkey(), &route, second_id, relay),
        &payer,
        &[],
    )
    .expect("Across evidence is verified before A finalizes");

    let first_evidence = derive_cctp_verified_fill_pda(&nonce).0;
    let second_evidence = derive_verified_fill_pda(&second_id).0;
    assert_eq!(fetch_token_balance(&svm, &route_vault), 10_000_000);
    send_ix(
        &mut svm,
        finalize_fill_ix(&payer.pubkey(), &route, hash, &first_evidence, nonce, &mint),
        &payer,
        &[],
    )
    .expect("CCTP A finalizes");
    assert_eq!(fetch_route(&svm, &route).active_fill, Some(nonce));
    let second_ix = finalize_fill_ix(
        &payer.pubkey(),
        &route,
        hash,
        &second_evidence,
        second_id,
        &mint,
    );
    let second_receipt = derive_fill_receipt_pda(&route, &second_id).0;
    let addresses = [
        route,
        route_vault,
        execution_vault,
        first_evidence,
        second_evidence,
        derive_fill_receipt_pda(&route, &nonce).0,
        second_receipt,
    ];
    let before = addresses.map(|key| svm.get_account(&key));
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            second_ix.clone(),
            &payer,
            &[],
            "Across B waits for CCTP A"
        ),
        custom_error(SraError::InvalidActiveFill)
    );
    assert_eq!(addresses.map(|key| svm.get_account(&key)), before);
    assert!(svm.get_account(&second_receipt).is_none());
    assert_eq!(fetch_token_balance(&svm, &route_vault), second_amount);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), AMOUNT);

    send_ix(
        &mut svm,
        execute_actions_ix(&payer.pubkey(), &route, nonce, &mint, &[owner_ata]),
        &payer,
        &[],
    )
    .expect("A actions succeed");
    assert_eq!(fetch_route(&svm, &route).active_fill, None);
    assert_eq!(
        status(&svm, &derive_fill_receipt_pda(&route, &nonce).0),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
    send_ix(&mut svm, second_ix, &payer, &[]).expect("queued Across B now finalizes");
    assert_eq!(fetch_route(&svm, &route).active_fill, Some(second_id));
    assert_eq!(status(&svm, &second_receipt), ExecutionStatus::Finalized);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), second_amount);
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);
}
