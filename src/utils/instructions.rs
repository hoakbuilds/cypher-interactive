use cypher::{
    client::{deposit_collateral_ix, init_open_orders_ix},
    utils::derive_dex_market_authority,
};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};

pub fn get_deposit_collateral_ix(
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_pc_vault: &Pubkey,
    source_token_account: &Pubkey,
    signer: &Pubkey,
    amount: u64,
) -> Instruction {
    deposit_collateral_ix(
        cypher_group_pubkey,
        cypher_user_pubkey,
        cypher_pc_vault,
        signer,
        source_token_account,
        amount,
    )
}

pub fn get_init_open_orders_ix(
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_market: &Pubkey,
    open_orders: &Pubkey,
    signer: &Pubkey,
) -> Instruction {
    let market_authority = derive_dex_market_authority(cypher_market);
    init_open_orders_ix(
        cypher_group_pubkey,
        cypher_user_pubkey,
        signer,
        cypher_market,
        open_orders,
        &market_authority,
    )
}
