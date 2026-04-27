use anchor_lang::prelude::*;

pub mod utils;

declare_id!("8n167Xs4Xx7zEq5fuyCDVgYd3vUjFmHAkGTdK2SCpmXD");

#[program]
pub mod sra_across_adapter {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        msg!("Greetings from: {:?}", ctx.program_id);
        Ok(())
    }

    /// Step-13 stub. The handler's `verify_adapter_ix` rejects any IX 2 that
    /// is not `execute_deposit_across` for the registered adapter, so once
    /// the wiring lands, even non-Across tests need a real instruction here
    /// to complete a 2-IX transaction. Step 15 replaces this body with the
    /// full Across CPI flow (seed-hash derivation, delegate PDA, custody
    /// approve, `deposit_now`).
    ///
    /// `AcrossDepositParams` keeps the routing-fields prefix
    /// (`input_amount`, `output_amount`, `target_token`, `action_idxs`) at
    /// the byte offsets pinned in `sra_common::adapter_ix_layout`. Step 15
    /// must append any Across-specific fields *after* this prefix so the
    /// handler's bidirectional attestation continues to work.
    pub fn execute_deposit_across(
        _ctx: Context<ExecuteDepositAcrossStub>,
        _params: AcrossDepositParams,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}

/// Stub `Accounts` shape for `execute_deposit_across`. Slot 1 holds
/// `sra_config` to match `sra_common::adapter_ix_layout::ACCT_SRA_CONFIG`;
/// the rest are stand-in slots that Step 15 will replace with the full
/// adapter account set (`mint_nonce`, `source_mint`, `adapter_authority`,
/// `adapter_custody`, Across CPI accounts, `instructions_sysvar`).
#[derive(Accounts)]
pub struct ExecuteDepositAcrossStub<'info> {
    /// CHECK: Step 15 replaces this with `fee_payer: Signer<'info>`.
    pub fee_payer: UncheckedAccount<'info>,
    /// CHECK: Step 15 replaces this with `Account<'info, SraConfig>` and
    /// pins it via `verify_handler_ix`.
    pub sra_config: UncheckedAccount<'info>,
}

/// Step-13 routing-fields prefix. Borsh layout matches
/// `sra_handler::source::PrepareParams` exactly so handler-side
/// `verify_adapter_ix` can byte-compare the data slice against IX 1's
/// equivalent fields. Step 15 will extend this struct with Across-specific
/// fields appended after `action_idxs`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct AcrossDepositParams {
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
}
