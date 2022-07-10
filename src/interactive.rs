use std::{io::{self, Write}, sync::Arc, str::FromStr};
use anchor_client::ClientError;
use cypher::states::{CypherUser, CypherGroup};
use serum_dex::{matching::Side, state::{MarketStateV2, OpenOrders}};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{signature::Keypair, pubkey::Pubkey};
use tokio::{sync::{broadcast::{Sender, channel}, RwLock}, select, task::JoinHandle};

use crate::{CypherInteractiveError, config::{CypherMarketConfig, CypherConfig}, utils::{get_new_order_ix, get_or_init_open_orders, derive_open_orders_address, get_serum_market}, providers::{OrderBook, OrderBookProvider, OpenOrdersProvider, CypherAccountProvider, CypherGroupProvider, OrderBookContext}, accounts_cache::AccountsCache, services::{ChainMetaService, AccountInfoService}};

struct CypherMarketContext {
    pub market_config: CypherMarketConfig,
    pub dex_market_pk: Pubkey,
    pub dex_market_state: Option<MarketStateV2>,
    pub open_orders_pk: Pubkey,
    pub open_orders: Option<OpenOrders>,
    pub open_orders_provider: Sender<OpenOrders>,
    pub orderbook: Arc<OrderBook>,
    pub orderbook_provider: Sender<Arc<OrderBook>>,
}


pub struct Interactive {
    cypher_config: Arc<CypherConfig>,
    cluster: String,
    rpc_client: Arc<RpcClient>,
    shutdown: Arc<Sender<bool>>,
    ai_service: Arc<AccountInfoService>,
    cm_service: Arc<ChainMetaService>,
    accounts_cache: Arc<AccountsCache>,
    accounts_cache_sender: Sender<Pubkey>,
    cypher_market: RwLock<CypherMarketContext>,
    cypher_user_provider: Arc<CypherAccountProvider>,
    cypher_user_provider_sender: Sender<Box<CypherUser>>,
    cypher_user: RwLock<Option<CypherUser>>,
    cypher_group_provider: Arc<CypherGroupProvider>,
    cypher_group_provider_sender: Sender<Box<CypherGroup>>,
    cypher_group: RwLock<Option<CypherGroup>>,
    keypair: Keypair,
    cypher_user_pk: Pubkey,
    cypher_group_pk: Pubkey,
    tasks: Vec<JoinHandle<()>>,
}

impl Interactive {
    pub fn new(
        cypher_config: Arc<CypherConfig>,
        cluster: String,
        rpc_client: Arc<RpcClient>,
        shutdown: Sender<bool>,
        keypair: Keypair,
        cypher_user_pk: Pubkey,
        cypher_group_pk: Pubkey,
    ) -> Self {
        Self {
            cypher_config,
            cluster,
            rpc_client,
            keypair,
            cypher_user_pk,
            cypher_group_pk,
            cm_service: Arc::new(ChainMetaService::default()),
            ai_service: Arc::new(AccountInfoService::default()),
            accounts_cache: Arc::new(AccountsCache::default()),
            accounts_cache_sender: channel::<Pubkey>(u16::MAX as usize).0,
            shutdown: Arc::new(shutdown),
            cypher_market: RwLock::new(CypherMarketContext::default()),
            cypher_user_provider: CypherAccountProviderWrapper::default(),
            cypher_user: RwLock::new(None),
            cypher_group_provider: CypherGroupProviderWrapper::default(),
            cypher_group: RwLock::new(None),
            tasks: Vec::new(),
        }
    }

    pub async fn init(
        mut self,
    ) -> Result<(), CypherInteractiveError> {
        // launch all necessary services to operate in all available markets

        match self.init_services().await {
            Ok(_) => (),
            Err(e) => {
                println!("An error occurred while starting the application services: {:?}", e);
                return Err(e)
            },
        }
        
        // start the services
        let ai_t = tokio::spawn(async move {
            self.ai_service.start_service().await;
        });
        self.tasks.push(ai_t);

        let cm_t = tokio::spawn(async move {
            self.cm_service.start_service().await;
        });
        self.tasks.push(cm_t);

        let market_ctx = self.cypher_market.read().await;

        let ob_t = tokio::spawn(
            async move {
                market_ctx.orderbook_provider.provider.start().await;
            }
        );

        for task in self.tasks {
            let res = tokio::join!(task);

            match res {
                (Ok(_),) => (),
                (Err(e),) => {
                    println!("There was an error joining with task: {}", e.to_string());
                }
            };
        }

        Ok(())
    }

    async fn init_services(
        &mut self,
    ) -> Result<(), CypherInteractiveError> {
        let mut ais_pks: Vec<Pubkey> = Vec::new();
        let mut ob_ctxs: Vec<OrderBookContext> = Vec::new();

        let group_config = self.cypher_config.get_group(&self.cluster).unwrap();

        // unbounded channel for the accounts cache to send messages whenever a given account gets updated
        let (accounts_cache_s, accounts_cache_r) = channel::<Pubkey>(u16::MAX as usize);
        self.accounts_cache_sender = accounts_cache_s;
        self.accounts_cache =
            Arc::new(AccountsCache::new(self.accounts_cache_sender.clone()));

        self.cm_service = Arc::new(ChainMetaService::new(
            Arc::clone(&self.rpc_client),
            self.shutdown.subscribe(),
        ));
        
        let (ca_s, ca_r) = channel::<Box<CypherUser>>(u16::MAX as usize);
        let arc_ca_s = Arc::new(ca_s);
        let ca_provider = Arc::new(CypherAccountProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_ca_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            self.cypher_user_pk,
        ));

        let (cg_s, cg_r) = channel::<Box<CypherGroup>>(u16::MAX as usize);
        let arc_cg_s = Arc::new(cg_s);
        let cg_provider = Arc::new(CypherGroupProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_cg_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            self.cypher_group_pk,
        ));

        for market in &group_config.markets {
            let dex_market_bids = Pubkey::from_str(market.bids.as_str()).unwrap();
            let dex_market_asks = Pubkey::from_str(market.asks.as_str()).unwrap();
            let dex_market_pk = Pubkey::from_str(&market.address).unwrap();
            let dex_market_account = match get_serum_market(
                Arc::clone(&self.rpc_client),
                dex_market_pk
            ).await {
                Ok(m) => m,
                Err(e) => {
                    println!("An error occurred while fetching the serum market account for {}. Ignoring market. Error: {}", market.name, e);
                    continue;
                }
            };

            let open_orders_pk = derive_open_orders_address(
                &dex_market_pk,
                &self.cypher_user_pk
            );

            let open_orders_account = match get_or_init_open_orders(
                &self.keypair,
                &self.cypher_group_pk,
                &self.cypher_user_pk,
                &dex_market_pk,
                &open_orders_pk,
                Arc::clone(&self.rpc_client)
            ).await {
                Ok(ooa) => ooa,
                Err(e) => {
                    println!("An error occurred while fetching or creating open orders account for {}. Ignoring market. Error: {:?}", market.name, e);
                    continue;
                },
            };
            ob_ctxs.push(OrderBookContext{
                market: dex_market_pk,
                bids: dex_market_bids,
                asks: dex_market_asks,
                coin_lot_size: dex_market_account.coin_lot_size,
                pc_lot_size: dex_market_account.pc_lot_size
            });
        }

        let (ob_s, ob_r) = channel::<Arc<OrderBook>>(u16::MAX as usize);
        let arc_ob_s = Arc::new(ob_s);
        let ob_provider = Arc::new(OrderBookProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_ob_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            ob_ctxs
        ));
        let ob_wrapper = OrderBookProviderWrapper {
            provider: ob_provider,
            sender: arc_ob_s
        };

        let (oo_s, oo_r) = channel::<OpenOrders>(u16::MAX as usize);
        let arc_oo_s = Arc::new(oo_s);
        let oo_provider = Arc::new(OpenOrdersProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_oo_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            open_orders_pk
        ));
        let oop_wrapper = OpenOrdersProviderWrapper {
            provider: oo_provider,
            sender: arc_oo_s,
        };

        ais_pks.extend(vec![
            dex_market_pk,
            dex_market_bids,
            dex_market_asks,
            open_orders_pk
        ]);

        let cypher_market = CypherMarketContext {
            market_config: market.clone(),
            dex_market_pk,
            dex_market_state: dex_market_account,
            open_orders_pk,
            open_orders: open_orders_account,
            open_orders_provider: oo_provider,
            orderbook: OrderBook::default(),
            orderbook_provider: ob_provider
        };

        ais_pks.push(self.cypher_group_pk);
        ais_pks.push(self.cypher_user_pk);

        self.ai_service = Arc::new(AccountInfoService::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&self.rpc_client),
            &ais_pks,
            self.shutdown.subscribe(),
        ));

        Ok(())
    }

}