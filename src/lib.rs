//! 单资产、只做多的确定性交易模拟内核。所有价格/金额使用最小货币单位。
//! 学习入口依次为 types → account → engine → strategy → replay。
#![forbid(unsafe_code)]

pub mod account;
pub mod book;
pub mod engine;
pub mod replay;
pub mod strategy;
pub mod types;

pub mod config;
pub mod journal;
pub mod paper;
pub mod storage;

pub mod store;

#[cfg(feature = "network")]
pub mod binance;
pub mod decimal;
pub mod execution;
pub mod feed;
pub mod research;

pub mod registry;

pub mod telemetry;
