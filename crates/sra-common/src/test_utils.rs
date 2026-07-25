use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::Instruction,
        sysvar::{
            self,
            instructions::{
                construct_instructions_data, store_current_index_checked, BorrowedAccountMeta,
                BorrowedInstruction, ID as INSTRUCTIONS_ID,
            },
        },
    },
};

use crate::source_deposit::{AcrossDepositParams, CctpDepositParams, PrepareParams};

fn evm_bytes32(byte: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].fill(byte);
    out
}

pub struct InstructionsSysvarFixture {
    key: Pubkey,
    owner: Pubkey,
    lamports: u64,
    data: Vec<u8>,
}

impl InstructionsSysvarFixture {
    pub fn new(current_index: u16, ixs: &[Instruction]) -> Self {
        let borrowed = ixs.iter().map(borrow_instruction).collect::<Vec<_>>();
        let mut data = construct_instructions_data(&borrowed);
        store_current_index_checked(&mut data, current_index).unwrap();
        Self {
            key: INSTRUCTIONS_ID,
            owner: sysvar::ID,
            lamports: 0,
            data,
        }
    }

    pub fn account_info(&mut self) -> AccountInfo<'_> {
        AccountInfo::new(
            &self.key,
            false,
            false,
            &mut self.lamports,
            &mut self.data,
            &self.owner,
            false,
            0,
        )
    }
}

pub fn empty_instruction() -> Instruction {
    Instruction {
        program_id: Pubkey::new_unique(),
        accounts: vec![],
        data: vec![],
    }
}

pub fn sample_prepare_params() -> PrepareParams {
    PrepareParams::new(
        123,
        crate::abi::u256_from_u64(456),
        42_161,
        evm_bytes32(0x33),
        evm_bytes32(0xAB),
        0,
        false,
        crate::constants::NO_FEE_BPS_OVERRIDE,
    )
}

pub fn sample_across_deposit(prepare: &PrepareParams) -> AcrossDepositParams {
    AcrossDepositParams::from_prepare(prepare, Pubkey::new_unique(), 600, 30)
        .expect("sample prepare params must produce a valid net amount")
}

pub fn sample_cctp_deposit(prepare: &PrepareParams) -> CctpDepositParams {
    CctpDepositParams::from_prepare(prepare, 2, crate::constants::FINALITY_THRESHOLD_FAST)
        .expect("sample prepare params must produce a valid net amount")
}

fn borrow_instruction(ix: &Instruction) -> BorrowedInstruction<'_> {
    BorrowedInstruction {
        program_id: &ix.program_id,
        accounts: ix
            .accounts
            .iter()
            .map(|meta| BorrowedAccountMeta {
                pubkey: &meta.pubkey,
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            })
            .collect(),
        data: &ix.data,
    }
}
