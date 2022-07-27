mod config;
mod interactive_cli;
mod cypher_context;
mod market_handler;
mod fast_tx_builder;
mod serum_slab;
mod utils;
mod services;
mod providers;
mod accounts_cache;

use config::*;

use clap::Parser;
use solana_client::{nonblocking::rpc_client::RpcClient, client_error::ClientError};
use tokio::sync::broadcast::channel;
use std::{fs::File, str::FromStr, io::Read, sync::Arc};
use solana_sdk::{signature::Keypair, signer::Signer, commitment_config::CommitmentConfig, pubkey::Pubkey};

use crate::{utils::{derive_cypher_user_address, get_or_init_cypher_user}, interactive_cli::InteractiveCli};

pub const CYPHER_CONFIG_PATH: &str = "./cfg/group.json";

#[derive(Parser)]
struct Cli {
    #[clap(short = 'k', long = "keypair", parse(from_os_str))]
    keypair: std::path::PathBuf,

    #[clap(short = 'c', long = "cluster")]
    cluster: String,

    #[clap(short = 'g', long = "group")]
    group: String
}

#[derive(Debug)]
pub enum CypherInteractiveError {
    KeypairFileOpen,
    KeypairFileRead,
    KeypairLoad,
    Input,
    Airdrop,
    Deposit,
    CouldNotFetchOpenOrders(ClientError),
    CouldNotCreateOpenOrders(ClientError),
    OpenOrdersNotFound,
    CouldNotFetchCypherUser(ClientError),
    CouldNotCreateCypherUser(ClientError),
    CypherUserNotFound,
    ChannelSend,
    CouldNotFindHandler,
    UserNotAvailable,
    GroupNotAvailable,
    OpenOrdersNotAvailable,
    OrderBookNotAvailable,
    InvalidOrderId(u128),
    TransactionSubmission(ClientError)
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    
    // load keypair
    let keypair_path = args.keypair.as_path().to_str().unwrap();    
    println!("Loading keypair from: {}", keypair_path);

    let keypair = load_keypair(keypair_path).unwrap();
    let pubkey = keypair.pubkey();
    println!("Loaded keypair with pubkey: {}", pubkey);
    
    let cypher_config = Arc::new(load_cypher_config(CYPHER_CONFIG_PATH).unwrap());
    
    let cluster = args.cluster;
    let group_name = args.group;
    let cluster_config = cypher_config.get_config_for_cluster(&cluster);
    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        cluster_config.rpc_url.to_string(),
        CommitmentConfig::confirmed(),
    ));
    println!("Connecting to cluster: {}", cluster);
    println!("Using group: {}", group_name);

    let group_config = Arc::new(cypher_config.get_group(&group_name).unwrap());
    let cypher_group_pk = Pubkey::from_str(&group_config.address).unwrap();
    let cypher_user_pk = derive_cypher_user_address(&cypher_group_pk, &keypair.pubkey()).0;

    let cypher_user_res = get_or_init_cypher_user(
        &keypair,
        &cypher_group_pk,
        &cypher_user_pk,
        Arc::clone(&rpc_client),
        cluster.to_string()
    ).await;

    match cypher_user_res {
        Ok(_) => {
            println!("Successfully fetched cypher user account with key: {}", cypher_user_pk);            
        },
        Err(e) => {
            println!("There was an error getting or creating the cypher user account. {:?}", e);
            return;
        }
    }

    let (shutdown_send, mut _shutdown_recv) = channel::<bool>(1);
    let arc_kp = Arc::new(keypair);

    let interactive = InteractiveCli::new(
        Arc::clone(&cypher_config),
        cluster.clone(),
        group_name.clone(),
        Arc::clone(&rpc_client),
        shutdown_send.clone(),
        Arc::clone(&arc_kp),
        cypher_user_pk,
        cypher_group_pk,
    );
    
    tokio::select! {
        cli_res = interactive.start() => {
            match cli_res {
                Ok(_) => (),
                Err(e) => {
                    println!("An error occurred while running the application loop: {:?}", e);
                }
            }
        },
        _ = tokio::signal::ctrl_c() => {
            match shutdown_send.send(true) {
                Ok(_) => {
                    println!("Sucessfully sent shutdown signal. Waiting for tasks to complete...")
                },
                Err(e) => {
                    println!("Failed to send shutdown error: {}", e);
                }
            };
        },
    }
}

fn load_keypair(path: &str) -> Result<Keypair, CypherInteractiveError> {

    let fd = File::open(path);

    let mut file = match fd {
        Ok(f) => f,
        Err(e) => {
            println!("Failed to load keypair file: {}", e);
            return Err(CypherInteractiveError::KeypairFileOpen);
        }
    };

    let file_string = &mut String::new();
    let file_read_res = file.read_to_string(file_string);

    let _ = if let Err(e) = file_read_res {
        println!("Failed to read keypair bytes from keypair file: {}", e);
        return Err(CypherInteractiveError::KeypairFileRead);
    };

    let keypair_bytes: Vec<u8> = file_string
        .replace('[', "")
        .replace(']', "")
        .replace(',', " ")
        .split(' ')
        .map(|x| u8::from_str(x).unwrap())
        .collect();

    let keypair = Keypair::from_bytes(keypair_bytes.as_ref());

    match keypair {
        Ok(kp) => Ok(kp),
        Err(e) => {
            println!("Failed to load keypair from bytes: {}", e);
            Err(CypherInteractiveError::KeypairLoad)
        }
    }
}