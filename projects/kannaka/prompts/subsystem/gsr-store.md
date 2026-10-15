# GS Records: the USDC Record Store Server

Covers `server/index.js` (routing, throttles, status mapping), `server/store.js`
(`Store`: purchases, settlement, the chain watcher, downloads),
`server/store-core.js` (pure rules: transfer matching, EIP-3009 authorization
checks, download tokens), `server/gasless.js` (`Relayer`), `server/atm.js`
(`Atm`: Coinbase Onramp sessions and 0x swap quotes), `server/read-body.js`,
and the store parts of `server/config.js`. Tests live in `test/store.test.js`,
`test/store-gasless.test.js`, `test/audit-findings.test.js`,
`test/throttle-lanes.test.js` and `test/atm.test.js`.

The store sells album downloads for USDC on Base. The chain is the payment
processor: `payTo` only receives, and the server holds no key that can move a
buyer's funds. The relayer key holds gas ETH only. What must never happen:

1. A purchase is marked paid by a Transfer that does not pay it, or one
   Transfer pays for two purchases.
2. A payer is bound to a purchase before the authorization is verified, or a
   refused attempt leaves a payer bound.
3. The relayer submits anything other than exactly the authorization the store
   issued, or submits twice for one purchase.
4. A client error is reported as 5xx (tells an agent to retry something that
   will never work), or a provider/key failure is reported as 4xx (tells the
   caller it is their fault).
5. A secret (relayer key, download secret, CDP key, 0x key, admin token) or a
   buyer's email reaches a log line or a response body.

## 1. Purchases and settlement (`store.js`)

`buy(sku, {email, fromAddr})`: 404 for an unknown or unpublished sku, 503 when
`enabled()` is false (needs a valid `payTo` and a `downloadSecret`). A supplied
`fromAddr` that is not an address or normalises to `ZERO_ADDRESS` is 400
`bad wallet address`. `fromBlock` is the head minus 30 blocks; a head-read
failure leaves it null and is logged, not thrown. The purchase starts in state
`awaiting`.

`matchTransfer(purchase, t, {payTo, headBlock, confirmations})` in
`store-core.js` is the single definition of "this Transfer pays this
purchase". Every rule returns a named reason: `removed`, `wrong_token`
(must be `USDC_BASE`), `wrong_recipient`, `wrong_amount` (exact string
equality of micro-units, no tolerance), `wrong_sender` (only when the purchase
names `fromAddr`), `too_early` (before `fromBlock`), `unconfirmed`. A diff that
loosens amount equality, drops the token check, or checks the sender on an
unnamed purchase changes who can pay.

`ledgerKey(t)` is `usdc:<txHash>:<logIndex>` and is the `ledger` table's
primary key. **This key is the only thing that stops one Transfer paying two
purchases.** `settle()` inserts it first; on a unique violation it returns
`already` for the same purchase, takes over an `unmatched` row exactly once
(`UPDATE ... WHERE key=? AND kind='unmatched'`), and otherwise answers
`transfer_already_used`. The purchase moves `awaiting -> paid` with a
`WHERE state='awaiting'` guard; the receipt mail fires only when that update
changed a row. Any new settle path must go through `settle()`; writing
`state='paid'` directly bypasses the ledger key.

For a purchase with no `fromAddr`, `matchTransfer` does not check the sender:
any confirmed Transfer of the exact amount to `payTo` after `fromBlock` can be
claimed by whoever holds the `publicId`, once.

`comp()` writes a `comp:<purchaseId>` ledger row of 0 cents and moves the state
the same guarded way. It is reachable only from `/admin/`.

## 2. The watcher (`Store.scan`)

Scans `eth_getLogs` on `USDC_BASE` for Transfer to `addrTopic(payTo)`, in
chunks of `RPC_CHUNK` blocks, up to `safeHead = head - (confirmations - 1)`,
persisting `usdc_last_block` in `kv` after each chunk. First run starts at the
current safe head (no back-scan). Guarded by `_scanning` so overlapping ticks
do nothing.

Candidates are `awaiting` purchases with `amount_micro = t.micro` **and
`from_addr = t.from`**, oldest first, filtered through `matchTransfer`. So the
watcher only ever settles a purchase that named its payer (or had one bound by
a gasless relay). A Transfer nobody explains is recorded as `kind='unmatched'`
with an empty `order_id`, to be taken over by a later `/tx` claim. A diff that
lets the watcher match unnamed purchases by amount alone hands an unrelated
payment to whichever purchase is oldest.

## 3. Gasless checkout (EIP-3009)

`gaslessTerms` issues a nonce (`randomBytes(32)`) and `authValidBefore`
(`authMinutes`, default 30) once per purchase (`WHERE auth_nonce IS NULL`), and
re-issues only when the deadline is within 60 s **and** `relay_tx IS NULL`. An
unnamed purchase gets `ZERO_ADDRESS` as a placeholder `message.from`; the
signer substitutes its own address.

`checkAuthorization(purchase, payTo, auth)` must pass before anything is
written. In order: state `awaiting`; a nonce and deadline were issued;
`auth.from` is an address and not `ZERO_ADDRESS` (`bad_from`); equals
`purchase.fromAddr` when one is set (`wrong_sender`); deadline more than 60 s
away (`expired`); `ethers.verifyTypedData` over `authMessage` (exact amount,
`payTo`, issued nonce, issued deadline, `validAfter` 0) recovers `auth.from`
(`bad_signature`). Any parse failure is `bad_signature`.

`Store.relay(purchase, auth)` order, load-bearing:
1. Not `awaiting`, or a relay already recorded: return `already`.
2. `checkAuthorization` (pure). Refusal returns here with nothing written.
3. Claim the slot: `UPDATE purchases SET relay_tx='pending', from_addr =
   COALESCE(from_addr, auth.from) WHERE relay_tx IS NULL AND state='awaiting'`.
   Losing the claim returns `already`. This binds the payer of an unnamed
   purchase, and only after step 2.
4. `Relayer.relay` re-runs `checkAuthorization`, then refuses on `relayer_busy`
   (`relayPerHour`), `relayer_dry` (below `relayMinWei`), `insufficient_usdc`,
   `authorization_used` (on-chain `authorizationState`), RPC errors; only then
   `transferWithAuthorization`.
5. On failure, `relay_tx` is reset to NULL and `from_addr` restored to its
   prior value (`WHERE relay_tx='pending'`), so another wallet can still pay.

**Moving the payer binding ahead of the signature check, or dropping the
unbind on failure, is the defect this ordering exists to prevent**: a refused
attempt from a wrong address would leave the purchase naming it, and the real
signer would then get `wrong_sender`. `hydratePurchase` maps the `'pending'`
sentinel to `relayTx: null`; the SQL `relay_tx IS NULL` predicate, not the
hydrated field, is what serialises two clicks.

A successful relay does not settle. The purchase stays `awaiting` until the
watcher (payer now bound) or a `/tx` claim settles it through `settle()`.

## 4. Downloads

`signDownload` is an HMAC-SHA256 over `publicId.exp`, truncated to 32
base64url characters; `verifyDownload` checks format, expiry, then
`timingSafeEqual`. `/dl/<publicId>` requires `state === 'paid'` (404
otherwise) and a valid token (403). Tokens are stateless and minted fresh on
each purchase read; `tokenHours` default 72.

## 5. HTTP surface and status mapping (`index.js`)

Throttles are per `(ip, lane)` token buckets (`throttle`), separate lanes:
`checkout` (10/min, buy and authorize), `claim` (40, `/tx`), `onramp` (6),
`swap` (20), `vesper` (12), `album` (20), `desk` (30). A refusal is 429 with
`retryAfterSec` and a `Retry-After` header. Lanes must stay independent; one
shared bucket let a visitor who priced swaps lose checkout. `ip` is the
**first** `X-Forwarded-For` entry; the shipped nginx config uses
`$proxy_add_x_forwarded_for`, which appends, so that entry is caller-supplied.
`ip` is a throttle key and a hint passed to Coinbase as `clientIp`, never an
authorization input.

| Route | Success | Refusal |
|---|---|---|
| `POST /api/store/<sku>/buy` | 402 with `purchase` and `payment` (terms + `gasless`) | 404, 503, 400 via thrown `status`; 429 from the throttle |
| `POST /api/purchase/<id>/authorize` | 200 | 409 with `reason` for every `ok:false`, including relayer-side reasons |
| `POST /api/purchase/<id>/tx` | 200 | 409 with `reason`; an RPC throw becomes 500 |
| `POST /api/atm/session` | 200 | 400 if `CLIENT_REASONS.has(reason)`, else 503 |
| `GET/POST /api/atm/swap` | 200 | 400 for `CLIENT_REASONS`, 409 `no_liquidity`, else 503 |
| unknown `/api/*` | | 404 JSON `{error: 'not found'}` |

`CLIENT_REASONS` is `bad_address`, `bad_amount`, `bad_taker`, `unknown_token`;
`test/atm.test.js` pins that every `onramp_*`/`swap_*` provider or key reason
is outside it. A new ATM reason must be placed deliberately on one side. Note
the asymmetry that exists today: `/authorize` answers 409 for `relayer_dry`,
`relayer_busy`, `send_failed` and `rpc: ...` as well as for `bad_signature`,
while the published agent guide (`/api/store/agent-guide`) says a 4xx is the
caller's to fix. A diff that splits those by reason should follow the ATM's
`CLIENT_REASONS` pattern; a diff that adds a new server-side reason to these
routes inherits the 409.

The outer `catch` answers `e.status || 500`; a 5xx body is always
`internal error` with the stack logged, a 4xx body is `e.message`. So a thrown
error with a `status` below 500 publishes its message: never attach a 4xx
status to an error whose message carries a path, key or provider body.
`readJson` throws 413 past the per-route byte limit and 400 on invalid JSON.

`/admin/*` requires `Authorization: Bearer <adminToken>`; 503 when no token is
configured, 401 otherwise.

## 6. The ATM (`atm.js`)

The store never holds fiat or coins. `session()` refuses when
`onrampReady()` is false (CDP key id and secret both set; the project id alone
opens nothing), on a bad address, and on any amount not in `AMOUNTS` (refused
with the menu, never rounded to a default). The CDP JWT (`cdpJwt`) is bound to
one method/host/path, lives 120 s, and is never logged. `swapQuote()` requires
`swapReady()` (0x key plus a valid `feeRecipient`), accepts only
`SELL_TOKENS`, parses amounts with `toUnits` (bigint, no floats), sends the
fee as `swapFeeBps` taken in USDC to `feeRecipient`, and for a firm quote
requires a taker. A native-ETH firm quote checks the taker's balance through
the relayer's provider because 0x does not report that shortfall. The 0x key
travels only in the request header; log lines carry the provider's status and
a 200-character slice of its response body.

## 7. Secrets and personal data

`GSR_RELAYER_KEY`, the download secret, `adminToken`, the CDP secret and the
0x key are read in `config.js` and must not appear in any `log(...)` call,
response, or error message. Existing log lines carry sku, `publicId`, payer
address and tx hash (public chain data), never `purchase.email`. The email
goes only to `smtpSend` for the receipt.

## 8. What is NOT a bug

- The buy endpoint answering 402 on success: it is the payment-required
  answer an agent pays against.
- An unnamed purchase not being settled by the watcher: by design it is
  settled only by a `/tx` claim or after a relay binds the payer.
- `ZERO_ADDRESS` in `typedData.message.from`: a placeholder the signer
  replaces; `checkAuthorization` refuses it as a signer.
- `settle()` converting micro-units to cents with `Math.round`: the ledger
  `amount_cents` is bookkeeping; matching uses exact micro-units.
- The first scan starting at the head without back-filling history.

## Checklist

- [ ] Any path that sets `state='paid'` without inserting the `ledgerKey`?
- [ ] Any write (payer binding, slot claim) before `checkAuthorization` passes?
      Is every failure path after the claim restoring `relay_tx` and
      `from_addr`?
- [ ] `matchTransfer` loosened (amount tolerance, token, recipient, sender,
      confirmations)?
- [ ] New refusal reason: 4xx only if the caller caused it, 5xx (or the ATM's
      503) if a provider, key or float did?
- [ ] New route: own throttle lane, and `ip` used only as a throttle key?
- [ ] New log line or error message: free of keys, tokens, secrets and email?
- [ ] Tests: `test/store-gasless.test.js` signs with real `ethers` wallets
      against a fake chain; a new authorization or relay rule should be pinned
      the same way, not with a stubbed `checkAuthorization`.
