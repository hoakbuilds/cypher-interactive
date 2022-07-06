mod config;
mod interactive;
mod fast_tx_builder;

use config::*;
use interactive::*;
use fast_tx_builder::*;

use clap::Parser;
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::broadcast::channel;
use std::{fs::File, str::FromStr, io::{Read, self}, sync::Arc};
use solana_sdk::{signature::Keypair, signer::Signer, commitment_config::CommitmentConfig};

pub const CYPHER_CONFIG_PATH: &str = "./cfg/group.json";

#[derive(Parser)]
struct Cli {
    #[clap(short = 'k', long = "keypair", parse(from_os_str))]
    keypair: std::path::PathBuf,

    #[clap(short = 'c', long = "cluster")]
    cluster: String
}

#[derive(Debug)]
pub enum CypherInteractiveError {
    KeypairFileOpenError,
    KeypairFileReadError,
    KeypairLoadError,
    InputError,
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
    let cluster_config = cypher_config.get_config_for_cluster(&cluster);
    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        cluster_config.rpc_url.to_string(),
        CommitmentConfig::confirmed(),
    ));
    println!("Connecting to cluster: {}", cluster);
    
    let (shutdown_send, mut _shutdown_recv) = channel::<bool>(1);

    let interactive = Interactive::new(
        Arc::clone(&cypher_config),
        Arc::clone(&rpc_client),
        shutdown_send.clone()
    );

    let res = interactive.run().await;
    match res {
        Ok(_) => (),
        Err(e) => {
            println!("There was an error while running the interactive command line: {:?}", e);
        }
    }

}



fn load_keypair(path: &str) -> Result<Keypair, CypherInteractiveError> {

    let fd = File::open(path);

    let mut file = match fd {
        Ok(f) => f,
        Err(e) => {
            println!("Failed to load keypair file: {}", e.to_string());
            return Err(CypherInteractiveError::KeypairFileOpenError);
        }
    };

    let file_string = &mut String::new();
    let file_read_res = file.read_to_string(file_string);

    let _ = if let Err(e) = file_read_res {
        println!(
            "Failed to read keypair bytes from keypair file: {}",
            e.to_string()
        );
        return Err(CypherInteractiveError::KeypairFileReadError);
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
            println!("Failed to load keypair from bytes: {}", e.to_string());
            Err(CypherInteractiveError::KeypairLoadError)
        }
    }
}