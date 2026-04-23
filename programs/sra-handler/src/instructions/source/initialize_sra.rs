use anchor_lang::prelude::*;
use sra_common::{
    seeds::{config_hash, SEED_SRA},
    types::SraConfig,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct SraInitConfig {
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
}

impl SraInitConfig {
    pub fn hash(&self, owner: &Pubkey) -> [u8; 32] {
        config_hash(
            owner,
            self.target_chain_id,
            &self.target_handler,
            &self.target_adapter,
            &self.recipient,
            &self.app_id,
        )
    }
}

#[derive(Accounts)]
#[instruction(config: SraInitConfig, salt: [u8; 32])]
pub struct InitializeSra<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + SraConfig::INIT_SPACE,
        seeds = [
            SEED_SRA,
            owner.key().as_ref(),
            &config.hash(&owner.key()),
            &salt,
        ],
        bump,
    )]
    pub sra_config: Account<'info, SraConfig>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<InitializeSra>,
    config: SraInitConfig,
    salt: [u8; 32],
) -> Result<()> {
    let sra_config = &mut ctx.accounts.sra_config;
    sra_config.owner = ctx.accounts.owner.key();
    sra_config.target_chain_id = config.target_chain_id;
    sra_config.target_handler = config.target_handler;
    sra_config.target_adapter = config.target_adapter;
    sra_config.recipient = config.recipient;
    sra_config.app_id = config.app_id;
    sra_config.salt = salt;
    sra_config.allowed_adapters = Vec::new();
    sra_config.bump = ctx.bumps.sra_config;
    Ok(())
}
