use anchor_lang::{error, prelude::*};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_keccak_hasher::hash as keccak_hash;

declare_id!("8WY8qMVBf5T1pTAehwAMVvVZNmy1pi2Te9TRQTRaShfF");

const DELEGATE_SEED_PREFIX: &[u8] = b"delegate";
const DEPOSIT_NOW_FIXED_SERIALIZED_LEN: usize = (32 * 4) + 8 + 32 + 8 + 32 + 4 + 4 + 4;

/// Seed prefix the Across SVM Spoke uses for fill-status PDAs (`["fills", relay_hash]`).
pub const FILL_SEED_PREFIX: &[u8] = b"fills";

#[error_code]
pub enum MockSpokePoolError {
    #[msg("Failed to serialize mock deposit-now seed data")]
    SeedSerializationFailed,

    #[msg("Delegate PDA does not match the deposit-now args")]
    InvalidDelegate,
}

#[derive(AnchorSerialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositNowParams {
    pub depositor: Pubkey,
    pub recipient: Pubkey,
    pub input_token: Pubkey,
    pub output_token: Pubkey,
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub destination_chain_id: u64,
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
    pub message: Vec<u8>,
}

/// Mirror of the deployed Across `FillStatus` enum. Variant order is the
/// on-chain Borsh discriminant, so it must not change.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum FillStatus {
    Unfilled,
    RequestedSlowFill,
    Filled,
}

/// Mirror of the deployed Across `FillStatusAccount`. Field order and types must
/// match the re-vendored IDL so the destination adapter deserializes mock and
/// real fills identically (pinned by `tests/mock_spoke_pool.rs`).
#[account]
#[derive(InitSpace)]
pub struct FillStatusAccount {
    pub status: FillStatus,
    pub relayer: Pubkey,
    pub fill_deadline: u32,
}

fn deposit_now_seed_hash(params: &DepositNowParams) -> Result<[u8; 32]> {
    let mut serialized =
        Vec::with_capacity(DEPOSIT_NOW_FIXED_SERIALIZED_LEN + params.message.len());
    params
        .serialize(&mut serialized)
        .map_err(|_| error!(MockSpokePoolError::SeedSerializationFailed))?;
    Ok(keccak_hash(&serialized).to_bytes())
}

fn delegate_pda(seed_hash: [u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[DELEGATE_SEED_PREFIX, seed_hash.as_ref()], &crate::ID)
}

#[program]
pub mod mock_spoke_pool {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn deposit_now(
        ctx: Context<DepositNow>,
        depositor: Pubkey,
        recipient: Pubkey,
        input_token: Pubkey,
        output_token: Pubkey,
        input_amount: u64,
        output_amount: [u8; 32],
        destination_chain_id: u64,
        exclusive_relayer: Pubkey,
        fill_deadline_offset: u32,
        exclusivity_period: u32,
        message: Vec<u8>,
    ) -> Result<()> {
        let params = DepositNowParams {
            depositor,
            recipient,
            input_token,
            output_token,
            input_amount,
            output_amount,
            destination_chain_id,
            exclusive_relayer,
            fill_deadline_offset,
            exclusivity_period,
            message,
        };
        let seed_hash = deposit_now_seed_hash(&params)?;
        let (delegate, bump) = delegate_pda(seed_hash);
        require_keys_eq!(
            ctx.accounts.delegate.key(),
            delegate,
            MockSpokePoolError::InvalidDelegate
        );

        let delegate_seeds: &[&[u8]] = &[DELEGATE_SEED_PREFIX, seed_hash.as_ref(), &[bump]];
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.depositor_token_account.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.delegate.to_account_info(),
                },
                &[delegate_seeds],
            ),
            input_amount,
            ctx.accounts.mint.decimals,
        )?;
        Ok(())
    }

    /// Relayer fast-fill: move the output tokens to the recipient and record a
    /// `Filled` status at `["fills", relay_hash]`, exactly as the deployed Across
    /// program does.
    pub fn fill_relay(
        ctx: Context<FillRelay>,
        relay_hash: [u8; 32],
        output_amount: u64,
        fill_deadline: u32,
    ) -> Result<()> {
        let _ = relay_hash; // binds the fill_status PDA via the accounts seeds.

        transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.relayer_token_account.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.recipient_token_account.to_account_info(),
                    authority: ctx.accounts.signer.to_account_info(),
                },
            ),
            output_amount,
            ctx.accounts.mint.decimals,
        )?;

        let fill_status = &mut ctx.accounts.fill_status;
        fill_status.status = FillStatus::Filled;
        fill_status.relayer = ctx.accounts.signer.key();
        fill_status.fill_deadline = fill_deadline;
        Ok(())
    }

    /// Test backdoor: plant a fill-status with an arbitrary status (e.g.
    /// `Unfilled`) without moving tokens, so the adapter's reject paths can be
    /// exercised against an Across-owned account.
    pub fn mock_fill(
        ctx: Context<MockFill>,
        relay_hash: [u8; 32],
        status: FillStatus,
    ) -> Result<()> {
        let _ = relay_hash; // binds the fill_status PDA via the accounts seeds.

        let fill_status = &mut ctx.accounts.fill_status;
        fill_status.status = status;
        fill_status.relayer = ctx.accounts.signer.key();
        fill_status.fill_deadline = 0;
        Ok(())
    }

    /// Test backdoor: reclaim a fill-status PDA's rent — the relayer-closure race
    /// the destination adapter must survive. The real program gates this on the
    /// relayer identity and a passed deadline; the mock omits those access
    /// controls because the adapter only reads existence/owner/status and never
    /// closes the account itself.
    pub fn close_fill_pda(_ctx: Context<CloseFillPda>) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct DepositNow<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    /// CHECK: state is unused in the mock; kept for IDL parity.
    #[account(mut)]
    pub state: UncheckedAccount<'info>,
    /// CHECK: validated against the derived delegate PDA.
    pub delegate: UncheckedAccount<'info>,
    #[account(mut)]
    pub depositor_token_account: Box<Account<'info, TokenAccount>>,
    #[account(mut, constraint = vault.mint == mint.key())]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(relay_hash: [u8; 32])]
pub struct FillRelay<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, constraint = relayer_token_account.mint == mint.key())]
    pub relayer_token_account: Box<Account<'info, TokenAccount>>,
    #[account(mut, constraint = recipient_token_account.mint == mint.key())]
    pub recipient_token_account: Box<Account<'info, TokenAccount>>,
    #[account(
        init,
        payer = signer,
        space = 8 + FillStatusAccount::INIT_SPACE,
        seeds = [FILL_SEED_PREFIX, relay_hash.as_ref()],
        bump,
    )]
    pub fill_status: Box<Account<'info, FillStatusAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(relay_hash: [u8; 32])]
pub struct MockFill<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    #[account(
        init,
        payer = signer,
        space = 8 + FillStatusAccount::INIT_SPACE,
        seeds = [FILL_SEED_PREFIX, relay_hash.as_ref()],
        bump,
    )]
    pub fill_status: Box<Account<'info, FillStatusAccount>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseFillPda<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    #[account(mut, close = signer)]
    pub fill_status: Box<Account<'info, FillStatusAccount>>,
}
