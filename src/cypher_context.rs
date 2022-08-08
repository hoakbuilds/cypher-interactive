use cypher::{CypherGroup, CypherUser};
use std::sync::Arc;
use tokio::{
    select,
    sync::{
        broadcast::{channel, Receiver},
        Mutex, RwLock,
    },
};

use crate::CypherInteractiveError;

pub struct CypherContext {
    shutdown: Mutex<Receiver<bool>>,
    cypher_user_provider_sender: Mutex<Receiver<Box<CypherUser>>>,
    cypher_user: RwLock<Option<CypherUser>>,
    cypher_group_provider_sender: Mutex<Receiver<Box<CypherGroup>>>,
    cypher_group: RwLock<Option<CypherGroup>>,
}

impl CypherContext {
    pub fn default() -> Self {
        Self {
            shutdown: Mutex::new(channel::<bool>(1).1),
            cypher_user_provider_sender: Mutex::new(
                channel::<Box<CypherUser>>(u16::MAX as usize).1,
            ),
            cypher_user: RwLock::new(None),
            cypher_group_provider_sender: Mutex::new(
                channel::<Box<CypherGroup>>(u16::MAX as usize).1,
            ),
            cypher_group: RwLock::new(None),
        }
    }

    pub fn new(
        shutdown: Receiver<bool>,
        cypher_user_provider_sender: Receiver<Box<CypherUser>>,
        cypher_group_provider_sender: Receiver<Box<CypherGroup>>,
    ) -> Self {
        Self {
            shutdown: Mutex::new(shutdown),
            cypher_user_provider_sender: Mutex::new(cypher_user_provider_sender),
            cypher_user: RwLock::new(None),
            cypher_group_provider_sender: Mutex::new(cypher_group_provider_sender),
            cypher_group: RwLock::new(None),
        }
    }

    pub async fn start(self: &Arc<Self>) {
        let mut shutdown = self.shutdown.lock().await;
        let mut ca_receiver = self.cypher_user_provider_sender.lock().await;
        let mut cg_receiver = self.cypher_group_provider_sender.lock().await;
        let mut shutdown_signal: bool = false;

        loop {
            select! {
                cypher_user = ca_receiver.recv() => {
                    if cypher_user.is_ok() {
                        *self.cypher_user.write().await = Some(*cypher_user.unwrap());
                    } else {
                        println!("[CC] Error updating cypher account: {:?}", cypher_user.err());
                    }
                },
                cypher_group = cg_receiver.recv() => {
                    if cypher_group.is_ok() {
                        *self.cypher_group.write().await = Some(*cypher_group.unwrap());
                    } else {
                        println!("[CC] Error updating cypher group: {:?}", cypher_group.err());
                    }
                },
                _ = shutdown.recv() => {
                    shutdown_signal = true;
                }
            }

            if shutdown_signal {
                println!("[CC] Received shutdown signal, stopping.");
                break;
            }
        }
    }

    pub async fn get_user(self: &Arc<Self>) -> Result<CypherUser, CypherInteractiveError> {
        let maybe_user = self.cypher_user.read().await;
        let user = match *maybe_user {
            Some(u) => u,
            None => {
                return Err(CypherInteractiveError::UserNotAvailable);
            }
        };

        Ok(user)
    }

    pub async fn get_group(self: &Arc<Self>) -> Result<CypherGroup, CypherInteractiveError> {
        let maybe_group = self.cypher_group.read().await;
        let group = match *maybe_group {
            Some(u) => u,
            None => {
                return Err(CypherInteractiveError::GroupNotAvailable);
            }
        };

        Ok(group)
    }
}
