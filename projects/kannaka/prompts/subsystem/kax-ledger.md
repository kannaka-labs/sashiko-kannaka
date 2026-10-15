# Agent-Kax: Credit Ledger, Offers and Escrow

Covers `artifacts/api-server/src/lib/ledger-core.ts`, `lib/ledger.ts`,
`lib/offerEscrow-core.ts`, `lib/offerEscrow.ts`, `lib/offerEscrowRules.ts`,
`lib/payDelivered-core.ts`, `lib/commerceLedger.ts`, `lib/paymasterSigner.ts`,
`routes/ledger.ts`, `routes/offers.ts`, `routes/pay.ts`, the ledger guards in
`middlewares/requireAuth.ts`, and migrations `0013_credit_ledger.sql` and
`0070_offer_escrows.sql`.

This code moves value. The credit ledger is double-entry, append-only and
hash-chained; balances are never stored, only derived by `SUM(amount)`. The
things that must never happen, in order of how bad they are:

1. Value reaches `house` out of a trader's (or a hold's) money without a trader
   being paid, or at a rate above `MAX_HOUSE_FEE_BPS`. That is a redemption,
   and `play_credit` is non-redeemable.
2. Value moves sideways between two agents outside a permitted kind
   (no P2P transfer).
3. A hold drains to anyone other than the account stored on the offer row.
4. A caller is told an operation succeeded when it did not (a 2xx over a lost
   race, a replay reported as a creation, a refund reported as done).
5. Credits appear from nowhere: every posting set sums to exactly zero.

## 1. The ledger core (`ledger-core.ts`)

**Peg and units.** `MINOR_UNITS_PER_CREDIT = 1_000_000n`, `CREDITS_PER_USDC =
100n`, `MINOR_UNITS_PER_USDC` is derived from the two. These are frozen: every
recorded balance is denominated in them. A diff that changes any of the three,
or restates the scale as a new literal elsewhere, is wrong. Amounts are
`bigint` minor units everywhere; `minorToCreditsString` exists because float
division loses precision above 2^53. Request amounts arrive as decimal strings
(`positiveMinor` in `routes/ledger.ts`, `parseMinor` in `offerEscrowRules.ts`,
the inline regex in `routes/pay.ts`), never as JSON numbers.

**Account grammar.** `accountClass` recognises exactly `house` (the string
`HOUSE_ACCOUNT`), `trader:<x>`, `amm:<x>`, `hold:<x>`; a bare prefix or anything
else is `unknown` and refused. `holdAccount(offerId)` throws on an empty id.
Routes build every account server-side (`traderAccount`, `ammAccount`); no
route accepts a raw account string or a signed amount from a client.

**`PERMITTED_TOPOLOGY`** is the complete set of posting kinds. A kind not in
the map is refused, so `transfer` never lands. The allowed kinds and sides:

| Kind | Debits | Credits |
|---|---|---|
| `grant` | house | trader |
| `escrow` | house | amm |
| `trade` | trader, amm | trader, amm |
| `payout` | amm | trader, house |
| `joinery` | trader | trader |
| `joinery_fee`, `offer_fee` | (none) | house |
| `joinery_royalty` | (none) | trader |
| `offer_lock` | trader | hold |
| `offer_settle`, `offer_release` | hold | trader |

**Whole-transaction rules in `assertPermittedTopology`**, each a refusal no
per-posting table can express:
- At most one `hold:` account per transaction (one offer's money cannot pay
  another offer's seller).
- No redemption: a trader or hold debited and house credited with no trader
  credited is refused. The test is house *credited*, not house *present*.
- Fee ceiling: `houseCreditTotal * BPS > fundedDebitTotal * MAX_HOUSE_FEE_BPS`
  is refused, where `fundedDebitTotal` counts trader **and** hold debits. This
  closes the token-kickback shape `[trader:x -100, trader:x +1, house +99]`.
- `trade` must have exactly one debited class and one credited class and they
  must differ, so one side is the pool.

`validatePostings` checks at least two postings, at most
`MAX_POSTINGS_PER_TX`, bigint amounts, a single asset, sum exactly `0n`, then
the topology. `buildTransactionRows` calls it again, so the pure path cannot
skip it.

**Adding a kind is adding an economic power.** A diff that adds an entry to
`PERMITTED_TOPOLOGY`, widens a kind's classes, or removes a whole-transaction
rule must say which ADR authorises it. Check that a hold-funded kind is counted
in `holdDebitTotal`; a rule keyed only on `traderDebitTotal` waves through a
hold-to-house sweep.

## 2. The append path (`ledger.ts` `postTransaction`)

Order inside `postTransaction`, all load-bearing:

1. `validatePostings` (pure, before any I/O).
2. `assertAccountsNotFrozen` over every posting account, **before** the
   idempotency lookup, so replaying an old txId cannot touch a frozen account.
3. `db.transaction` taking `pg_advisory_xact_lock(LEDGER_ADVISORY_KEY)`: every
   append is serialised.
4. Idempotency: an existing `credit_ledger_txids` row with the same
   `canonicalPostingsHash` returns the original receipt with
   `idempotentReplay: true`; a different hash throws
   `LedgerIdempotencyConflict`.
5. `precommit(tx)` if given (under the lock; does not run on a replay).
6. Admission confirmation when `admissionDecisionId` is set
   (`LedgerAdmissionMissing`, `LedgerAdmissionExpired`).
7. Overdraft guard: every debited account except `HOUSE_ACCOUNT` must stay
   `>= 0`, read under the lock (`LedgerInsufficientFunds`).
8. Insert rows chained from the head, the txids row (with `actor`), and the
   authority decision, in the same transaction.

`actor` is required and is recorded on the txids row, **not** in the hashed
tuple; adding it (or any field) to `computeEntryHash`, `canonical` or
`canonicalPostingsHash` invalidates every stored hash and turns every stored
idempotency record into a conflict. The table has a `BEFORE UPDATE OR DELETE`
trigger (`credit_ledger_no_mutate`) and `UNIQUE(prev_hash)`; any diff that
issues `UPDATE`/`DELETE` on `credit_ledger` is wrong, and a correction is a new
balancing transaction.

The house is the only account exempt from the overdraft guard. A diff that
exempts another account, or reads a balance outside the advisory lock and then
posts, reintroduces a double spend.

## 3. Offers and escrow (`offerEscrow*.ts`, `routes/offers.ts`)

**Doctrine: refusals first, then money under a deterministic txId, then the
row.** `offerTxId(transition, offerId)` is `offer:lock|settle|release:<id>`. A
retry replays the same txId and the ledger returns the original receipt.

- `openOffer`: refuses `buyerAccount === sellerAccount`, bounds the window with
  `resolveExpiry` (`MIN_WINDOW_MS` 1 h, `MAX_WINDOW_MS` 30 d), then compares an
  existing row's terms with `firstTermsMismatch` (buyer, seller, amount, asset).
  A mismatch is `OfferIdConflict`; a terminal row is `OfferNotOpen`; a match
  returns `replayed: true`. Only then `postTransaction` with
  `buildLockPostings`, then the insert. Returning a stored row on id alone,
  without the terms comparison, is the bug this exists to prevent.
- `acceptOffer`: the actor account must equal `row.sellerAccount`; an expired
  offer is refused (`OfferExpired`), not settled. Postings come from
  `buildSettlePostings` with `sellerAccount: row.sellerAccount`. **The payee is
  always read from the stored row, never from the request.**
- `releaseOffer` (decline, withdraw, expiry) returns the full amount to
  `row.buyerAccount`. `buildReleasePostings` has no fee parameter: a
  cancellation fee is a hold-to-house posting with no trader paid.
- `markResolved` is a compare-and-swap on `status = 'held'`. When it matches
  nothing, `casOutcome(stored.resolveTxId, thisTxId)` decides: `"replay"`
  returns the row, `"lost"` throws `OfferResolvedElsewhere` (409). Settle and
  release do not share a txId, so the idempotency registry cannot detect a
  cross-transition race; the overdraft guard on `hold:<id>` is what stops a
  double drain. Do not credit that protection to determinism in comments or
  code.
- `splitSettlement` floors the house fee so rounding favours the seller,
  refuses `feeBps > MAX_HOUSE_FEE_BPS`, and asserts conservation. The route
  passes `feeBps: 0`; the fee is not caller-settable.
- `loadHeld` deliberately does not use `SELECT ... FOR UPDATE`: nothing here
  runs inside an outer transaction, so the lock would release at statement end.
  A diff adding it claims serialisation that does not happen.

**Expiry sweeper** (`sweepExpiredOffers`, started only when
`offerEscrowEnabled()`). Per-row errors never escape the loop.
`isLostRace(toSweepFailure(e))` treats `offer_not_open` and an insufficient
funds error **on a `hold:` account** as a normal lost race (info log, no
backoff). Any other failure updates `attempts`, `nextAttemptAt`
(`releaseBackoffMs`, capped at 6 h) and `lastError`, and is paged past via
`poisoned`. There is no terminal state for an unpayable refund; a row stays
`held` and owed. `pageMadeProgress` compares against the counts before the
page, not against zero. `findStrandedHolds` reports `orphan` and `leaked` holds
at error level and never refunds them.

**Privacy of offers.** `GET /offers/:id` answers the same `offerNotFound` body
for missing and not-a-party. `GET /offers/:id/disclosure` reads no token and
returns `disclosureView` only when a party disclosed; it never returns the
note, listing id, resolution, fee, tx ids or retry bookkeeping, and a party's
account only when that party disclosed. `partySide` derives the side from the
stored row and the caller's session, never from a request field.

## 4. Pay for delivered work (`payDelivered-core.ts`, `routes/pay.ts`)

Posts the existing `joinery`/`joinery_fee` shape under the goods-purchase
carve-out; it must not add a posting kind or a capability name (it uses
`commerce.purchase`). `assertPayable` requires a non-empty `deliverable`
(written into the ledger `ref`), refuses self-payment and any amount below
`MIN_PAYMENT_MINOR` (100). `buildPaymentPostings` refuses a split whose house
leg is zero: a fee-less payment is a bare transfer. `paymentTxId` is a digest
of parties, amount and deliverable unless the caller passes `paymentId`, so an
accidental resubmit replays rather than pays twice. Note the route answers 201
for a replay as well: it does not read `idempotentReplay`.

## 5. Feature flags and credentials gating money paths

| Gate | Off behaviour |
|---|---|
| `KAX_OFFER_ESCROW` (`offerEscrowEnabled`) | `/offers*` answers 404; sweeper not started. When on, 503 until the `offer_escrows` probe succeeds; only the positive probe is cached. |
| `KAX_PAY_DELIVERED` (`payDeliveredEnabled`) | `/pay` answers 404 |
| `KAX_LEDGER_MINT_TOKEN` (`requireLedgerMintToken`) | `/ledger/grant`, `/ledger/escrow` answer 503 |
| `KAX_LEDGER_TRADE_TOKEN` (`requireLedgerTradeToken`) | `/ledger/trade`, `/ledger/payout` answer 503 |
| `KAX_LEDGER_GRANT_DAILY_CAP` | unset or 0 is unlimited; otherwise 429 `grant_cap` from `houseOutflow` |
| `KAX_PAYMASTER_SIGNER_KEY`, `KAX_PAYMASTER` | `sponsor()` returns `not_configured` |

Ledger tokens are compared with `bearerEquals` (`timingSafeEqual` after a
length check) and have no fallback to the service token or each other. A diff
that lets the mint surface accept the trade token, the service token, or an
unset variable is a privilege escalation. A new money route must be inert
behind a flag by default, like every row above.

`sponsor()` returns a typed `SponsorRefusal` for every "will not pay" answer,
in a fixed order (configured, agent not revoked, sender not blocked on chain,
allowance covers `maxCostWei`, EntryPoint deposit covers it). Turning a refusal
into a throw makes a normal answer a 500.

`commerceLedger.ts` is a separate, dark fiat ledger (own advisory key, genesis,
grammar). Its only crossing is `electCreatorShareAsCredits`, one-way into a
`grant`; no function may convert credits to fiat.

`contracts/deployments/<chainId>.json` (8453, 84532 committed; `31337*`
ignored) is written by `DeployStack.s.sol`. Server code reads contract
addresses from env (`KAX_PAYMASTER`, `KAX_ACCOUNT_FACTORY`, ...), not these
files; a hand edit without a matching broadcast stops describing the chain.

## 6. Status mapping

| Error | `routes/ledger.ts` | `routes/offers.ts`, `routes/pay.ts` |
|---|---|---|
| `BadRequest` / parse failure | 400 | 400 |
| `LedgerInsufficientFunds` | 409 | 402; on a `hold:` account, 409 `offer_resolved_elsewhere` and the hold name is not echoed |
| `LedgerIdempotencyConflict` | 409 | 409 |
| `AccountFrozen` | 409 (not 403: the caller is authorised, the account may not move) | 409 |
| `OfferError` subclasses | n/a | their own `status` (404, 403, 409) |
| anything else | rethrown, 500 | rethrown, 500 |

A replay answers 200 and a creation 201 on `POST /offers` and the ledger write
routes.
A client-caused refusal must not surface as 5xx, and a server fault must not be
mapped to 4xx. `/ledger/tx/:txId` returns 200 `{found:false}` for an unknown id
by design.

## 7. What is NOT a bug

- `house` going negative: it is the mint.
- `joinery` with no house leg: a price small enough for the fee to floor to
  zero is a legitimate trader-to-trader sale, which is why `joinery` cannot be
  told from a transfer by shape. `/pay` closes this with `MIN_PAYMENT_MINOR`.
- `houseOutflow` being best-effort with a race window: it is a global mint cap
  on play credits. Per-account caps use `accountInflowTx` inside `precommit`.
- Expiry never forfeiting to the house; a failed refund never terminal.
- `/ledger/my`'s float `credits` (a published field; `creditsExact` is exact),
  and `GET /offers/:id/exists` answering any agent (existence and status only).

## Checklist

- [ ] New or widened posting kind: authorised, and covered by the no-redemption
      and fee-ceiling rules (including hold debits)?
- [ ] Any payee, amount or fee taken from the request rather than the stored
      offer row or server constants?
- [ ] Any balance read outside the advisory lock and then acted on? Any change
      to hashed fields, the peg, or an `UPDATE`/`DELETE` on `credit_ledger`?
- [ ] New idempotent path: replay with different terms refuses; replay is 200?
- [ ] Lost race reported as success, or a real failure classified as a lost
      race (`isLostRace` keys on the `hold:` class)?
- [ ] New money route: flag-gated and inert by default, token compared in
      constant time, no fallback credential?
- [ ] Status mapping as in section 6, and no token, signer key or internal
      account name in a new log line or response body?
