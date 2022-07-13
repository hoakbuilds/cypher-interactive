use std::{sync::Arc, num::NonZeroU64};
use cypher::states::{CypherGroup, CypherUser};
use serum_dex::{matching::{Side, OrderType}, state::{MarketStateV2, OpenOrders}, instruction::{NewOrderInstructionV3, SelfTradeBehavior}};
use solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient};
use solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Keypair};
use tokio::{sync::{broadcast::Receiver, Mutex, RwLock}, select};

use crate::{CypherInteractiveError, providers::OrderBook, utils::get_new_order_ix};

pub struct HandlerContext {
    pub user: Box<CypherUser>,
    pub group: Box<CypherGroup>,
    pub hash: Box<Hash>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct CancelOrderInfo {
    pub symbol: String,
    pub order_id: u128,
}

#[derive(Debug, PartialEq, Clone)]
pub struct LimitOrderInfo {
    pub symbol: String,
    pub price: u64,
    pub amount: u64,
    pub side: Side,
}

#[derive(Debug, PartialEq, Clone)]
pub struct MarketOrderInfo{
    pub symbol: String,
    pub amount: u64,
    pub side: Side,
}

pub struct MarketContext {
    pub name: String,
    pub market_index: usize,
    pub signer: Arc<Keypair>,
    pub cypher_user_pk: Pubkey,
    pub dex_market_pk: Pubkey,
    pub open_orders_pk: Pubkey,
}

pub struct Handler {
    pub market_context: Box<MarketContext>,
    rpc_client: Arc<RpcClient>,
    shutdown_receiver: Mutex<Receiver<bool>>,
    open_orders_provider: Mutex<Receiver<OpenOrders>>,
    orderbook_provider: Mutex<Receiver<Arc<OrderBook>>>,
    dex_market: Option<MarketStateV2>,
    open_orders: RwLock<Option<OpenOrders>>,
    orderbook: RwLock<Arc<OrderBook>>,
}

impl Handler {
    pub fn new(
        market_context: Box<MarketContext>,
        rpc_client: Arc<RpcClient>,
        shutdown_receiver: Receiver<bool>,
        open_orders_provider: Receiver<OpenOrders>,
        orderbook_provider: Receiver<Arc<OrderBook>>,
        dex_market: Option<MarketStateV2>
    ) -> Self {
        Self {
            market_context,
            rpc_client,
            shutdown_receiver: Mutex::new(shutdown_receiver),
            open_orders_provider: Mutex::new(open_orders_provider),
            orderbook_provider: Mutex::new(orderbook_provider),
            dex_market,
            open_orders: RwLock::new(None),
            orderbook: RwLock::new(Arc::new(OrderBook::default()))
        }
    }

    pub async fn start(
        self: &Arc<Self>
    ) {
        let mut oo_receiver = self.open_orders_provider.lock().await;
        let mut ob_receiver = self.orderbook_provider.lock().await;
        let mut shutdown = self.shutdown_receiver.lock().await;
        let mut shutdown_signal: bool = false;

        loop {
            select! {
                oo = oo_receiver.recv() => {    
                    if oo.is_ok() {
                        *self.open_orders.write().await = Some(oo.unwrap());
                    }                
                },
                ob = ob_receiver.recv() => {
                    if ob.is_ok() {
                        *self.orderbook.write().await = ob.unwrap();
                    }
                },
                _ = shutdown.recv() => {
                    println!(" Received shutdown signal, stopping.");
                    shutdown_signal = true;
                }
            }
            
            if shutdown_signal {
                println!("[HANDLER-{}] Received shutdown signal, stopping.", self.market_context.name);
                break;
            }
        }
    }
    
    pub async fn get_open_orders(
        self: &Arc<Self>,
    ) -> Result<OpenOrders, CypherInteractiveError> {
        let maybe_oo = self.open_orders.read().await;
        let oo = match *maybe_oo {
            Some(oo) => oo,
            None => {
                return Err(CypherInteractiveError::OpenOrdersNotAvailable);
            }
        };

        Ok(oo)
    }

    pub async fn get_orderbook(
        self: &Arc<Self>,
    ) -> Result<Arc<OrderBook>, CypherInteractiveError> {
        let ob = self.orderbook.read().await;

        Ok(Arc::clone(&ob))
    }

    pub async fn market_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_info: &MarketOrderInfo
    ) -> Result<(), ClientError> {
        let market_ctx = &self.market_context;
        let dex_market_state = self.dex_market.unwrap();
        let cypher_market = ctx.group.get_cypher_market(market_ctx.market_index);
        let cypher_token = ctx.group.get_cypher_token(market_ctx.market_index);

        //todo get best price

        let order = get_new_order_ix(
            &ctx.group,
            cypher_market,
            cypher_token,
            &dex_market_state,
            &market_ctx.open_orders_pk,
            &market_ctx.cypher_user_pk,
            &market_ctx.signer,
            NewOrderInstructionV3 {
                side: order_info.side,
                limit_price: todo!(),
                max_coin_qty: todo!(),
                max_native_pc_qty_including_fees: todo!(),
                self_trade_behavior: todo!(),
                order_type: todo!(),
                client_order_id: todo!(),
                limit: todo!(),
                max_ts: todo!()
            }
        );

        Ok(())
    }

    pub async fn limit_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_info: &LimitOrderInfo,
    ) -> Result<(), ClientError> {
        let market_ctx = &self.market_context;
        let dex_market_state = self.dex_market.unwrap();
        let cypher_market = ctx.group.get_cypher_market(market_ctx.market_index);
        let cypher_token = ctx.group.get_cypher_token(market_ctx.market_index);

        //todo get best price

        let max_native_pc_qty = order_info.amount * order_info.price;

        let order = get_new_order_ix(
            &ctx.group,
            cypher_market,
            cypher_token,
            &dex_market_state,
            &market_ctx.open_orders_pk,
            &market_ctx.cypher_user_pk,
            &market_ctx.signer,
            NewOrderInstructionV3 {
                side: order_info.side,
                limit_price: NonZeroU64::new(order_info.price).unwrap(),
                max_coin_qty: NonZeroU64::new(order_info.amount).unwrap(),
                max_native_pc_qty_including_fees: NonZeroU64::new(max_native_pc_qty).unwrap(),
                self_trade_behavior: SelfTradeBehavior::DecrementTake,
                order_type: OrderType::Limit,
                client_order_id: 1_u64,
                limit: u16::MAX,
                max_ts: i64::MAX
            }
        );

        Ok(())
    }

    pub async fn cancel_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_id: u128,
    ) -> Result<(), ClientError> {

        Ok(())
    }
}