use tokio::sync::{
    broadcast::{channel, Receiver},
    Mutex,
};

use {
    crate::accounts_cache::{AccountState, AccountsCache},
    solana_client::{client_error::ClientError, nonblocking::rpc_client::RpcClient},
    solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey},
    std::{sync::Arc, time::Duration},
    tokio::time::sleep,
};

pub struct AccountInfoService {
    cache: Arc<AccountsCache>,
    client: Arc<RpcClient>,
    keys: Vec<Pubkey>,
    shutdown_receiver: Mutex<Receiver<bool>>,
}

impl AccountInfoService {
    pub fn default() -> Self {
        Self {
            cache: Arc::new(AccountsCache::default()),
            client: Arc::new(RpcClient::new("http://localhost:8899".to_string())),
            keys: Vec::new(),
            shutdown_receiver: Mutex::new(channel::<bool>(1).1),
        }
    }

    pub fn new(
        cache: Arc<AccountsCache>,
        client: Arc<RpcClient>,
        keys: &[Pubkey],
        shutdown_receiver: Receiver<bool>,
    ) -> AccountInfoService {
        AccountInfoService {
            cache,
            client,
            keys: Vec::from(keys),
            shutdown_receiver: Mutex::new(shutdown_receiver),
        }
    }

    pub async fn start_service(self: &Arc<Self>) {
        let rpc_cloned_self = self.clone();

        for i in (0..self.keys.len()).step_by(100) {
            rpc_cloned_self
                .update_infos(i, self.keys.len().min(i + 100))
                .await
                .unwrap();
        }

        let cself = Arc::clone(&rpc_cloned_self);
        let mut shutdown = rpc_cloned_self.shutdown_receiver.lock().await;
        tokio::select! {
            _ = cself.update_infos_replay() => {},
            _ = shutdown.recv() => {
                println!("[AIS] Received shutdown signal, stopping.");
            }
        }
    }

    #[inline(always)]
    async fn update_infos(self: &Arc<Self>, from: usize, to: usize) -> Result<(), ClientError> {
        let account_keys = &self.keys[from..to];
        let rpc_result = self
            .client
            .get_multiple_accounts_with_commitment(account_keys, CommitmentConfig::confirmed())
            .await;

        let res = match rpc_result {
            Ok(r) => r,
            Err(e) => {
                return Err(e);
            }
        };

        let mut infos = res.value;

        while !infos.is_empty() {
            let next = infos.pop().unwrap();
            let i = infos.len();
            let key = account_keys[i];

            let println = match next {
                Some(ai) => ai,
                None => {
                    continue;
                }
            };

            _ = self.cache.insert(
                key,
                AccountState {
                    account: println,
                    slot: res.context.slot,
                },
            );
        }

        Ok(())
    }

    #[inline(always)]
    async fn update_infos_replay(self: Arc<Self>) {
        loop {
            let aself = self.clone();

            for i in (0..self.keys.len()).step_by(100) {
                _ = aself.update_infos(i, self.keys.len().min(i + 100)).await;
            }

            sleep(Duration::from_millis(500)).await;
        }
    }
}
