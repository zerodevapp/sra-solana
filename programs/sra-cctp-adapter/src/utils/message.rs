//! Zero-copy readers over raw CCTP v2 message bytes.
//!
//! `verify_cctp_mint` receives the full message as instruction data and binds
//! it to Circle's attestation, so these readers are the layout authority for
//! the destination leg. Offsets are verified against
//! `circlefin/solana-cctp-contracts` (`message.rs` / `burn_message.rs`) and
//! pinned by the golden-vector tests below. Integers are big-endian (EVM
//! parity); the u64 money fields ride in 32-byte slots whose top 24 bytes
//! must be zero.

use anchor_lang::prelude::*;

use crate::CctpAdapterError;

/// CCTP v2 message header length; the body starts here.
pub const HEADER_LEN: usize = 148;
/// BurnMessage v2 body length with empty `hook_data`.
pub const BURN_BODY_LEN: usize = 228;
/// The BurnMessage body version this adapter accepts. Pinned from the live
/// mainnet TokenMessengerMinter v2 state (`message_body_version`, read
/// 2026-07-28); the Surfpool E2E re-reads it from the fork.
pub const BURN_MESSAGE_BODY_VERSION: u32 = 1;

fn u32_be(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

fn bytes32(data: &[u8], at: usize) -> [u8; 32] {
    data[at..at + 32].try_into().unwrap()
}

/// A u64 carried in an EVM-width 32-byte slot: the top 24 bytes must be zero
/// or the value would silently truncate.
fn u64_slot(data: &[u8], at: usize) -> Result<u64> {
    require!(
        data[at..at + 24].iter().all(|b| *b == 0),
        CctpAdapterError::MalformedMessage
    );
    Ok(u64::from_be_bytes(
        data[at + 24..at + 32].try_into().unwrap(),
    ))
}

/// CCTP v2 message header (`Message` in Circle's message-transmitter-v2).
#[derive(Debug)]
pub struct CctpMessage<'a>(&'a [u8]);

impl<'a> CctpMessage<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        require!(data.len() >= HEADER_LEN, CctpAdapterError::MalformedMessage);
        Ok(Self(data))
    }

    pub fn version(&self) -> u32 {
        u32_be(self.0, 0)
    }

    pub fn source_domain(&self) -> u32 {
        u32_be(self.0, 4)
    }

    pub fn destination_domain(&self) -> u32 {
        u32_be(self.0, 8)
    }

    /// Iris-assigned at attestation time; all-zero at burn time.
    pub fn nonce(&self) -> [u8; 32] {
        bytes32(self.0, 12)
    }

    /// The source domain's TokenMessenger (an EVM address left-padded to 32).
    pub fn sender(&self) -> [u8; 32] {
        bytes32(self.0, 44)
    }

    /// The destination-domain receiver — a burn message's is the
    /// TokenMessengerMinter program.
    pub fn recipient(&self) -> Pubkey {
        Pubkey::new_from_array(bytes32(self.0, 76))
    }

    /// `Pubkey::default()` means permissionless `receive_message`.
    pub fn destination_caller(&self) -> Pubkey {
        Pubkey::new_from_array(bytes32(self.0, 108))
    }

    pub fn min_finality_threshold(&self) -> u32 {
        u32_be(self.0, 140)
    }

    pub fn finality_threshold_executed(&self) -> u32 {
        u32_be(self.0, 144)
    }

    pub fn body(&self) -> &'a [u8] {
        &self.0[HEADER_LEN..]
    }
}

/// CCTP v2 BurnMessage body (`BurnMessage` in Circle's
/// token-messenger-minter-v2).
#[derive(Debug)]
pub struct BurnMessage<'a>(&'a [u8]);

impl<'a> BurnMessage<'a> {
    pub fn parse(body: &'a [u8]) -> Result<Self> {
        require!(
            body.len() >= BURN_BODY_LEN,
            CctpAdapterError::MalformedMessage
        );
        Ok(Self(body))
    }

    pub fn version(&self) -> u32 {
        u32_be(self.0, 0)
    }

    /// The burned source token (an EVM address left-padded to 32).
    pub fn burn_token(&self) -> [u8; 32] {
        bytes32(self.0, 4)
    }

    /// On Solana as destination this is the USDC token account minted to.
    pub fn mint_recipient(&self) -> Pubkey {
        Pubkey::new_from_array(bytes32(self.0, 36))
    }

    pub fn amount(&self) -> Result<u64> {
        u64_slot(self.0, 68)
    }

    /// The source-side depositor (an EVM address left-padded to 32).
    pub fn message_sender(&self) -> [u8; 32] {
        bytes32(self.0, 100)
    }

    pub fn max_fee(&self) -> Result<u64> {
        u64_slot(self.0, 132)
    }

    /// Iris-set; the delivered amount is `amount − fee_executed`.
    pub fn fee_executed(&self) -> Result<u64> {
        u64_slot(self.0, 164)
    }

    pub fn expiration_block(&self) -> Result<u64> {
        u64_slot(self.0, 196)
    }

    /// Accepted and ignored by verification — it carries no funds.
    pub fn hook_data(&self) -> &'a [u8] {
        &self.0[BURN_BODY_LEN..]
    }
}

/// Test-only encoder mirroring the readers byte-for-byte, so unit and LiteSVM
/// tests build authentic burn messages. The golden-vector test below pins the
/// encoder and the readers to each other and to the verified offsets.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone)]
pub struct TestBurnMessage {
    pub version: u32,
    pub source_domain: u32,
    pub destination_domain: u32,
    pub nonce: [u8; 32],
    pub sender: [u8; 32],
    pub recipient: Pubkey,
    pub destination_caller: Pubkey,
    pub min_finality_threshold: u32,
    pub finality_threshold_executed: u32,
    pub body_version: u32,
    pub burn_token: [u8; 32],
    pub mint_recipient: Pubkey,
    pub message_sender: [u8; 32],
    pub amount: u64,
    pub max_fee: u64,
    pub fee_executed: u64,
    pub expiration_block: u64,
    pub hook_data: Vec<u8>,
}

#[cfg(any(test, feature = "test-utils"))]
impl TestBurnMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN + BURN_BODY_LEN + self.hook_data.len()];
        out[0..4].copy_from_slice(&self.version.to_be_bytes());
        out[4..8].copy_from_slice(&self.source_domain.to_be_bytes());
        out[8..12].copy_from_slice(&self.destination_domain.to_be_bytes());
        out[12..44].copy_from_slice(&self.nonce);
        out[44..76].copy_from_slice(&self.sender);
        out[76..108].copy_from_slice(&self.recipient.to_bytes());
        out[108..140].copy_from_slice(&self.destination_caller.to_bytes());
        out[140..144].copy_from_slice(&self.min_finality_threshold.to_be_bytes());
        out[144..148].copy_from_slice(&self.finality_threshold_executed.to_be_bytes());

        let body = &mut out[HEADER_LEN..];
        body[0..4].copy_from_slice(&self.body_version.to_be_bytes());
        body[4..36].copy_from_slice(&self.burn_token);
        body[36..68].copy_from_slice(&self.mint_recipient.to_bytes());
        body[92..100].copy_from_slice(&self.amount.to_be_bytes());
        body[100..132].copy_from_slice(&self.message_sender);
        body[156..164].copy_from_slice(&self.max_fee.to_be_bytes());
        body[188..196].copy_from_slice(&self.fee_executed.to_be_bytes());
        body[220..228].copy_from_slice(&self.expiration_block.to_be_bytes());
        body[228..].copy_from_slice(&self.hook_data);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::keccak;

    /// Every field distinct and asymmetric, so any offset slip shows up.
    fn canonical() -> TestBurnMessage {
        TestBurnMessage {
            version: 1,
            source_domain: 3,
            destination_domain: 5,
            nonce: [0x11; 32],
            sender: [0x22; 32],
            recipient: Pubkey::new_from_array([0x33; 32]),
            destination_caller: Pubkey::new_from_array([0x44; 32]),
            min_finality_threshold: 1000,
            finality_threshold_executed: 2000,
            body_version: 1,
            burn_token: [0x55; 32],
            mint_recipient: Pubkey::new_from_array([0x66; 32]),
            message_sender: [0x77; 32],
            amount: 4_000_000,
            max_fee: 4_000,
            fee_executed: 17,
            expiration_block: 123_456_789,
            hook_data: vec![],
        }
    }

    /// Golden vector: the canonical message's keccak hash — the exact value an
    /// attestation signs over. Any change to the encoder or the layout moves
    /// this hash; re-pin only against Circle's reference implementation.
    #[test]
    fn canonical_message_keccak_pinned() {
        let bytes = canonical().encode();
        assert_eq!(bytes.len(), HEADER_LEN + BURN_BODY_LEN);
        assert_eq!(
            hex::encode(keccak::hash(&bytes).0),
            "07a15016e91673319284b403e7eba0e69ac3ea4f94086c215ee634767cc814a3",
        );
    }

    /// Every reader returns exactly what the encoder wrote at its offset.
    #[test]
    fn readers_round_trip_every_field() {
        let m = canonical();
        let bytes = m.encode();
        let msg = CctpMessage::parse(&bytes).unwrap();
        assert_eq!(msg.version(), m.version);
        assert_eq!(msg.source_domain(), m.source_domain);
        assert_eq!(msg.destination_domain(), m.destination_domain);
        assert_eq!(msg.nonce(), m.nonce);
        assert_eq!(msg.sender(), m.sender);
        assert_eq!(msg.recipient(), m.recipient);
        assert_eq!(msg.destination_caller(), m.destination_caller);
        assert_eq!(msg.min_finality_threshold(), m.min_finality_threshold);
        assert_eq!(
            msg.finality_threshold_executed(),
            m.finality_threshold_executed
        );

        let body = BurnMessage::parse(msg.body()).unwrap();
        assert_eq!(body.version(), m.body_version);
        assert_eq!(body.burn_token(), m.burn_token);
        assert_eq!(body.mint_recipient(), m.mint_recipient);
        assert_eq!(body.amount().unwrap(), m.amount);
        assert_eq!(body.message_sender(), m.message_sender);
        assert_eq!(body.max_fee().unwrap(), m.max_fee);
        assert_eq!(body.fee_executed().unwrap(), m.fee_executed);
        assert_eq!(body.expiration_block().unwrap(), m.expiration_block);
        assert!(body.hook_data().is_empty());
    }

    /// Mutating any single field changes the parse result of exactly that
    /// field — offsets neither overlap nor miss.
    #[test]
    fn every_field_mutation_changes_its_parse() {
        let baseline = canonical().encode();
        #[allow(clippy::type_complexity)]
        let cases: [(&str, fn(&mut TestBurnMessage)); 17] = [
            ("version", |m| m.version += 1),
            ("source_domain", |m| m.source_domain += 1),
            ("destination_domain", |m| m.destination_domain += 1),
            ("nonce", |m| m.nonce = [0x99; 32]),
            ("sender", |m| m.sender = [0x99; 32]),
            ("recipient", |m| {
                m.recipient = Pubkey::new_from_array([0x99; 32])
            }),
            ("destination_caller", |m| {
                m.destination_caller = Pubkey::new_from_array([0x99; 32])
            }),
            ("min_finality_threshold", |m| m.min_finality_threshold += 1),
            ("finality_threshold_executed", |m| {
                m.finality_threshold_executed += 1
            }),
            ("body_version", |m| m.body_version += 1),
            ("burn_token", |m| m.burn_token = [0x99; 32]),
            ("mint_recipient", |m| {
                m.mint_recipient = Pubkey::new_from_array([0x99; 32])
            }),
            ("message_sender", |m| m.message_sender = [0x99; 32]),
            ("amount", |m| m.amount += 1),
            ("max_fee", |m| m.max_fee += 1),
            ("fee_executed", |m| m.fee_executed += 1),
            ("expiration_block", |m| m.expiration_block += 1),
        ];
        for (name, mutate) in cases {
            let mut mutated = canonical();
            mutate(&mut mutated);
            let bytes = mutated.encode();
            assert_eq!(bytes.len(), baseline.len(), "{name}: length must not move");
            // Exactly one field differs from the canonical parse.
            let differing = baseline
                .iter()
                .zip(bytes.iter())
                .filter(|(a, b)| a != b)
                .count();
            assert!(differing > 0, "{name}: mutation must change the bytes");
            let msg = CctpMessage::parse(&bytes).unwrap();
            let body = BurnMessage::parse(msg.body()).unwrap();
            let m = canonical();
            let changed = [
                msg.version() != m.version,
                msg.source_domain() != m.source_domain,
                msg.destination_domain() != m.destination_domain,
                msg.nonce() != m.nonce,
                msg.sender() != m.sender,
                msg.recipient() != m.recipient,
                msg.destination_caller() != m.destination_caller,
                msg.min_finality_threshold() != m.min_finality_threshold,
                msg.finality_threshold_executed() != m.finality_threshold_executed,
                body.version() != m.body_version,
                body.burn_token() != m.burn_token,
                body.mint_recipient() != m.mint_recipient,
                body.message_sender() != m.message_sender,
                body.amount().unwrap() != m.amount,
                body.max_fee().unwrap() != m.max_fee,
                body.fee_executed().unwrap() != m.fee_executed,
                body.expiration_block().unwrap() != m.expiration_block,
            ];
            assert_eq!(
                changed.iter().filter(|c| **c).count(),
                1,
                "{name}: exactly one parsed field must change"
            );
        }
    }

    /// A non-zero byte anywhere in a u64 slot's top 24 bytes is rejected —
    /// the value would otherwise silently truncate.
    #[test]
    fn nonzero_slot_padding_rejected_for_every_u64_slot() {
        // (slot offset within the body, reader)
        #[allow(clippy::type_complexity)]
        let slots: [(usize, fn(&BurnMessage) -> Result<u64>); 4] = [
            (68, |b| b.amount()),
            (132, |b| b.max_fee()),
            (164, |b| b.fee_executed()),
            (196, |b| b.expiration_block()),
        ];
        for (offset, read) in slots {
            for padding_byte in [0, 23] {
                let mut bytes = canonical().encode();
                bytes[HEADER_LEN + offset + padding_byte] = 0x01;
                let msg = CctpMessage::parse(&bytes).unwrap();
                let body = BurnMessage::parse(msg.body()).unwrap();
                assert_eq!(
                    read(&body).unwrap_err(),
                    error!(CctpAdapterError::MalformedMessage),
                    "slot at body offset {offset}, padding byte {padding_byte}"
                );
            }
        }
    }

    /// Truncation at every boundary: below the header floor the header parse
    /// fails; below the body floor the body parse fails; at the exact floors
    /// both succeed.
    #[test]
    fn truncation_at_every_boundary_rejected() {
        let full = canonical().encode();

        for len in [0, 1, HEADER_LEN - 1] {
            assert_eq!(
                CctpMessage::parse(&full[..len]).unwrap_err(),
                error!(CctpAdapterError::MalformedMessage),
                "header truncated to {len}"
            );
        }
        for len in [HEADER_LEN, HEADER_LEN + 1, HEADER_LEN + BURN_BODY_LEN - 1] {
            let msg = CctpMessage::parse(&full[..len]).unwrap();
            assert_eq!(
                BurnMessage::parse(msg.body()).unwrap_err(),
                error!(CctpAdapterError::MalformedMessage),
                "body truncated to {len}"
            );
        }
        let msg = CctpMessage::parse(&full).unwrap();
        assert!(BurnMessage::parse(msg.body()).is_ok(), "exact floors parse");
    }

    /// `hook_data` past the fixed body is exposed but never required.
    #[test]
    fn hook_data_is_carried_through() {
        let mut m = canonical();
        m.hook_data = vec![0xAB, 0xCD, 0xEF];
        let bytes = m.encode();
        let msg = CctpMessage::parse(&bytes).unwrap();
        let body = BurnMessage::parse(msg.body()).unwrap();
        assert_eq!(body.hook_data(), &[0xAB, 0xCD, 0xEF]);
        assert_eq!(body.amount().unwrap(), m.amount, "fixed fields unaffected");
    }
}
