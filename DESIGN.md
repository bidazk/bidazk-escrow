# Bidazk Escrow: design

The buyer pays into a contract, not to the seller and not to Bidazk. The money
can only ever leave to that deal's buyer or that deal's seller. Time settles the
deal when either side goes silent; a referee settles it only when the buyer
disputes.

## Roles

| Role | Who | Power |
|---|---|---|
| Buyer | Pays | Funds the deal, confirms delivery, disputes inside the inspection window |
| Seller | Delivers | Marks delivered before the deadline, cancels (which refunds the buyer) |
| Arbiter | Bidazk | Only disputed deals, and only a split between that buyer and that seller |
| Anyone | Any wallet | Triggers time-based outcomes once the clock allows. Safe because the money can only go to the stored party |

The arbiter and the settlement token are fixed in the constructor. A per-deal
token argument would let a caller point the contract at a malicious token.

## States

```
                       ┌──────────── cancel (seller) ─────────────┐
                       │                                          ▼
 create (buyer) ──▶ FUNDED ── deadline passed, not delivered ──▶ REFUNDED
                       │         (anyone)
         mark_delivered (seller, before deadline)
                       ▼
                   DELIVERED ── inspection window ends ──▶ RELEASED
                       │         (anyone)                    ▲
            dispute (buyer, inside window)                   │
                       ▼                                     │
                   DISPUTED ── resolve (arbiter) ──▶ RESOLVED│
                                                             │
     confirm (buyer) from FUNDED or DELIVERED ───────────────┘
```

`CANCELLED` is the terminal state for a seller cancel (drawn into REFUNDED
above for space). Terminal states: RELEASED, REFUNDED, CANCELLED, RESOLVED.
Nothing moves a deal out of them.

## Transitions

| Function | Signs | From | Condition | Money |
|---|---|---|---|---|
| `create` | buyer | new | amount > 0; buyer ≠ seller; arbiter is neither; windows in bounds | buyer → contract |
| `mark_delivered` | stored seller | FUNDED | now ≤ deadline | none; records `delivered_at` |
| `confirm` | stored buyer | FUNDED, DELIVERED | none | contract → seller |
| `dispute` | stored buyer | DELIVERED | now < delivered_at + inspection | none; frozen |
| `claim_release` | anyone | DELIVERED | now ≥ delivered_at + inspection | contract → seller |
| `claim_refund` | anyone | FUNDED | now > deadline | contract → buyer |
| `cancel` | stored seller | FUNDED | none | contract → buyer |
| `resolve(buyer_share)` | arbiter | DISPUTED | 0 ≤ share ≤ amount | share → buyer, rest → seller |

## Invariants

1. Money leaves only to the deal's buyer or seller. No function takes a destination.
2. Each deal pays out at most once. Every function checks state first; terminal
   states reject everything; state is written before any transfer.
3. Money is never stuck forever: silent seller → refund after deadline; silent
   buyer → release after inspection.
4. Identity is checked against addresses stored at creation, never arguments.
5. Before the deadline, only the seller can trigger a refund (`cancel`), so a
   delivery in progress cannot be pulled out from under them.

## Loopholes and what closes them

| Scenario | Closed by |
|---|---|
| Seller never ships | `claim_refund` after deadline |
| Seller out of stock | `cancel`, instant refund |
| Buyer receives item and goes silent | `claim_release` after inspection |
| Wrong or damaged item | `dispute` → arbiter split |
| Seller marks delivered without delivering | Buyer disputes in window (v2: delivery PIN) |
| Buyer cancels mid-delivery | No buyer cancel; refund opens only after deadline |
| Buyer claims "never delivered" after delivery | Refund closes once delivered; only dispute remains |
| Double confirm / pay twice | State check; terminal states reject all |
| Refund after confirm | Confirm is final; unhappy buyers dispute instead |
| Buyer and seller are the same wallet | `create` rejects |
| Arbiter trades with itself | `create` rejects arbiter as either party |
| Someone confirms another's deal | Stored-buyer `require_auth` |
| Arbiter key stolen | Only disputed deals, only split between the two parties |
| Malicious token | Token fixed at construction |
| Clock manipulation | Ledger timestamp |
| Deal data archived | TTL extended on every write and on instance reads |
| Admin rewrites the rules | Contract has no upgrade function |

## Accepted risk

An arbiter that never resolves leaves a dispute frozen. Production answer: an
operating SLA (resolve within 7 days); v2 adds a dispute timeout defaulting to
refund.

## Not built yet

Platform fee on release · delivery PIN (hash stored, preimage required to mark
delivered) · USDC instead of XLM · requests and seller bids feeding `create`.
