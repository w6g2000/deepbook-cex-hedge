use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use fastcrypto::traits::Signer;
use hex::FromHex;
use sui_sdk::{
    rpc_types::{SuiTransactionBlockResponse, SuiTransactionBlockResponseOptions},
    SuiClient, SuiClientBuilder,
    types::{
        Identifier, TypeTag,
        base_types::{ObjectID, ObjectRef, SequenceNumber, SuiAddress},
        programmable_transaction_builder::ProgrammableTransactionBuilder,
        quorum_driver_types::ExecuteTransactionRequestType,
        transaction::{CallArg, ObjectArg, Transaction, TransactionData},
    },
};
use sui_types::crypto::{Signature, SuiKeyPair};
use tracing::debug;

use super::client::TransactionExecutor;
use super::types::{MoveCall, MoveCallArg};

const DEFAULT_GAS_BUDGET: u64 = 50_000_000;
const SUI_COIN_TYPE: &str = "0x2::sui::SUI";

#[derive(Debug, Clone)]
pub struct SuiExecutorConfig {
    pub rpc_endpoint: String,
    pub signer_key: String,
    pub gas_budget: Option<u64>,
    pub gas_price: Option<u64>,
}

impl SuiExecutorConfig {
    pub fn gas_budget_or_default(&self) -> u64 {
        self.gas_budget.unwrap_or(DEFAULT_GAS_BUDGET)
    }
}

pub struct SuiSdkExecutor {
    client: SuiClient,
    signer: Arc<SuiKeyPair>,
    sender: SuiAddress,
    gas_budget: u64,
    gas_price_override: Option<u64>,
}

impl std::fmt::Debug for SuiSdkExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuiSdkExecutor")
            .field("sender", &self.sender)
            .field("gas_budget", &self.gas_budget)
            .field("gas_price_override", &self.gas_price_override)
            .finish()
    }
}

impl SuiSdkExecutor {
    pub async fn connect(config: SuiExecutorConfig) -> Result<Self> {
        let signer = Arc::new(parse_signer_key(&config.signer_key)?);
        let sender = SuiAddress::from(&signer.public());
        let client = SuiClientBuilder::default()
            .build(&config.rpc_endpoint)
            .await
            .with_context(|| {
                format!(
                    "failed to create Sui client for endpoint {}",
                    config.rpc_endpoint
                )
            })?;

        Ok(Self {
            client,
            signer,
            sender,
            gas_budget: config.gas_budget_or_default(),
            gas_price_override: config.gas_price,
        })
    }

    async fn select_gas_object(&self) -> Result<ObjectRef> {
        let mut coins = self
            .client
            .coin_read_api()
            .get_coins(self.sender, Some(SUI_COIN_TYPE.to_string()), None, Some(1))
            .await
            .with_context(|| format!("failed to fetch gas coin for sender {}", self.sender))?;

        let coin = coins
            .data
            .pop()
            .ok_or_else(|| anyhow!("sender {} has no SUI coins for gas", self.sender))?;

        if coin.balance < self.gas_budget {
            bail!(
                "gas coin balance {} is smaller than required gas budget {}",
                coin.balance,
                self.gas_budget
            );
        }

        Ok(coin.object_ref())
    }

    fn convert_type_arguments(&self, type_arguments: Vec<String>) -> Result<Vec<TypeTag>> {
        type_arguments
            .into_iter()
            .map(|tag| {
                sui_sdk::types::parse_sui_type_tag(&tag)
                    .with_context(|| format!("invalid type argument {}", tag))
            })
            .collect()
    }

    fn convert_argument(&self, arg: MoveCallArg) -> Result<CallArg> {
        match arg {
            MoveCallArg::Pure(pure) => Ok(CallArg::Pure(pure.data)),
            MoveCallArg::Shared(shared) => Ok(CallArg::Object(ObjectArg::SharedObject {
                id: ObjectID::from_str(&shared.object_id)
                    .with_context(|| format!("invalid shared object id {}", shared.object_id))?,
                initial_shared_version: SequenceNumber::from(shared.initial_shared_version),
                mutability: if shared.mutable {
                    sui_sdk::types::transaction::SharedObjectMutability::Mutable
                } else {
                    sui_sdk::types::transaction::SharedObjectMutability::Immutable
                },
            })),
        }
    }
}

#[async_trait::async_trait]
impl TransactionExecutor for SuiSdkExecutor {
    async fn execute_move_call(&self, call: MoveCall) -> Result<SuiTransactionBlockResponse> {
        let MoveCall {
            package_id,
            module: module_name,
            function: function_name,
            type_arguments,
            arguments,
        } = call;

        let package =
            ObjectID::from_str(&package_id).with_context(|| "invalid package id".to_string())?;
        let module = Identifier::new(module_name.clone())
            .with_context(|| format!("invalid module identifier {}", module_name))?;
        let function = Identifier::new(function_name.clone())
            .with_context(|| format!("invalid function identifier {}", function_name))?;

        let mut builder = ProgrammableTransactionBuilder::new();
        let type_arguments = self.convert_type_arguments(type_arguments)?;
        let call_args = arguments
            .into_iter()
            .map(|arg| self.convert_argument(arg))
            .collect::<Result<Vec<_>>>()?;
        builder
            .move_call(package, module, function, type_arguments, call_args)
            .with_context(|| "failed to encode move call".to_string())?;

        let programmable = builder.finish();
        let gas_object = self.select_gas_object().await?;
        let gas_price = match self.gas_price_override {
            Some(price) => price,
            None => self
                .client
                .read_api()
                .get_reference_gas_price()
                .await
                .context("failed to fetch reference gas price")?,
        };

        let tx_data = TransactionData::new_programmable(
            self.sender,
            vec![gas_object],
            programmable,
            self.gas_budget,
            gas_price,
        );

        let signer: &dyn Signer<Signature> = self.signer.as_ref();
        let transaction = Transaction::from_data_and_signer(tx_data, vec![signer]);

        let options = SuiTransactionBlockResponseOptions::new()
            .with_effects()
            .with_events();

        let response = self
            .client
            .quorum_driver_api()
            .execute_transaction_block(
                transaction,
                options,
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("failed to execute transaction block")?;

        debug!(
            tx_digest = %response.digest,
            "executed deepbook move call via Sui SDK"
        );

        Ok(response)
    }
}

fn parse_signer_key(value: &str) -> Result<SuiKeyPair> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("signer key must not be empty");
    }

    if let Ok(decoded) = general_purpose::STANDARD.decode(trimmed) {
        if let Ok(key_pair) = SuiKeyPair::from_bytes(&decoded) {
            return Ok(key_pair);
        }
    }

    let raw = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes = Vec::from_hex(raw).with_context(|| "failed to decode signer key as hex")?;
    SuiKeyPair::from_bytes(&bytes).map_err(|err| anyhow!(err.to_string()))
}
