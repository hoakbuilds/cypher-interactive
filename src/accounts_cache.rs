use {
    dashmap::{mapref::one::Ref, DashMap},
    solana_sdk::account::Account,
    solana_sdk::pubkey::Pubkey,
    tokio::sync::broadcast::{channel, Sender},
};

pub struct AccountsCache {
    map: DashMap<Pubkey, AccountState>,
    sender: Sender<Pubkey>,
}

#[derive(Debug)]
pub struct AccountState {
    pub account: Account,
    pub slot: u64,
}

impl AccountsCache {
    pub fn default() -> Self {
        Self {
            map: DashMap::default(),
            sender: channel::<Pubkey>(u16::MAX as usize).0,
        }
    }

    pub fn new(sender: Sender<Pubkey>) -> Self {
        AccountsCache {
            map: DashMap::new(),
            sender,
        }
    }

    pub fn get(&self, key: &Pubkey) -> Option<Ref<'_, Pubkey, AccountState>> {
        self.map.get(key)
    }

    pub fn insert(&self, key: Pubkey, data: AccountState) -> Result<(), AccountsCacheError> {
        self.map.insert(key, data);

        match self.sender.send(key) {
            Ok(_) => {
                Ok(())
            }
            Err(_) => {
                println!(
                    "Failed to send message about updated account {}",
                    key.to_string()
                );
                Err(AccountsCacheError::ChannelSendError)
            }
        }
    }
}

pub enum AccountsCacheError {
    ChannelSendError,
}
