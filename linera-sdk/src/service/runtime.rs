// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Runtime types to interface with the host executing the service.

use std::sync::Mutex;

use linera_base::{
    abi::ServiceAbi,
    data_types::{Amount, BlockHeight, Timestamp},
    http,
    identifiers::{AccountOwner, ApplicationId, ChainId},
};
use serde::Serialize;

use super::wit::{base_runtime_api as base_wit, service_runtime_api as service_wit};
use crate::{DataBlobHash, KeyValueStore, Service, ViewStorageContext};

/// The runtime available during execution of a query.
pub struct ServiceRuntime<Application>
where
    Application: Service,
{
    application_parameters: Mutex<Option<Application::Parameters>>,
    application_id: Mutex<Option<ApplicationId<Application::Abi>>>,
    chain_id: Mutex<Option<ChainId>>,
    next_block_height: Mutex<Option<BlockHeight>>,
    timestamp: Mutex<Option<Timestamp>>,
    chain_balance: Mutex<Option<Amount>>,
    owner_balances: Mutex<Option<Vec<(AccountOwner, Amount)>>>,
    balance_owners: Mutex<Option<Vec<AccountOwner>>>,
}

impl<Application> ServiceRuntime<Application>
where
    Application: Service,
{
    /// Creates a new [`ServiceRuntime`] instance for a service.
    pub(crate) fn new() -> Self {
        ServiceRuntime {
            application_parameters: Mutex::new(None),
            application_id: Mutex::new(None),
            chain_id: Mutex::new(None),
            next_block_height: Mutex::new(None),
            timestamp: Mutex::new(None),
            chain_balance: Mutex::new(None),
            owner_balances: Mutex::new(None),
            balance_owners: Mutex::new(None),
        }
    }

    /// Returns the key-value store to interface with storage.
    pub fn key_value_store(&self) -> KeyValueStore {
        KeyValueStore::for_services()
    }

    /// Returns a storage context suitable for a root view.
    pub fn root_view_storage_context(&self) -> ViewStorageContext {
        ViewStorageContext::new_unsafe(self.key_value_store(), Vec::new(), ())
    }
}

impl<Application> ServiceRuntime<Application>
where
    Application: Service,
{
    /// Returns the application parameters provided when the application was created.
    pub fn application_parameters(&self) -> Application::Parameters {
        Self::fetch_value_through_cache(&self.application_parameters, || {
            let bytes = base_wit::application_parameters();
            serde_json::from_slice(&bytes).expect("Application parameters must be deserializable")
        })
    }

    /// Returns the ID of the current application.
    pub fn application_id(&self) -> ApplicationId<Application::Abi> {
        Self::fetch_value_through_cache(&self.application_id, || {
            ApplicationId::from(base_wit::get_application_id()).with_abi()
        })
    }

    /// Returns the ID of the current chain.
    pub fn chain_id(&self) -> ChainId {
        Self::fetch_value_through_cache(&self.chain_id, || base_wit::get_chain_id().into())
    }

    /// Returns the height of the next block that can be added to the current chain.
    pub fn next_block_height(&self) -> BlockHeight {
        Self::fetch_value_through_cache(&self.next_block_height, || {
            base_wit::get_block_height().into()
        })
    }

    /// Retrieves the current system time, i.e. the timestamp of the block in which this is called.
    pub fn system_time(&self) -> Timestamp {
        Self::fetch_value_through_cache(&self.timestamp, || {
            base_wit::read_system_timestamp().into()
        })
    }

    /// Returns the current chain balance.
    pub fn chain_balance(&self) -> Amount {
        Self::fetch_value_through_cache(&self.chain_balance, || {
            base_wit::read_chain_balance().into()
        })
    }

    /// Returns the balance of one of the accounts on this chain.
    pub fn owner_balance(&self, owner: AccountOwner) -> Amount {
        base_wit::read_owner_balance(owner.into()).into()
    }

    /// Returns the balances of all accounts on the chain.
    pub fn owner_balances(&self) -> Vec<(AccountOwner, Amount)> {
        Self::fetch_value_through_cache(&self.owner_balances, || {
            base_wit::read_owner_balances()
                .into_iter()
                .map(|(owner, amount)| (owner.into(), amount.into()))
                .collect()
        })
    }

    /// Returns the owners of accounts on this chain.
    pub fn balance_owners(&self) -> Vec<AccountOwner> {
        Self::fetch_value_through_cache(&self.balance_owners, || {
            base_wit::read_balance_owners()
                .into_iter()
                .map(AccountOwner::from)
                .collect()
        })
    }

    /// Makes an HTTP request to the given URL as an oracle and returns the answer, if any.
    ///
    /// Should only be used with queries where it is very likely that all validators will receive
    /// the same response, otherwise most block proposals will fail.
    ///
    /// Cannot be used in fast blocks: A block using this call should be proposed by a regular
    /// owner, not a super owner.
    pub fn http_request(&self, request: http::Request) -> http::Response {
        base_wit::perform_http_request(&request.into()).into()
    }

    /// Reads a data blob with the given hash from storage.
    pub fn read_data_blob(&self, hash: DataBlobHash) -> Vec<u8> {
        base_wit::read_data_blob(hash.0.into())
    }

    /// Asserts that a data blob with the given hash exists in storage.
    pub fn assert_data_blob_exists(&self, hash: DataBlobHash) {
        base_wit::assert_data_blob_exists(hash.0.into())
    }
}

impl<Application> ServiceRuntime<Application>
where
    Application: Service,
{
    /// Schedules an operation to be included in the block being built.
    ///
    /// The operation is specified as an opaque blob of bytes.
    pub fn schedule_raw_operation(&self, operation: Vec<u8>) {
        service_wit::schedule_operation(&operation);
    }

    /// Schedules an operation to be included in the block being built.
    ///
    /// The operation is serialized using BCS.
    pub fn schedule_operation(&self, operation: &impl Serialize) {
        let bytes = bcs::to_bytes(operation).expect("Failed to serialize application operation");

        service_wit::schedule_operation(&bytes);
    }

    /// Queries another application.
    pub fn query_application<A: ServiceAbi>(
        &self,
        application: ApplicationId<A>,
        query: &A::Query,
    ) -> A::QueryResponse {
        let query_bytes =
            serde_json::to_vec(&query).expect("Failed to serialize query to another application");

        let response_bytes =
            service_wit::try_query_application(application.forget_abi().into(), &query_bytes);

        serde_json::from_slice(&response_bytes)
            .expect("Failed to deserialize query response from application")
    }
}

impl<Application> ServiceRuntime<Application>
where
    Application: Service,
{
    /// Loads a value from the `slot` cache or fetches it and stores it in the cache.
    fn fetch_value_through_cache<T>(slot: &Mutex<Option<T>>, fetch: impl FnOnce() -> T) -> T
    where
        T: Clone,
    {
        let mut value = slot
            .lock()
            .expect("Mutex should never be poisoned because service runs in a single thread");

        if value.is_none() {
            *value = Some(fetch());
        }

        value.clone().expect("Value should be populated above")
    }
}
