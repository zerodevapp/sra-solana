mod common;

use anchor_lang::{error::ErrorCode, AccountSerialize, InstructionData, ToAccountMetas};
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signer::Signer};
use sra_common::errors::SraError;

fn manage_sra_ix(owner: Pubkey, sra: Pubkey, initiator: Pubkey, active: bool) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ManageSra {
            owner,
            sra_config: sra,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ManageSra { initiator, active }.data(),
    }
}

fn manage_sra(
    svm: &mut LiteSVM,
    fx: &PassiveFixture,
    initiator: Pubkey,
    active: bool,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    send_ix_with_meta(
        svm,
        manage_sra_ix(fx.owner.pubkey(), fx.sra_config, initiator, active),
        &fx.payer,
        &[&fx.owner],
    )
}

#[test]
fn manage_sra_replaces_only_runtime_control() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let replacement = solana_sdk::signature::Keypair::new();
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &fx.accounts.sra_vault,
        1_000_000,
    );
    let vault_before = svm.get_account(&fx.accounts.sra_vault).unwrap();
    let mut state = fetch_sra_config(&svm, &fx.sra_config);
    state.deposit_nonce = u64::MAX;
    let mut account = svm.get_account(&fx.sra_config).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(fx.sra_config, account).unwrap();
    let immutable_before = (
        state.owner,
        state.final_handler,
        state.handler_data.clone(),
        state.salt,
        state.deposit_nonce,
        state.bump,
    );
    let meta = manage_sra(&mut svm, &fx, replacement.pubkey(), false).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra_config);
    assert_eq!(state.initiator, replacement.pubkey());
    assert!(!state.active);
    assert_eq!(
        (
            state.owner,
            state.final_handler,
            state.handler_data,
            state.salt,
            state.deposit_nonce,
            state.bump,
        ),
        immutable_before,
    );
    assert_eq!(
        svm.get_account(&fx.accounts.sra_vault).unwrap(),
        vault_before
    );
    let event = extract_anchor_event::<sra_handler::events::SraManaged>(&meta.logs)
        .expect("SraManaged event must be emitted");
    assert_eq!(event.sra_config, fx.sra_config);
    assert_eq!(event.owner, fx.owner.pubkey());
    assert_eq!(event.initiator, replacement.pubkey());
    assert!(!event.active);
    assert_eq!(event.deposit_nonce, u64::MAX);
}

#[test]
fn manage_sra_non_owner_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);
    let before = svm.get_account(&fx.sra_config).unwrap().data;
    for signer in [&fx.initiator, &attacker] {
        let err = send_ix_expect_err(
            &mut svm,
            manage_sra_ix(signer.pubkey(), fx.sra_config, attacker.pubkey(), false),
            &fx.payer,
            &[signer],
            "only the owner may manage an SRA",
        );
        assert_eq!(svm.get_account(&fx.sra_config).unwrap().data, before);
        assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
    }
}

#[test]
fn manage_sra_zero_initiator_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra_config).unwrap().data;
    let err = manage_sra(&mut svm, &fx, Pubkey::default(), false).unwrap_err();
    assert_eq!(svm.get_account(&fx.sra_config).unwrap().data, before);
    assert_eq!(err.err, custom_error(SraError::InvalidInitiator));
}

#[test]
fn manage_sra_requires_owner_signature() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra_config).unwrap().data;
    let mut ix = manage_sra_ix(
        fx.owner.pubkey(),
        fx.sra_config,
        fx.initiator.pubkey(),
        false,
    );
    ix.accounts[0].is_signer = false;
    let err = send_ix_expect_err(&mut svm, ix, &fx.payer, &[], "owner must sign management");
    assert_eq!(svm.get_account(&fx.sra_config).unwrap().data, before);
    assert_eq!(err, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn manage_sra_owner_can_reactivate() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), false).unwrap();
    assert!(!fetch_sra_config(&svm, &fx.sra_config).active);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), true).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra_config);
    assert!(state.active);
    assert_eq!(state.initiator, fx.initiator.pubkey());
    assert_eq!(state.deposit_nonce, 0);
}
