use super::types::{MoveCall, MoveCallArg, PureArg, SharedObjectArg};
use anyhow::{Result, ensure};

#[derive(Debug, Clone)]
pub struct LimitOrderContract {
    package_id: String,
    module: String,
    function: String,
    type_arguments: [String; 2],
}

impl LimitOrderContract {
    pub fn new(
        package_id: impl Into<String>,
        module: impl Into<String>,
        function: impl Into<String>,
        base_type: impl Into<String>,
        quote_type: impl Into<String>,
    ) -> Self {
        Self {
            package_id: package_id.into(),
            module: module.into(),
            function: function.into(),
            type_arguments: [base_type.into(), quote_type.into()],
        }
    }

    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    pub fn module(&self) -> &str {
        &self.module
    }

    pub fn function(&self) -> &str {
        &self.function
    }

    pub fn type_arguments(&self) -> &[String; 2] {
        &self.type_arguments
    }

    pub fn type_arguments_vec(&self) -> Vec<String> {
        self.type_arguments.iter().cloned().collect()
    }
}

#[derive(Debug, Clone)]
pub struct DeepbookOrderConfig {
    contract: LimitOrderContract,
    shared_objects: Vec<SharedObjectArg>,
    clock: SharedObjectArg,
}

impl DeepbookOrderConfig {
    pub fn new(
        contract: LimitOrderContract,
        shared_objects: Vec<SharedObjectArg>,
        clock: SharedObjectArg,
    ) -> Self {
        assert!(
            !shared_objects.is_empty(),
            "deepbook limit order config requires shared objects"
        );
        Self {
            contract,
            shared_objects,
            clock,
        }
    }

    pub fn contract(&self) -> &LimitOrderContract {
        &self.contract
    }

    pub fn shared_objects(&self) -> &[SharedObjectArg] {
        &self.shared_objects
    }

    pub fn clock(&self) -> &SharedObjectArg {
        &self.clock
    }

    pub(crate) fn into_builder(self) -> LimitOrderBuilder {
        LimitOrderBuilder {
            contract: self.contract,
            shared_objects: self.shared_objects,
            clock: self.clock,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LimitOrderBuilder {
    contract: LimitOrderContract,
    shared_objects: Vec<SharedObjectArg>,
    clock: SharedObjectArg,
}

impl LimitOrderBuilder {
    pub(crate) fn build_call(&self, pure_arguments: Vec<PureArg>) -> MoveCall {
        let mut arguments =
            Vec::with_capacity(self.shared_objects.len() + pure_arguments.len() + 1);
        for shared in &self.shared_objects {
            arguments.push(MoveCallArg::Shared(shared.clone()));
        }
        for pure in pure_arguments {
            arguments.push(MoveCallArg::Pure(pure));
        }
        arguments.push(MoveCallArg::Shared(self.clock.clone()));

        MoveCall::new(
            self.contract.package_id(),
            self.contract.module(),
            self.contract.function(),
            self.contract.type_arguments_vec(),
            arguments,
        )
    }

    pub(crate) fn contract(&self) -> &LimitOrderContract {
        &self.contract
    }

    pub(crate) fn shared_objects(&self) -> &[SharedObjectArg] {
        &self.shared_objects
    }

    pub(crate) fn clock(&self) -> &SharedObjectArg {
        &self.clock
    }
}

#[derive(Debug, Clone)]
pub struct LimitOrderParameters {
    pub restriction: u8,
    pub self_matching_prevention: u8,
    pub post_only: bool,
    pub price_scale: u64,
    pub quantity_scale: u64,
    pub client_order_id_start: u64,
}

impl Default for LimitOrderParameters {
    fn default() -> Self {
        Self {
            restriction: 0,
            self_matching_prevention: 0,
            post_only: true,
            price_scale: 1,
            quantity_scale: 1,
            client_order_id_start: 1,
        }
    }
}

impl LimitOrderParameters {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.price_scale > 0,
            "price_scale must be greater than zero"
        );
        ensure!(
            self.quantity_scale > 0,
            "quantity_scale must be greater than zero"
        );
        Ok(())
    }
}
