use crate::{
    fast_tx_builder::FastTxnBuilder,
    providers::{OpenOrdersContext, OrderBook},
    utils::{get_cancel_order_ix, get_new_order_ix, get_open_orders},
    CypherInteractiveError,
};
use cypher::{CypherGroup, CypherUser};
use serum_dex::{
    instruction::{CancelOrderInstructionV2, NewOrderInstructionV3, SelfTradeBehavior},
    matching::{OrderType, Side},
    state::{MarketStateV2, OpenOrders},
};
use solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient};
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    transaction::Transaction,
};
use std::{num::NonZeroU64, sync::Arc};
use tokio::{
    select,
    sync::{broadcast::Receiver, Mutex, RwLock},
};

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
pub struct MarketOrderInfo {
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
    open_orders_provider: Mutex<Receiver<OpenOrdersContext>>,
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
        open_orders_provider: Receiver<OpenOrdersContext>,
        orderbook_provider: Receiver<Arc<OrderBook>>,
        dex_market: Option<MarketStateV2>,
    ) -> Self {
        Self {
            market_context,
            rpc_client,
            shutdown_receiver: Mutex::new(shutdown_receiver),
            open_orders_provider: Mutex::new(open_orders_provider),
            orderbook_provider: Mutex::new(orderbook_provider),
            dex_market,
            open_orders: RwLock::new(None),
            orderbook: RwLock::new(Arc::new(OrderBook::default())),
        }
    }

    pub async fn start(self: &Arc<Self>) {
        let mut oo_receiver = self.open_orders_provider.lock().await;
        let mut ob_receiver = self.orderbook_provider.lock().await;
        let mut shutdown = self.shutdown_receiver.lock().await;
        let mut shutdown_signal: bool = false;

        loop {
            select! {
                oo = oo_receiver.recv() => {
                    if oo.is_ok() {
                        let ooc = oo.unwrap();
                        if ooc.pubkey == self.market_context.open_orders_pk {
                            *self.open_orders.write().await = Some(ooc.open_orders);
                        }
                    }
                },
                ob = ob_receiver.recv() => {
                    if ob.is_ok() {
                        let obc = ob.unwrap();
                        if obc.market == self.market_context.dex_market_pk {
                            *self.orderbook.write().await = obc;
                        }
                    }
                },
                _ = shutdown.recv() => {
                    println!(" Received shutdown signal, stopping.");
                    shutdown_signal = true;
                }
            }

            if shutdown_signal {
                println!(
                    "[HANDLER-{}] Received shutdown signal, stopping.",
                    self.market_context.name
                );
                break;
            }
        }
    }

    pub async fn get_open_orders(self: &Arc<Self>) -> Result<OpenOrders, CypherInteractiveError> {
        let maybe_oo = self.open_orders.read().await;
        let oo = match *maybe_oo {
            Some(oo) => oo,
            None => {
                return Err(CypherInteractiveError::OpenOrdersNotAvailable);
            }
        };

        Ok(oo)
    }

    pub async fn get_orderbook(self: &Arc<Self>) -> Result<Arc<OrderBook>, CypherInteractiveError> {
        let ob = self.orderbook.read().await;

        Ok(Arc::clone(&ob))
    }

    pub async fn market_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_info: &MarketOrderInfo,
    ) -> Result<Signature, CypherInteractiveError> {
        let dex_market_state = self.dex_market.unwrap();
        let cypher_market = Box::new(
            ctx.group
                .get_cypher_market(self.market_context.market_index)
                .unwrap(),
        );
        let cypher_token = Box::new(
            ctx.group
                .get_cypher_token(self.market_context.market_index)
                .unwrap(),
        );

        //todo get best price

        let order = get_new_order_ix(
            &ctx.group,
            &cypher_market,
            &cypher_token,
            &dex_market_state,
            &self.market_context.open_orders_pk,
            &self.market_context.cypher_user_pk,
            &self.market_context.signer,
            NewOrderInstructionV3 {
                side: order_info.side,
                limit_price: todo!(),
                max_coin_qty: todo!(),
                max_native_pc_qty_including_fees: todo!(),
                self_trade_behavior: todo!(),
                order_type: todo!(),
                client_order_id: todo!(),
                limit: todo!(),
                max_ts: todo!(),
            },
        );

        let res = self
            .submit_transactions(order, &self.market_context.signer, *ctx.hash)
            .await;

        match res {
            Ok(s) => Ok(s),
            Err(e) => Err(CypherInteractiveError::TransactionSubmission(e)),
        }
    }

    pub async fn limit_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_info: &LimitOrderInfo,
    ) -> Result<Signature, CypherInteractiveError> {
        let dex_market_state = self.dex_market.unwrap();
        let cypher_market = Box::new(
            ctx.group
                .get_cypher_market(self.market_context.market_index)
                .unwrap(),
        );
        let cypher_token = Box::new(
            ctx.group
                .get_cypher_token(self.market_context.market_index)
                .unwrap(),
        );

        let max_native_pc_qty = order_info.amount * order_info.price;

        let order_ix = get_new_order_ix(
            &ctx.group,
            &cypher_market,
            &cypher_token,
            &dex_market_state,
            &self.market_context.open_orders_pk,
            &self.market_context.cypher_user_pk,
            &self.market_context.signer,
            NewOrderInstructionV3 {
                side: order_info.side,
                limit_price: NonZeroU64::new(order_info.price).unwrap(),
                max_coin_qty: NonZeroU64::new(order_info.amount).unwrap(),
                max_native_pc_qty_including_fees: NonZeroU64::new(max_native_pc_qty).unwrap(),
                self_trade_behavior: SelfTradeBehavior::DecrementTake,
                order_type: OrderType::Limit,
                client_order_id: 1_u64,
                limit: u16::MAX,
                max_ts: i64::MAX,
            },
        );

        let res = self
            .submit_transactions(order_ix, &self.market_context.signer, *ctx.hash)
            .await;

        match res {
            Ok(s) => Ok(s),
            Err(e) => Err(CypherInteractiveError::TransactionSubmission(e)),
        }
    }

    pub async fn cancel_order(
        self: &Arc<Self>,
        ctx: HandlerContext,
        order_id: u128,
    ) -> Result<Signature, CypherInteractiveError> {
        let dex_market_state = self.dex_market.unwrap();
        let cypher_market = Box::new(
            ctx.group
                .get_cypher_market(self.market_context.market_index)
                .unwrap(),
        );
        let cypher_token = Box::new(
            ctx.group
                .get_cypher_token(self.market_context.market_index)
                .unwrap(),
        );

        let open_orders_account = Box::new(self.get_open_orders().await.unwrap());
        let open_orders = Box::new(get_open_orders(&open_orders_account));
        let maybe_order = open_orders.iter().find(|o| o.order_id == order_id);
        let order = match maybe_order {
            Some(o) => Box::new(o),
            None => {
                return Err(CypherInteractiveError::InvalidOrderId(order_id));
            }
        };
        let cancel_order_ix = get_cancel_order_ix(
            &ctx.group,
            &cypher_market,
            &cypher_token,
            &dex_market_state,
            &self.market_context.open_orders_pk,
            &self.market_context.cypher_user_pk,
            &self.market_context.signer,
            CancelOrderInstructionV2 {
                order_id,
                side: order.side,
            },
        );

        let res = self
            .submit_transactions(cancel_order_ix, &self.market_context.signer, *ctx.hash)
            .await;

        match res {
            Ok(s) => Ok(s),
            Err(e) => Err(CypherInteractiveError::TransactionSubmission(e)),
        }
    }

    async fn submit_transactions(
        self: &Arc<Self>,
        ix: Instruction,
        signer: &Keypair,
        blockhash: Hash,
    ) -> Result<Signature, ClientError> {
        let mut txn_builder: Box<FastTxnBuilder> = Box::new(FastTxnBuilder::new());
        txn_builder.add(ix);

        let tx = txn_builder.build(blockhash, signer, None);
        let res = self.send_and_confirm_transaction(&tx).await;
        match res {
            Ok(s) => Ok(s),
            Err(e) => Err(e),
        }
    }

    async fn send_and_confirm_transaction(
        self: &Arc<Self>,
        tx: &Transaction,
    ) -> Result<Signature, ClientError> {
        let submit_res = self.rpc_client.send_and_confirm_transaction(tx).await;

        match submit_res {
            Ok(s) => Ok(s),
            Err(e) => Err(e),
        }
    }
}
