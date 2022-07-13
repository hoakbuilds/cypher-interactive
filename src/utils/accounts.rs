use std::sync::Arc;
use anchor_lang::{Owner, ZeroCopy};
use arrayref::array_ref;
use bytemuck::checked::from_bytes;
use cypher::{
    constants::{B_CYPHER_USER, B_OPEN_ORDERS},
    quote_mint, states::{CypherGroup, CypherUser},
};
use cypher_tester::{associated_token, get_request_builder, parse_dex_account};
use serum_dex::state::{OpenOrders, MarketStateV2};
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient};
use solana_sdk::{
    account::Account,
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    system_program,
};
use spl_associated_token_account::instruction::create_associated_token_account;

use crate::{fast_tx_builder::FastTxnBuilder, CypherInteractiveError};

use super::{get_init_open_orders_ix, get_request_airdrop_ix, get_deposit_collateral_ix};

pub fn get_zero_copy_account<T: ZeroCopy + Owner>(solana_account: &Account) -> Box<T> {
    let data = &solana_account.data.as_slice();
    let disc_bytes = array_ref![data, 0, 8];
    assert_eq!(disc_bytes, &T::discriminator());
    Box::new(*from_bytes::<T>(&data[8..std::mem::size_of::<T>() + 8]))
}

pub fn derive_cypher_user_address(group_address: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[B_CYPHER_USER, group_address.as_ref(), &owner.to_bytes()],
        &cypher::ID,
    )
}

pub fn derive_open_orders_address(dex_market_pk: &Pubkey, cypher_user_pk: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            B_OPEN_ORDERS,
            dex_market_pk.as_ref(),
            cypher_user_pk.as_ref(),
        ],
        &cypher::ID,
    ).0
}

pub fn derive_quote_token_address(wallet_address: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            &wallet_address.to_bytes(),
            &spl_token::id().to_bytes(),
            &quote_mint::ID.to_bytes(),
        ],
        &associated_token::ID,
    )
    .0
}

pub async fn get_token_account(
    client: Arc<RpcClient>,
    token_account: &Pubkey,
) -> Result<UiTokenAmount, ClientError> {
    let ta_res = client
        .get_token_account_balance_with_commitment(token_account, CommitmentConfig::confirmed())
        .await;

    let ta = match ta_res {
        Ok(ta) => ta.value,
        Err(e) => {
            return Err(e);
        }
    };

    Ok(ta)
}

pub async fn get_serum_market(
    client: Arc<RpcClient>,
    market: Pubkey,
) -> Result<MarketStateV2, ClientError> {
    let ai_res = client
        .get_account_with_commitment(&market, CommitmentConfig::confirmed())
        .await;

    let ai = match ai_res {
        Ok(ai) => ai.value.unwrap(),
        Err(e) => {
            println!("There was an error while fetching the serum market: {}", e);
            return Err(e);
        }
    };

    let market = parse_dex_account(ai.data);

    Ok(market)
}

pub async fn get_or_init_cypher_user(
    owner: &Keypair,
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    rpc_client: Arc<RpcClient>,
    cluster: String,
) -> Result<Box<CypherUser>, CypherInteractiveError> {
    let account_state = fetch_cypher_user(
        cypher_user_pubkey,
        Arc::clone(&rpc_client)
    ).await;

    if account_state.is_ok() {
        let account = account_state.unwrap();
        Ok(account)
    } else {
        println!("Cypher user account does not existing, creating account.");
        let res = init_cypher_user(
            cypher_group_pubkey,
            owner,
            Arc::clone(&rpc_client)
        ).await;

        match res {
            Ok(s) => {
                println!("Successfully created cypher user account: https://explorer.solana.com/tx/{}?cluster={}", s, cluster);
            },
            Err(e) => {
                return Err(e);
            }
        };

        let cypher_user = fetch_cypher_user(cypher_user_pubkey, Arc::clone(&rpc_client))
            .await
            .unwrap();

        Ok(cypher_user)
    }
}

async fn fetch_cypher_user(
    cypher_user_pubkey: &Pubkey,
    rpc_client: Arc<RpcClient>,
) -> Result<Box<CypherUser>, CypherInteractiveError> {
    let res = rpc_client
        .get_account_with_commitment(cypher_user_pubkey, CommitmentConfig::confirmed())
        .await;

    if res.is_err() {
        return Err(CypherInteractiveError::CouldNotFetchCypherUser(res.err().unwrap()));
    }
    
    let maybe_account = res.unwrap().value;

    if maybe_account.is_some() {
        let account_state = get_zero_copy_account::<CypherUser>(&maybe_account.unwrap());
        Ok(account_state)
    } else {
        Err(CypherInteractiveError::CypherUserNotFound)
    }
}

pub async fn init_cypher_user(
    group_address: &Pubkey,
    owner: &Keypair,
    rpc: Arc<RpcClient>,
) -> Result<Signature, CypherInteractiveError> {
    let (address, bump) = derive_cypher_user_address(group_address, &owner.pubkey());

    let ixs = get_request_builder()
        .accounts(cypher::accounts::InitCypherUser {
            cypher_group: *group_address,
            cypher_user: address,
            owner: owner.pubkey(),
            system_program: system_program::id(),
        })
        .args(cypher::instruction::InitCypherUser { bump })
        .instructions()
        .unwrap();

    let mut builder = FastTxnBuilder::new();
    for ix in ixs {
        builder.add(ix);
    }
    let hash_res = rpc.get_latest_blockhash().await;
    let hash = match hash_res {
        Ok(h) => h,
        Err(e) => {
            return Err(CypherInteractiveError::CouldNotCreateCypherUser(e));
        }
    };
    let tx = builder.build(hash, owner, None);
    let tx_res = rpc.send_and_confirm_transaction_with_spinner(&tx).await;
    let sig = match tx_res {
        Ok(s) => s,
        Err(e) => {
            return Err(CypherInteractiveError::CouldNotCreateCypherUser(e));
        }
    };
    Ok(sig)
}

pub async fn get_or_init_open_orders(
    owner: &Keypair,
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_market: &Pubkey,
    open_orders: &Pubkey,
    rpc_client: Arc<RpcClient>,
    cluster: String
) -> Result<OpenOrders, CypherInteractiveError> {
    let account_state = fetch_open_orders(open_orders, Arc::clone(&rpc_client)).await;

    if account_state.is_ok() {
        let acc = account_state.unwrap();
        println!(
            "Open orders account for market {} with key {} already exists.",
            cypher_market, open_orders
        );
        Ok(acc)
    } else {
        println!("Open orders account does not exist, creating..");

        let res = init_open_orders(
            cypher_group_pubkey,
            cypher_user_pubkey,
            cypher_market,
            open_orders,
            owner,
            Arc::clone(&rpc_client),
        )
        .await;

        match res {
            Ok(s) => {                
                println!("Successfully created open orders account: https://explorer.solana.com/tx/{}?cluster={}", s, cluster);
            },
            Err(e) => {
                return Err(e);
            }
        };
        let open_orders = fetch_open_orders(open_orders, Arc::clone(&rpc_client))
            .await
            .unwrap();
        Ok(open_orders)
    }
}

async fn fetch_open_orders(
    open_orders: &Pubkey,
    rpc_client: Arc<RpcClient>,
) -> Result<OpenOrders, CypherInteractiveError> {
    let res = rpc_client
        .get_account_with_commitment(open_orders, CommitmentConfig::confirmed())
        .await;
        
    if res.is_err() {
        return Err(CypherInteractiveError::CouldNotFetchOpenOrders(res.err().unwrap()));
    }
    
    let maybe_account = res.unwrap().value;

    if maybe_account.is_some() {
        let ooa: OpenOrders = parse_dex_account(maybe_account.unwrap().data);
        Ok(ooa)
    } else {
        Err(CypherInteractiveError::OpenOrdersNotFound)
    }
}

pub async fn init_open_orders(
    cypher_group_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    cypher_market: &Pubkey,
    open_orders: &Pubkey,
    signer: &Keypair,
    rpc_client: Arc<RpcClient>,
) -> Result<Signature, CypherInteractiveError> {
    let ixs = get_init_open_orders_ix(
        cypher_group_pubkey,
        cypher_user_pubkey,
        cypher_market,
        open_orders,
        &signer.pubkey(),
    );

    let mut builder = FastTxnBuilder::new();
    for ix in ixs {
        builder.add(ix);
    }
    let hash = rpc_client.get_latest_blockhash().await.unwrap();
    let tx = builder.build(hash, signer, None);
    let res = rpc_client
        .send_and_confirm_transaction_with_spinner(&tx)
        .await;
    match res {
        Ok(s) => {
            Ok(s)
        }
        Err(e) => {
            Err(CypherInteractiveError::CouldNotCreateOpenOrders(e))
        }
    }
}


pub async fn request_airdrop(
    owner: &Keypair,
    rpc_client: Arc<RpcClient>,
) -> Result<Signature, CypherInteractiveError> {
    let token_account = derive_quote_token_address(owner.pubkey());
    let airdrop_ix = get_request_airdrop_ix(&token_account, 10_000_000_000);

    let mut builder = FastTxnBuilder::new();

    let token_account_res = get_token_account(Arc::clone(&rpc_client), &token_account).await;
    match token_account_res {
        Ok(_) => (),
        Err(_) => {
            println!(
                "Quote token account does not exist, creating account with key: {} for mint {}.",
                token_account,
                quote_mint::ID
            );
            builder.add(create_associated_token_account(
                &owner.pubkey(),
                &owner.pubkey(),
                &quote_mint::ID,
            ));
        }
    }
    for ix in airdrop_ix {
        builder.add(ix);
    }

    let hash = rpc_client.get_latest_blockhash().await.unwrap();
    let tx = builder.build(hash, owner, None);
    let res = rpc_client
        .send_and_confirm_transaction_with_spinner(&tx)
        .await;
    match res {
        Ok(s) => {
            Ok(s)
        }
        Err(e) => {
            println!("There was an error requesting airdrop: {}", e);
            Err(CypherInteractiveError::Airdrop)
        }
    }
}

pub async fn deposit_quote_token(
    owner: &Keypair,
    cypher_user_pubkey: &Pubkey,
    cypher_group: &CypherGroup,
    rpc_client: Arc<RpcClient>,
    amount: u64,
) -> Result<Signature, CypherInteractiveError> {
    let source_ata = derive_quote_token_address(owner.pubkey());

    let ixs = get_deposit_collateral_ix(
        &cypher_group.self_address,
        cypher_user_pubkey,
        &cypher_group.quote_vault(),
        &source_ata,
        &owner.pubkey(),
        amount,
    );
    let mut builder = FastTxnBuilder::new();
    for ix in ixs {
        builder.add(ix);
    }
    let hash = rpc_client.get_latest_blockhash().await.unwrap();
    let tx = builder.build(hash, owner, None);
    let res = rpc_client
        .send_and_confirm_transaction_with_spinner(&tx)
        .await;

    match res {
        Ok(s) => {
            Ok(s)
        }
        Err(e) => {
            println!("There was an error depositing funds into cypher account: {}", e);
            Err(CypherInteractiveError::Deposit)
        }
    }
}
