use anyhow::Result;
use sui_sdk::rpc_types::SuiTransactionBlockResponse;

use super::types::MoveCall;

#[async_trait::async_trait]
pub trait TransactionExecutor: Send + Sync {
    async fn execute_move_call(&self, call: MoveCall) -> Result<SuiTransactionBlockResponse>;
}
