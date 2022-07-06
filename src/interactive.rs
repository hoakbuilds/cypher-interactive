use std::{io::{self, Write}, sync::Arc};
use anchor_client::ClientError;
use serum_dex::matching::Side;
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::{sync::broadcast::Sender, select};

use crate::{CypherInteractiveError, config::CypherConfig};

pub struct Interactive {
    cypher_config: Arc<CypherConfig>,
    rpc_client: Arc<RpcClient>,
    shutdown: Arc<Sender<bool>>,    
}

impl Interactive {
    pub fn new(
        cypher_config: Arc<CypherConfig>,
        rpc_client: Arc<RpcClient>,
        shutdown: Sender<bool>,
    ) -> Self {
        Self {
            cypher_config,
            rpc_client,
            shutdown: Arc::new(shutdown),
        }
    }

    pub async fn run(
        &self
    ) -> Result<(), CypherInteractiveError> {
        // launch all necessary services to operate in all available markets

        self.start().await
    }

    async fn start(
        &self,
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
        &self
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

            match self.process_command(command.clone()) {
                Ok(_) => (),
                Err(e) => {
                    println!("Something went wrong while processing the command: {:?}. Err: {:?}", command, e);
                }
            }
        }
        
        Ok(())
    }

    fn read_input(
        &self
    ) -> Result<String, CypherInteractiveError> {
        let mut buffer = String::new();

        print!(">");
        io::stdout().flush().unwrap();

        match io::stdin().read_line(&mut buffer) {
            Ok(_) => (),
            Err(e) => {
                println!("There was an error processing the input, please try again. Err: {:?}", e);
            },
        };
        let res = trim_newline(&mut buffer);

        Ok(res)
    }

    fn process_command(
        &self,
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
                match self.market_order(&info) {
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
            InteractiveCommand::CancelAll => todo!(),
            InteractiveCommand::Exit => (),
        }

        Ok(())
    }

    fn account_status(
        &self
    ) {

    }

    fn markets_status(
        &self
    ) {

    }
    
    fn tokens_status(
        &self
    ) {

    }

    fn market_order(
        &self,
        order_info: &MarketOrderInfo
    ) -> Result<(), ClientError> {

        Ok(())
    }

    fn limit_order(
        &self,
        order_info: &LimitOrderInfo
    ) -> Result<(), ClientError> {

        Ok(())
    }

    fn cancel_order(
        &self,
        order_id: u128,
    ) -> Result<(), ClientError> {

        Ok(())
    }

    fn cancel_all_orders(
        &self
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
                return Err(CypherInteractiveError::InputError);
            }
        };

        let price = match splits[5].parse::<u64>() {
            Ok(a) => a,
            Err(_) => {
                return Err(CypherInteractiveError::InputError);
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
                return Err(CypherInteractiveError::InputError);
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
                return Err(CypherInteractiveError::InputError);
            }
        };

        return Ok(Some(InteractiveCommand::Cancel(order_id)));
    }

    Ok(None)
}