use anchor_lang::prelude::*;

// Account::try_from cannot represent a deliberately empty optional PDA.
pub(crate) fn read_optional_program_account<T: AccountDeserialize + Owner>(
    info: AccountInfo,
) -> Result<Option<T>> {
    if info.data_is_empty() {
        return Ok(None);
    }
    require_keys_eq!(
        *info.owner,
        T::owner(),
        ErrorCode::AccountOwnedByWrongProgram
    );
    let data = info.try_borrow_data()?;
    T::try_deserialize(&mut data.as_ref()).map(Some)
}
