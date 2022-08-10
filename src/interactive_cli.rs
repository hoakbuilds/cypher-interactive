use std::{
    io::{self, Write},
    str::FromStr,
    sync::Arc,
};

use cypher::{
    constants::QUOTE_TOKEN_IDX, utils::derive_open_orders_address, CypherGroup, CypherUser,
};
use jet_proto_math::Number;
use safe_transmute::util;
use serum_dex::{matching::Side, state::OpenOrders};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use tokio::{
    select,
    sync::broadcast::{channel, Sender},
    task::JoinHandle,
};

use crate::{
    accounts_cache::AccountsCache,
    config::CypherConfig,
    cypher_context::CypherContext,
    market_handler::{
        CancelOrderInfo, Handler, HandlerContext, LimitOrderInfo, MarketContext, MarketOrderInfo,
    },
    providers::{
        CypherAccountProvider, CypherGroupProvider, OpenOrdersContext, OpenOrdersProvider,
        OrderBook, OrderBookContext, OrderBookProvider,
    },
    services::{AccountInfoService, ChainMetaService},
    utils::{
        deposit_quote_token, get_open_orders_with_qty, get_or_init_open_orders, get_serum_market,
        request_airdrop, set_delegate, create_cypher_user,
    },
    CypherInteractiveError,
};

#[derive(Debug, PartialEq, Clone)]
enum InteractiveCommand {
    NewAccount(u64),
    Help,
    Airdrop,
    Delegate(String),
    Deposit(f64),
    MarketsStatus,
    TokensStatus,
    AccountStatus,
    OrderBookStatus(OrderBookInfo),
    Limit(LimitOrderInfo),
    Market(MarketOrderInfo),
    Cancel(CancelOrderInfo),
    Exit,
}

#[derive(Debug, PartialEq, Clone)]
struct OrderBookInfo {
    symbol: String,
    depth: usize,
}

pub struct InteractiveCli {
    cypher_config: Arc<CypherConfig>,
    cluster: String,
    group: String,
    rpc_client: Arc<RpcClient>,
    shutdown: Sender<bool>,
    ai_service: Arc<AccountInfoService>,
    cm_service: Arc<ChainMetaService>,
    accounts_cache: Arc<AccountsCache>,
    accounts_cache_sender: Sender<Pubkey>,
    cypher_user_provider: Arc<CypherAccountProvider>,
    cypher_user_provider_sender: Arc<Sender<Box<CypherUser>>>,
    cypher_group_provider: Arc<CypherGroupProvider>,
    cypher_group_provider_sender: Arc<Sender<Box<CypherGroup>>>,
    open_orders_provider: Arc<OpenOrdersProvider>,
    orderbook_provider: Arc<OrderBookProvider>,
    handlers: Vec<Arc<Handler>>,
    cypher_context: Arc<CypherContext>,
    keypair: Arc<Keypair>,
    cypher_user_pk: Pubkey,
    cypher_group_pk: Pubkey,
    tasks: Vec<JoinHandle<()>>,
}

impl InteractiveCli {
    pub fn new(
        cypher_config: Arc<CypherConfig>,
        cluster: String,
        group: String,
        rpc_client: Arc<RpcClient>,
        shutdown: Sender<bool>,
        keypair: Arc<Keypair>,
        cypher_user_pk: Pubkey,
        cypher_group_pk: Pubkey,
    ) -> Self {
        Self {
            cypher_config,
            cluster,
            group,
            rpc_client,
            shutdown,
            keypair,
            cypher_user_pk,
            cypher_group_pk,
            cm_service: Arc::new(ChainMetaService::default()),
            ai_service: Arc::new(AccountInfoService::default()),
            accounts_cache: Arc::new(AccountsCache::default()),
            accounts_cache_sender: channel::<Pubkey>(u16::MAX as usize).0,
            cypher_user_provider: Arc::new(CypherAccountProvider::default()),
            cypher_user_provider_sender: Arc::new(channel::<Box<CypherUser>>(u16::MAX as usize).0),
            cypher_group_provider: Arc::new(CypherGroupProvider::default()),
            cypher_group_provider_sender: Arc::new(
                channel::<Box<CypherGroup>>(u16::MAX as usize).0,
            ),
            open_orders_provider: Arc::new(OpenOrdersProvider::default()),
            orderbook_provider: Arc::new(OrderBookProvider::default()),
            handlers: Vec::new(),
            cypher_context: Arc::new(CypherContext::default()),
            tasks: Vec::new(),
        }
    }

    pub async fn start(mut self) -> Result<(), CypherInteractiveError> {
        // launch all necessary services to operate in all available markets
        match self.start_services().await {
            Ok(_) => (),
            Err(e) => {
                println!(
                    "An error occurred while starting the application services: {:?}",
                    e
                );
                return Err(e);
            }
        }

        let ai_service = Arc::clone(&self.ai_service);
        // start the services
        let ai_t = tokio::spawn(async move {
            ai_service.start_service().await;
        });
        self.tasks.push(ai_t);

        let cm_service = Arc::clone(&self.cm_service);
        let cm_t = tokio::spawn(async move {
            cm_service.start_service().await;
        });
        self.tasks.push(cm_t);

        let cgp = Arc::clone(&self.cypher_group_provider);
        let cg_t = tokio::spawn(async move {
            cgp.start().await;
        });
        self.tasks.push(cg_t);

        let cat = Arc::clone(&self.cypher_user_provider);
        let ca_t = tokio::spawn(async move {
            cat.start().await;
        });
        self.tasks.push(ca_t);

        let obp = Arc::clone(&self.orderbook_provider);
        let obp_t = tokio::spawn(async move {
            obp.start().await;
        });
        self.tasks.push(obp_t);

        let oop = Arc::clone(&self.open_orders_provider);
        let oop_t = tokio::spawn(async move {
            oop.start().await;
        });
        self.tasks.push(oop_t);

        let cc = Arc::clone(&self.cypher_context);
        let cc_t = tokio::spawn(async move {
            cc.start().await;
        });
        self.tasks.push(cc_t);

        for handler in self.handlers.clone() {
            let t = tokio::spawn(async move {
                handler.start().await;
            });
            self.tasks.push(t);
        }

        match self.run().await {
            Ok(_) => (),
            Err(e) => {
                println!(
                    "There was an error while running the interactive command line: {:?}",
                    e
                );
            }
        }

        Ok(())
    }

    async fn start_services(&mut self) -> Result<(), CypherInteractiveError> {
        let mut ais_pks: Vec<Pubkey> = Vec::new();
        let mut ob_ctxs: Vec<OrderBookContext> = Vec::new();
        let mut open_orders_pks: Vec<Pubkey> = Vec::new();

        let group_config = self.cypher_config.get_group(&self.group).unwrap();

        // unbounded channel for the accounts cache to send messages whenever a given account gets updated
        let (accounts_cache_s, _) = channel::<Pubkey>(u16::MAX as usize);
        self.accounts_cache_sender = accounts_cache_s;
        self.accounts_cache = Arc::new(AccountsCache::new(self.accounts_cache_sender.clone()));

        self.cm_service = Arc::new(ChainMetaService::new(
            Arc::clone(&self.rpc_client),
            self.shutdown.subscribe(),
        ));

        let (ca_s, _) = channel::<Box<CypherUser>>(u16::MAX as usize);
        let arc_ca_s = Arc::new(ca_s);
        self.cypher_user_provider_sender = Arc::clone(&arc_ca_s);
        self.cypher_user_provider = Arc::new(CypherAccountProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_ca_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            self.cypher_user_pk,
        ));

        let (cg_s, _) = channel::<Box<CypherGroup>>(u16::MAX as usize);
        let arc_cg_s = Arc::new(cg_s);
        self.cypher_group_provider_sender = Arc::clone(&arc_cg_s);
        self.cypher_group_provider = Arc::new(CypherGroupProvider::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&arc_cg_s),
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            self.cypher_group_pk,
        ));

        let (ob_s, _) = channel::<Arc<OrderBook>>(u16::MAX as usize);
        let arc_ob_s = Arc::new(ob_s);

        let (oo_s, _) = channel::<OpenOrdersContext>(u16::MAX as usize);
        let arc_oo_s = Arc::new(oo_s);

        for market in &group_config.markets {
            let dex_market_bids = Pubkey::from_str(market.bids.as_str()).unwrap();
            let dex_market_asks = Pubkey::from_str(market.asks.as_str()).unwrap();
            let dex_market_pk = Pubkey::from_str(&market.address).unwrap();
            let dex_market_account = match get_serum_market(
                Arc::clone(&self.rpc_client),
                dex_market_pk,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    println!("An error occurred while fetching the serum market account for {}. Ignoring market. Error: {}", market.name, e);
                    continue;
                }
            };

            let open_orders_pk = derive_open_orders_address(&dex_market_pk, &self.cypher_user_pk).0;
            open_orders_pks.push(open_orders_pk);

            let _ooa = match get_or_init_open_orders(
                &self.keypair,
                &self.cypher_group_pk,
                &self.cypher_user_pk,
                &dex_market_pk,
                &open_orders_pk,
                Arc::clone(&self.rpc_client),
                self.cluster.to_string(),
            )
            .await
            {
                Ok(ooa) => ooa,
                Err(e) => {
                    println!("An error occurred while fetching or creating open orders account for {}. Ignoring market. Error: {:?}", market.name, e);
                    continue;
                }
            };
            println!(
                "Preparing orderbook context for market {}. Market: {} Bids: {} Asks: {}.",
                market.name, dex_market_pk, dex_market_bids, dex_market_asks
            );
            ob_ctxs.push(OrderBookContext {
                market: dex_market_pk,
                bids: dex_market_bids,
                asks: dex_market_asks,
                coin_lot_size: dex_market_account.coin_lot_size,
                pc_lot_size: dex_market_account.pc_lot_size,
            });

            ais_pks.extend(vec![
                dex_market_pk,
                dex_market_bids,
                dex_market_asks,
                open_orders_pk,
            ]);

            println!("Preparing handler for market {}.", market.name);
            self.handlers.push(Arc::new(Handler::new(
                Box::new(MarketContext {
                    name: market.name.to_string(),
                    market_index: market.market_index,
                    signer: Arc::clone(&self.keypair),
                    cypher_user_pk: self.cypher_user_pk,
                    dex_market_pk,
                    open_orders_pk,
                }),
                Arc::clone(&self.rpc_client),
                self.shutdown.subscribe(),
                arc_oo_s.subscribe(),
                arc_ob_s.subscribe(),
                Some(dex_market_account),
            )));
        }

        self.orderbook_provider = Arc::new(OrderBookProvider::new(
            Arc::clone(&self.accounts_cache),
            arc_ob_s,
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            ob_ctxs,
        ));

        self.open_orders_provider = Arc::new(OpenOrdersProvider::new(
            Arc::clone(&self.accounts_cache),
            arc_oo_s,
            self.accounts_cache_sender.subscribe(),
            self.shutdown.subscribe(),
            open_orders_pks,
        ));

        ais_pks.push(self.cypher_group_pk);
        ais_pks.push(self.cypher_user_pk);

        self.ai_service = Arc::new(AccountInfoService::new(
            Arc::clone(&self.accounts_cache),
            Arc::clone(&self.rpc_client),
            &ais_pks,
            self.shutdown.subscribe(),
        ));

        self.cypher_context = Arc::new(CypherContext::new(
            self.shutdown.subscribe(),
            arc_ca_s.subscribe(),
            arc_cg_s.subscribe(),
        ));

        Ok(())
    }

    async fn run(&self) -> Result<(), CypherInteractiveError> {
        let mut shutdown = self.shutdown.subscribe();

        select! {
            res = self.run_loop() => {
                match res {
                    Ok(_) => (),
                    Err(e) => {
                        println!("An error occurred while running the application loop: {:?}", e);
                    }
                }
            },
            _ = shutdown.recv() => {
                println!(" Received shutdown signal, stopping.");
            }
        }

        Ok(())
    }

    async fn run_loop(&self) -> Result<(), CypherInteractiveError> {
        println!(
            "Welcome to the cypher.trade interactive CLI.\nType 'help' to get a list of available commands."
        );

        loop {
            let input = match self.read_input() {
                Ok(i) => i,
                Err(e) => {
                    println!(
                        "There was an error processing the input, please try again. Err: {:?}",
                        e
                    );
                    continue;
                }
            };

            let maybe_command = match get_command(input) {
                Ok(c) => c,
                Err(e) => {
                    println!(
                        "There was an error processing the input, please try again. Err: {:?}",
                        e
                    );
                    None
                }
            };

            let command = match maybe_command {
                Some(c) => c,
                None => {
                    continue;
                }
            };

            if command == InteractiveCommand::Exit {
                break;
            }

            match self.process_command(command.clone()).await {
                Ok(_) => (),
                Err(e) => {
                    println!(
                        "Something went wrong while processing the command: {:?}. Err: {:?}",
                        command, e
                    );
                }
            }
        }

        Ok(())
    }

    fn read_input(&self) -> Result<String, CypherInteractiveError> {
        let mut buffer = String::new();

        print!(">");
        io::stdout().flush().unwrap();

        match io::stdin().read_line(&mut buffer) {
            Ok(_) => (),
            Err(e) => {
                println!(
                    "There was an error processing the input, please try again. Err: {:?}",
                    e
                );
            }
        }
        trim_newline(&mut buffer);

        Ok(buffer)
    }

    async fn process_command(
        &self,
        command: InteractiveCommand,
    ) -> Result<(), CypherInteractiveError> {
        match command {
            InteractiveCommand::Help => {
                println!(">>> new {{account_number}}\n\t- creates a new account with the specified account number");
                println!(">>> airdrop \n\t- airdrop quote token (devnet only)");
                println!(">>> deposit {{amount_ui}}\n\t- deposits quote token");
                println!(">>> delegate {{pubkey}}\n\t- delegates the account to the given public key, delegates cannot close the account or withdraw");
                println!(">>> status\n\t- displays cypher account status and open orders information for available markets");
                println!(">>> markets\n\t- displays cypher group's available markets and relevant information");
                println!(">>> tokens\n\t- displays cypher group's available tokens and relevant information");
                println!(">>> orderbook {{symbol}} {{max_depth}}\n\t- displays the given market's orderbook up to a given depth");
                println!(">>> limit {{side}} {{symbol}} {{amount}} {{price}}\n\t- submits a limit order on the given order book side at the given price for the given amount");
                println!(">>> market {{side}} {{symbol}} {{amount}}\n\t- submits a market order on the given order book side at the best available price for the given amount");
                println!(">>> cancel {{symbol}} {{order_id}}\n\t- cancels the order with the given order id and symbol");
                println!(">>> exit\n\t- exits the application");
            }
            InteractiveCommand::NewAccount(account_number) => self.new_account(account_number).await,
            InteractiveCommand::Airdrop => self.airdrop().await,
            InteractiveCommand::Delegate(pk) => self.delegate(pk).await,
            InteractiveCommand::Deposit(amount) => self.deposit(amount).await,
            InteractiveCommand::TokensStatus => self.tokens_status().await,
            InteractiveCommand::MarketsStatus => self.markets_status().await,
            InteractiveCommand::AccountStatus => self.account_status().await,
            InteractiveCommand::OrderBookStatus(info) => self.orderbook_status(info).await,
            InteractiveCommand::Limit(info) => self.limit_order(info).await,
            InteractiveCommand::Market(info) => self.market_order(info).await,
            InteractiveCommand::Cancel(info) => self.cancel_order(info).await,
            InteractiveCommand::Exit => (),
        }

        Ok(())
    }

    pub fn get_handler(&self, market: String) -> Result<&Arc<Handler>, CypherInteractiveError> {
        let maybe_handler = self
            .handlers
            .iter()
            .find(|h| h.market_context.name == market);
        let handler = match maybe_handler {
            Some(h) => {
                println!("Found handler for the market {}.", h.market_context.name);
                h
            }
            None => {
                println!(
                    "Could not find a suitable handler for the market: {}.",
                    market
                );
                return Err(CypherInteractiveError::CouldNotFindHandler);
            }
        };

        Ok(handler)
    }

    async fn airdrop(&self) {
        if self.cluster != "devnet" {
            println!("This command is only available for 'devnet' cluster.");
            return;
        }
        let req_res = request_airdrop(&self.keypair, Arc::clone(&self.rpc_client)).await;

        match req_res {
            Ok(s) => {
                println!("Successfully requested airdrop. https://explorer.solana.com/tx/{}?cluster=devnet", s);
            }
            Err(e) => {
                println!("There was an error requesting airdrop: {:?}", e);
            }
        }
    }

    async fn new_account(&self, account_number: u64) {
        let req_res = create_cypher_user(
            &self.cypher_group_pk, &self.keypair, account_number, Arc::clone(&self.rpc_client)
        ).await;

        match req_res {
            Ok(s) => {
                println!("Successfully created new account with number {}. https://explorer.solana.com/tx/{}?cluster=devnet", account_number, s);
            }
            Err(e) => {
                println!("There was an error creating a new account: {:?}", e);
            }
        }
    }

    async fn delegate(&self, pubkey: String) {
        let delegate_pk = Pubkey::from_str(&pubkey).unwrap();
        let req_res = set_delegate(
            &self.cypher_group_pk,
            &self.cypher_user_pk,
            &delegate_pk,
            &self.keypair,
            Arc::clone(&self.rpc_client),
        )
        .await;

        match req_res {
            Ok(s) => {
                println!("Successfully delegated account to {}. https://explorer.solana.com/tx/{}?cluster=devnet", pubkey, s);
            }
            Err(e) => {
                println!("There was an error delegating to account: {:?}", e);
            }
        }
    }

    async fn deposit(&self, amount: f64) {
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };
        let native_amount = amount * 10_u64.checked_pow(6).unwrap() as f64;
        let res = deposit_quote_token(
            &self.keypair,
            &self.cypher_user_pk,
            &group,
            Arc::clone(&self.rpc_client),
            native_amount as u64,
        )
        .await;

        match res {
            Ok(s) => {
                println!(
                    "Successfully deposited USDC. https://explorer.solana.com/tx/{}?cluster=devnet",
                    s
                );
            }
            Err(e) => {
                println!("There was an error depositing USDC: {:?}", e);
            }
        }
    }

    async fn account_status(&self) {
        let cypher_config = &self.cypher_config;
        let group_config = cypher_config.get_group(&self.group).unwrap();
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };
        let maybe_user = self.cypher_context.get_user().await;
        let user = match maybe_user {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher user not available.");
                return;
            }
        };

        let quote_divisor: Number = 10_u64.checked_pow(6).unwrap().into();
        let (c_ratio, assets_value, liabs_value) = user.get_margin_c_ratio_components(&group);
        let assets_value_ui = assets_value / quote_divisor;
        let liabs_value_ui = liabs_value / quote_divisor;
        println!("----- Account Status -----");
        println!("\tDelegation: {}", user.delegate);
        println!("\tAssets Value (native): {}", assets_value);
        println!("\tLiabilities Value (native): {}", liabs_value);
        println!("\tAssets Value (ui): {}", assets_value_ui);
        println!("\tLiabilities Value (ui): {}", liabs_value_ui);
        println!("\tC Ratio: {}", c_ratio);
        for market in &group_config.markets {
            let cypher_token = group.get_cypher_token(market.market_index).unwrap();
            let maybe_position = user.get_position(market.market_index);
            let position = match maybe_position {
                Some(p) => p,
                None => {
                    continue;
                }
            };
            let divisor: Number = 10_u64
                .checked_pow(cypher_token.decimals() as u32)
                .unwrap()
                .into();
            let native_borrows = position.base_borrows();
            let native_deposits = position.base_deposits();
            let borrows: Number = native_borrows / divisor;
            let deposits: Number = native_deposits / divisor;

            println!("\tToken: {}", market.base_symbol);
            println!("\t\tBorrows (native): {}", native_borrows);
            println!("\t\tDeposits (native): {}", native_deposits);
            println!("\t\tBorrows (ui): {}", borrows);
            println!("\t\tDeposits (ui): {}", deposits);
            println!(
                "\t\tUnsettled coin (native): {}",
                position.oo_info.coin_free
            );
            println!(
                "\t\tUnsettled price coin (native): {}",
                position.oo_info.pc_free
            );
            println!(
                "\t\tLocked coin (native): {}",
                position.oo_info.coin_total - position.oo_info.coin_free
            );
            println!(
                "\t\tLocked price coin (native): {}",
                position.oo_info.pc_total - position.oo_info.pc_free
            );
        }

        let usdc_token = group.get_cypher_token(QUOTE_TOKEN_IDX);
        let maybe_usdc_position = user.get_position(QUOTE_TOKEN_IDX);
        let usdc_position = match maybe_usdc_position {
            Some(p) => p,
            None => {
                return;
            }
        };
        let usdc_native_borrows = usdc_position.base_borrows();
        let usdc_native_deposits = usdc_position.base_deposits();
        let usdc_borrows = usdc_native_borrows / quote_divisor;
        let usdc_deposits = usdc_native_deposits / quote_divisor;
        println!("\tToken: USDC");
        println!("\t\tDeposits (native): {}", usdc_native_deposits);
        println!("\t\tBorrows (native): {}", usdc_native_borrows);
        println!("\t\tDeposits (ui): {}", usdc_deposits);
        println!("\t\tBorrows (ui): {}", usdc_borrows);
        println!("----- Open Orders -----");

        for market in &group_config.markets {
            println!("\t----- {} Orders -----", market.name);
            let res = self.get_handler(market.name.to_string());
            if res.is_ok() {
                let handler = res.unwrap();
                let open_orders_account = handler.get_open_orders().await.unwrap();
                let ob = handler.get_orderbook().await.unwrap();

                let open_orders = get_open_orders_with_qty(&open_orders_account, &ob).await;

                for order in open_orders {
                    println!(
                        "\t\t{:?} {} for {} - Order ID: {}",
                        order.side, order.quantity, order.price, order.order_id
                    );
                }
            }
            println!("\t----- {} Orders -----", market.name);
        }
        println!("----- Open Orders -----");
        println!("----- Account Status -----");
    }

    async fn markets_status(&self) {
        let cypher_config = &self.cypher_config;
        let group_config = cypher_config.get_group(&self.group).unwrap();
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };

        println!("----- Markets Status -----");
        for market in &group_config.markets {
            let cypher_market = group.get_cypher_market(market.market_index).unwrap();

            println!(
                "\tMarket: {}\n\t\tType: {}\n\t\tOracle Price: {}\n\t\tTWAP: {}",
                market.name,
                market.market_type,
                cypher_market.oracle_price.price,
                cypher_market.market_price
            );
        }
        println!("----- Markets Status -----");
    }

    async fn tokens_status(&self) {
        let cypher_config = &self.cypher_config;
        let group_config = cypher_config.get_group(&self.group).unwrap();
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };

        println!("----- Tokens Status -----");
        for market in &group_config.markets {
            let cypher_token = group.get_cypher_token(market.market_index).unwrap();

            println!("\tToken: {}", market.base_symbol);
            println!("\t\tBorrows (native): {}", cypher_token.base_borrows())
        }

        let usdc_token = group.get_cypher_token(QUOTE_TOKEN_IDX).unwrap();
        let usdc_native_borrows = usdc_token.base_borrows();
        let usdc_native_deposits = usdc_token.base_deposits();
        let usdc_borrows =
            usdc_native_borrows / 10_u64.checked_pow(usdc_token.decimals().into()).unwrap();
        let usdc_deposits =
            usdc_native_deposits / 10_u64.checked_pow(usdc_token.decimals().into()).unwrap();
        let utilization =
            (usdc_native_borrows.as_u64(0) as f64 * 100.0) / usdc_native_deposits.as_u64(0) as f64;

        let borrow_rate = if utilization > usdc_token.config.optimal_util as f64 {
            let extra_util = utilization - usdc_token.config.optimal_util as f64;
            let slope = (usdc_token.config.max_apr as f64 - usdc_token.config.optimal_apr as f64)
                / (100.0 - usdc_token.config.optimal_util as f64);
            usdc_token.config.optimal_apr as f64 + slope * extra_util
        } else {
            let slope =
                usdc_token.config.optimal_apr as f64 / usdc_token.config.optimal_util as f64;
            slope * utilization
        };

        let deposit_rate = (borrow_rate * utilization) / 100.0;

        println!("\tToken: USDC");
        println!(
            "\t\tOptimal Utilization: {}",
            usdc_token.config.optimal_util
        );
        println!("\t\tOptimal Rate: {}", usdc_token.config.optimal_apr);
        println!("\t\tMax Rate: {}", usdc_token.config.max_apr);
        println!("\t\tDeposit Rate: {}", deposit_rate);
        println!("\t\tBorrow Rate: {}", borrow_rate);
        println!("\t\tDeposits (native): {}", usdc_native_deposits);
        println!("\t\tBorrows (native): {}", usdc_native_borrows);
        println!("\t\tDeposits (ui): {}", usdc_deposits);
        println!("\t\tBorrows (ui): {}", usdc_borrows);
        println!("----- Tokens Status -----");
    }

    async fn orderbook_status(&self, info: OrderBookInfo) {
        let maybe_handler = self.get_handler(info.symbol.to_string());
        let handler = match maybe_handler {
            Ok(h) => h,
            Err(_) => {
                println!(
                    "Something went wrong while fetching the handler for market {}",
                    info.symbol
                );
                return;
            }
        };

        let maybe_ob = handler.get_orderbook().await;
        let ob = match maybe_ob {
            Ok(ob) => ob,
            Err(_) => {
                println!(
                    "Something went wrong while fetching orderbook for market {}",
                    info.symbol
                );
                return;
            }
        };

        let mut bids = ob.bids.read().await.clone();
        let mut asks = ob.asks.read().await.clone();

        if bids.is_empty() && asks.is_empty() {
            println!("OrderBook for {} is empty.", info.symbol);
            return;
        }

        bids.sort_by(|a, b| b.price.cmp(&a.price));
        asks.sort_by(|a, b| a.price.cmp(&b.price));
        let num_bids = bids.len();
        let num_asks = asks.len();

        println!("----- OrderBook Status -----");
        println!("Bids: {:^5} Asks: {:^5}", num_bids, num_asks);

        println!(
            "{:^10} {:^10} | {:^10} {:^10}",
            "Bid Size", "Bid Price", "Ask Price", "Ask Size"
        );
        if num_bids >= num_asks {
            for (idx, bid) in bids.iter().enumerate() {
                let ask = asks.get(idx);

                if ask.is_none() {
                    println!(
                        "{:^10} {:^10} | {:^10} {:^10}",
                        bid.quantity, bid.price, 0, 0
                    );
                } else {
                    let ask = ask.unwrap();
                    println!(
                        "{:^10} {:^10} | {:^10} {:^10}",
                        bid.quantity, bid.price, ask.price, ask.quantity
                    );
                }
            }
        } else {
            for (idx, ask) in asks.iter().enumerate() {
                let bid = bids.get(idx);

                if bid.is_none() {
                    println!(
                        "{:^10} {:^10} | {:^10} {:^10}",
                        0, 0, ask.price, ask.quantity
                    );
                } else {
                    let bid = bid.unwrap();
                    println!(
                        "{:^10} {:^10} | {:^10} {:^10}",
                        bid.quantity, bid.price, ask.price, ask.quantity
                    );
                }
            }
        }
        println!("----- OrderBook Status -----");
    }

    async fn limit_order(&self, info: LimitOrderInfo) {
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };
        let maybe_user = self.cypher_context.get_user().await;
        let user = match maybe_user {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher user not available.");
                return;
            }
        };
        let maybe_handler = self.get_handler(info.symbol.to_string());
        let handler = match maybe_handler {
            Ok(h) => h,
            Err(e) => {
                println!(
                    "Could not find an handler for market {}. Err: {:?}",
                    info.symbol, e
                );
                return;
            }
        };
        let hash = self.cm_service.get_latest_blockhash().await;
        let ctx = HandlerContext {
            user: Box::new(user),
            group: Box::new(group),
            hash: Box::new(hash),
        };
        match handler.limit_order(ctx, &info).await {
            Ok(s) => {
                println!(
                    "Successfully placed order. https://explorer.solana.com/tx/{}?cluster=devnet",
                    s
                );
            }
            Err(e) => {
                println!("There was an error placing limit order. Err: {:?}", e);
            }
        }
    }

    async fn market_order(&self, info: MarketOrderInfo) {
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };
        let maybe_user = self.cypher_context.get_user().await;
        let user = match maybe_user {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher user not available.");
                return;
            }
        };
        let maybe_handler = self.get_handler(info.symbol.to_string());
        let handler = match maybe_handler {
            Ok(h) => h,
            Err(e) => {
                println!(
                    "Could not find an handler for market {}. Err: {:?}",
                    info.symbol, e
                );
                return;
            }
        };
        let hash = self.cm_service.get_latest_blockhash().await;
        let ctx = HandlerContext {
            user: Box::new(user),
            group: Box::new(group),
            hash: Box::new(hash),
        };
        match handler.market_order(ctx, &info).await {
            Ok(s) => {
                println!(
                    "Successfully placed order. https://explorer.solana.com/tx/{}?cluster=devnet",
                    s
                );
            }
            Err(e) => {
                println!("There was an error placing market order. Err: {:?}", e);
            }
        }
    }

    async fn cancel_order(&self, info: CancelOrderInfo) {
        let maybe_group = self.cypher_context.get_group().await;
        let group = match maybe_group {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher group not available.");
                return;
            }
        };
        let maybe_user = self.cypher_context.get_user().await;
        let user = match maybe_user {
            Ok(g) => g,
            Err(_) => {
                println!("Cypher user not available.");
                return;
            }
        };
        let maybe_handler = self.get_handler(info.symbol.to_string());
        let handler = match maybe_handler {
            Ok(h) => h,
            Err(e) => {
                println!(
                    "Could not find an handler for market {}. Err: {:?}",
                    info.symbol, e
                );
                return;
            }
        };
        let hash = self.cm_service.get_latest_blockhash().await;
        let ctx = HandlerContext {
            user: Box::new(user),
            group: Box::new(group),
            hash: Box::new(hash),
        };
        match handler.cancel_order(ctx, info.order_id).await {
            Ok(s) => {
                println!("Successfully cancelled order. https://explorer.solana.com/tx/{}?cluster=devnet", s);
            }
            Err(e) => {
                println!("There was an error placing market order. Err: {:?}", e);
            }
        }
    }
}

fn trim_newline(s: &mut String) {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
}

fn get_command(buffer: String) -> Result<Option<InteractiveCommand>, CypherInteractiveError> {
    if buffer.is_empty() {
        return Ok(None);
    }

    let splits: Vec<&str> = buffer.split(' ').collect();

    if splits.is_empty() {
        return Ok(None);
    }

    let command_word = splits[0].to_lowercase();

    if command_word == "help" {
        return Ok(Some(InteractiveCommand::Help));
    } else if command_word == "exit" {
        return Ok(Some(InteractiveCommand::Exit));
    } else if command_word == "status" {
        return Ok(Some(InteractiveCommand::AccountStatus));
    } else if command_word == "markets" {
        return Ok(Some(InteractiveCommand::MarketsStatus));
    } else if command_word == "tokens" {
        return Ok(Some(InteractiveCommand::TokensStatus));
    } else if command_word == "airdrop" {
        return Ok(Some(InteractiveCommand::Airdrop));
    } else if command_word == "new" {
        if splits.len() < 2 {
            return Ok(None);
        }
        let amount = match splits[1].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::NewAccount(amount)));
    } else if command_word == "delegate" {
        if splits.len() < 2 {
            return Ok(None);
        }
        let pk = splits[1].to_string();

        return Ok(Some(InteractiveCommand::Delegate(pk)));
    } else if command_word == "deposit" {
        if splits.len() < 2 {
            return Ok(None);
        }

        let amount = match splits[1].parse::<f64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::Deposit(amount)));
    } else if command_word == "orderbook" {
        if splits.len() < 3 {
            return Ok(None);
        }
        let symbol = splits[1].to_string();
        let depth = match splits[2].parse::<usize>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };
        return Ok(Some(InteractiveCommand::OrderBookStatus(OrderBookInfo {
            symbol,
            depth,
        })));
    } else if command_word == "limit" {
        if splits.len() < 5 {
            return Ok(None);
        }

        let side = if splits[1] == "buy" {
            Side::Bid
        } else {
            Side::Ask
        };
        let symbol = splits[2].to_string();
        let amount = match splits[3].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        let price = match splits[4].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::Limit(LimitOrderInfo {
            symbol,
            price,
            amount,
            side,
        })));
    } else if command_word == "market" {
        if splits.len() < 4 {
            return Ok(None);
        }

        let side = if splits[1] == "buy" {
            Side::Bid
        } else {
            Side::Ask
        };
        let symbol = splits[2].to_string();
        let amount = match splits[3].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        println!("market {:?} {} {}", side, symbol, amount);

        return Ok(Some(InteractiveCommand::Market(MarketOrderInfo {
            symbol,
            amount,
            side,
        })));
    } else if command_word == "cancel" {
        if splits.len() < 3 {
            return Ok(None);
        }

        let symbol = splits[1].to_string();
        let order_id = match splits[2].parse::<u128>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::Cancel(CancelOrderInfo {
            symbol,
            order_id,
        })));
    }

    Ok(None)
}
