mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use litesvm::LiteSVM;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use sra_common::errors::SraError;

fn clone_account(svm: &mut LiteSVM, source: &Pubkey) -> Pubkey {
    let copy = Pubkey::new_unique();
    svm.set_account(copy, svm.get_account(source).unwrap())
        .unwrap();
    copy
}

#[test]
fn owner_recovers_full_source_balance_active_or_paused_without_changing_config() {
    for active in [true, false] {
        let mut svm = setup_svm();
        let fx = PassiveFixture::new(&mut svm);
        set_source_balance(&mut svm, &fx, 2_000_000);
        set_sra_config(&mut svm, &fx.sra_config, |state| {
            state.deposit_nonce = u64::MAX;
        });
        send_ix(
            &mut svm,
            manage_sra_ix(
                fx.owner.pubkey(),
                fx.sra_config,
                fx.initiator.pubkey(),
                active,
            ),
            &fx.payer,
            &[&fx.owner],
        )
        .unwrap();
        mint_to(
            &mut svm,
            &fx.payer,
            &fx.payer,
            &fx.source_mint,
            &fx.owner_ata,
            123,
        );
        let mint_before = svm.get_account(&fx.source_mint).unwrap();
        assert!(!recover_source_vault_ix(&fx).accounts[1].is_writable);
        let event = recover_source_vault(&mut svm, &fx);
        assert_eq!(event.sra_config, fx.sra_config);
        assert_eq!(event.owner, fx.owner.pubkey());
        assert_eq!(event.source_mint, fx.source_mint);
        assert_eq!(event.amount, 2_000_000);
        assert_eq!(event.deposit_nonce, u64::MAX);
        assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), 2_000_123);
        assert_eq!(svm.get_account(&fx.source_mint).unwrap(), mint_before);
    }
}

#[test]
fn empty_source_vault_returns_nothing_to_recover() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 0);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[recover_source_vault_ix(&fx)],
        &[&fx.owner],
        None,
        "empty recovery must reject",
    );
    assert_sra_error(error, 0, SraError::NothingToRecover);
}

#[test]
fn source_recovery_rejects_wrong_owner_and_unsigned_owner() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let attacker = funded_keypair(&mut svm, SOL);
    let attacker_ata = create_ata(&mut svm, &fx.payer, &attacker.pubkey(), &fx.source_mint);
    let mut ix = recover_source_vault_ix(&fx);
    ix.accounts[0].pubkey = attacker.pubkey();
    ix.accounts[4].pubkey = attacker_ata;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[ix],
        &[&attacker],
        None,
        "wrong owner must reject",
    );
    assert_eq!(error, anchor_error(ErrorCode::ConstraintHasOne));

    let mut ix = recover_source_vault_ix(&fx);
    ix.accounts[0].is_signer = false;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[ix],
        &[],
        None,
        "unsigned owner must reject",
    );
    assert_eq!(error, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn source_recovery_pins_mint_vault_and_canonical_owner_ata() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let other_mint = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 6);
    let noncanonical_vault = clone_account(&mut svm, &fx.accounts.sra_vault);
    let noncanonical_owner_ata = clone_account(&mut svm, &fx.owner_ata);
    let other_owner_ata = create_ata(&mut svm, &fx.payer, &fx.payer.pubkey(), &fx.source_mint);
    for (index, substitute, expected) in [
        (2, other_mint, ErrorCode::ConstraintAssociated),
        (3, other_owner_ata, ErrorCode::ConstraintTokenOwner),
        (3, noncanonical_vault, ErrorCode::ConstraintAssociated),
        (4, other_owner_ata, ErrorCode::ConstraintTokenOwner),
        (4, noncanonical_owner_ata, ErrorCode::ConstraintAssociated),
    ] {
        let mut ix = recover_source_vault_ix(&fx);
        ix.accounts[index].pubkey = substitute;
        let error = assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[ix],
            &[&fx.owner],
            None,
            "substituted recovery account must reject",
        );
        assert_eq!(error, anchor_error(expected));
    }
}

#[test]
fn owner_recovers_additional_mint_without_address_book_entry() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let mint = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 9);
    let vault = create_ata(&mut svm, &fx.payer, &fx.sra_config, &mint);
    let owner_ata = create_ata(&mut svm, &fx.payer, &fx.owner.pubkey(), &mint);
    mint_to(&mut svm, &fx.payer, &fx.payer, &mint, &vault, 2_000_000);
    let before = passive_snapshot(&svm, &fx, None);
    let mut ix = recover_source_vault_ix(&fx);
    ix.accounts[2].pubkey = mint;
    ix.accounts[3].pubkey = vault;
    ix.accounts[4].pubkey = owner_ata;
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("additional mint recovery must succeed");
    assert_eq!(fetch_token_balance(&svm, &vault), 0);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), 2_000_000);
    assert_eq!(fetch_mint_supply(&svm, &mint), 2_000_000);
    assert_eq!(passive_snapshot(&svm, &fx, None), before);
}
