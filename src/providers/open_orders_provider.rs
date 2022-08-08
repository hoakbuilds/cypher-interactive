use {
    crate::{accounts_cache::AccountsCache, CypherInteractiveError},
    cypher::utils::parse_dex_account,
    serum_dex::state::OpenOrders,
    solana_sdk::pubkey::Pubkey,
    std::sync::Arc,
    tokio::sync::{
        broadcast::{channel, Receiver, Sender},
        Mutex,
    },
};

#[derive(Clone)]
pub struct OpenOrdersContext {
    pub open_orders: OpenOrders,
    pub pubkey: Pubkey,
}

pub struct OpenOrdersProvider {
    cache: Arc<AccountsCache>,
    sender: Arc<Sender<OpenOrdersContext>>,
    receiver: Mutex<Receiver<Pubkey>>,
    shutdown_receiver: Mutex<Receiver<bool>>,
    open_orders_pks: Vec<Pubkey>,
}

impl OpenOrdersProvider {
    pub fn default() -> Self {
        Self {
            cache: Arc::new(AccountsCache::default()),
            sender: Arc::new(channel::<OpenOrdersContext>(u16::MAX as usize).0),
            receiver: Mutex::new(channel::<Pubkey>(u16::MAX as usize).1),
            shutdown_receiver: Mutex::new(channel::<bool>(1).1),
            open_orders_pks: Vec::new(),
        }
    }

    pub fn new(
        cache: Arc<AccountsCache>,
        sender: Arc<Sender<OpenOrdersContext>>,
        receiver: Receiver<Pubkey>,
        shutdown_receiver: Receiver<bool>,
        open_orders_pks: Vec<Pubkey>,
    ) -> Self {
        Self {
            cache,
            sender,
            receiver: Mutex::new(receiver),
            shutdown_receiver: Mutex::new(shutdown_receiver),
            open_orders_pks,
        }
    }

    pub async fn start(self: &Arc<Self>) {
        let mut receiver = self.receiver.lock().await;
        let mut shutdown = self.shutdown_receiver.lock().await;
        let mut shutdown_signal: bool = false;

        loop {
            tokio::select! {
                key = receiver.recv() => {
                    if key.is_err() {
                        continue;
                    } else {
                        _ = self.process_updates(key.unwrap()).await;
                    }
                },
                _ = shutdown.recv() => {
                    shutdown_signal = true;
                }
            }

            if shutdown_signal {
                break;
            }
        }
    }

    async fn process_updates(&self, key: Pubkey) -> Result<(), CypherInteractiveError> {
        for oo_pk in &self.open_orders_pks {
            if key == *oo_pk {
                let ai = self.cache.get(&key).unwrap();

                let dex_open_orders: OpenOrders = parse_dex_account(ai.account.data.to_vec());

                match self.sender.send(OpenOrdersContext {
                    open_orders: dex_open_orders,
                    pubkey: key,
                }) {
                    Ok(_) => {
                        return Ok(());
                    }
                    Err(_) => {
                        return Err(CypherInteractiveError::ChannelSend);
                    }
                }
            }
        }

        Ok(())
    }
}
