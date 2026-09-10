mod common;

use anchor_lang::{error::ErrorCode, AccountSerialize, InstructionData, ToAccountMetas};
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_common::errors::SraError;

struct SourceFixture {
    payer: Keypair,
    owner: Keypair,
    initiator: Keypair,
    sra: Pubkey,
    source_mint: Pubkey,
}

impl SourceFixture {
    fn new(svm: &mut LiteSVM) -> Self {
        let payer = funded_keypair(svm, 10 * SOL);
        let owner = funded_keypair(svm, SOL);
        let initiator = funded_keypair(svm, SOL);
        let address_book_root = init_address_book_root(svm, &payer, &owner);
        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        let handler_data = sample_handler_data(address_book_root);
        let salt = [0xB3; 32];
        let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
        let ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root,
            &sra,
            handler_data,
            salt,
            initiator.pubkey(),
            source_mint,
        );
        send_ix(svm, ix, &payer, &[&owner]).unwrap();
        Self {
            payer,
            owner,
            initiator,
            sra,
            source_mint,
        }
    }
}

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
    fx: &SourceFixture,
    initiator: Pubkey,
    active: bool,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    send_ix_with_meta(
        svm,
        manage_sra_ix(fx.owner.pubkey(), fx.sra, initiator, active),
        &fx.payer,
        &[&fx.owner],
    )
}

#[test]
fn manage_sra_replaces_only_runtime_control() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let replacement = Keypair::new();
    let vault = derive_ata(&fx.sra, &fx.source_mint);
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &vault,
        1_000_000,
    );
    let vault_before = svm.get_account(&vault).unwrap();
    let mut state = fetch_sra_config(&svm, &fx.sra);
    state.deposit_nonce = u64::MAX;
    let mut account = svm.get_account(&fx.sra).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(fx.sra, account).unwrap();
    let immutable_before = (
        state.owner,
        state.final_handler,
        state.handler_data.clone(),
        state.salt,
        state.deposit_nonce,
        state.bump,
    );
    let meta = manage_sra(&mut svm, &fx, replacement.pubkey(), false).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra);
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
    assert_eq!(svm.get_account(&vault).unwrap(), vault_before);
    let event = extract_anchor_event::<sra_handler::events::SraManaged>(&meta.logs)
        .expect("SraManaged event must be emitted");
    assert_eq!(event.sra_config, fx.sra);
    assert_eq!(event.owner, fx.owner.pubkey());
    assert_eq!(event.initiator, replacement.pubkey());
    assert!(!event.active);
    assert_eq!(event.deposit_nonce, u64::MAX);
}

#[test]
fn manage_sra_non_owner_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);
    let before = svm.get_account(&fx.sra).unwrap().data;
    for signer in [&fx.initiator, &attacker] {
        let err = send_ix_expect_err(
            &mut svm,
            manage_sra_ix(signer.pubkey(), fx.sra, attacker.pubkey(), false),
            &fx.payer,
            &[signer],
            "only the owner may manage an SRA",
        );
        assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
        assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
    }
}

#[test]
fn manage_sra_zero_initiator_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra).unwrap().data;
    let err = manage_sra(&mut svm, &fx, Pubkey::default(), false).unwrap_err();
    assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
    assert_eq!(err.err, custom_error(SraError::InvalidInitiator));
}

#[test]
fn manage_sra_requires_owner_signature() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra).unwrap().data;
    let mut ix = manage_sra_ix(fx.owner.pubkey(), fx.sra, fx.initiator.pubkey(), false);
    ix.accounts[0].is_signer = false;
    let err = send_ix_expect_err(&mut svm, ix, &fx.payer, &[], "owner must sign management");
    assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
    assert_eq!(err, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn manage_sra_owner_can_reactivate() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), false).unwrap();
    assert!(!fetch_sra_config(&svm, &fx.sra).active);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), true).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra);
    assert!(state.active);
    assert_eq!(state.initiator, fx.initiator.pubkey());
    assert_eq!(state.deposit_nonce, 0);
}
