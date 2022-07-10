use arrayref::array_refs;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::{
    broadcast::{channel, Receiver, Sender},
    Mutex,
};

use crate::{
    accounts_cache::AccountsCache,
    serum_slab::{OrderBookOrder, Slab},
    CypherInteractiveError,
};

#[derive(Default)]
pub struct OrderBookContext {
    pub market: Pubkey,
    pub bids: Pubkey,
    pub asks: Pubkey,
    pub coin_lot_size: u64,
    pub pc_lot_size: u64,
}

#[derive(Default)]
pub struct OrderBook {
    pub market: Pubkey,
    pub bids: RwLock<Vec<OrderBookOrder>>,
    pub asks: RwLock<Vec<OrderBookOrder>>,
}

impl OrderBook {
    pub fn new(market: Pubkey) -> Self {
        Self {
            market: Pubkey::default(),
            bids: RwLock::new(Vec::new()),
            asks: RwLock::new(Vec::new()),
        }
    }
}

pub struct OrderBookProvider {
    cache: Arc<AccountsCache>,
    sender: Arc<Sender<Arc<OrderBook>>>,
    receiver: Mutex<Receiver<Pubkey>>,
    shutdown_receiver: Mutex<Receiver<bool>>,
    books_keys: Vec<OrderBookContext>,
    books: Vec<Arc<OrderBook>>,
}

impl OrderBookProvider {
    pub fn default() -> Self {
        Self {
            cache: Arc::new(AccountsCache::default()),
            sender: Arc::new(channel::<Arc<OrderBook>>(u16::MAX as usize).0),
            receiver: Mutex::new(channel::<Pubkey>(u16::MAX as usize).1),
            shutdown_receiver: Mutex::new(channel::<bool>(1).1),
            books_keys: Vec::new(),
            books: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cache: Arc<AccountsCache>,
        sender: Arc<Sender<Arc<OrderBook>>>,
        receiver: Receiver<Pubkey>,
        shutdown_receiver: Receiver<bool>,
        books: Vec<OrderBookContext>,
    ) -> Self {
        Self {
            cache,
            sender,
            receiver: Mutex::new(receiver),
            shutdown_receiver: Mutex::new(shutdown_receiver),
            books: Vec::new(),
            books_keys: books,
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
                        println!("[OBP] There was an error while processing a provider update, restarting loop.");
                        continue;
                    } else {
                        let res = self.process_updates(key.unwrap()).await;
                        match res {
                            Ok(_) => (),
                            Err(_) => {
                                println!("[OBP] There was an error sending an update about the orderbook.");
                            },
                        }
                    }
                },
                _ = shutdown.recv() => {
                    shutdown_signal = true;
                }
            }

            if shutdown_signal {
                println!("[OBP] Received shutdown signal, stopping.",);
                break;
            }
        }
    }

    #[allow(clippy::ptr_offset_with_cast)]
    async fn process_updates(self: &Arc<Self>, key: Pubkey) -> Result<(), CypherInteractiveError> {
        let mut updated: bool = false;

        for ob_keys in &self.books_keys {
            let ob = match self.books.iter().find(|ob| ob.market == ob_keys.market) {
                Some(ob) => ob,
                None => {
                    continue;
                }
            };

            if key == ob_keys.bids {
                let bid_ai = self.cache.get(&key).unwrap();

                let (_bid_head, bid_data, _bid_tail) = array_refs![&bid_ai.account.data, 5; ..; 7];
                let bid_data = &mut bid_data[8..].to_vec().clone();
                let bids = Slab::new(bid_data);

                let obl = bids.get_depth(25, ob_keys.pc_lot_size, ob_keys.coin_lot_size, false);

                *ob.bids.write().await = obl;
                updated = true;
            } else if key == ob_keys.asks {
                let ask_ai = self.cache.get(&key).unwrap();

                let (_ask_head, ask_data, _ask_tail) = array_refs![&ask_ai.account.data, 5; ..; 7];
                let ask_data = &mut ask_data[8..].to_vec().clone();
                let asks = Slab::new(ask_data);

                let obl = asks.get_depth(25, ob_keys.pc_lot_size, ob_keys.coin_lot_size, true);

                *ob.asks.write().await = obl;
                updated = true;
            }

            if updated {
                let res = self.sender.send(Arc::clone(&ob));

                match res {
                    Ok(_) => {
                        println!("[OBP] Updated orderbook for market: {}.", ob_keys.market);
                    }
                    Err(_) => {
                        return Err(CypherInteractiveError::ChannelSend);
                    }
                };
            }
        }

        Ok(())
    }
    
    pub fn subscribe(&self) -> Receiver<Arc<OrderBook>> {
        self.sender.subscribe()
    }
}
