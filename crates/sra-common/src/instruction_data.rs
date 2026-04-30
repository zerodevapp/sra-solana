use anchor_lang::{error, prelude::*, AnchorDeserialize, AnchorSerialize};
use solana_sha256_hasher::hash;

use crate::errors::SraError;

pub const GLOBAL_IX_NAMESPACE: &[u8] = b"global:";

pub fn instruction_discriminator(name: &str) -> [u8; 8] {
    let mut discriminator = [0u8; 8];
    let preimage = [GLOBAL_IX_NAMESPACE, name.as_bytes()].concat();
    discriminator.copy_from_slice(&hash(&preimage).to_bytes()[..8]);
    discriminator
}

pub fn encode_anchor_instruction_data<T: AnchorSerialize>(name: &str, value: &T) -> Vec<u8> {
    let mut data = instruction_discriminator(name).to_vec();
    value.serialize(&mut data).unwrap();
    data
}

pub fn decode_anchor_instruction_data<T: AnchorDeserialize>(name: &str, data: &[u8]) -> Result<T> {
    require!(data.len() >= 8, SraError::InvalidPairedInstruction);
    require!(
        data[..8] == instruction_discriminator(name),
        SraError::InvalidPairedInstruction
    );
    T::try_from_slice(&data[8..]).map_err(|_| error!(SraError::InvalidPairedInstruction))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_deposit::PrepareParams;

    #[test]
    fn decode_rejects_wrong_discriminator() {
        let params = PrepareParams {
            input_amount: 1,
            output_amount: 2,
            target_token: [3u8; 32],
            action_idxs: vec![4],
        };
        let data = encode_anchor_instruction_data("wrong_name", &params);

        let err = decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &data)
            .expect_err("wrong discriminator must reject");
        assert_eq!(err, error!(SraError::InvalidPairedInstruction));
    }
}
