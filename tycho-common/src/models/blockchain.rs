use std::{
    any::Any,
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    models::{
        contract::{AccountBalance, AccountChangesWithTx, AccountDelta},
        protocol::{
            ComponentBalance, ProtocolChangesWithTx, ProtocolComponent, ProtocolComponentStateDelta,
        },
        token::CurrencyToken,
        Address, Chain, ComponentId, ExtractorIdentity, NormalisedMessage,
    },
    Bytes,
};

#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Debug)]
pub struct Block {
    pub number: u64,
    pub chain: Chain,
    pub hash: Bytes,
    pub parent_hash: Bytes,
    pub ts: NaiveDateTime,
}

impl Block {
    pub fn new(
        number: u64,
        chain: Chain,
        hash: Bytes,
        parent_hash: Bytes,
        ts: NaiveDateTime,
    ) -> Self {
        Block { hash, parent_hash, number, chain, ts }
    }
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Transaction {
    pub hash: Bytes,
    pub block_hash: Bytes,
    pub from: Bytes,
    pub to: Option<Bytes>,
    pub index: u64,
}

impl Transaction {
    pub fn new(hash: Bytes, block_hash: Bytes, from: Bytes, to: Option<Bytes>, index: u64) -> Self {
        Transaction { hash, block_hash, from, to, index }
    }
}

pub struct BlockTransactionDeltas<T> {
    pub extractor: String,
    pub chain: Chain,
    pub block: Block,
    pub revert: bool,
    pub deltas: Vec<TransactionDeltaGroup<T>>,
}

#[allow(dead_code)]
pub struct TransactionDeltaGroup<T> {
    changes: T,
    protocol_component: HashMap<String, ProtocolComponent>,
    component_balances: HashMap<String, ComponentBalance>,
    component_tvl: HashMap<String, f64>,
    tx: Transaction,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockAggregatedChanges {
    pub extractor: String,
    pub chain: Chain,
    pub block: Block,
    pub finalized_block_height: u64,
    pub revert: bool,
    pub state_deltas: HashMap<String, ProtocolComponentStateDelta>,
    pub account_deltas: HashMap<Bytes, AccountDelta>,
    pub new_tokens: HashMap<Address, CurrencyToken>,
    pub new_protocol_components: HashMap<String, ProtocolComponent>,
    pub deleted_protocol_components: HashMap<String, ProtocolComponent>,
    pub component_balances: HashMap<ComponentId, HashMap<Bytes, ComponentBalance>>,
    pub account_balances: HashMap<Address, HashMap<Address, AccountBalance>>,
    pub component_tvl: HashMap<String, f64>,
}

impl BlockAggregatedChanges {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        extractor: &str,
        chain: Chain,
        block: Block,
        finalized_block_height: u64,
        revert: bool,
        state_deltas: HashMap<String, ProtocolComponentStateDelta>,
        account_deltas: HashMap<Bytes, AccountDelta>,
        new_tokens: HashMap<Address, CurrencyToken>,
        new_components: HashMap<String, ProtocolComponent>,
        deleted_components: HashMap<String, ProtocolComponent>,
        component_balances: HashMap<ComponentId, HashMap<Bytes, ComponentBalance>>,
        account_balances: HashMap<Address, HashMap<Address, AccountBalance>>,
        component_tvl: HashMap<String, f64>,
    ) -> Self {
        Self {
            extractor: extractor.to_string(),
            chain,
            block,
            finalized_block_height,
            revert,
            state_deltas,
            account_deltas,
            new_tokens,
            new_protocol_components: new_components,
            deleted_protocol_components: deleted_components,
            component_balances,
            account_balances,
            component_tvl,
        }
    }
}

impl std::fmt::Display for BlockAggregatedChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "block_number: {}, extractor: {}", self.block.number, self.extractor)
    }
}

#[typetag::serde]
impl NormalisedMessage for BlockAggregatedChanges {
    fn source(&self) -> ExtractorIdentity {
        ExtractorIdentity::new(self.chain, &self.extractor)
    }

    fn drop_state(&self) -> Arc<dyn NormalisedMessage> {
        Arc::new(Self {
            extractor: self.extractor.clone(),
            chain: self.chain,
            block: self.block.clone(),
            finalized_block_height: self.finalized_block_height,
            revert: self.revert,
            account_deltas: HashMap::new(),
            state_deltas: HashMap::new(),
            new_tokens: self.new_tokens.clone(),
            new_protocol_components: self.new_protocol_components.clone(),
            deleted_protocol_components: self.deleted_protocol_components.clone(),
            component_balances: self.component_balances.clone(),
            account_balances: self.account_balances.clone(),
            component_tvl: self.component_tvl.clone(),
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub trait BlockScoped {
    fn block(&self) -> Block;
}

impl BlockScoped for BlockAggregatedChanges {
    fn block(&self) -> Block {
        self.block.clone()
    }
}

/// Changes grouped by their respective transaction.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxWithChanges {
    pub protocol_components: HashMap<ComponentId, ProtocolComponent>,
    pub account_deltas: HashMap<Address, AccountDelta>,
    pub state_updates: HashMap<ComponentId, ProtocolComponentStateDelta>,
    pub balance_changes: HashMap<ComponentId, HashMap<Address, ComponentBalance>>,
    pub account_balance_changes: HashMap<Address, HashMap<Address, AccountBalance>>,
    pub tx: Transaction,
}

impl TxWithChanges {
    pub fn new(
        protocol_components: HashMap<ComponentId, ProtocolComponent>,
        account_deltas: HashMap<Address, AccountDelta>,
        protocol_states: HashMap<ComponentId, ProtocolComponentStateDelta>,
        balance_changes: HashMap<ComponentId, HashMap<Address, ComponentBalance>>,
        account_balance_changes: HashMap<Address, HashMap<Address, AccountBalance>>,
        tx: Transaction,
    ) -> Self {
        Self {
            account_deltas,
            protocol_components,
            state_updates: protocol_states,
            balance_changes,
            account_balance_changes,
            tx,
        }
    }

    /// Merges this update with another one.
    ///
    /// The method combines two `ChangesWithTx` instances if they are for the same
    /// transaction.
    ///
    /// NB: It is assumed that `other` is a more recent update than `self` is and the two are
    /// combined accordingly.
    ///
    /// # Errors
    /// This method will return an error if any of the above conditions is violated.
    pub fn merge(&mut self, other: TxWithChanges) -> Result<(), String> {
        if self.tx.block_hash != other.tx.block_hash {
            return Err(format!(
                "Can't merge TxWithChanges from different blocks: 0x{:x} != 0x{:x}",
                self.tx.block_hash, other.tx.block_hash,
            ));
        }
        if self.tx.hash == other.tx.hash {
            return Err(format!(
                "Can't merge TxWithChanges from the same transaction: 0x{:x}",
                self.tx.hash
            ));
        }
        if self.tx.index > other.tx.index {
            return Err(format!(
                "Can't merge TxWithChanges with lower transaction index: {} > {}",
                self.tx.index, other.tx.index
            ));
        }

        self.tx = other.tx;

        // Merge new protocol components
        // Log a warning if a new protocol component for the same id already exists, because this
        // should never happen.
        for (key, value) in other.protocol_components {
            match self.protocol_components.entry(key) {
                Entry::Occupied(mut entry) => {
                    warn!(
                        "Overwriting new protocol component for id {} with a new one. This should never happen! Please check logic",
                        entry.get().id
                    );
                    entry.insert(value);
                }
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
            }
        }

        // Merge Account Updates
        for (address, update) in other.account_deltas.clone().into_iter() {
            match self.account_deltas.entry(address) {
                Entry::Occupied(mut e) => {
                    e.get_mut().merge(update)?;
                }
                Entry::Vacant(e) => {
                    e.insert(update);
                }
            }
        }

        // Merge Protocol States
        for (key, value) in other.state_updates {
            match self.state_updates.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry.get_mut().merge(value)?;
                }
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
            }
        }

        // Merge component balance changes
        for (component_id, balance_changes) in other.balance_changes {
            let token_balances = self
                .balance_changes
                .entry(component_id)
                .or_default();
            for (token, balance) in balance_changes {
                token_balances.insert(token, balance);
            }
        }

        // Merge account balance changes
        for (account_addr, balance_changes) in other.account_balance_changes {
            let token_balances = self
                .account_balance_changes
                .entry(account_addr)
                .or_default();
            for (token, balance) in balance_changes {
                token_balances.insert(token, balance);
            }
        }

        Ok(())
    }
}

impl From<AccountChangesWithTx> for TxWithChanges {
    fn from(value: AccountChangesWithTx) -> Self {
        Self {
            protocol_components: value.protocol_components,
            account_deltas: value.account_deltas,
            state_updates: HashMap::new(),
            balance_changes: value.component_balances,
            account_balance_changes: value.account_balances,
            tx: value.tx,
        }
    }
}

impl From<ProtocolChangesWithTx> for TxWithChanges {
    fn from(value: ProtocolChangesWithTx) -> Self {
        Self {
            protocol_components: value.new_protocol_components,
            account_deltas: HashMap::new(),
            state_updates: value.protocol_states,
            balance_changes: value.balance_changes,
            account_balance_changes: HashMap::new(),
            tx: value.tx,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum BlockTag {
    /// Finalized block
    Finalized,
    /// Safe block
    Safe,
    /// Latest block
    Latest,
    /// Earliest block (genesis)
    Earliest,
    /// Pending block (not yet part of the blockchain)
    Pending,
    /// Block by number
    Number(u64),
}

#[cfg(test)]
pub mod fixtures {
    use std::str::FromStr;

    use super::*;
    use crate::models::ChangeType;

    pub fn transaction01() -> Transaction {
        Transaction::new(
            Bytes::zero(32),
            Bytes::zero(32),
            Bytes::zero(20),
            Some(Bytes::zero(20)),
            10,
        )
    }

    pub fn create_transaction(hash: &str, block: &str, index: u64) -> Transaction {
        Transaction::new(
            hash.parse().unwrap(),
            block.parse().unwrap(),
            Bytes::zero(20),
            Some(Bytes::zero(20)),
            index,
        )
    }

    #[test]
    fn test_merge_tx_with_changes() {
        let component_id = "ambient_USDC_ETH".to_string();
        let base_token = Bytes::from_str("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let quote_token = Bytes::from_str("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48").unwrap();
        let contract_addr = Bytes::from_str("aaaaaaaaa24eeeb8d57d431224f73832bc34f688").unwrap();
        let tx_hash0 = "0x2f6350a292c0fc918afe67cb893744a080dacb507b0cea4cc07437b8aff23cdb";
        let tx_hash1 = "0x0d9e0da36cf9f305a189965b248fc79c923619801e8ab5ef158d4fd528a291ad";
        let block = "0x0000000000000000000000000000000000000000000000000000000000000000";
        let mut changes1 = TxWithChanges::new(
            HashMap::from([(
                component_id.clone(),
                ProtocolComponent {
                    id: component_id.clone(),
                    protocol_system: "test".to_string(),
                    protocol_type_name: "vm:pool".to_string(),
                    chain: Chain::Ethereum,
                    tokens: vec![base_token.clone(), quote_token.clone()],
                    contract_addresses: vec![contract_addr.clone()],
                    static_attributes: Default::default(),
                    change: Default::default(),
                    creation_tx: Bytes::from_str(tx_hash0).unwrap(),
                    created_at: Default::default(),
                },
            )]),
            [(
                contract_addr.clone(),
                AccountDelta::new(
                    Chain::Ethereum,
                    contract_addr.clone(),
                    HashMap::new(),
                    None,
                    Some(vec![0, 0, 0, 0].into()),
                    ChangeType::Creation,
                ),
            )]
            .into_iter()
            .collect(),
            HashMap::new(),
            HashMap::from([(
                component_id.clone(),
                HashMap::from([(
                    base_token.clone(),
                    ComponentBalance {
                        token: base_token.clone(),
                        balance: Bytes::from(800_u64).lpad(32, 0),
                        balance_float: 800.0,
                        component_id: component_id.clone(),
                        modify_tx: Bytes::from_str(tx_hash0).unwrap(),
                    },
                )]),
            )]),
            HashMap::from([(
                contract_addr.clone(),
                HashMap::from([(
                    base_token.clone(),
                    AccountBalance {
                        token: base_token.clone(),
                        balance: Bytes::from(800_u64).lpad(32, 0),
                        modify_tx: Bytes::from_str(tx_hash0).unwrap(),
                        account: contract_addr.clone(),
                    },
                )]),
            )]),
            create_transaction(tx_hash0, block, 1),
        );
        let changes2 = TxWithChanges::new(
            HashMap::from([(
                component_id.clone(),
                ProtocolComponent {
                    id: component_id.clone(),
                    protocol_system: "test".to_string(),
                    protocol_type_name: "vm:pool".to_string(),
                    chain: Chain::Ethereum,
                    tokens: vec![base_token.clone(), quote_token],
                    contract_addresses: vec![contract_addr.clone()],
                    static_attributes: Default::default(),
                    change: Default::default(),
                    creation_tx: Bytes::from_str(tx_hash1).unwrap(),
                    created_at: Default::default(),
                },
            )]),
            [(
                contract_addr.clone(),
                AccountDelta::new(
                    Chain::Ethereum,
                    contract_addr.clone(),
                    HashMap::new(),
                    None,
                    Some(vec![0, 0, 0, 0].into()),
                    ChangeType::Creation,
                ),
            )]
            .into_iter()
            .collect(),
            HashMap::new(),
            HashMap::from([(
                component_id.clone(),
                HashMap::from([(
                    base_token.clone(),
                    ComponentBalance {
                        token: base_token.clone(),
                        balance: Bytes::from(1000_u64).lpad(32, 0),
                        balance_float: 1000.0,
                        component_id: component_id.clone(),
                        modify_tx: Bytes::from_str(tx_hash1).unwrap(),
                    },
                )]),
            )]),
            HashMap::from([(
                contract_addr.clone(),
                HashMap::from([(
                    base_token.clone(),
                    AccountBalance {
                        token: base_token.clone(),
                        balance: Bytes::from(1000_u64).lpad(32, 0),
                        modify_tx: Bytes::from_str(tx_hash1).unwrap(),
                        account: contract_addr.clone(),
                    },
                )]),
            )]),
            create_transaction(tx_hash1, block, 2),
        );

        assert!(changes1.merge(changes2).is_ok());
        assert_eq!(
            changes1
                .account_balance_changes
                .get(&contract_addr)
                .unwrap()
                .get(&base_token)
                .unwrap()
                .balance,
            Bytes::from(1000_u64).lpad(32, 0),
        );
        assert_eq!(
            changes1
                .balance_changes
                .get(&component_id)
                .unwrap()
                .get(&base_token)
                .unwrap()
                .balance,
            Bytes::from(1000_u64).lpad(32, 0),
        );
        assert_eq!(changes1.tx.hash, Bytes::from_str(tx_hash1).unwrap(),);
    }

    #[test]
    fn test_merge_different_blocks() {
        let mut tx1 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x01", "0x0abc", 1),
        );

        let tx2 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x02", "0x0def", 2),
        );

        assert!(tx1.merge(tx2).is_err());
    }

    #[test]
    fn test_merge_same_transaction() {
        let mut tx1 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x01", "0x0abc", 1),
        );

        let tx2 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x01", "0x0abc", 1),
        );

        assert!(tx1.merge(tx2).is_err());
    }

    #[test]
    fn test_merge_lower_transaction_index() {
        let mut tx1 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x02", "0x0abc", 2),
        );

        let tx2 = TxWithChanges::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixtures::create_transaction("0x01", "0x0abc", 1),
        );

        assert!(tx1.merge(tx2).is_err());
    }
}
