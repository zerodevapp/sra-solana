use anchor_lang::prelude::*;
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};
use sra_common::errors::SraError;

fn is_program_owner(owner: &Pubkey) -> bool {
    bpf_loader::check_id(owner)
        || bpf_loader_deprecated::check_id(owner)
        || bpf_loader_upgradeable::check_id(owner)
        || loader_v4::check_id(owner)
}

/// A trusted adapter must be a deployed, loader-owned, executable program
/// account. Rejecting a mistyped pubkey here (before it can become custody owner
/// or a trusted fill attester) is cheaper than discovering it at fill time, so
/// both the source deposit path and destination route admin run this check.
pub(crate) fn validate_adapter_program(adapter_program: &AccountInfo) -> Result<()> {
    require!(
        !adapter_program.data_is_empty() && adapter_program.executable,
        SraError::InvalidAdapterProgram
    );
    require!(
        is_program_owner(adapter_program.owner),
        SraError::InvalidAdapterProgram
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAccount {
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Vec<u8>,
        executable: bool,
    }

    impl TestAccount {
        fn account_info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(
                &self.key,
                false,
                self.executable,
                &mut self.lamports,
                &mut self.data,
                &self.owner,
                self.executable,
                0,
            )
        }
    }

    #[test]
    fn validate_adapter_program_accepts_supported_loader_owners() {
        let cases = [
            ("legacy", bpf_loader::ID),
            ("upgradeable", bpf_loader_upgradeable::ID),
            ("loader_v4", loader_v4::ID),
        ];

        for (name, owner) in cases {
            let mut program = TestAccount {
                key: Pubkey::new_unique(),
                owner,
                lamports: 0,
                data: vec![1],
                executable: true,
            };

            validate_adapter_program(&program.account_info())
                .unwrap_or_else(|_| panic!("{name} loader-owned program should be accepted"));
        }
    }

    #[test]
    fn validate_adapter_program_rejects_non_executable_and_non_loader_owned() {
        // Non-executable (e.g. a data account mistyped as a program).
        let mut data_acct = TestAccount {
            key: Pubkey::new_unique(),
            owner: bpf_loader_upgradeable::ID,
            lamports: 0,
            data: vec![1],
            executable: false,
        };
        assert!(validate_adapter_program(&data_acct.account_info()).is_err());

        // Executable but owned by a non-loader program.
        let mut wrong_owner = TestAccount {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 0,
            data: vec![1],
            executable: true,
        };
        assert!(validate_adapter_program(&wrong_owner.account_info()).is_err());
    }
}
