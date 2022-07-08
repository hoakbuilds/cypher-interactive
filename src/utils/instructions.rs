use std::str::FromStr;
use cypher::quote_mint;
use cypher_tester::{dex, get_faucet_request_builder, get_request_builder};
use serum_dex::instruction::MarketInstruction;
use solana_sdk::{pubkey::Pubkey, instruction::{AccountMeta, Instruction}, system_program, rent::Rent, sysvar::SysvarId};

use super::derive_dex_market_authority;

pub fn get_deposit_collateral_ix(
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_pc_vault: &Pubkey,
    source_token_account: &Pubkey,
    signer: &Pubkey,
    amount: u64,
) -> Vec<Instruction> {
    let ixs = get_request_builder()
        .accounts(cypher::accounts::DepositCollateral {
            cypher_group: *cypher_group_pubkey,
            cypher_user: *cypher_user_pubkey,
            cypher_pc_vault: *cypher_pc_vault,
            deposit_from: *source_token_account,
            user_signer: *signer,
            token_program: spl_token::ID,
        })
        .args(cypher::instruction::DepositCollateral { amount })
        .instructions()
        .unwrap();
    ixs
}

pub fn get_init_open_orders_ix(
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_market: &Pubkey,
    open_orders: &Pubkey,
    signer: &Pubkey,
) -> Vec<Instruction> {
    let market_authority = derive_dex_market_authority(cypher_market).0;
    let data = MarketInstruction::InitOpenOrders.pack();
    let accounts: Vec<AccountMeta> = vec![
        AccountMeta::new(*cypher_group_pubkey, false),
        AccountMeta::new(*cypher_user_pubkey, false),
        AccountMeta::new(*signer, true),
        AccountMeta::new_readonly(*cypher_market, false),
        AccountMeta::new_readonly(market_authority, false),
        AccountMeta::new(*open_orders, false),
        AccountMeta::new_readonly(Rent::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(dex::id(), false),
    ];

    vec![Instruction {
        accounts,
        data,
        program_id: cypher::ID,
    }]
}

pub fn get_request_airdrop_ix(token_account: &Pubkey, amount: u64) -> Vec<Instruction> {
    let ixs = get_faucet_request_builder()
        .accounts(test_driver::accounts::FaucetToUser {
            faucet_info: Pubkey::from_str("9euKg1WZtat7iupnqZJPhVFUq1Eg3VJVAdAsv5T88Nf1").unwrap(),
            mint: quote_mint::ID,
            mint_authority: Pubkey::from_str("ALtS7g1kR3T1YkAZFo8SwKP36nhCKVf11Eh4xDsxKY1U")
                .unwrap(),
            target: *token_account,
            token_program: spl_token::ID,
        })
        .args(test_driver::instruction::FaucetToUser { amount })
        .instructions()
        .unwrap();
    ixs
}
