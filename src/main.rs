mod accounts_cache;
mod config;
mod cypher_context;
mod fast_tx_builder;
mod interactive_cli;
mod market_handler;
mod providers;
mod serum_slab;
mod services;
mod utils;

use config::*;

use clap::Parser;
use cypher::utils::derive_cypher_user_address;
use solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient};
use solana_sdk::{
    commitment_config::CommitmentConfig, pubkey::Pubkey, signature::Keypair, signer::Signer,
};
use std::{fs::File, io::Read, str::FromStr, sync::Arc};
use tokio::sync::broadcast::channel;

use crate::{interactive_cli::InteractiveCli, utils::get_or_init_cypher_user};

pub const CYPHER_CONFIG_PATH: &str = "./cfg/group.json";

#[derive(Parser)]
struct Cli {
    #[clap(short = 'u', long = "user-keypair", parse(from_os_str))]
    user_keypair: std::path::PathBuf,

    #[clap(short = 's', long = "signer-keypair", parse(from_os_str))]
    signer_keypair: std::path::PathBuf,

    #[clap(short = 'c', long = "cluster")]
    cluster: String,

    #[clap(short = 'g', long = "group")]
    group: String,
}

#[derive(Debug)]
pub enum CypherInteractiveError {
    KeypairFileOpen,
    KeypairFileRead,
    KeypairLoad,
    Input,
    Airdrop,
    Deposit,
    SetDelegate,
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
    TransactionSubmission(ClientError),
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();

    // load keypairs
    let user_keypair_path = args.user_keypair.as_path().to_str().unwrap();
    println!("Loading user keypair from: {}", user_keypair_path);

    let user_keypair = load_keypair(user_keypair_path).unwrap();
    let user_pubkey = user_keypair.pubkey();
    println!("Loaded user keypair with pubkey: {}", user_pubkey);

    let signer_keypair_path = args.signer_keypair.as_path().to_str().unwrap();
    println!("Loading signer keypair from: {}", signer_keypair_path);

    let signer_keypair = load_keypair(signer_keypair_path).unwrap();
    let signer_pubkey = signer_keypair.pubkey();
    println!("Loaded signer keypair with pubkey: {}", signer_pubkey);

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
    let cypher_user_pk = derive_cypher_user_address(&cypher_group_pk, &user_keypair.pubkey()).0;

    let cypher_user_res = get_or_init_cypher_user(
        &user_keypair,
        &cypher_group_pk,
        &cypher_user_pk,
        Arc::clone(&rpc_client),
        cluster.to_string(),
    )
    .await;

    match cypher_user_res {
        Ok(_) => {
            println!(
                "Successfully fetched cypher user account with key: {}",
                cypher_user_pk
            );
        }
        Err(e) => {
            println!(
                "There was an error getting or creating the cypher user account. {:?}",
                e
            );
            return;
        }
    }

    let (shutdown_send, mut _shutdown_recv) = channel::<bool>(1);
    let arc_kp = Arc::new(signer_keypair);

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
