use std::convert::identity;

use cypher::{
    client::{cancel_order_ix, new_order_v3_ix, settle_funds_ix, ToPubkey},
    utils::{derive_dex_market_authority, gen_dex_vault_signer_key},
    CypherGroup, CypherMarket, CypherToken,
};
use serum_dex::{
    instruction::{CancelOrderInstructionV2, NewOrderInstructionV3},
    matching::Side,
    state::{MarketStateV2, OpenOrders},
};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Keypair, signer::Signer};

use crate::{providers::OrderBook, serum_slab::OrderBookOrder};

pub struct ManagedOrder {
    pub order_id: u128,
    pub client_order_id: u64,
    pub price: u64,
    pub quantity: u64,
    pub side: Side,
}

pub fn get_open_orders(open_orders: &OpenOrders) -> Vec<ManagedOrder> {
    let mut oo: Vec<ManagedOrder> = Vec::new();
    let orders = open_orders.orders;

    for i in 0..orders.len() {
        let order_id = open_orders.orders[i];
        let client_order_id = open_orders.client_order_ids[i];

        if order_id != u128::default() {
            let price = (order_id >> 64) as u64;
            let side = open_orders.slot_side(i as u8).unwrap();

            oo.push(ManagedOrder {
                order_id,
                client_order_id,
                side,
                price,
                quantity: u64::default(),
            });
        }
    }

    oo
}

pub async fn get_open_orders_with_qty(
    open_orders: &OpenOrders,
    orderbook: &OrderBook,
) -> Vec<ManagedOrder> {
    let mut oo: Vec<ManagedOrder> = Vec::new();
    let orders = open_orders.orders;

    for i in 0..orders.len() {
        let order_id = open_orders.orders[i];
        let client_order_id = open_orders.client_order_ids[i];

        if order_id != u128::default() {
            let price = (order_id >> 64) as u64;
            let side = open_orders.slot_side(i as u8).unwrap();
            let ob_order = get_order_book_line(orderbook, client_order_id, side).await;

            if ob_order.is_some() {
                oo.push(ManagedOrder {
                    order_id,
                    client_order_id,
                    side,
                    price,
                    quantity: ob_order.unwrap().quantity,
                });
            }
        }
    }

    oo
}

async fn get_order_book_line(
    orderbook: &OrderBook,
    client_order_id: u64,
    side: Side,
) -> Option<OrderBookOrder> {
    if side == Side::Ask {
        for order in orderbook.asks.read().await.iter() {
            if order.client_order_id == client_order_id {
                return Some(OrderBookOrder {
                    order_id: order.order_id,
                    price: order.price,
                    quantity: order.quantity,
                    client_order_id: order.client_order_id,
                });
            }
        }
    }

    if side == Side::Bid {
        for order in orderbook.bids.read().await.iter() {
            if order.client_order_id == client_order_id {
                return Some(OrderBookOrder {
                    order_id: order.order_id,
                    price: order.price,
                    quantity: order.quantity,
                    client_order_id: order.client_order_id,
                });
            }
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
pub fn get_cancel_order_ix(
    cypher_group: &CypherGroup,
    cypher_market: &CypherMarket,
    cypher_token: &CypherToken,
    dex_market_state: &MarketStateV2,
    open_orders_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    signer: &Keypair,
    ix_data: CancelOrderInstructionV2,
) -> Instruction {
    let prune_authority = derive_dex_market_authority(&cypher_market.dex_market);
    let vault_signer = gen_dex_vault_signer_key(
        dex_market_state.vault_signer_nonce,
        &cypher_market.dex_market,
    );
    cancel_order_ix(
        &cypher_group.self_address,
        &cypher_group.vault_signer,
        cypher_user_pubkey,
        &signer.pubkey(),
        &cypher_token.mint,
        &cypher_token.vault,
        &cypher_group.quote_vault(),
        &cypher_market.dex_market,
        &prune_authority,
        open_orders_pubkey,
        &identity(dex_market_state.event_q).to_pubkey(),
        &identity(dex_market_state.bids).to_pubkey(),
        &identity(dex_market_state.asks).to_pubkey(),
        &identity(dex_market_state.coin_vault).to_pubkey(),
        &identity(dex_market_state.pc_vault).to_pubkey(),
        &vault_signer,
        ix_data,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn get_new_order_ix(
    cypher_group: &CypherGroup,
    cypher_market: &CypherMarket,
    cypher_token: &CypherToken,
    dex_market_state: &MarketStateV2,
    open_orders_pubkey: &Pubkey,
    cypher_user_pubkey: &Pubkey,
    signer: &Keypair,
    ix_data: NewOrderInstructionV3,
) -> Instruction {
    let vault_signer = gen_dex_vault_signer_key(
        dex_market_state.vault_signer_nonce,
        &cypher_market.dex_market,
    );
    new_order_v3_ix(
        &cypher_group.self_address,
        &cypher_group.vault_signer,
        &cypher_market.price_history,
        cypher_user_pubkey,
        &signer.pubkey(),
        &cypher_token.mint,
        &cypher_token.vault,
        &cypher_group.quote_vault(),
        &cypher_market.dex_market,
        open_orders_pubkey,
        &identity(dex_market_state.req_q).to_pubkey(),
        &identity(dex_market_state.event_q).to_pubkey(),
        &identity(dex_market_state.bids).to_pubkey(),
        &identity(dex_market_state.asks).to_pubkey(),
        &identity(dex_market_state.coin_vault).to_pubkey(),
        &identity(dex_market_state.pc_vault).to_pubkey(),
        &vault_signer,
        ix_data,
    )
}

pub fn get_settle_funds_ix(
    cypher_group: &CypherGroup,
    cypher_market: &CypherMarket,
    cypher_token: &CypherToken,
    dex_market_state: &MarketStateV2,
    cypher_user_pubkey: &Pubkey,
    open_orders_pubkey: &Pubkey,
    signer: &Keypair,
) -> Instruction {
    let vault_signer = gen_dex_vault_signer_key(
        dex_market_state.vault_signer_nonce,
        &cypher_market.dex_market,
    );
    settle_funds_ix(
        &cypher_group.self_address,
        &cypher_group.vault_signer,
        cypher_user_pubkey,
        &signer.pubkey(),
        &cypher_token.mint,
        &cypher_token.vault,
        &cypher_group.quote_vault(),
        &cypher_market.dex_market,
        open_orders_pubkey,
        &identity(dex_market_state.coin_vault).to_pubkey(),
        &identity(dex_market_state.pc_vault).to_pubkey(),
        &vault_signer,
    )
}
