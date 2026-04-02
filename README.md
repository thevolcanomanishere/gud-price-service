# gud-price-service

A minimal Rust webserver that serves live Chainlink prices using [`gud-price`](https://github.com/thevolcanomanishere/gud-price).

## Why this exists

`gud-price-service` wraps the `gud-price` Rust library in a simple HTTP API so other apps and agents can fetch price data without embedding on-chain RPC logic.

Built on top of your `gud-price` project:
- GitHub: https://github.com/thevolcanomanishere/gud-price

## Features

- Stateless HTTP API
- Live price reads via `gud-price`
- Short TTL in-memory cache
- Discovery endpoint for supported pairs (JSON or CSV via `format=csv`)
- Optional slim plain-text mode for easy machine consumption
- `llms.txt` served at `/llms.txt` and `/.well-known/llms.txt`
- Price endpoint always picks the feed with the most recent update across available chains
- Price responses only expose `updated_at` as a UTC ISO timestamp (no `started_at`)
- Chain-specific RPC overrides use the Lasso load-balanced endpoints for Ethereum, Arbitrum, and Base

## Endpoints

- `GET /health`
- `GET /discovery` (JSON default, add `?format=csv` for CSV output)
- `GET /price/{pair}` (JSON; returns the most recently updated feed across every supported chain and emits `updated_at` as UTC)
- `GET /price/{pair}?slim=true` (plain text price only)
- `GET /llms.txt`
- `GET /.well-known/llms.txt`

## Quick Start

```bash
cargo run
```

Optional env vars:

- `PORT` (default: `3000`)
- `PRICE_CACHE_TTL_SECS` (default: `5`)

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
```

## Railway

This repo includes Railway-ready config:

- `railway.toml`
- `Procfile`

Deploy by connecting this repo in Railway and shipping as-is.

## Testing

```bash
cargo test
```
