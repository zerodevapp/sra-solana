use anchor_lang::AccountSerialize;
use litesvm::LiteSVM;
use solana_sdk::pubkey::Pubkey;

pub fn set_active_fill(svm: &mut LiteSVM, route: &Pubkey, fill_id: Option<[u8; 32]>) {
    let mut state = super::fetch_route(svm, route);
    state.active_fill = fill_id;
    let mut account = svm.get_account(route).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(*route, account).unwrap();
}
