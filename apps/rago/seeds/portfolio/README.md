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
