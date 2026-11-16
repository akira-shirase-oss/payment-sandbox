# Payment Sandbox — end goal

**Simulated money, real accounting: every synthetic balance can be explained
from journal entries.**

An independently owned payment platform for development and continuous
integration. CMS test stores use our checkout, APIs and signed webhooks
without a Stripe account; a separate simulated network supplies approvals,
declines and uncertain outcomes. We implement our own internal accounting
and recovery. **This is not a live processor** — no real cards, balances or
banking connection, and no claim of exact card-network fidelity.

## In scope (the first useful release)

- One synthetic purchase end to end: create payment → authorize hold →
  capture → post balanced entries → emit webhook event → test fulfillment →
  partial refund and reconcile.
- A balanced ledger — immutable posted entries, corrections by reversal —
  plus an inspection UI that shows operations, postings and balances.
- A durable signed webhook, delivered at-least-once, to a consumer that
  deduplicates and fulfills under its own business key.
- The recovery drill: kill the server after the simulated network accepts
  an operation but before its response is stored; recover **without**
  another logical charge or fulfillment.

## Full direction (later milestones, each needs an explicit contract)

Simulated fees, settlement, payouts, disputes, invoices and fixed recurring
plans driven by virtual test clocks.

## Out of scope (explicit exclusions)

- Real payment credentials, money custody, redeemable balances — never.
- Any claim of exact card-network fidelity; real processing is a separate
  business and a separate design.
- Production use of simulated evidence: no production store may fulfill a
  real order from a sandbox result.

## Non-negotiable invariants

1. **The journal is the only authority.** Checked integer amounts with an
   explicit currency scale; debits and credits balance per currency; posted
   entries are immutable and corrected only by reversing entries; unique
   business operations; materialized balances reconcile to the journal.
2. **No database transaction spans a simulator call.** Operation identity is
   persisted before the network call; the simulator stores its own result; a
   lost response remains `uncertain` until readback, and reconciliation
   uses that same identity.
3. **Capacity is reserved transactionally.** Payment, authorization hold,
   capture, refund and fulfillment are distinct records; capture/refund
   capacity is reserved inside the transaction so simultaneous requests
   cannot overrun it — the excess fails.
4. **At-least-once delivery, deduplicated consumption.** Inbox/outbox ids
   prevent duplicate logical work but cannot eliminate repeated external
   deliveries; fulfillment carries its own unique business key.
5. **Sandbox-only by construction.** Synthetic payment tokens, sandbox-only
   keys and prominent labels; raw-card fields rejected; production provider
   routes disabled; outbound allowlists; project isolation; hidden secrets;
   explicit confirmation for dataset reset.
6. **Virtual time cannot rewrite posted history.** Crash, clock jump or
   backup restore resumes and reconciles — never blind replay, never
   rewriting committed journal history.

## Required failure cases (contract, not options)

| Trigger | Required result |
|---|---|
| Same key, different amount | Conflict; never silently reinterpret a previously accepted operation |
| Concurrent capture/refund | Capacity and journal constraints hold; the unauthorized excess request fails |
| Duplicate or reordered webhook | Consumer deduplicates and reconciles state; fulfillment has its own unique business key |
| Crash, clock jump or backup restore | Resume/reconcile without blind replay; virtual time cannot rewrite already posted history |

## Ship gate

Ship when a development store can test payment failures reproducibly **and**
every synthetic balance can be explained from journal entries.

## Evidence plan

Two projects, 100,000 seeded ledger postings, the mock network. Measure
100 operations/s sustained, a 5× short burst and a hot-account refund race;
record commit p95/p99, lock waits, journal imbalance and duplicate logical
operations. The correctness gate is **zero** imbalance, unauthorized
transfer or duplicate fulfillment in the defined tests. Full runs are
million-scale per the workspace standard; results are committed with
machine + commit. Numbers in the guide are proposed lab workloads, not
measured performance. **Never send benchmark traffic to real payment
networks.**
