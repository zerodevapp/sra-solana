//! Pins the mock CCTP TokenMessengerMinter against the REAL `deposit_for_burn`
//! interface: the mock's independently-declared params Borsh layout must match
//! the vendored IDL's generated `DepositForBurnParams`, and the mock's Anchor
//! instruction discriminator must equal the real `global:deposit_for_burn`
//! selector. Together these prove the mock faithfully decodes what the
//! adapter's IDL-derived CPI sends, so neither side can drift alone. Account
//! order is proven executably by the source-flow suites and against the real
//! deployed program by the Surfpool E2E.

use anchor_lang::{AnchorSerialize, InstructionData};
use solana_sdk::pubkey::Pubkey;
use sra_cctp_adapter::utils::token_messenger_minter_v2::types::DepositForBurnParams as IdlParams;

#[test]
fn mock_cctp_deposit_for_burn_matches_real_interface() {
    let mint_recipient = Pubkey::new_unique();
    let destination_caller = Pubkey::new_unique();
    let (amount, destination_domain, max_fee, min_finality_threshold) = (
        0x0102_0304_0506_0708u64,
        3u32,
        0x1112_1314_1516_1718u64,
        1000u32,
    );

    let mock_encoding = mock_cctp::instruction::DepositForBurn {
        params: mock_cctp::DepositForBurnParams {
            amount,
            destination_domain,
            mint_recipient,
            destination_caller,
            max_fee,
            min_finality_threshold,
        },
    }
    .data();

    // Expected = the real Anchor `global:deposit_for_burn` discriminator ‖ the
    // Borsh of the vendored IDL's own params type. If the mock's params layout
    // or discriminator ever diverges from Circle's interface, this fails.
    let mut expected =
        sra_common::instruction_data::instruction_discriminator("deposit_for_burn").to_vec();
    IdlParams {
        amount,
        destination_domain,
        mint_recipient,
        destination_caller,
        max_fee,
        min_finality_threshold,
    }
    .serialize(&mut expected)
    .unwrap();

    assert_eq!(mock_encoding, expected);
    // 8 discriminator + 8 amount + 4 domain + 32 recipient + 32 caller +
    // 8 max_fee + 4 threshold. A silently added/widened field changes the len.
    assert_eq!(mock_encoding.len(), 96);
}
