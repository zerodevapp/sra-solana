use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::{seeds::SEED_SRA, types::SraConfig};

pub(crate) fn transfer_from_sra_vault<'info>(
    sra_config: &Account<'info, SraConfig>,
    sra_vault: &Account<'info, TokenAccount>,
    source_mint: &Account<'info, Mint>,
    to: &Account<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    amount: u64,
) -> Result<()> {
    let config_hash = sra_config
        .handler_data
        .config_hash(&sra_config.owner, &sra_config.final_handler);
    let bump = [sra_config.bump];
    let seeds: &[&[u8]] = &[
        SEED_SRA,
        sra_config.owner.as_ref(),
        config_hash.as_ref(),
        sra_config.salt.as_ref(),
        bump.as_ref(),
    ];
    transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: sra_vault.to_account_info(),
                mint: source_mint.to_account_info(),
                to: to.to_account_info(),
                authority: sra_config.to_account_info(),
            },
            &[seeds],
        ),
        amount,
        source_mint.decimals,
    )
}
