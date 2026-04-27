use anchor_lang::{declare_program, prelude::*};

use crate::utils::seed_hash::DepositNowParams;

declare_program!(svm_spoke);

pub struct DepositNowCpiAccounts<'info> {
    pub across_program: AccountInfo<'info>,
    pub signer: AccountInfo<'info>,
    pub state: AccountInfo<'info>,
    pub delegate: AccountInfo<'info>,
    pub depositor_token_account: AccountInfo<'info>,
    pub vault: AccountInfo<'info>,
    pub mint: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub associated_token_program: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
}

impl<'info> DepositNowCpiAccounts<'info> {
    fn into_cpi_accounts(self) -> svm_spoke::cpi::accounts::DepositNow<'info> {
        svm_spoke::cpi::accounts::DepositNow {
            signer: self.signer,
            state: self.state,
            delegate: self.delegate,
            depositor_token_account: self.depositor_token_account,
            vault: self.vault,
            mint: self.mint,
            token_program: self.token_program,
            associated_token_program: self.associated_token_program,
            system_program: self.system_program,
        }
    }
}

/// CPI helper for Across SVM Spoke `deposit_now(...)` using generated IDL bindings.
pub fn across_deposit_now<'info>(
    accounts: DepositNowCpiAccounts<'info>,
    params: &DepositNowParams,
) -> Result<()> {
    across_deposit_now_with_signer(accounts, params, &[])
}

/// CPI helper variant that allows signer seeds when the adapter needs PDA signing.
pub fn across_deposit_now_with_signer<'a, 'b, 'c, 'info>(
    accounts: DepositNowCpiAccounts<'info>,
    params: &DepositNowParams,
    signer_seeds: &'a [&'b [&'c [u8]]],
) -> Result<()> {
    let cpi_program = accounts.across_program.clone();
    let cpi_accounts = accounts.into_cpi_accounts();
    let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds);

    svm_spoke::cpi::deposit_now(
        cpi_ctx,
        params.depositor,
        params.recipient,
        params.input_token,
        params.output_token,
        params.input_amount,
        params.output_amount,
        params.destination_chain_id,
        params.exclusive_relayer,
        params.fill_deadline_offset,
        params.exclusivity_period,
        params.message.clone(),
    )
}
