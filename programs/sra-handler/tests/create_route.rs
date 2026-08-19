mod common;

use common::*;
use solana_sdk::{pubkey::Pubkey, signer::Signer};
use sra_common::{
    constants::{MAX_ACTION_IDXS, MAX_ADAPTERS, MAX_EXECUTORS},
    errors::SraError,
    types::{AccountTemplate, FixedRole, MinOut, SlotRole, SolanaAction},
};

// --------------------------------------------------------------------------
// happy path
// --------------------------------------------------------------------------

#[test]
fn create_route_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [1u8; 32]);
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    let (execution_authority, _) = derive_execution_authority_pda(&route_pda);
    let route_vault = derive_ata(&route_pda, &mint);
    let execution_vault = derive_ata(&execution_authority, &mint);

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params.clone(),
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("create_route should succeed");

    let route = fetch_route(&svm, &route_pda);
    assert_eq!(route.owner, owner.pubkey());
    assert_eq!(route.source_chain_id, params.source_chain_id);
    assert_eq!(route.expected_mint, mint);
    assert_eq!(route.action_config, params.action_config);
    assert_eq!(route.salt, params.salt);
    assert_eq!(route.allowed_adapters, vec![ADAPTER_PROGRAM_ID]);
    assert!(
        route.allowed_executors.is_empty(),
        "the sample route starts with no executors"
    );
    assert!(!route.paused, "paused must start false");
    assert_ne!(route.bump, 0, "canonical bump stamped");

    // Both vault ATAs created and empty.
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
}

#[test]
fn create_route_reuses_preexisting_vault_atas_with_funds() {
    const ROUTE_VAULT_AMOUNT: u64 = 1_000_000;

    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [25u8; 32]);
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    let (execution_authority, _) = derive_execution_authority_pda(&route_pda);
    let route_vault = create_ata(&mut svm, &payer, &route_pda, &mint);
    let execution_vault = create_ata(&mut svm, &payer, &execution_authority, &mint);
    mint_to(
        &mut svm,
        &payer,
        &payer,
        &mint,
        &route_vault,
        ROUTE_VAULT_AMOUNT,
    );

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner])
        .expect("create_route should reuse preexisting vault ATAs");

    assert_eq!(fetch_route(&svm, &route_pda).owner, owner.pubkey());
    assert_eq!(fetch_token_balance(&svm, &route_vault), ROUTE_VAULT_AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
}

#[test]
fn create_route_emits_route_created() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [2u8; 32]);
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params.clone(),
        &[ADAPTER_PROGRAM_ID],
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&owner]).expect("create_route succeeds");

    let ev = extract_anchor_event::<sra_handler::events::RouteCreated>(&meta.logs)
        .expect("RouteCreated event must be emitted");
    assert_eq!(ev.route, route_pda);
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.source_chain_id, params.source_chain_id);
    assert_eq!(ev.expected_mint, mint);
    assert_eq!(ev.salt, params.salt);
    assert_eq!(ev.allowed_adapters, vec![ADAPTER_PROGRAM_ID]);
    assert!(ev.allowed_executors.is_empty());
    assert!(!ev.paused, "event carries post-write state");
}

// --------------------------------------------------------------------------
// allowed_executors (init + shape validation)
// --------------------------------------------------------------------------

#[test]
fn create_route_persists_allowed_executors() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let executors = vec![Pubkey::new_unique(), Pubkey::new_unique()];
    let mut params = sample_create_route_params(mint, [20u8; 32]);
    // Executors are not in route_config_hash, so setting them can't change the hash.
    let hash_without = route_config_hash_for(&owner.pubkey(), &params);
    params.allowed_executors = executors.clone();
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    assert_eq!(
        hash, hash_without,
        "allowed_executors must not affect route_config_hash"
    );
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("create_route with executors should succeed");

    assert_eq!(fetch_route(&svm, &route_pda).allowed_executors, executors);
}

#[test]
fn create_route_empty_executors_allowed() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // The sample carries an empty executor set; create must accept it.
    let params = sample_create_route_params(mint, [21u8; 32]);
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("empty allowed_executors is permitted");

    assert!(fetch_route(&svm, &route_pda).allowed_executors.is_empty());
}

#[test]
fn create_route_too_many_executors_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [22u8; 32]);
    params.allowed_executors = (0..=MAX_EXECUTORS).map(|_| Pubkey::new_unique()).collect();

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("oversized allowed_executors must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::TooManyExecutors));
}

#[test]
fn create_route_duplicate_executor_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let dup = Pubkey::new_unique();
    let mut params = sample_create_route_params(mint, [23u8; 32]);
    params.allowed_executors = vec![dup, dup];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("a duplicate executor must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::DuplicateExecutor));
}

#[test]
fn create_route_zero_executor_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [24u8; 32]);
    params.allowed_executors = vec![Pubkey::default()];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("a zero executor pubkey must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidExecutor));
}

// --------------------------------------------------------------------------
// identity / derivability
// --------------------------------------------------------------------------

#[test]
fn create_route_deterministic_pda() {
    let owner = Pubkey::new_unique();
    let params = sample_create_route_params(Pubkey::new_unique(), [9u8; 32]);
    let hash = route_config_hash_for(&owner, &params);
    let (a, _) = derive_route_pda(&owner, &hash, &params.salt);
    let (b, _) = derive_route_pda(&owner, &hash, &params.salt);
    assert_eq!(a, b, "same inputs must derive the same route PDA");
}

#[test]
fn create_route_config_hash_matches_offchain() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [3u8; 32]);
    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params.clone(),
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("create_route succeeds");

    // The route exists at the address derived purely off-chain from the config
    // hash — derivability without any registry lookup.
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    let route = fetch_route(&svm, &route_pda);
    assert_eq!(route.owner, owner.pubkey());
    assert_eq!(route.action_config, params.action_config);
}

#[test]
fn create_route_wrong_config_hash_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [4u8; 32]);
    // A hash that matches the route PDA seeds but NOT the supplied params: the
    // body recomputes from params and must reject the mismatch.
    let mut tampered = route_config_hash_for(&owner.pubkey(), &params);
    tampered[0] ^= 0xFF;

    let ix = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        tampered,
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("tampered config hash must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidRouteConfig));
}

// --------------------------------------------------------------------------
// fail-fast bounds
// --------------------------------------------------------------------------

#[test]
fn create_route_too_many_actions_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [5u8; 32]);
    params.action_config = (0..=MAX_ACTION_IDXS)
        .map(|_| SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 1,
        })
        .collect();

    // route_config_hash cannot be computed over an oversized list, so pass a
    // placeholder; the body's bound check fires before the hash recompute.
    let ix = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        [0u8; 32],
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("oversized action_config must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::TooManyActionIdxs));
}

#[test]
fn create_route_too_many_adapters_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [6u8; 32]);
    let adapters = vec![ADAPTER_PROGRAM_ID; MAX_ADAPTERS + 1];
    params.allowed_adapters = adapters.clone();

    let ix = create_route_ix(&payer.pubkey(), &owner.pubkey(), params, &adapters);
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("oversized allowed_adapters must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::TooManyAdapters));
}

#[test]
fn create_route_duplicate_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [13u8; 32]);
    params.allowed_adapters = vec![ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("duplicate allowed_adapters must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::DuplicateAdapter));
}

#[test]
fn create_route_action_bps_sum_over_10000_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [7u8; 32]);
    params.action_config = vec![
        SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 6_000,
        },
        SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 5_000,
        },
    ];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("Σ amount_bps > 100% must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidActionBpsSum));
}

#[test]
fn create_route_action_bps_sum_under_10000_rejected() {
    // Under-consumption would strand a residual past terminal success.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [21u8; 32]);
    params.action_config = vec![SolanaAction::SplTransfer {
        recipient: Pubkey::new_unique(),
        amount_bps: 9_999,
    }];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("Σ amount_bps < 100% must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidActionBpsSum));
}

#[test]
fn create_route_empty_action_config_rejected() {
    // An empty commitment would strand the whole fill past terminal success.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [22u8; 32]);
    params.action_config = vec![];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("empty action_config must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidActionBpsSum));
}

#[test]
fn create_route_action_bps_sum_exactly_10000_succeeds() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [8u8; 32]);
    params.action_config = vec![
        SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 4_000,
        },
        SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 6_000,
        },
    ];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner])
        .expect("Σ amount_bps == 100% is the inclusive boundary");
}

#[test]
fn create_route_non_executable_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // A real account that exists but is not a program (mistyped pubkey).
    let bogus_adapter = plant_non_executable_account(&mut svm);
    let mut params = sample_create_route_params(mint, [10u8; 32]);
    params.allowed_adapters = vec![bogus_adapter];

    let ix = create_route_ix(&payer.pubkey(), &owner.pubkey(), params, &[bogus_adapter]);
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("non-executable adapter must be rejected at create time")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidAdapterProgram));
}

// --------------------------------------------------------------------------
// asset binding (mint in route identity) + duplicates
// --------------------------------------------------------------------------

#[test]
fn create_route_same_params_different_mint_distinct_pdas() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint_a = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let mint_b = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // Identical in every dimension except the mint → the asset is in the identity.
    let params_a = sample_create_route_params(mint_a, [11u8; 32]);
    let mut params_b = params_a.clone();
    params_b.expected_mint = mint_b;

    let hash_a = route_config_hash_for(&owner.pubkey(), &params_a);
    let hash_b = route_config_hash_for(&owner.pubkey(), &params_b);
    let (route_a, _) = derive_route_pda(&owner.pubkey(), &hash_a, &params_a.salt);
    let (route_b, _) = derive_route_pda(&owner.pubkey(), &hash_b, &params_b.salt);
    assert_ne!(
        route_a, route_b,
        "different mints must yield distinct routes"
    );

    send_ix(
        &mut svm,
        create_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            params_a,
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .expect("route A succeeds");
    send_ix(
        &mut svm,
        create_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            params_b,
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .expect("route B coexists");

    assert_eq!(fetch_route(&svm, &route_a).expected_mint, mint_a);
    assert_eq!(fetch_route(&svm, &route_b).expected_mint, mint_b);
}

#[test]
fn create_route_duplicate_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let params = sample_create_route_params(mint, [12u8; 32]);
    send_ix(
        &mut svm,
        create_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            params.clone(),
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .expect("first create_route succeeds");

    let result = send_ix(
        &mut svm,
        create_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            params,
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    );
    assert!(
        result.is_err(),
        "second create_route at the same PDA must fail"
    );
}

// --------------------------------------------------------------------------
// VerifiedCpi (dynamic action) validated end-to-end through create_route
// --------------------------------------------------------------------------

fn verified_cpi_action(slots: Vec<SlotRole>) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: Pubkey::new_unique(),
        input_mint: Pubkey::new_unique(),
        input_bps: 10_000,
        account_template: AccountTemplate { slots },
        output_mint: Pubkey::new_unique(),
        min_out: MinOut::SignedQuote {
            quoter: Pubkey::new_unique(),
        },
        allow_permissioned_output: false,
    }
}

#[test]
fn create_route_oracle_min_out_rejected() {
    // A committed oracle min-out could never execute; refuse before funds stage.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut action = verified_cpi_action(vec![
        SlotRole::Fixed(FixedRole::SigningAuthority),
        SlotRole::Fixed(FixedRole::Source),
        SlotRole::Fixed(FixedRole::Destination),
        SlotRole::Fixed(FixedRole::OutputMint),
        SlotRole::Free,
    ]);
    if let SolanaAction::VerifiedCpi { min_out, .. } = &mut action {
        *min_out = MinOut::Oracle {
            feed: Pubkey::new_unique(),
            max_slippage_bps: 50,
        };
    }
    let mut params = sample_create_route_params(mint, [23u8; 32]);
    params.action_config = vec![action];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("oracle min_out must be rejected at create")
        .err;
    assert_eq!(err, custom_error(SraError::UnsupportedAction));
}

#[test]
fn create_route_verified_cpi_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    let mut params = sample_create_route_params(mint, [13u8; 32]);
    params.action_config = vec![verified_cpi_action(vec![
        SlotRole::Fixed(FixedRole::SigningAuthority),
        SlotRole::Fixed(FixedRole::Source),
        SlotRole::Fixed(FixedRole::Destination),
        SlotRole::Fixed(FixedRole::OutputMint),
        SlotRole::Free,
    ])];
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params.clone(),
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("a well-formed VerifiedCpi route is created");

    assert_eq!(
        fetch_route(&svm, &route_pda).action_config,
        params.action_config
    );
}

#[test]
fn create_route_verified_cpi_unpinned_template_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // Template omits the Source pin — the relayer could redirect the input —
    // so create_route must reject it before committing the route identity.
    let mut params = sample_create_route_params(mint, [14u8; 32]);
    params.action_config = vec![verified_cpi_action(vec![
        SlotRole::Fixed(FixedRole::SigningAuthority),
        SlotRole::Fixed(FixedRole::Destination),
        SlotRole::Free,
    ])];

    let ix = create_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("a VerifiedCpi template missing a pinned value slot must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidActionConfig));
}
