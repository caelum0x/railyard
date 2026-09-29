# Portfolio reconciliation (Wave P PY1.5)

Nightly check for the Railyard consumer contract: every product's own counts
against Rago's totals, per customer, meter and UTC month. Any difference exits
1 and names the rows; the timer's failure pages like the Relay reconciliation
(`tools/reconcile-relay-rago`).

```sh
node tools/reconcile-portfolio/reconcile.mjs --product counts.json --rago totals.json
```

Each lane exports its own counts (Scribase: add-on and messaging tables;
Menivor: its ledger; Rally: its send and reveal tables). The Rago side is the
query in `reconcile.mjs`'s header, run read-only in the shared Postgres. On
the fixtures in `test/fixtures/` the difference is 0.
