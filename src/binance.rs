//! Binance Spot Testnet REST；固定主机、禁止重定向，错误不打印签名 URL/密钥。
use crate::decimal::{self, SCALE};
use crate::execution::*;
use crate::journal::hex;
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use serde_json::Value;
use sha2::Sha256;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
const BASE: &str = "https://testnet.binance.vision/api/v3";

pub struct BinanceTestnet {
    client: Client,
    key: String,
    secret: String,
}
impl BinanceTestnet {
    pub fn from_env() -> Result<Self, crate::paper::PaperError> {
        let (key, secret) = credentials()?;
        let client = Client::builder()
            .timeout(Duration::from_secs(12))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "HTTP client initialization failed")?;
        Ok(Self {
            client,
            key,
            secret,
        })
    }
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        pairs: &[(&str, String)],
        signed: bool,
    ) -> Result<Value, VenueError> {
        let mut pairs = pairs.to_vec();
        if signed {
            // 每次签名使用交易所时间，避免本机 NTP 偏差；不自动重发写请求。
            let time = self.request(reqwest::Method::GET, "/time", &[], false)?["serverTime"]
                .as_u64()
                .ok_or(VenueError::Unavailable)?;
            pairs.push(("recvWindow", "5000".into()));
            pairs.push(("timestamp", time.to_string()));
        }
        let query = reqwest::Url::parse_with_params("https://unused.invalid", &pairs)
            .map_err(|_| VenueError::Unavailable)?
            .query()
            .unwrap_or("")
            .to_string();
        let query = if signed {
            let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
                .map_err(|_| VenueError::Unavailable)?;
            mac.update(query.as_bytes());
            format!("{query}&signature={}", hex(&mac.finalize().into_bytes()))
        } else {
            query
        };
        let mut request = self.client.request(method, format!("{BASE}{path}?{query}"));
        if signed {
            request = request.header("X-MBX-APIKEY", &self.key);
        }
        let response = request.send().map_err(|_| VenueError::Unknown)?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .take(2_097_153)
            .read_to_end(&mut bytes)
            .map_err(|_| VenueError::Unknown)?;
        if bytes.len() > 2_097_152 {
            return Err(VenueError::Unknown);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| VenueError::Unknown)?;
        if status.is_success() {
            return Ok(value);
        }
        let code = value["code"].as_i64().unwrap_or(0);
        if status.is_server_error() || code == -1007 {
            Err(VenueError::Unknown)
        } else if code == -2013 {
            Err(VenueError::NotFound)
        } else if matches!(status.as_u16(), 418 | 429 | 403) {
            Err(VenueError::Unavailable)
        } else {
            Err(VenueError::Rejected(code))
        }
    }
    fn identity(intent: &OrderIntent) -> Vec<(&'static str, String)> {
        vec![
            ("symbol", intent.symbol.clone()),
            ("origClientOrderId", intent.client_order_id.clone()),
        ]
    }
}
fn parse(v: Value) -> Result<OrderObservation, VenueError> {
    serde_json::from_value(v).map_err(|_| VenueError::Unknown)
}
fn units(v: &Value) -> Result<i128, VenueError> {
    decimal::parse(v.as_str().ok_or(VenueError::Unavailable)?).map_err(|_| VenueError::Unavailable)
}
impl ExecutionVenue for BinanceTestnet {
    fn validate_intent(&mut self, intent: &OrderIntent) -> Result<(), VenueError> {
        let info = self.request(
            reqwest::Method::GET,
            "/exchangeInfo",
            &[("symbol", intent.symbol.clone())],
            false,
        )?;
        let s = info["symbols"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or(VenueError::Unavailable)?;
        if s["symbol"] != intent.symbol
            || s["status"] != "TRADING"
            || s["quoteAsset"] != "USDT"
            || s["isSpotTradingAllowed"] != true
        {
            return Err(VenueError::Unavailable);
        }
        let p = decimal::parse(&intent.price).map_err(|_| VenueError::Unavailable)?;
        let q = decimal::parse(&intent.quantity).map_err(|_| VenueError::Unavailable)?;
        let n = p.checked_mul(q).ok_or(VenueError::Unavailable)? / SCALE;
        let mut grid = (false, false);
        for f in s["filters"].as_array().ok_or(VenueError::Unavailable)? {
            match f["filterType"].as_str().unwrap_or("") {
                "PRICE_FILTER" => {
                    let tick = units(&f["tickSize"])?;
                    if tick == 0
                        || p % tick != 0
                        || p < units(&f["minPrice"])?
                        || (units(&f["maxPrice"])? != 0 && p > units(&f["maxPrice"])?)
                    {
                        return Err(VenueError::Rejected(-1013));
                    }
                    grid.0 = true;
                }
                "LOT_SIZE" => {
                    let step = units(&f["stepSize"])?;
                    if step == 0
                        || q % step != 0
                        || q < units(&f["minQty"])?
                        || q > units(&f["maxQty"])?
                    {
                        return Err(VenueError::Rejected(-1013));
                    }
                    grid.1 = true;
                }
                "MIN_NOTIONAL" => {
                    if n < units(&f["minNotional"])? {
                        return Err(VenueError::Rejected(-1013));
                    }
                }
                "NOTIONAL" if n < units(&f["minNotional"])? || n > units(&f["maxNotional"])? => {
                    return Err(VenueError::Rejected(-1013));
                }
                _ => (), // 动态价格带/账户级过滤仍由交易所强制检查，不声称本地完整复制。
            }
        }
        if !grid.0 || !grid.1 || !self.open_orders(&intent.symbol)?.is_empty() {
            return Err(VenueError::Unavailable);
        }
        let account = self.account()?;
        let asset = match intent.side {
            crate::types::Side::Buy => "USDT",
            crate::types::Side::Sell => s["baseAsset"].as_str().ok_or(VenueError::Unavailable)?,
        };
        let available = account
            .balances
            .iter()
            .find(|b| b.asset == asset)
            .map(|b| decimal::parse(&b.free))
            .transpose()
            .map_err(|_| VenueError::Unavailable)?
            .unwrap_or(0);
        let required = match intent.side {
            crate::types::Side::Buy => {
                n.checked_mul(10020).ok_or(VenueError::Unavailable)? / 10000 + 1
            }
            crate::types::Side::Sell => q,
        };
        if !account.can_trade || available < required {
            return Err(VenueError::Rejected(-2010));
        }
        Ok(())
    }
    fn submit(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        parse(
            self.request(
                reqwest::Method::POST,
                "/order",
                &[
                    ("symbol", i.symbol.clone()),
                    (
                        "side",
                        match i.side {
                            crate::types::Side::Buy => "BUY",
                            crate::types::Side::Sell => "SELL",
                        }
                        .into(),
                    ),
                    ("type", "LIMIT".into()),
                    ("timeInForce", "GTC".into()),
                    ("quantity", i.quantity.clone()),
                    ("price", i.price.clone()),
                    ("newClientOrderId", i.client_order_id.clone()),
                    ("newOrderRespType", "RESULT".into()),
                ],
                true,
            )?,
        )
    }
    fn query(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        parse(self.request(reqwest::Method::GET, "/order", &Self::identity(i), true)?)
    }
    fn query_known(
        &mut self,
        i: &OrderIntent,
        order_id: Option<u64>,
    ) -> Result<OrderObservation, VenueError> {
        let Some(order_id) = order_id else {
            return self.query(i);
        };
        let value = self.request(
            reqwest::Method::GET,
            "/order",
            &[
                ("symbol", i.symbol.clone()),
                ("orderId", order_id.to_string()),
            ],
            true,
        )?;
        bind_known_observation(value, i, order_id)
    }
    fn cancel(&mut self, i: &OrderIntent) -> Result<(), VenueError> {
        self.request(reqwest::Method::DELETE, "/order", &Self::identity(i), true)?;
        Ok(())
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        serde_json::from_value(self.request(reqwest::Method::GET, "/account", &[], true)?)
            .map_err(|_| VenueError::Unknown)
    }
    fn open_orders(&mut self, symbol: &str) -> Result<Vec<OrderObservation>, VenueError> {
        serde_json::from_value(self.request(
            reqwest::Method::GET,
            "/openOrders",
            &[("symbol", symbol.into())],
            true,
        )?)
        .map_err(|_| VenueError::Unknown)
    }
    fn trades(&mut self, o: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        let trades: Vec<TradeObservation> = serde_json::from_value(self.request(
            reqwest::Method::GET,
            "/myTrades",
            &[
                ("symbol", o.symbol.clone()),
                ("orderId", o.order_id.to_string()),
                ("limit", "1000".into()),
            ],
            true,
        )?)
        .map_err(|_| VenueError::Unknown)?;
        if trades.len() == 1000
            || trades
                .iter()
                .any(|t| t.symbol != o.symbol || t.order_id != o.order_id)
        {
            return Err(VenueError::Unavailable);
        }
        Ok(trades)
    }
}
/// 只用于记录观测时间，不参与签名。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn credentials() -> Result<(String, String), crate::paper::PaperError> {
    if let (Ok(key), Ok(secret)) = (
        std::env::var("KAZE_BINANCE_TESTNET_KEY"),
        std::env::var("KAZE_BINANCE_TESTNET_SECRET"),
    ) {
        if key.is_empty() || secret.is_empty() {
            return Err("empty testnet credentials".into());
        }
        return Ok((key, secret));
    }
    let path = std::path::Path::new("configs/testnet.credentials.env");
    let metadata = path
        .symlink_metadata()
        .map_err(|_| "configure local testnet environment or configs/testnet.credentials.env")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 8192 {
        return Err("credentials must be a regular local file <=8 KiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "credentials file must have mode 600 (chmod 600 configs/testnet.credentials.env)"
                    .into(),
            );
        }
    }
    let text = std::fs::read_to_string(path)?;
    let mut key = None;
    let mut secret = None;
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let (name, value) = line
            .split_once('=')
            .ok_or("invalid credentials file assignment")?;
        let target = match name {
            "KAZE_BINANCE_TESTNET_KEY" => &mut key,
            "KAZE_BINANCE_TESTNET_SECRET" => &mut secret,
            _ => return Err("unexpected credentials variable".into()),
        };
        if target.is_some() || value.is_empty() {
            return Err("duplicate or empty credentials assignment".into());
        }
        *target = Some(value.to_owned());
    }
    Ok((
        key.ok_or("testnet key missing")?,
        secret.ok_or("testnet secret missing")?,
    ))
}

/// 撤单后的远端 client ID 只在稳定 orderId 和终态同时匹配时接受，原值进入证据。
fn bind_known_observation(
    value: Value,
    intent: &OrderIntent,
    order_id: u64,
) -> Result<OrderObservation, VenueError> {
    let mut observation = parse(value)?;
    if observation.order_id != order_id || observation.symbol != intent.symbol {
        return Err(VenueError::Unknown);
    }
    if observation.client_order_id != intent.client_order_id {
        if !observation.status.terminal() {
            return Err(VenueError::Unknown);
        }
        observation.reported_client_order_id = Some(observation.client_order_id);
        observation.client_order_id = intent.client_order_id.clone();
    }
    Ok(observation)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_remote_client_id_is_bound_only_by_known_venue_id() {
        let intent = OrderIntent {
            client_order_id: "kaze-original".into(),
            symbol: "BTCUSDT".into(),
            side: crate::types::Side::Buy,
            price: "100".into(),
            quantity: "1".into(),
        };
        let value = serde_json::json!({"symbol":"BTCUSDT","clientOrderId":"exchange-cancel-id","orderId":7,"price":"100","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","status":"CANCELED","side":"BUY","updateTime":20});
        let o = bind_known_observation(value.clone(), &intent, 7).unwrap();
        assert_eq!(o.client_order_id, "kaze-original");
        assert_eq!(
            o.reported_client_order_id.as_deref(),
            Some("exchange-cancel-id")
        );
        assert!(bind_known_observation(value.clone(), &intent, 8).is_err());
        let mut open = value;
        open["status"] = "NEW".into();
        assert!(bind_known_observation(open, &intent, 7).is_err());
    }
}
