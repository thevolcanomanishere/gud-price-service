	# SEO Optimization Plan — gud-price-service

A practical, staged plan to maximise organic discoverability of
`gud-price-service.up.railway.app` across search engines, AI crawlers
(GPTBot, ClaudeBot, PerplexityBot), and developer audiences.

This service today is API-only: `GET /` returns `llms.txt` as
`text/plain`, and there is no HTML surface, no `robots.txt`, no
`sitemap.xml`, and no structured data. That is the single biggest
blocker to ranking for queries like "free chainlink price API",
"BTC USD JSON endpoint", "on-chain price feed API", etc.

---

## 1. Goals & target keywords

Primary intent: developers / AI agents looking for a free, hosted,
JSON HTTP wrapper around Chainlink price feeds.

Target head terms:
- "chainlink price api"
- "free crypto price api json"
- "on-chain price feed http api"
- "btc usd api endpoint"
- "gold price api" / "xau usd api"
- "chainlink oracle http wrapper"

Long-tail:
- "chainlink price feed in google sheets"
- "import chainlink price excel webservice"
- "shields.io chainlink badge"
- "llm agent price api chainlink"
- "tokenized equities chainlink feed list"

Success metrics:
- First-page Google ranking for 3+ long-tail queries within 90 days.
- 1k+ monthly organic clicks to the landing page.
- Referral traffic from GitHub README + Shields badge hotlinks.
- Indexing by GPTBot / ClaudeBot / PerplexityBot (verify via logs).

---

## 2. Quick wins (ship this week)

These are high-impact, low-effort changes.

1. **Serve an HTML landing page at `/`** (content-negotiated).
   - Keep `llms.txt` at `/llms.txt` and `/.well-known/llms.txt`.
   - For `GET /` with `Accept: text/html`, return a static HTML page.
   - For `Accept: text/plain` or curl default, keep returning llms.txt
     (preserve existing agent behaviour and tests).
   - Implementation: branch in `get_llms_txt` in `src/routes.rs` on the
     `Accept` header; embed HTML via `include_str!("../static/index.html")`.

2. **Add `robots.txt`** at `/robots.txt`:
   ```
   User-agent: *
   Allow: /
   Sitemap: https://gud-price-service.up.railway.app/sitemap.xml

   User-agent: GPTBot
   Allow: /

   User-agent: ClaudeBot
   Allow: /

   User-agent: PerplexityBot
   Allow: /

   User-agent: Google-Extended
   Allow: /
   ```

3. **Add `sitemap.xml`** listing the landing page, `/llms.txt`,
   `/discovery`, and (optionally) a curated list of top pair pages
   like `/price/BTC_USD`, `/price/ETH_USD`, `/price/XAU_USD`.

4. **Add canonical URL + OG/Twitter meta tags** to the HTML page.

5. **Register a custom domain** (e.g. `gudprice.dev` or
   `api.gudprice.xyz`). `*.up.railway.app` subdomains carry almost no
   domain authority and get heavily deduped by Google.

---

## 3. Landing page content blueprint (`/`)

Single static HTML file, <30 KB, no JS required, fast LCP.

Required sections (in order):

1. `<title>` — "gud-price-service · Free Chainlink Price API (BTC, ETH, XAU, 600+ feeds)"
2. `<meta name="description">` — 150–160 chars summary with primary keywords.
3. H1: "Free Chainlink Price API"
4. H2: "What it does" — 2-sentence pitch + live price widget fetched
   from `/price/BTC_USD`.
5. H2: "Quick start" — 3 `curl` examples (copy-button).
6. H2: "Endpoints" — table of all routes with one-line descriptions.
7. H2: "Supported pairs" — static table of top 20 pairs with a link
   to `/discovery` for the full list. Each pair name should be an
   internal link to an anchor that embeds the live slim price.
8. H2: "Use in Google Sheets / Excel" — copy-paste formulas.
9. H2: "Shields.io badges" — sample markdown.
10. H2: "FAQ" — JSON-LD `FAQPage` schema with 8–10 Q&As targeting
    long-tail queries ("Is this free?", "Which chains are supported?",
    "How fresh are prices?", "Can I self-host?").
11. Footer: GitHub link, llms.txt link, tip link.

Structured data (JSON-LD in `<head>`):
- `WebSite` with `potentialAction` SearchAction pointing at
  `/price/{pair}`.
- `SoftwareApplication` or `WebAPI` describing the service.
- `FAQPage` matching the FAQ section.
- `BreadcrumbList` on sub-pages.

Performance targets:
- LCP < 1.5s, CLS = 0, TBT < 50ms.
- Single self-hosted WOFF2 font (or system stack only).
- Inline critical CSS, no render-blocking JS.
- `Cache-Control: public, max-age=300, stale-while-revalidate=86400`.

---

## 4. Per-pair HTML pages (big organic win)

This is the highest-leverage SEO move. The service knows ~686 pairs.
Generate one HTML page per pair at `/p/{pair}` (new route, to avoid
breaking `/price/{pair}` JSON contract).

Template fields per page:
- `<title>`: `"{PAIR} Live Chainlink Price · Free JSON API"`
- H1: `"{DESCRIPTION} — Live Chainlink Price"`
- Live price block (server-rendered from current price at request
  time; fall back to cached value).
- "How to fetch" `curl` / Sheets / Excel snippets for that pair.
- "Feeds used" table: chain, address, updated_at.
- Internal links to 6 related pairs (same asset class).
- JSON-LD `Dataset` + `FinancialProduct` schema.
- Canonical: `https://<domain>/p/{PAIR}`.

686 well-interlinked pages with unique, data-backed content is a
strong topical cluster. Add these URLs to `sitemap.xml`, chunked
into `sitemap-pairs.xml` referenced from a sitemap index.

Guardrails:
- Rate-limit page render to avoid RPC amplification; reuse the same
  short TTL cache used by `/price/{pair}`.
- Return `Cache-Control: public, max-age=30`.
- Return `503` with cached stale content on provider failure, never
  a blank page (prevents Google soft-404s).

---

## 5. Technical SEO (server-side)

Add to `src/routes.rs` / middleware:

- **Canonical redirects**: force HTTPS, force non-trailing-slash,
  force lowercase host. Railway already does HTTPS; add
  `Strict-Transport-Security: max-age=63072000; includeSubDomains; preload`.
- **404 pages**: return HTML 404 for HTML `Accept`, JSON 404
  otherwise. Current unknown-pair path already returns JSON 404
  (keep for API clients).
- **Security headers** (helps with Lighthouse SEO + trust):
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`,
  `Content-Security-Policy` scoped to the HTML page only.
- **`Link: <canonical>; rel="canonical"` header** on HTML responses.
- **Structured `ETag` / `Last-Modified`** on HTML pages so Googlebot
  can do conditional GETs.
- **Gzip/brotli** — Railway's proxy should already handle this;
  verify with `curl -H 'Accept-Encoding: br' -I`.
- **`/favicon.ico`, `/apple-touch-icon.png`, `/manifest.webmanifest`**
  — missing icons can hurt SERP appearance and Lighthouse.

---

## 6. Off-page SEO

- **GitHub README**: add the live landing page URL above the badges,
  add keywords ("Chainlink API", "price oracle HTTP wrapper") to the
  repo description and topics (`chainlink`, `price-api`, `oracle`,
  `defi`, `rust`, `axum`).
- **Submit to directories**:
  - publicapis.dev / public-apis GitHub list (PR)
  - rapidapi.com (listing)
  - apilist.fun
  - chainlink ecosystem page / awesome-chainlink
  - awesome-rust
  - awesome-defi
- **Backlink sources**:
  - Dev.to / Hashnode article: "How to pull Chainlink prices into
    Google Sheets in one line" (targets long-tail).
  - r/ethdev, r/defi, r/rust "Show HN"-style posts.
  - HN Show HN submission.
  - Stack Overflow: answer existing questions about chainlink price
    ingestion with a link.
- **Shields.io badges in the wild**: each README that embeds a
  `/badge/{pair}` badge is a soft backlink. Encourage adoption by
  providing a "Copy badge" button on each pair page.

---

## 7. LLM discoverability (AI-SEO)

The service already has `llms.txt` which is great. Extend with:

- **`/.well-known/ai-plugin.json`** (OpenAI plugin manifest, still
  crawled by some agents).
- **OpenAPI 3.1 spec at `/openapi.json`** and `/docs` (Scalar or
  Redoc static render). Link from `llms.txt`.
- **`llms-full.txt`** variant with full endpoint + schema + example
  payloads inlined (llms.txt convention).
- Ensure `User-Agent: GPTBot|ClaudeBot|PerplexityBot|CCBot|Google-Extended`
  are explicitly allowed in `robots.txt` (see §2).
- Keep response bodies under ~4 KB for the landing page so entire
  content fits in a single LLM tool-call.

---

## 8. Analytics & verification

- Add **Plausible** or **Umami** (privacy-friendly, no cookie banner
  needed, ~1 KB script) — Google Analytics will tank LCP.
- **Google Search Console**: verify via DNS TXT on the custom domain,
  submit sitemap, monitor coverage and CWV.
- **Bing Webmaster Tools**: submit sitemap (Bing powers ChatGPT
  search and DuckDuckGo).
- **IndexNow**: ping Bing/Yandex on deploy with the list of changed
  URLs (cheap, one-liner).
- Log and dashboard crawler User-Agents in Railway logs to confirm
  GPTBot/ClaudeBot are hitting the site.

---

## 9. Content marketing (ongoing)

Low-frequency, high-quality posts hosted on `/blog/{slug}` (static
HTML, same template as landing page):

1. "Every Chainlink price feed in one JSON endpoint" — auto-generated
   tables from `/discovery`. Refresh weekly via CI.
2. "Chainlink price feeds by asset class: commodities, equities,
   macro, FX" — taxonomy hub linking to per-pair pages.
3. "Freshness and latency of Chainlink feeds across chains" —
   original data piece using this service's own probe results.
4. "Embedding live crypto prices in Notion / Obsidian / Raycast".
5. Monthly "new pairs added" changelog post.

Each post: 800–1500 words, original data tables, internal links to
3–5 pair pages, external link to Chainlink docs.

---

## 10. Implementation order

Phase 1 (1–2 days, highest ROI):
- [ ] Buy custom domain, point at Railway.
- [ ] Add `/robots.txt` and `/sitemap.xml` routes.
- [ ] Content-negotiate `/` to serve HTML landing page for browsers.
- [ ] Add OG/Twitter/canonical meta + FAQ JSON-LD.
- [ ] Submit to Google Search Console + Bing.

Phase 2 (3–5 days):
- [ ] Add `/p/{pair}` HTML pages + chunked sitemap.
- [ ] Add `/openapi.json` + `/docs`.
- [ ] Add favicon set + manifest.
- [ ] Wire Plausible analytics.
- [ ] IndexNow on deploy hook.

Phase 3 (ongoing):
- [ ] Publish 2 blog posts.
- [ ] Submit to 5 public API directories.
- [ ] Upgrade README with keyword-rich description + topics.
- [ ] Monitor GSC weekly; iterate on underperforming pages.

---

## 11. Non-goals

- No client-side rendering / SPA framework — static HTML only.
- No cookies / consent banners.
- No keyword stuffing or doorway pages; every URL must serve real
  data or genuinely useful content.
- Do not block any AI crawlers in `robots.txt`; discoverability is
  the whole point.
