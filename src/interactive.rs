use std::{io::{self, Write}, sync::Arc, str::FromStr, ops::Deref};
use anchor_client::ClientError;
use anchor_lang::Accounts;
use cypher::states::{CypherUser, CypherGroup};
use serum_dex::{matching::Side, state::{MarketStateV2, OpenOrders}};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{signature::Keypair, pubkey::Pubkey};
use tokio::{sync::{broadcast::{Sender, channel}, RwLock}, select};

use crate::{CypherInteractiveError, config::{CypherMarketConfig, CypherConfig}, utils::{get_new_order_ix, get_or_init_open_orders, derive_open_orders_address, get_serum_market}, providers::OrderBook, accounts_cache::AccountsCache, services::{ChainMetaService, AccountInfoService}};


struct CypherMarketContext {
    pub market_config: CypherMarketConfig,
    pub dex_market_pk: Pubkey,
    pub dex_market_state: MarketStateV2,
    pub open_orders_pk: Pubkey,
    pub open_orders: OpenOrders,
    pub orderbook: OrderBook,
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
    cypher_markets: RwLock<Vec<CypherMarketContext>>,
    cypher_user: RwLock<Option<CypherUser>>,
    cypher_group: RwLock<Option<CypherGroup>>,
    keypair: Keypair,
    cypher_user_pk: Pubkey,
    cypher_group_pk: Pubkey,

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
            cypher_markets: RwLock::new(Vec::new()),
            cypher_user: RwLock::new(None),
            cypher_group: RwLock::new(None),
        }
    }

    pub async fn init(
        mut self,
    ) -> Result<(), CypherInteractiveError> {
        // launch all necessary services to operate in all available markets

        match self.start_services().await {
            Ok(_) => {
                Ok(())
            },
            Err(e) => {
                println!("An error occurred while starting the application services: {:?}", e);
                Err(e)
            },
        }
    }

    async fn start_services(
        &mut self,
    ) -> Result<(), CypherInteractiveError> {
        let mut ais_pks: Vec<Pubkey> = Vec::new();
        let group_config = &self.cypher_config.get_group(&self.cluster).unwrap();

        // unbounded channel for the accounts cache to send messages whenever a given account gets updated
        let (accounts_cache_s, accounts_cache_r) = channel::<Pubkey>(u16::MAX as usize);
        self.accounts_cache_sender = accounts_cache_s;
        self.accounts_cache =
            Arc::new(AccountsCache::new(self.accounts_cache_sender.clone()));

        self.cm_service = Arc::new(ChainMetaService::new(
            Arc::clone(&self.rpc_client),
            self.shutdown.subscribe(),
        ));

        for market in &group_config.markets {
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

            let cypher_market = CypherMarketContext {
                market_config: market.clone(),
                dex_market_pk,
                dex_market_state: dex_market_account,
                open_orders_pk,
                open_orders: open_orders_account,
                orderbook: OrderBook::default()
            };
            let mut cypher_markets = self.cypher_markets.write().await;
            cypher_markets.push(cypher_market);
        }

        Ok(())
    }

    pub async fn start(
        self: &Arc<Self>
    ) -> Result<(), CypherInteractiveError> {
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

    async fn run_loop(
        self: &Arc<Self>
    ) -> Result<(), CypherInteractiveError> {
        println!(
            "Welcome to the cypher.trade interactive CLI.\nType 'help' to get a list of available commands."
        );

        loop {
            let input = match self.read_input() {
                Ok(i) => i,
                Err(e) => {                    
                    println!("There was an error processing the input, please try again. Err: {:?}", e);
                    continue;
                }
            };

            let maybe_command = match get_command(input) {
                Ok(c) => {
                    //println!("{:?}", c);
                    c
                },
                Err(e) => {
                    println!("There was an error processing the input, please try again. Err: {:?}", e);
                    None
                },
            };

            let command = match maybe_command {
                Some(c) => {
                    //println!("{:?}", c);
                    c
                },
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
                    println!("Something went wrong while processing the command: {:?}. Err: {:?}", command, e);
                }
            }
        }
        
        Ok(())
    }

    fn read_input(
        self: &Arc<Self>
    ) -> Result<String, CypherInteractiveError> {
        let mut buffer = String::new();

        print!(">");
        io::stdout().flush().unwrap();

        match io::stdin().read_line(&mut buffer) {
            Ok(_) => (),
            Err(e) => {
                println!("There was an error processing the input, please try again. Err: {:?}", e);
            },
        }
        let res = trim_newline(&mut buffer);

        Ok(res)
    }

    async fn process_command(
        self: &Arc<Self>,
        command: InteractiveCommand
    ) -> Result<(), CypherInteractiveError>  {

        match command {
            InteractiveCommand::Help => {                
                println!(">>> status\n\t- displays cypher account status and open orders information for available markets");
                println!(">>> limit {{side}} {{symbol}} {{amount}} at {{price}}\n\t- submits a limit order on the given order book side at the given price for the given amount");
                println!(">>> market {{side}} {{symbol}} {{amount}}\n\t- submits a market order on the given order book side at the best available price for the given amount");
                println!(">>> cancel {{order_id}}\n\t- cancels the order with the given order id");
                println!(">>> cancel all\n\t- cancels all orders in the order book");
                println!(">>> exit\n\t- exits the application");
            },
            InteractiveCommand::TokensStatus => self.tokens_status(),
            InteractiveCommand::MarketsStatus => self.markets_status(),
            InteractiveCommand::AccountStatus => self.account_status(),
            InteractiveCommand::Limit(info) => {
                match self.limit_order(&info) {
                    Ok(_) => (),
                    Err(e) => {
                        println!("There was an error placing limit order. Err: {}", e);
                    },
                }
            },
            InteractiveCommand::Market(info) => {
                match self.market_order(&info).await {
                    Ok(_) => (),
                    Err(e) => {
                        println!("There was an error placing market order. Err: {}", e);
                    },
                }
            },
            InteractiveCommand::Cancel(order_id) => {
                match self.cancel_order(order_id) {
                    Ok(_) => (),
                    Err(e) => {
                        println!("There was an error cancelling order with id {}. Err: {}", order_id, e);
                    },
                }
            },
            InteractiveCommand::CancelAll => {
                match self.cancel_all_orders() {
                    Ok(_) => (),
                    Err(e) => {
                        println!("There was an error cancelling all orders. Err: {}", e);
                    },
                }
            },
            InteractiveCommand::Exit => (),
        }

        Ok(())
    }

    fn account_status(
        self: &Arc<Self>
    ) {

    }

    fn markets_status(
        self: &Arc<Self>
    ) {

    }
    
    fn tokens_status(
        self: &Arc<Self>
    ) {

    }

    async fn market_order(
        self: &Arc<Self>,
        order_info: &MarketOrderInfo
    ) -> Result<(), ClientError> {
        let cypher_markets = self.cypher_markets.read().await;
        let market = match cypher_markets.iter().find(|m| m.market_config.name == order_info.symbol) {
            Some(m) => m,
            None => {
                println!("Could not find market with symbol {}.", order_info.symbol);
                return Ok(());
            },
        };

        Ok(())
    }

    fn limit_order(
        self: &Arc<Self>,
        order_info: &LimitOrderInfo
    ) -> Result<(), ClientError> {

        //let ix = get_new_order_ix();

        Ok(())
    }

    fn cancel_order(
        self: &Arc<Self>,
        order_id: u128,
    ) -> Result<(), ClientError> {

        Ok(())
    }

    fn cancel_all_orders(
        self: &Arc<Self>,
    ) -> Result<(), ClientError> {

        Ok(())
    }


}

#[derive(Debug, PartialEq, Clone)]
struct LimitOrderInfo {
    symbol: String,
    price: u64,
    amount: u64,
    side: Side,
}

#[derive(Debug, PartialEq, Clone)]
struct MarketOrderInfo{
    symbol: String,
    amount: u64,
    side: Side,
}

#[derive(Debug, PartialEq, Clone)]
enum InteractiveCommand {
    Help,
    MarketsStatus,
    TokensStatus,
    AccountStatus,
    Limit(LimitOrderInfo),
    Market(MarketOrderInfo),
    Cancel(u128),
    CancelAll,
    Exit,
}

fn trim_newline(s: &mut String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }

    s.to_lowercase()
}

fn get_command(buffer: String) -> Result<Option<InteractiveCommand>, CypherInteractiveError> {
    if buffer.is_empty() {
        return Ok(None);
    }

    let splits: Vec<&str> = buffer.split(' ').collect();

    // print!("splits: ");
    // for i in &splits {
    //     print!("{} ", i);
    // }
    // io::stdout().flush().unwrap();

    if splits.is_empty() {
        return Ok(None);
    }

    let command_word = splits[0].to_string();
    
    if command_word == "help" {

        //println!("help command");
        return Ok(Some(InteractiveCommand::Help));

    } else if command_word == "status" {

        //println!("status command");
        return Ok(Some(InteractiveCommand::AccountStatus));

    } else if command_word == "limit" {

        //println!("limit command");
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

        let price = match splits[5].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::Limit(LimitOrderInfo{
            symbol,
            price,
            amount,
            side,
        })));

    } else if command_word == "market" {

        //println!("market command");
        if splits.len() < 3 {
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
        
        return Ok(Some(InteractiveCommand::Market(MarketOrderInfo{
            symbol,
            amount,
            side,
        })));

    } else if command_word == "cancel" {

        //println!("cancel command");
        if splits.len() < 2 {
            return Ok(None);
        }

        if splits[1] == "all" {
            return Ok(Some(InteractiveCommand::CancelAll));
        }

        let order_id = match splits[1].parse::<u128>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::Input);
            }
        };

        return Ok(Some(InteractiveCommand::Cancel(order_id)));
    }

    Ok(None)
}