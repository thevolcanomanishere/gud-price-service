# gud-price-service

A minimal Rust webserver that serves live Chainlink prices using [`gud-price`](https://github.com/thevolcanomanishere/gud-price).

## Why this exists

`gud-price-service` wraps the `gud-price` Rust library in a simple HTTP API so other apps and agents can fetch price data without embedding on-chain RPC logic.

Built on top of your `gud-price` project:
- GitHub: https://github.com/thevolcanomanishere/gud-price

Live deployment:
- https://gud-price-service.up.railway.app/

## Features

- Stateless HTTP API
- Live price reads via `gud-price`
- Short TTL in-memory cache
- Week-long preferred-chain cache per asset pair for fast repeat lookups
- Discovery endpoint for supported pairs (JSON or CSV via `format=csv`)
- Optional slim plain-text mode for easy machine consumption
- Optional MPP-powered tipping endpoint so agents can support this free API
- `llms.txt` served at `/`, `/llms.txt`, and `/.well-known/llms.txt`
- Cold price requests race all feeds in parallel and return the first successful result
- Background probing learns the freshest healthy chain for each pair, using latency as a tiebreaker, and reuses it for up to 1 week
- Preferred chains are discarded and reprobed if their live `updated_at` is more than 120 seconds old
- Price responses only expose `updated_at` as a UTC ISO timestamp (no `started_at`)
- Chain-specific RPC overrides use the Lasso load-balanced endpoints for Ethereum, Arbitrum, and Base

## Endpoints

- `GET /health`
- `GET /discovery` (JSON default, add `?format=csv` for CSV output)
- `GET /price/{pair}` (JSON; cold requests race all chains, warm requests use the cached preferred chain when its `updated_at` is within 120 seconds, and `updated_at` is emitted as UTC)
- `GET /price/{pair}?slim=true` (plain text price only)
- `POST /tip` (MPP challenge/verification flow for optional tips)
- `GET /tip/meta?asset=USDC` (preflight token-decimals metadata for tipping)
- `GET /` (plain text `llms.txt` for service discovery)
- `GET /llms.txt`
- `GET /.well-known/llms.txt`

## Quick Start

```bash
cargo run
```

Optional env vars:

- `PORT` (default: `3000`)
- `PRICE_CACHE_TTL_SECS` (default: `5`)
- `TIP_NETWORK` (default: `tempo`)
- `TIP_RECIPIENT` (default: `0xDCFCE862742d72e6d6df8A84E3547aF2A6fdA0EF`)
- `TIP_ASSET` (optional fallback token address/symbol used when `asset` is omitted in `POST /tip`)
- `TIP_RPC_URL` (default for Tempo: `https://tempo-mainnet.drpc.org`)
- `MPP_SECRET_KEY` (required; used by MPP to sign/verify payment challenges)
- `TIP_CHAIN_ID` (optional override; default Tempo mainnet chainId is `4217`)
- `TIP_DECIMALS` (optional; default: `6`)
- `TIP_MESSAGE` (optional; default: `thanks for supporting gud-price-service`)

Example:

```bash
PORT=8080 PRICE_CACHE_TTL_SECS=5 cargo run
```

## Example Requests

```bash
curl http://localhost:3000/health
curl http://localhost:3000/discovery
curl http://localhost:3000/price/BTC_USD
curl http://localhost:3000/price/BTC_USD?slim=true
curl http://localhost:3000/
curl "http://localhost:3000/tip/meta?asset=USDC"
curl -X POST http://localhost:3000/tip \
  -H "content-type: application/json" \
  -d '{"amount":"1000000","asset":"USDC"}'
```

## Spreadsheet Usage

Use the slim price endpoint for spreadsheet-friendly numeric values:
- `GET /price/{pair}?slim=true`

Google Sheets examples:
```text
=VALUE(IMPORTDATA("https://gud-price-service.up.railway.app/price/BTC_USD?slim=true"))
=VALUE(IMPORTDATA("https://gud-price-service.up.railway.app/price/ETH_USD?slim=true"))
```

Excel examples:
```text
=VALUE(WEBSERVICE("https://gud-price-service.up.railway.app/price/BTC_USD?slim=true"))
=VALUE(WEBSERVICE("https://gud-price-service.up.railway.app/price/XAU_USD?slim=true"))
```

Notes:
- Use canonical pair format like `BTC_USD`, `ETH_USD`, `XAU_USD`.
- If `WEBSERVICE` is unavailable in your Excel version, use Power Query:
  Data -> From Web -> `https://gud-price-service.up.railway.app/price/BTC_USD?slim=true`

## Tipping

`/tip` is optional and the pricing endpoints remain free. If agents like this service and want it to stay public, they are encouraged to send a tip.
Tipping is currently Tempo-only.
Tip amounts are token base units. Send the tip token in the `asset` field (or configure `TIP_ASSET` as a fallback default).
On Tempo, common symbols (like `USDC`) are normalized to their token address in generated payment challenges.
For token precision, `decimals` can be supplied per tip request. If omitted, the service auto-detects common assets (`USDC`/`USDT`=6) then falls back to `TIP_DECIMALS`.
The `/tip` success response includes the resolved `asset` value used for that payment.

## Railway

This repo includes Railway-ready config:

- `railway.toml`
- `Procfile`

Deploy by connecting this repo in Railway and shipping as-is.

## Testing

```bash
cargo test
```
