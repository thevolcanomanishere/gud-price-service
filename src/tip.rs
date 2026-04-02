use axum::http::HeaderMap;
use axum::response::IntoResponse;
use mpp::server::axum::PaymentRequired;
use mpp::server::{Mpp, TempoConfig, tempo};
use mpp::{ChargeRequest, PaymentChallenge, Receipt, parse_authorization};
use serde::{Deserialize, Serialize};
use std::env;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const DEFAULT_TIP_MESSAGE: &str = "thanks for supporting gud-price-service";
const DEFAULT_TIP_NETWORK: &str = "base";
const DEFAULT_TIP_DECIMALS: u8 = 6;
const DEFAULT_TIP_RECIPIENT: &str = "0x0D572d5c38503F446162B113277f7aa2Ac5C4961";

#[derive(Debug, Clone)]
pub struct TipConfig {
    pub network: String,
    pub recipient: String,
    pub default_asset: Option<String>,
    pub message: String,
    pub rpc_url: String,
    pub chain_id: Option<u64>,
    pub decimals: u8,
}

impl TipConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            network: env::var("TIP_NETWORK").unwrap_or_else(|_| DEFAULT_TIP_NETWORK.to_string()),
            recipient: env::var("TIP_RECIPIENT")
                .unwrap_or_else(|_| DEFAULT_TIP_RECIPIENT.to_string()),
            default_asset: optional_env("TIP_ASSET")?,
            message: env::var("TIP_MESSAGE").unwrap_or_else(|_| DEFAULT_TIP_MESSAGE.to_string()),
            rpc_url: required_env("TIP_RPC_URL")?,
            chain_id: optional_u64_env("TIP_CHAIN_ID")?,
            decimals: optional_u8_env("TIP_DECIMALS")?.unwrap_or(DEFAULT_TIP_DECIMALS),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct TipRequest {
    pub amount: String,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub decimals: Option<u8>,
}

impl TipRequest {
    pub fn validate(&self) -> Result<(), String> {
        let amount = self.amount.trim();
        if amount.is_empty() {
            return Err("amount is required".to_string());
        }

        if !is_valid_positive_amount(amount) {
            return Err(
                "amount must be a positive token amount string (integer or decimal)".to_string(),
            );
        }

        if let Some(decimals) = self.decimals {
            if decimals > 38 {
                return Err("decimals must be <= 38".to_string());
            }
        }

        Ok(())
    }

    fn resolve_asset(&self, default_asset: Option<&str>) -> Result<String, String> {
        if let Some(asset) = self
            .asset
            .as_ref()
            .map(|asset| asset.trim())
            .filter(|asset| !asset.is_empty())
        {
            return Ok(asset.to_string());
        }

        if let Some(asset) = default_asset
            .map(str::trim)
            .filter(|asset| !asset.is_empty())
        {
            return Ok(asset.to_string());
        }

        Err("asset is required (send asset in request or configure TIP_ASSET)".to_string())
    }

    fn resolve_decimals(&self, asset: &str, default_decimals: u8) -> u8 {
        if let Some(decimals) = self.decimals {
            return decimals;
        }

        detect_asset_decimals(asset).unwrap_or(default_decimals)
    }
}

fn resolve_tip_meta(
    query_asset: Option<&str>,
    query_decimals: Option<u8>,
    default_asset: Option<&str>,
    default_decimals: u8,
) -> Result<TipMetaResponse, String> {
    let asset = query_asset
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            default_asset
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .ok_or_else(|| {
            "asset is required (send asset in query or configure TIP_ASSET)".to_string()
        })?;

    if let Some(decimals) = query_decimals {
        if decimals > 38 {
            return Err("decimals must be <= 38".to_string());
        }
        return Ok(TipMetaResponse {
            asset,
            decimals,
            source: "request".to_string(),
        });
    }

    if let Some(detected) = detect_asset_decimals(&asset) {
        return Ok(TipMetaResponse {
            asset,
            decimals: detected,
            source: "detected".to_string(),
        });
    }

    Ok(TipMetaResponse {
        asset,
        decimals: default_decimals,
        source: "default".to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipReceiptResponse {
    pub status: String,
    pub amount: String,
    pub asset: String,
    pub network: String,
    pub recipient: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct TipMetaQuery {
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub decimals: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipMetaResponse {
    pub asset: String,
    pub decimals: u8,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct TipErrorResponse {
    pub error: String,
}

pub enum TipOutcome {
    Challenge(PaymentChallenge),
    Receipt(TipReceiptResponse, Receipt),
}

pub trait TipProcessor: Send + Sync {
    fn process_tip<'a>(
        &'a self,
        headers: &'a HeaderMap,
        request: &'a TipRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TipOutcome, String>> + Send + 'a>>;

    fn tip_meta(&self, query: &TipMetaQuery) -> Result<TipMetaResponse, String>;
}

type TempoMpp = Mpp<mpp::server::TempoChargeMethod<mpp::server::TempoProvider>>;

pub struct MppTipProcessor {
    config: TipConfig,
    mpp: TempoMpp,
}

impl MppTipProcessor {
    pub fn from_config(config: TipConfig) -> Result<Self, String> {
        let mut builder = tempo(TempoConfig {
            recipient: &config.recipient,
        });

        if let Some(default_asset) = config.default_asset.as_deref() {
            builder = builder.currency(default_asset);
        }

        builder = builder
            .rpc_url(&config.rpc_url)
            .decimals(u32::from(config.decimals));

        if let Some(chain_id) = config.chain_id {
            builder = builder.chain_id(chain_id);
        }

        let mpp = Mpp::create(builder).map_err(|err| err.to_string())?;
        Ok(Self { config, mpp })
    }

    fn build_request(
        &self,
        amount: &str,
        asset: &str,
        decimals: u8,
    ) -> Result<ChargeRequest, String> {
        let request = ChargeRequest {
            amount: amount.to_string(),
            currency: asset.to_string(),
            // Preserve backward compatibility: integer amounts stay in base units.
            decimals: amount.contains('.').then_some(decimals),
            recipient: Some(self.config.recipient.clone()),
            description: Some(self.config.message.clone()),
            ..Default::default()
        };

        request.with_base_units().map_err(|err| err.to_string())
    }

    fn challenge(&self, request: &ChargeRequest) -> Result<PaymentChallenge, String> {
        self.mpp
            .charge_challenge_with_options(request, None, Some(self.config.message.as_str()))
            .map_err(|err| err.to_string())
    }
}

impl TipProcessor for MppTipProcessor {
    fn process_tip<'a>(
        &'a self,
        headers: &'a HeaderMap,
        request: &'a TipRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TipOutcome, String>> + Send + 'a>> {
        Box::pin(async move {
            request.validate()?;
            let asset = request.resolve_asset(self.config.default_asset.as_deref())?;
            let decimals = request.resolve_decimals(&asset, self.config.decimals);
            let expected = self.build_request(&request.amount, &asset, decimals)?;

            let auth = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok());

            let Some(auth_header) = auth else {
                return self.challenge(&expected).map(TipOutcome::Challenge);
            };

            let credential = match parse_authorization(auth_header) {
                Ok(credential) => credential,
                Err(_) => {
                    return self.challenge(&expected).map(TipOutcome::Challenge);
                }
            };

            match self
                .mpp
                .verify_credential_with_expected_request(&credential, &expected)
                .await
            {
                Ok(receipt) => Ok(TipOutcome::Receipt(
                    TipReceiptResponse {
                        status: "tipped".to_string(),
                        amount: request.amount.clone(),
                        asset: asset.clone(),
                        network: self.config.network.clone(),
                        recipient: self.config.recipient.clone(),
                        message: self.config.message.clone(),
                    },
                    receipt,
                )),
                Err(_) => self.challenge(&expected).map(TipOutcome::Challenge),
            }
        })
    }

    fn tip_meta(&self, query: &TipMetaQuery) -> Result<TipMetaResponse, String> {
        resolve_tip_meta(
            query.asset.as_deref(),
            query.decimals,
            self.config.default_asset.as_deref(),
            self.config.decimals,
        )
    }
}

pub fn payment_required_response(challenge: PaymentChallenge) -> axum::response::Response {
    PaymentRequired(challenge).into_response()
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required environment variable: {name}"))
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(format!("invalid unicode in environment variable: {name}"))
        }
    }
}

fn optional_u64_env(name: &str) -> Result<Option<u64>, String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("{name} must be a valid unsigned integer")),
        _ => Ok(None),
    }
}

fn optional_u8_env(name: &str) -> Result<Option<u8>, String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse::<u8>()
            .map(Some)
            .map_err(|_| format!("{name} must be a valid unsigned integer")),
        _ => Ok(None),
    }
}

fn detect_asset_decimals(asset: &str) -> Option<u8> {
    let normalized = asset.trim().to_ascii_lowercase();

    match normalized.as_str() {
        "eth" | "weth" => Some(18),
        "usdc" | "usdt" => Some(6),
        // Base canonical bridge/WETH token.
        "0x4200000000000000000000000000000000000006" => Some(18),
        // Base USDC.
        "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" => Some(6),
        // Base USDT.
        "0xfde4c96c8593536e31f229ea8f37b2adab8b9bb2" => Some(6),
        _ => None,
    }
}

fn is_valid_positive_amount(amount: &str) -> bool {
    let trimmed = amount.trim();
    if trimmed.is_empty() {
        return false;
    }

    let mut seen_dot = false;
    let mut has_digit = false;
    let mut has_non_zero_digit = false;

    for ch in trimmed.chars() {
        if ch == '.' {
            if seen_dot {
                return false;
            }
            seen_dot = true;
            continue;
        }

        if !ch.is_ascii_digit() {
            return false;
        }

        has_digit = true;
        if ch != '0' {
            has_non_zero_digit = true;
        }
    }

    has_digit && has_non_zero_digit
}

pub type SharedTipProcessor = Arc<dyn TipProcessor>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn validates_amount() {
        assert!(
            TipRequest {
                amount: "1000".to_string(),
                asset: None,
                decimals: None,
            }
            .validate()
            .is_ok()
        );
        assert!(
            TipRequest {
                amount: "0".to_string(),
                asset: None,
                decimals: None,
            }
            .validate()
            .is_err()
        );
        assert!(
            TipRequest {
                amount: "abc".to_string(),
                asset: None,
                decimals: None,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validates_decimal_amounts() {
        assert!(
            TipRequest {
                amount: "0.01".to_string(),
                asset: None,
                decimals: Some(6),
            }
            .validate()
            .is_ok()
        );
        assert!(
            TipRequest {
                amount: "0.00".to_string(),
                asset: None,
                decimals: Some(6),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn missing_required_tip_config_fails() {
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        unsafe {
            env::remove_var("TIP_RECIPIENT");
            env::remove_var("TIP_ASSET");
            env::remove_var("TIP_RPC_URL");
        }

        let error = TipConfig::from_env().unwrap_err();
        assert!(error.contains("TIP_RPC_URL"));
    }

    #[test]
    fn resolves_asset_from_request_or_fallback() {
        let request_asset = TipRequest {
            amount: "1000".to_string(),
            asset: Some("0xasset".to_string()),
            decimals: None,
        };
        assert_eq!(
            request_asset.resolve_asset(Some("0xfallback")).unwrap(),
            "0xasset"
        );

        let fallback_asset = TipRequest {
            amount: "1000".to_string(),
            asset: None,
            decimals: None,
        };
        assert_eq!(
            fallback_asset.resolve_asset(Some("0xfallback")).unwrap(),
            "0xfallback"
        );

        let missing_asset = TipRequest {
            amount: "1000".to_string(),
            asset: None,
            decimals: None,
        };
        assert!(missing_asset.resolve_asset(None).is_err());
    }

    #[test]
    fn resolves_decimals_from_request_asset_or_default() {
        let explicit = TipRequest {
            amount: "1".to_string(),
            asset: Some("USDC".to_string()),
            decimals: Some(9),
        };
        assert_eq!(explicit.resolve_decimals("USDC", 18), 9);

        let detected = TipRequest {
            amount: "1".to_string(),
            asset: Some("USDT".to_string()),
            decimals: None,
        };
        assert_eq!(detected.resolve_decimals("USDT", 18), 6);

        let fallback = TipRequest {
            amount: "1".to_string(),
            asset: Some("unknown".to_string()),
            decimals: None,
        };
        assert_eq!(fallback.resolve_decimals("unknown", 18), 18);
    }

    #[test]
    fn tip_meta_uses_request_detected_and_default_sources() {
        let request = resolve_tip_meta(Some("USDC"), Some(9), Some("WETH"), 18).unwrap();
        assert_eq!(request.decimals, 9);
        assert_eq!(request.source, "request");

        let detected = resolve_tip_meta(Some("USDT"), None, Some("WETH"), 18).unwrap();
        assert_eq!(detected.decimals, 6);
        assert_eq!(detected.source, "detected");

        let fallback = resolve_tip_meta(Some("TOKEN"), None, Some("WETH"), 18).unwrap();
        assert_eq!(fallback.decimals, 18);
        assert_eq!(fallback.source, "default");
    }
}
