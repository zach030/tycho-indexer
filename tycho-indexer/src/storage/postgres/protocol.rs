#![allow(unused_variables)]
#![allow(unused_imports)]

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use ethers::types::H256;
use serde_json::Value;

use crate::{
    extractor::evm::{utils::TryDecode, ProtocolState},
    hex_bytes::Bytes,
    models::{Chain, ProtocolSystem, ProtocolType},
    storage::{
        postgres::{orm, schema, PostgresGateway},
        Address, BlockIdentifier, BlockOrTimestamp, ContractDelta, ProtocolGateway, StorableBlock,
        StorableContract, StorableProtocolState, StorableProtocolType, StorableToken,
        StorableTransaction, StorageError, TxHash, Version,
    },
};

// decode Protocol State query results
async fn decode_protocol_states(
    result: Result<Vec<orm::ProtocolState>, diesel::result::Error>,
    context: &str,
    conn: &mut AsyncPgConnection,
) -> Result<Vec<ProtocolState>, StorageError> {
    match result {
        Ok(states) => {
            let mut protocol_states: HashMap<String, ProtocolState> = HashMap::new();
            for state in states {
                let component_id = schema::protocol_component::table
                    .filter(schema::protocol_component::id.eq(state.protocol_component_id))
                    .select(schema::protocol_component::external_id)
                    .first::<String>(conn)
                    .await
                    .map_err(|err| {
                        StorageError::NoRelatedEntity(
                            "ProtocolComponent".to_owned(),
                            "ProtocolState".to_owned(),
                            state.id.to_string(),
                        )
                    })?;
                let tx_hash = schema::transaction::table
                    .filter(schema::transaction::id.eq(state.modify_tx))
                    .select(schema::transaction::hash)
                    .first::<Bytes>(conn)
                    .await
                    .map_err(|err| {
                        StorageError::NoRelatedEntity(
                            "Transaction".to_owned(),
                            "ProtocolState".to_owned(),
                            state.id.to_string(),
                        )
                    })?;

                let protocol_state =
                    ProtocolState::from_storage(state, component_id.clone(), &tx_hash)?;

                if let Some(existing_state) = protocol_states.get_mut(&component_id) {
                    // found a protocol state with a matching component id - merge states
                    dbg!(&existing_state);
                    dbg!(&protocol_state);
                    existing_state
                        .merge(protocol_state)
                        .map_err(|err| StorageError::DecodeError(err.to_string()))?;
                } else {
                    // no matching state found - add as a new state to the list
                    protocol_states.insert(component_id, protocol_state);
                }
            }

            let decoded_states: Vec<ProtocolState> = protocol_states.into_values().collect();
            Ok(decoded_states)
        }
        Err(err) => Err(StorageError::from_diesel(err, "ProtocolStates", context, None)),
    }
}

#[async_trait]
impl<B, TX, A, D, T> ProtocolGateway for PostgresGateway<B, TX, A, D, T>
where
    B: StorableBlock<orm::Block, orm::NewBlock, i64>,
    TX: StorableTransaction<orm::Transaction, orm::NewTransaction, i64>,
    D: ContractDelta + From<A>,
    A: StorableContract<orm::Contract, orm::NewContract, i64>,
    T: StorableToken<orm::Token, orm::NewToken, i64>,
{
    type DB = AsyncPgConnection;
    type Token = T;
    type ProtocolState = ProtocolState;
    type ProtocolType = ProtocolType;

    // TODO: uncomment to implement in ENG 2049
    // async fn get_components(
    //     &self,
    //     chain: &Chain,
    //     system: Option<ProtocolSystem>,
    //     ids: Option<&[&str]>,
    // ) -> Result<Vec<ProtocolComponent>, StorageError> {
    //     todo!()
    // }

    // TODO: uncomment to implement in ENG 2049
    // async fn upsert_components(&self, new: &[&ProtocolComponent]) -> Result<(), StorageError> {
    //     todo!()
    // }

    async fn upsert_protocol_type(
        &self,
        new: &Self::ProtocolType,
        conn: &mut Self::DB,
    ) -> Result<(), StorageError> {
        use super::schema::protocol_type::dsl::*;

        let values: orm::NewProtocolType = new.to_storage();

        diesel::insert_into(protocol_type)
            .values(&values)
            .on_conflict(name)
            .do_update()
            .set(&values)
            .execute(conn)
            .await
            .map_err(|err| StorageError::from_diesel(err, "ProtocolType", &values.name, None))?;

        Ok(())
    }

    // Gets all protocol states from the db filtered by chain, component ids and/or protocol system.
    async fn get_protocol_states(
        &self,
        chain: &Chain,
        at: Option<Version>,
        system: Option<ProtocolSystem>,
        ids: Option<&[&str]>,
        conn: &mut Self::DB,
    ) -> Result<Vec<Self::ProtocolState>, StorageError> {
        let chain_db_id = self.get_chain_id(chain);
        let version_ts = match &at {
            Some(version) => Some(version.to_ts(conn).await?),
            None => None,
        };

        match (ids, system) {
            (Some(ids), _) => {
                decode_protocol_states(
                    orm::ProtocolState::by_id(ids, chain_db_id, version_ts, conn).await,
                    ids.join(",").as_str(),
                    conn,
                )
                .await
            }
            (_, Some(system)) => {
                decode_protocol_states(
                    orm::ProtocolState::by_protocol_system(system, chain_db_id, version_ts, conn)
                        .await,
                    system.to_string().as_str(),
                    conn,
                )
                .await
            }
            _ => {
                dbg!(chain_db_id);
                decode_protocol_states(
                    orm::ProtocolState::by_chain(chain_db_id, version_ts, conn).await,
                    chain.to_string().as_str(),
                    conn,
                )
                .await
            }
        }
    }

    async fn update_protocol_states(
        &self,
        chain: &Chain,
        new: &[ProtocolState],
        conn: &mut Self::DB,
    ) -> Result<(), StorageError> {
        let chain_db_id = self.get_chain_id(chain);
        let txns: HashMap<H256, (i64, NaiveDateTime)> = orm::Transaction::by_hashes(
            new.iter()
                .map(|state| state.modify_tx.as_bytes())
                .collect::<Vec<&[u8]>>()
                .as_slice(),
            conn,
        )
        .await?
        .into_iter()
        .map(|(tx, ts)| {
            (H256::try_decode(&tx.hash, "tx hash").expect("Failed to decode tx hash"), (tx.id, ts))
        })
        .collect();

        let components: HashMap<String, i64> = orm::ProtocolComponent::by_external_ids(
            new.iter()
                .map(|state| state.component_id.as_str())
                .collect::<Vec<&str>>()
                .as_slice(),
            conn,
        )
        .await?
        .into_iter()
        .map(|component| (component.external_id.clone(), component.id))
        .collect();

        let mut state_data = Vec::new();

        for state in new {
            let tx_db = *txns
                .get(&state.modify_tx)
                .expect("Failed to find tx");
            let component_db_id = *components
                .get(&state.component_id)
                .expect("Failed to find component");
            let mut db_states = ProtocolState::to_storage(state, component_db_id, tx_db.0, tx_db.1);
            state_data.append(&mut db_states);
        }

        if !state_data.is_empty() {
            diesel::insert_into(schema::protocol_state::table)
                .values(&state_data)
                .execute(conn)
                .await?;
        }
        Ok(())
    }

    async fn get_tokens(
        &self,
        chain: Chain,
        address: Option<&[&Address]>,
        conn: &mut Self::DB,
    ) -> Result<Vec<Self::Token>, StorageError> {
        todo!()
    }

    async fn add_tokens(
        &self,
        chain: Chain,
        token: &[&Self::Token],
        conn: &mut Self::DB,
    ) -> Result<(), StorageError> {
        todo!()
    }

    async fn get_state_delta(
        &self,
        chain: &Chain,
        system: Option<ProtocolSystem>,
        id: Option<&[&str]>,
        start_version: Option<&BlockOrTimestamp>,
        end_version: &BlockOrTimestamp,
        conn: &mut Self::DB,
    ) -> Result<ProtocolState, StorageError> {
        todo!()
    }

    async fn revert_protocol_state(
        &self,
        to: &BlockIdentifier,
        conn: &mut Self::DB,
    ) -> Result<(), StorageError> {
        todo!()
    }

    async fn _get_or_create_protocol_system_id(
        &self,
        new: ProtocolSystem,
        conn: &mut Self::DB,
    ) -> Result<i64, StorageError> {
        use super::schema::protocol_system::dsl::*;
        let new_system = orm::ProtocolSystemType::from(new);

        let existing_entry = protocol_system
            .filter(name.eq(new_system.clone()))
            .first::<orm::ProtocolSystem>(conn)
            .await;

        if let Ok(entry) = existing_entry {
            return Ok(entry.id);
        } else {
            let new_entry = orm::NewProtocolSystem { name: new_system };

            let inserted_protocol_system = diesel::insert_into(protocol_system)
                .values(&new_entry)
                .get_result::<orm::ProtocolSystem>(conn)
                .await
                .map_err(|err| {
                    StorageError::from_diesel(err, "ProtocolSystem", &new.to_string(), None)
                })?;
            Ok(inserted_protocol_system.id)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
    use diesel_async::AsyncConnection;
    use ethers::types::U256;
    use serde_json::{json, Value};

    use crate::{
        extractor::evm,
        models,
        models::{FinancialType, ImplementationType},
        storage::postgres::{
            db_fixtures, orm,
            schema::{self, sql_types::ProtocolSystemType},
            PostgresGateway,
        },
    };

    use super::*;

    type EVMGateway = PostgresGateway<
        evm::Block,
        evm::Transaction,
        evm::Account,
        evm::AccountUpdate,
        evm::ERC20Token,
    >;

    async fn setup_db() -> AsyncPgConnection {
        let db_url = std::env::var("DATABASE_URL").unwrap();
        let mut conn = AsyncPgConnection::establish(&db_url)
            .await
            .unwrap();
        conn.begin_test_transaction()
            .await
            .unwrap();

        conn
    }

    /// This sets up the data needed to test the gateway. The setup is structured such that each
    /// protocol state's historical changes are kept together this makes it easy to reason about
    /// that change an account should have at each version Please not that if you change
    /// something here, also update the state fixtures right below, which contain protocol states
    /// at each version.     
    async fn setup_data(conn: &mut AsyncPgConnection) {
        let chain_id = db_fixtures::insert_chain(conn, "ethereum").await;
        let blk = db_fixtures::insert_blocks(conn, chain_id).await;
        let txn = db_fixtures::insert_txns(
            conn,
            &[
                (
                    blk[0],
                    1i64,
                    "0xbb7e16d797a9e2fbc537e30f91ed3d27a254dd9578aa4c3af3e5f0d3e8130945",
                ),
                (
                    blk[0],
                    2i64,
                    "0x794f7df7a3fe973f1583fbb92536f9a8def3a89902439289315326c04068de54",
                ),
                // ----- Block 01 LAST
                (
                    blk[1],
                    1i64,
                    "0x3108322284d0a89a7accb288d1a94384d499504fe7e04441b0706c7628dee7b7",
                ),
                (
                    blk[1],
                    2i64,
                    "0x50449de1973d86f21bfafa7c72011854a7e33a226709dc3e2e4edcca34188388",
                ),
                // ----- Block 02 LAST
            ],
        )
        .await;
        let protocol_system_id =
            db_fixtures::insert_protocol_system(conn, orm::ProtocolSystemType::Ambient).await;
        let protocol_type_id = db_fixtures::insert_protocol_type(
            conn,
            "Pool",
            orm::FinancialType::Swap,
            orm::ImplementationType::Custom,
        )
        .await;
        let protocol_component_id = db_fixtures::insert_protocol_component(
            conn,
            "state1",
            chain_id,
            protocol_system_id,
            protocol_type_id,
            txn[0],
        )
        .await;

        // protocol state for state1-reserve1
        db_fixtures::insert_protocol_state(
            conn,
            protocol_component_id,
            txn[0],
            "reserve1".to_owned(),
            Bytes::from(U256::from(1100)),
            Some(txn[2]),
        )
        .await;

        // protocol state for state1-reserve2
        db_fixtures::insert_protocol_state(
            conn,
            protocol_component_id,
            txn[0],
            "reserve2".to_owned(),
            Bytes::from(U256::from(500)),
            None,
        )
        .await;

        // protocol state update for state1-reserve1
        db_fixtures::insert_protocol_state(
            conn,
            protocol_component_id,
            txn[2],
            "reserve1".to_owned(),
            Bytes::from(U256::from(1000)),
            None,
        )
        .await;
    }

    fn protocol_state() -> ProtocolState {
        let attributes: HashMap<String, Bytes> = vec![
            ("reserve1".to_owned(), Bytes::from(U256::from(1000))),
            ("reserve2".to_owned(), Bytes::from(U256::from(500))),
        ]
        .into_iter()
        .collect();
        ProtocolState::new(
            "state1".to_owned(),
            attributes,
            "0x3108322284d0a89a7accb288d1a94384d499504fe7e04441b0706c7628dee7b7"
                .parse()
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn test_get_protocol_states() {
        let mut conn = setup_db().await;
        setup_data(&mut conn).await;

        let expected = vec![protocol_state()];

        let gateway = EVMGateway::from_connection(&mut conn).await;

        let result = gateway
            .get_protocol_states(&Chain::Ethereum, None, None, None, &mut conn)
            .await
            .unwrap();

        assert_eq!(result, expected)
    }

    #[tokio::test]
    async fn test_get_protocol_states_at() {
        let mut conn = setup_db().await;
        setup_data(&mut conn).await;

        let gateway = EVMGateway::from_connection(&mut conn).await;

        let mut protocol_state = protocol_state();
        let attributes: HashMap<String, Bytes> = vec![
            ("reserve1".to_owned(), Bytes::from(U256::from(1100))),
            ("reserve2".to_owned(), Bytes::from(U256::from(500))),
        ]
        .into_iter()
        .collect();
        protocol_state.updated_attributes = attributes;
        protocol_state.modify_tx =
            "0xbb7e16d797a9e2fbc537e30f91ed3d27a254dd9578aa4c3af3e5f0d3e8130945"
                .parse()
                .unwrap();
        let expected = vec![protocol_state];

        let result = gateway
            .get_protocol_states(
                &Chain::Ethereum,
                Some(Version::from_block_number(Chain::Ethereum, 1)),
                None,
                None,
                &mut conn,
            )
            .await
            .unwrap();

        assert_eq!(result, expected)
    }

    #[tokio::test]
    async fn test_protocol_update_states() {
        let mut conn = setup_db().await;
        setup_data(&mut conn).await;

        let gateway = EVMGateway::from_connection(&mut conn).await;
        let chain = Chain::Ethereum;

        let mut new_state = protocol_state();
        let attributes: HashMap<String, Bytes> = vec![
            ("reserve1".to_owned(), Bytes::from(U256::from(700))),
            ("reserve2".to_owned(), Bytes::from(U256::from(700))),
        ]
        .into_iter()
        .collect();
        new_state.updated_attributes = attributes;

        // update the protocol state
        gateway
            .update_protocol_states(&chain, &[new_state.clone()], &mut conn)
            .await
            .expect("Failed to update protocol states");

        // check the orginal state and the updated one both exist in the db
        let db_states = gateway
            .get_protocol_states(
                &chain,
                None,
                None,
                Some(&[new_state.component_id.as_str()]),
                &mut conn,
            )
            .await
            .expect("Failed ");
        assert_eq!(db_states.len(), 1)
    }

    #[tokio::test]
    async fn test_get_or_create_protocol_system_id() {
        let mut conn = setup_db().await;
        let gw = EVMGateway::from_connection(&mut conn).await;

        let first_id = gw
            ._get_or_create_protocol_system_id(ProtocolSystem::Ambient, &mut conn)
            .await
            .unwrap();

        let second_id = gw
            ._get_or_create_protocol_system_id(ProtocolSystem::Ambient, &mut conn)
            .await
            .unwrap();
        assert_eq!(first_id, second_id);
    }

    #[tokio::test]
    async fn test_add_protocol_type() {
        let mut conn = setup_db().await;
        let gw = EVMGateway::from_connection(&mut conn).await;

        let d = NaiveDate::from_ymd_opt(2015, 6, 3).unwrap();
        let t = NaiveTime::from_hms_milli_opt(12, 34, 56, 789).unwrap();
        let dt = NaiveDateTime::new(d, t);

        let protocol_type = models::ProtocolType {
            name: "Protocol".to_string(),
            financial_type: FinancialType::Debt,
            attribute_schema: Some(json!({"attribute": "schema"})),
            implementation: ImplementationType::Custom,
        };

        gw.upsert_protocol_type(&protocol_type, &mut conn)
            .await
            .unwrap();

        let inserted_data = schema::protocol_type::table
            .filter(schema::protocol_type::name.eq("Protocol"))
            .select(schema::protocol_type::all_columns)
            .first::<orm::ProtocolType>(&mut conn)
            .await
            .unwrap();

        assert_eq!(inserted_data.name, "Protocol".to_string());
        assert_eq!(inserted_data.financial_type, orm::FinancialType::Debt);
        assert_eq!(inserted_data.attribute_schema, Some(json!({"attribute": "schema"})));
        assert_eq!(inserted_data.implementation, orm::ImplementationType::Custom);

        let updated_protocol_type = models::ProtocolType {
            name: "Protocol".to_string(),
            financial_type: FinancialType::Leverage,
            attribute_schema: Some(json!({"attribute": "another_schema"})),
            implementation: ImplementationType::Vm,
        };

        gw.upsert_protocol_type(&updated_protocol_type, &mut conn)
            .await
            .unwrap();

        let newly_inserted_data = schema::protocol_type::table
            .filter(schema::protocol_type::name.eq("Protocol"))
            .select(schema::protocol_type::all_columns)
            .load::<orm::ProtocolType>(&mut conn)
            .await
            .unwrap();

        assert_eq!(newly_inserted_data.len(), 1);
        assert_eq!(newly_inserted_data[0].name, "Protocol".to_string());
        assert_eq!(newly_inserted_data[0].financial_type, orm::FinancialType::Leverage);
        assert_eq!(
            newly_inserted_data[0].attribute_schema,
            Some(json!({"attribute": "another_schema"}))
        );
        assert_eq!(newly_inserted_data[0].implementation, orm::ImplementationType::Vm);
    }
}
