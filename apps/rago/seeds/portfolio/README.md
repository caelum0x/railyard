# Portfolio seeds (Wave P PY1.5, contract 5.3)

One seed per product for the Railyard consumer contract: the meters Scribase,
Menivor and Rally post to Rago, each under a $0 mirror plan (the product's own
Dodo or credit billing stays the charge of record). Import each into that
product's own Rago org with `../import.sh <seed>`.

Every event: `{ transaction_id: "<product>:<meter>:<source id>",
external_customer_id, code: "<meter>", timestamp, properties }`. A repeated
`transaction_id` is a no-op, so every sender can retry freely.

`validate.mjs` checks the seeds against the contract fixtures
(`tests/contract/consumers/`), and runs in `node --test`.

## Add-on grants (Wave P PY2.4, contract 5.4)

`grants.json` is the portfolio grants org: one count meter per grant
(`portfolio.grant.launch_reel`, `portfolio.grant.mail`) under a $0 monthly plan
whose package charges carry the grant as free units, so Rago's current usage is
the balance. Scribase subscribes each organization (`portfolio-grants-<org>`)
when its add-on is on; Menivor and Rally debit with
`<aud>:grant:<org>:<YYYY-MM>:<n>` and refuse a debit beyond the balance
(`tests/contract/fixtures/grants/contract.json`, `tests/contract/grants.test.ts`).
