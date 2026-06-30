use anchor_lang::prelude::*;
use sra_common::{constants::MAX_EXECUTORS, errors::SraError};

/// Validate `allowed_executors`: bounded by `MAX_EXECUTORS`, non-zero, distinct.
/// Empty is permitted (freezes `execute_actions`).
pub(crate) fn validate_executors(executors: &[Pubkey]) -> Result<()> {
    require!(executors.len() <= MAX_EXECUTORS, SraError::TooManyExecutors);
    for (i, executor) in executors.iter().enumerate() {
        require_keys_neq!(*executor, Pubkey::default(), SraError::InvalidExecutor);
        require!(
            !executors[i + 1..].contains(executor),
            SraError::DuplicateExecutor
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_empty() {
        validate_executors(&[]).unwrap();
    }

    #[test]
    fn accepts_distinct_non_zero() {
        validate_executors(&[Pubkey::new_unique(), Pubkey::new_unique()]).unwrap();
    }

    #[test]
    fn rejects_over_bound() {
        let set: Vec<Pubkey> = (0..=MAX_EXECUTORS).map(|_| Pubkey::new_unique()).collect();
        assert!(validate_executors(&set).is_err());
    }

    #[test]
    fn accepts_at_bound() {
        let set: Vec<Pubkey> = (0..MAX_EXECUTORS).map(|_| Pubkey::new_unique()).collect();
        validate_executors(&set).unwrap();
    }

    #[test]
    fn rejects_zero() {
        assert!(validate_executors(&[Pubkey::default()]).is_err());
        assert!(validate_executors(&[Pubkey::new_unique(), Pubkey::default()]).is_err());
    }

    #[test]
    fn rejects_duplicate() {
        let dup = Pubkey::new_unique();
        assert!(validate_executors(&[dup, dup]).is_err());
    }
}
