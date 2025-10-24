use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectArg {
    pub object_id: String,
    pub initial_shared_version: u64,
    pub mutable: bool,
}

impl SharedObjectArg {
    pub fn new(object_id: impl Into<String>, initial_shared_version: u64, mutable: bool) -> Self {
        Self {
            object_id: object_id.into(),
            initial_shared_version,
            mutable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PureArg {
    pub name: &'static str,
    pub data: Vec<u8>,
}

impl PureArg {
    pub fn from_u8(name: &'static str, value: u8) -> Result<Self> {
        Ok(Self {
            name,
            data: bcs::to_bytes(&value)?,
        })
    }

    pub fn from_u64(name: &'static str, value: u64) -> Result<Self> {
        Ok(Self {
            name,
            data: bcs::to_bytes(&value)?,
        })
    }

    pub fn from_bool(name: &'static str, value: bool) -> Result<Self> {
        Ok(Self {
            name,
            data: bcs::to_bytes(&value)?,
        })
    }

    pub fn raw(name: &'static str, data: Vec<u8>) -> Self {
        Self { name, data }
    }
}

#[derive(Debug, Clone)]
pub enum MoveCallArg {
    Shared(SharedObjectArg),
    Pure(PureArg),
}

#[derive(Debug, Clone)]
pub struct MoveCall {
    pub package_id: String,
    pub module: String,
    pub function: String,
    pub type_arguments: Vec<String>,
    pub arguments: Vec<MoveCallArg>,
}

impl MoveCall {
    pub fn new(
        package_id: impl Into<String>,
        module: impl Into<String>,
        function: impl Into<String>,
        type_arguments: Vec<String>,
        arguments: Vec<MoveCallArg>,
    ) -> Self {
        Self {
            package_id: package_id.into(),
            module: module.into(),
            function: function.into(),
            type_arguments,
            arguments,
        }
    }
}
