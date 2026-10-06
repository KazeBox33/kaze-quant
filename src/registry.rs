//! 编译期注册策略工厂；配置固定 name/version/parameters，状态与账本同事务提交。
//! 不动态加载任意库，不在热路径执行脚本或读取未来数据。
use crate::paper::PaperError;
use crate::strategy::Strategy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomCheckpoint {
    pub name: String,
    pub version: u32,
    pub state: Value,
}
pub type StrategyFactory = fn(&Value, Option<&Value>) -> Result<Box<dyn Strategy>, PaperError>;
#[derive(Default)]
pub struct StrategyRegistry {
    factories: BTreeMap<(String, u32), StrategyFactory>,
}
impl StrategyRegistry {
    pub fn register(
        &mut self,
        name: &str,
        version: u32,
        factory: StrategyFactory,
    ) -> Result<(), PaperError> {
        validate_identity(name, version)?;
        let key = (name.to_owned(), version);
        if self.factories.contains_key(&key) {
            return Err("duplicate strategy name/version".into());
        }
        self.factories.insert(key, factory);
        Ok(())
    }
    pub fn build(
        &self,
        name: &str,
        version: u32,
        parameters: &Value,
        state: Option<&Value>,
    ) -> Result<Box<dyn Strategy>, PaperError> {
        validate_identity(name, version)?;
        if serde_json::to_vec(parameters)?.len() > 8192
            || state.is_some_and(|s| serde_json::to_vec(s).map_or(true, |b| b.len() > 65536))
        {
            return Err("custom strategy parameters/state exceed limits".into());
        }
        let factory = self
            .factories
            .get(&(name.to_owned(), version))
            .ok_or("strategy factory not registered at this exact version")?;
        factory(parameters, state)
    }
}
pub fn validate_identity(name: &str, version: u32) -> Result<(), PaperError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        || version == 0
    {
        return Err("invalid custom strategy name/version".into());
    }
    Ok(())
}
