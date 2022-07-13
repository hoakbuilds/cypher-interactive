use solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast::{channel, Receiver};
use tokio::sync::Mutex;
use tokio::{
    sync::RwLock,
    time::{sleep, Duration},
};

pub struct ChainMetaService {
    client: Arc<RpcClient>,
    recent_blockhash: RwLock<Hash>,
    slot: RwLock<u64>,
    shutdown_receiver: Mutex<Receiver<bool>>,
}

impl ChainMetaService {
    pub fn default() -> Self {
        Self {
            client: Arc::new(RpcClient::new("http://localhost:8899".to_string())),
            recent_blockhash: RwLock::new(Hash::default()),
            slot: RwLock::new(u64::default()),
            shutdown_receiver: Mutex::new(channel::<bool>(1).1),
        }
    }

    pub fn new(client: Arc<RpcClient>, shutdown_receiver: Receiver<bool>) -> ChainMetaService {
        ChainMetaService {
            client,
            shutdown_receiver: Mutex::new(shutdown_receiver),
            ..ChainMetaService::default()
        }
    }

    #[inline(always)]
    async fn update_chain_meta(self: &Arc<Self>) -> Result<(), ClientError> {
        let hash_res = self
            .client
            .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
            .await;
        let hash = match hash_res {
            Ok(hash) => hash,
            Err(e) => {
                return Err(e);
            }
        };
        *self.recent_blockhash.write().await = hash.0;

        let slot_res = self.client.get_slot().await;
        let slot = match slot_res {
            Ok(slot) => slot,
            Err(e) => {
                return Err(e);
            }
        };
        *self.slot.write().await = slot;

        Ok(())
    }

    #[inline(always)]
    async fn update_chain_meta_replay(self: Arc<Self>) {
        loop {
            _ = self.update_chain_meta().await;

            sleep(Duration::from_millis(2500)).await;
        }
    }

    #[inline(always)]
    pub async fn start_service(self: &Arc<Self>) {
        let aself = self.clone();

        let cself = Arc::clone(&aself);
        let mut shutdown = aself.shutdown_receiver.lock().await;
        tokio::select! {
            _ = cself.update_chain_meta_replay() => {},
            _ = shutdown.recv() => {}
        }
    }

    #[inline(always)]
    pub async fn get_latest_blockhash(self: &Arc<Self>) -> Hash {
        //Copy and return hash
        *self.recent_blockhash.read().await
    }
}
