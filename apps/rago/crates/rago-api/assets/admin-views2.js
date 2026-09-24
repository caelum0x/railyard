"use strict";
// Rago Billing console — views: wallets, coupons, add-ons, taxes, webhooks,
// dunning, events, data exports, settings. Boots the app at the end.
(function () {
  const R = window.Rago;
  const { $, esc, money, date, datetime, badge, api, enc, table, kv, btn, icon } = R;
  const { head, card, page, finish, PER_PAGE } = R.ui;

  R.navItem("Customers", "/wallets", "Wallets", "wallets");
  R.navItem("Catalog", "/coupons", "Coupons", "coupons");
  R.navItem("Catalog", "/add_ons", "Add-ons", "addons");
  R.navItem("Catalog", "/taxes", "Taxes", "taxes");
  R.navItem("Operations", "/dunning", "Dunning", "dunning");
  R.navItem("Operations", "/webhooks", "Webhooks", "webhooks");
  R.navItem("Operations", "/events", "Events", "events");
  R.navItem("Operations", "/exports", "Data exports", "exports");
  R.navItem("Operations", "/settings", "Settings", "settings");

  // ---- wallets -----------------------------------------------------------------------
  R.walletForm = function (preset) {
    R.openForm({
      title: "New prepaid wallet",
      fields: [
        { name: "external_customer_id", label: "Customer external ID", required: true, value: preset.external_customer_id },
        { name: "name", label: "Name", placeholder: "Prepaid credits" },
        { name: "currency", label: "Currency", required: true, value: preset.currency || "USD" },
        { name: "rate_amount", label: "Price of one credit", required: true, value: "1" },
        { name: "paid_credits", label: "Paid credits to purchase now", type: "number" },
        { name: "granted_credits", label: "Free credits to grant now", type: "number" },
      ],
      submitLabel: "Create wallet",
      onSubmit: async (v) => {
        const w = Object.assign({}, v, {
          rate_amount: String(v.rate_amount),
          paid_credits: v.paid_credits !== undefined ? String(v.paid_credits) : undefined,
          granted_credits: v.granted_credits !== undefined ? String(v.granted_credits) : undefined,
        });
        const r = await api.post("/api/v1/wallets", { wallet: w });
        location.hash = "#/wallets/" + r.wallet.lago_id;
        return "Wallet created";
      },
    });
  };
  R.route("/wallets", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/wallets?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Wallets", "Prepaid credit balances drawn down by invoices.", btn("new", "New wallet", "plus", "primary")) +
      card("All wallets", table([
        { h: "Customer", f: (r) => esc(r.external_customer_id), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name || "Wallet") },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Credits", f: (r) => esc(r.credits_balance), cls: "num" },
        { h: "Balance", f: (r) => money(r.balance_cents, r.currency), cls: "num" },
        { h: "Consumed", f: (r) => esc(r.consumed_credits), cls: "num" },
        { h: "Expires", f: (r) => date(r.expiration_at) },
      ], data.wallets, { href: (r) => "/wallets/" + r.lago_id, empty: "No wallets yet." }) + R.pager(data.meta, pg));
    finish(view, { new: () => R.walletForm({}) });
  });
  R.route("/wallets/:id", async (p, q) => {
    const base = "/api/v1/wallets/" + enc(p.id);
    const pg = page(q);
    const [w, tx, rules] = await Promise.all([
      api.get(base),
      api.get(base + "/transactions?per_page=" + PER_PAGE + "&page=" + pg),
      api.get(base + "/recurring_rules"),
    ]);
    const wa = w.wallet;
    const active = wa.status === "active";
    const view = $("#view");
    view.innerHTML = head(wa.name || "Wallet", "Customer " + '<a href="#/customers/' + enc(wa.external_customer_id) + '">' + esc(wa.external_customer_id) + "</a>",
      '<a class="btn" href="#/wallets/' + enc(wa.lago_id) + '/mint">Real-time (Mint)</a>' +
      (active ? btn("topup", "Top up", "plus", "primary") + btn("rule", "Recurring top-up", "retry") + btn("edit", "Edit", "edit") + btn("terminate", "Terminate", "trash", "danger") : ""), '<a href="#/wallets">Wallets</a>') +
      '<div class="grid2">' +
      card("Balance", '<div class="body">' + kv([
        ["Status", badge(wa.status)], ["Credits", esc(wa.credits_balance)], ["Balance", money(wa.balance_cents, wa.currency)],
        ["Ongoing balance", money(wa.ongoing_balance_cents, wa.currency)], ["Credit price", esc(wa.rate_amount) + " " + esc(wa.currency)],
        ["Consumed credits", esc(wa.consumed_credits)], ["Expires", datetime(wa.expiration_at)], ["Priority", esc(wa.priority)],
      ]) + "</div>") +
      card("Recurring top-ups", table([
        { h: "Trigger", f: (r) => badge(r.trigger) },
        { h: "Interval", f: (r) => esc(r.interval || "-") },
        { h: "Threshold", f: (r) => esc(r.threshold_credits || "-"), cls: "num" },
        { h: "Paid", f: (r) => esc(r.paid_credits), cls: "num" },
        { h: "Granted", f: (r) => esc(r.granted_credits), cls: "num" },
      ], rules.recurring_transaction_rules, { empty: "No recurring top-up." })) + "</div>" +
      card("Transactions", table([
        { h: "Type", f: (r) => badge(r.transaction_type) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Credits", f: (r) => esc(r.credit_amount), cls: "num" },
        { h: "Amount", f: (r) => esc(r.amount), cls: "num" },
        { h: "Invoice", f: (r) => (r.lago_invoice_id ? '<a href="#/invoices/' + esc(r.lago_invoice_id) + '">view</a>' : "-") },
        { h: "Settled", f: (r) => datetime(r.settled_at) },
        { h: "Created", f: (r) => datetime(r.created_at) },
      ], tx.wallet_transactions, { empty: "No transactions yet." }) + R.pager(tx.meta, pg));
    finish(view, {
      topup: () => R.openForm({
        title: "Top up wallet",
        intro: "Paid credits are invoiced to the customer; granted credits are free.",
        fields: [{ name: "paid_credits", label: "Paid credits", type: "number" }, { name: "granted_credits", label: "Granted credits", type: "number" }],
        submitLabel: "Top up",
        onSubmit: async (v) => {
          if (!v.paid_credits && !v.granted_credits) throw new Error("Enter paid or granted credits.");
          await api.post("/api/v1/wallet_transactions", { wallet_transaction: { wallet_id: wa.lago_id, paid_credits: v.paid_credits !== undefined ? String(v.paid_credits) : undefined, granted_credits: v.granted_credits !== undefined ? String(v.granted_credits) : undefined } });
          return v.paid_credits ? "Invoice issued for the paid credits; they are added once it is paid" : "Credits granted";
        },
        after: R.render,
      }),
      rule: () => R.openForm({
        title: "Recurring top-up",
        fields: [
          { name: "rule_type", label: "Trigger", type: "select", options: [["interval", "On an interval"], ["threshold", "When credits fall below a threshold"]] },
          { name: "interval", label: "Interval", type: "select", options: [["monthly", "Monthly"], ["weekly", "Weekly"], ["quarterly", "Quarterly"], ["yearly", "Yearly"]] },
          { name: "threshold_credits", label: "Threshold credits", type: "number" },
          { name: "paid_credits", label: "Paid credits per top-up", type: "number" },
          { name: "granted_credits", label: "Granted credits per top-up", type: "number" },
        ],
        submitLabel: "Save rule",
        onSubmit: async (v) => {
          const rule = { rule_type: v.rule_type, interval: v.rule_type === "interval" ? v.interval : undefined,
            threshold_credits: v.threshold_credits !== undefined ? String(v.threshold_credits) : undefined,
            paid_credits: String(v.paid_credits || 0), granted_credits: String(v.granted_credits || 0) };
          await api.post(base + "/recurring_rules", { recurring_rule: rule });
          return "Recurring top-up saved";
        },
        after: R.render,
      }),
      edit: () => R.openForm({
        title: "Edit wallet",
        fields: [
          { name: "name", label: "Name", value: wa.name },
          { name: "expiration_at", label: "Expires at (ISO 8601)", value: wa.expiration_at || "", placeholder: "2027-01-01T00:00:00Z" },
          { name: "priority", label: "Priority (lower draws first)", type: "number", value: wa.priority },
        ],
        onSubmit: async (v) => { await api.put(base, { wallet: v }); return "Wallet updated"; },
        after: R.render,
      }),
      terminate: () => R.confirmAction("Terminate wallet", "Remaining credits are forfeited.", "Terminate", async () => { await api.del(base); return "Wallet terminated"; }),
    });
  });

  // ---- real-time (Mint) wallet console -------------------------------------------------
  // Live Mint balance, the wallet's holds, top-ups and the Mint ledger. Amounts
  // at Mint are microdollars (1 cent = 10,000 micros).
  const microsToCents = (m) => (m === null || m === undefined ? null : Math.round(Number(m) / 10000));
  const usd = (micros) => money(microsToCents(micros), "USD");
  R.route("/wallets/:id/mint", async (p, q) => {
    const base = "/api/v1/wallets/" + enc(p.id);
    const pg = page(q);
    const status = q.status || "";
    const [ov, hl] = await Promise.all([
      api.get(base + "/realtime/overview?days=30"),
      api.get(base + "/realtime/holds?per_page=" + PER_PAGE + "&page=" + pg + (status ? "&status=" + enc(status) : "")),
    ]);
    const o = ov.wallet_realtime_overview;
    const m = o.mint || {};
    const h = o.holds || {};
    const statement = m.statement || {};
    const entries = (statement.entries || []).slice().reverse();
    const providers = ((m.providers && (m.providers.providers || m.providers)) || []).filter((x) => x && x.checkout);
    const view = $("#view");
    let lastLink = null;
    const actions = (o.realtime_enabled && m.configured && o.mint_account_id ? btn("topup", "Top-up link", "plus", "primary") : "") +
      (h.failed_reversals > 0 ? btn("reverse", "Retry reversals", "retry") : "") + btn("settings", "Real-time settings", "settings");
    const filters = ["", "open", "captured", "reversed", "released", "flagged"].map((st) =>
      '<a class="btn' + (st === status ? " primary" : "") + '" href="#/wallets/' + enc(p.id) + "/mint" + (st ? "?status=" + st : "") + '">' + esc(st || "all") + "</a>").join(" ");
    view.innerHTML = head((o.wallet_name || "Wallet") + " real-time", "Usage is reserved at Mint as it happens and captured when the invoice is finalized.", actions,
      '<a href="#/wallets/' + enc(p.id) + '">Wallet</a>') +
      (m.error ? '<div class="card"><div class="body">' + esc(m.error) + "</div></div>" : "") +
      '<div class="grid2">' +
      card("Mint balance", '<div class="body">' + kv([
        ["Real-time debit", badge(o.realtime_enabled ? "active" : "disabled")], ["On shortfall", esc(o.realtime_policy)],
        ["Mint account", '<span class="mono">' + esc(o.mint_account_id || "-") + "</span>"],
        ["Available", m.available_micros === undefined ? "-" : usd(m.available_micros)],
        ["Held at Mint", m.held_micros === undefined ? "-" : usd(m.held_micros)],
      ]) + "</div>") +
      card("Holds", '<div class="body">' + kv([
        ["Open (reserved)", money(h.open_cents, "USD") + ' <span class="muted">(' + esc(h.open_count) + ")</span>"],
        ["Captured to seller", usd(h.captured_micros)],
        ["Reversed on void", usd(h.reversed_micros)],
        ["Flagged shortfall", money(h.flagged_cents, "USD") + ' <span class="muted">(' + esc(h.flagged_count) + ")</span>"],
        ["Failed reversals", h.failed_reversals > 0 ? badge("failed") + " " + esc(h.failed_reversals) : "0"],
      ]) + "</div>") + "</div>" +
      card("Holds by event", '<div class="body">' + filters + "</div>" + table([
        { h: "Event", f: (r) => esc(r.event_transaction_id), cls: "mono" },
        { h: "At", f: (r) => datetime(r.event_timestamp) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Amount", f: (r) => money(r.amount_cents, "USD"), cls: "num" },
        { h: "Captured", f: (r) => usd(r.captured_micros), cls: "num" },
        { h: "Reversed", f: (r) => usd(r.reversed_micros), cls: "num" },
        { h: "Invoice", f: (r) => (r.lago_invoice_id ? '<a href="#/invoices/' + esc(r.lago_invoice_id) + '">' + esc(r.invoice_number || "view") + "</a>" : "-") },
        { h: "Note", f: (r) => esc(r.last_error || "") },
      ], hl.holds, { empty: "No holds yet." }) + R.pager(hl.meta, pg)) +
      card("Top-ups (last " + esc(o.ledger_days) + " days)", table([
        { h: "When", f: (r) => datetime(new Date(Number(r.ts)).toISOString()) },
        { h: "Type", f: (r) => badge(r.op === "Mint" ? "deposit" : "transfer in") },
        { h: "Amount", f: (r) => usd(r.delta_micros), cls: "num" },
        { h: "Balance after", f: (r) => usd(r.available_after_micros), cls: "num" },
      ], (m.top_ups || []).slice().reverse(), { empty: "No top-ups in this period." })) +
      card("Mint ledger (last " + esc(o.ledger_days) + " days)",
        (statement.totals ? '<div class="body">' + kv([
          ["Opening", usd((statement.opening || {}).available_micros)], ["Money in", usd(statement.totals.credits_micros)],
          ["Money out", usd(statement.totals.debits_micros)], ["Closing", usd((statement.closing || {}).available_micros)],
        ]) + "</div>" : "") + table([
          { h: "When", f: (r) => datetime(new Date(Number(r.ts)).toISOString()) },
          { h: "Operation", f: (r) => esc(r.op) },
          { h: "Change", f: (r) => usd(r.delta_micros), cls: "num" },
          { h: "Available after", f: (r) => usd(r.available_after_micros), cls: "num" },
          { h: "Held after", f: (r) => usd(r.held_after_micros), cls: "num" },
          { h: "Receipt", f: (r) => esc(String(r.receipt_hash || "").slice(0, 12)), cls: "mono" },
        ], entries, { empty: m.error ? "Ledger unavailable." : "No ledger entries in this period." }));
    finish(view, {
      topup: () => R.openForm({
        title: "Top-up link",
        intro: "Creates a hosted checkout at Mint. Send the link to the customer; their Mint balance rises once they pay.",
        fields: [
          { name: "amount", label: "Amount (USD)", type: "money", required: true },
          providers.length
            ? { name: "provider", label: "Payment provider", type: "select", options: providers.map((x) => [x.provider, x.provider]) }
            : { name: "provider", label: "Payment provider", required: true, placeholder: "stripe", help: "Mint did not report an active checkout provider." },
        ],
        submitLabel: "Create link",
        onSubmit: async (v) => {
          const r = await api.post(base + "/realtime/top_ups", { top_up: { amount_cents: v.amount, provider: v.provider } });
          const url = (r.top_up || {}).checkout_url;
          if (!url) throw new Error("Mint returned no checkout link.");
          lastLink = { checkout_url: url, amount: (v.amount / 100).toFixed(2) + " USD", provider: v.provider };
          return "Top-up link created";
        },
        after: () => { if (lastLink) R.showJson("Top-up link", lastLink); },
      }),
      reverse: () => R.confirmAction("Retry reversals", "Move money captured for voided invoices back from the seller's Mint account to the customer's.", "Retry", async () => {
        const r = (await api.post(base + "/realtime/reversals", {})).reversals;
        if (r.failed > 0) throw new Error(r.failed + " reversal(s) still failing. Check the notes on the holds.");
        return r.reversed + " hold(s) reversed";
      }),
      settings: () => R.openForm({
        title: "Real-time settings",
        intro: "Real-time wallets reserve each event's price at Mint. Wallet currency must be USD.",
        fields: [
          { name: "realtime_enabled", label: "Reserve usage in real time", type: "checkbox", value: o.realtime_enabled },
          { name: "realtime_policy", label: "When the customer cannot cover an event", type: "select", value: o.realtime_policy,
            options: [["reject", "Reject the event"], ["flag", "Accept it and flag the shortfall"]] },
          { name: "mint_account_id", label: "Customer Mint account", value: o.mint_account_id || "" },
        ],
        onSubmit: async (v) => { await api.put(base + "/realtime", { wallet: v }); return "Real-time settings saved"; },
        after: R.render,
      }),
    });
  });

  // ---- coupons --------------------------------------------------------------------------
  R.applyCouponForm = function (ext) {
    R.openForm({
      title: "Apply coupon",
      fields: [
        { name: "external_customer_id", label: "Customer external ID", required: true, value: ext },
        { name: "coupon_code", label: "Coupon code", required: true },
      ],
      submitLabel: "Apply",
      onSubmit: async (v) => { await api.post("/api/v1/applied_coupons", { applied_coupon: v }); return "Coupon applied"; },
      after: R.render,
    });
  };
  R.route("/coupons", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/coupons?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Coupons", "Discounts applied to customers' invoices.", btn("apply", "Apply to customer", "plus") + btn("new", "New coupon", "plus", "primary")) +
      card("All coupons", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Discount", f: (r) => (r.coupon_type === "percentage" ? esc(r.percentage_rate) + "%" : money(r.amount_cents, r.amount_currency)), cls: "num" },
        { h: "Frequency", f: (r) => esc(r.frequency + (r.frequency_duration ? " x" + r.frequency_duration : "")) },
        { h: "Reusable", f: (r) => (r.reusable ? "yes" : "no") },
        { h: "", f: (r) => '<button type="button" data-del="' + esc(r.code) + '">' + icon("trash") + "Delete</button>" },
      ], data.coupons, { empty: "No coupons yet." }) + R.pager(data.meta, pg));
    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete coupon", "Delete coupon " + b.dataset.del + "? Already-applied coupons keep applying.", "Delete", async () => {
      await api.del("/api/v1/coupons/" + enc(b.dataset.del));
      return "Coupon deleted";
    })));
    finish(view, {
      apply: () => R.applyCouponForm(""),
      new: () => R.openForm({
        title: "New coupon",
        fields: [
          { name: "name", label: "Name", required: true },
          { name: "code", label: "Code", required: true },
          { name: "coupon_type", label: "Type", type: "select", options: [["fixed_amount", "Fixed amount"], ["percentage", "Percentage"]] },
          { name: "amount", label: "Amount (fixed)", type: "money" },
          { name: "amount_currency", label: "Currency (fixed)", value: "USD" },
          { name: "percentage_rate", label: "Percentage (percentage type)", placeholder: "10" },
          { name: "frequency", label: "Applies", type: "select", options: [["once", "Once"], ["recurring", "For N invoices"], ["forever", "Forever"]] },
          { name: "frequency_duration", label: "N invoices (recurring)", type: "number" },
          { name: "reusable", label: "Can be applied to a customer more than once", type: "checkbox" },
        ],
        submitLabel: "Create coupon",
        onSubmit: async (v) => {
          const c = { name: v.name, code: v.code, coupon_type: v.coupon_type, frequency: v.frequency, frequency_duration: v.frequency_duration, reusable: v.reusable };
          if (v.coupon_type === "percentage") {
            if (!v.percentage_rate) throw new Error("Enter a percentage.");
            c.percentage_rate = String(v.percentage_rate);
          } else {
            if (v.amount === undefined) throw new Error("Enter an amount.");
            c.amount_cents = v.amount; c.amount_currency = v.amount_currency;
          }
          await api.post("/api/v1/coupons", { coupon: c });
          return "Coupon created";
        },
        after: R.render,
      }),
    });
  });

  // ---- add-ons ----------------------------------------------------------------------------
  R.route("/add_ons", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/add_ons?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Add-ons", "One-time charges you can invoice to any customer.", btn("invoice", "Invoice an add-on", "invoices") + btn("new", "New add-on", "plus", "primary")) +
      card("All add-ons", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        { h: "Description", f: (r) => esc(r.description) },
        { h: "", f: (r) => '<button type="button" data-del="' + esc(r.code) + '">' + icon("trash") + "Delete</button>" },
      ], data.add_ons, { empty: "No add-ons yet." }) + R.pager(data.meta, pg));
    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete add-on", "Delete add-on " + b.dataset.del + "?", "Delete", async () => {
      await api.del("/api/v1/add_ons/" + enc(b.dataset.del));
      return "Add-on deleted";
    })));
    finish(view, {
      invoice: () => R.oneOffInvoiceForm(""),
      new: () => R.openForm({
        title: "New add-on",
        fields: [
          { name: "name", label: "Name", required: true },
          { name: "code", label: "Code", required: true },
          { name: "amount", label: "Amount", type: "money", required: true },
          { name: "amount_currency", label: "Currency", value: "USD", required: true },
          { name: "description", label: "Description" },
        ],
        submitLabel: "Create add-on",
        onSubmit: async (v) => {
          await api.post("/api/v1/add_ons", { add_on: { name: v.name, code: v.code, amount_cents: v.amount, amount_currency: v.amount_currency, description: v.description } });
          return "Add-on created";
        },
        after: R.render,
      }),
    });
  });

  // ---- taxes -----------------------------------------------------------------------------------
  R.route("/taxes", async () => {
    const data = await api.get("/api/v1/taxes");
    const view = $("#view");
    view.innerHTML = head("Taxes", "Tax rates applied to invoices.", btn("new", "New tax rate", "plus", "primary")) +
      card("Tax rates", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Rate", f: (r) => esc(r.rate) + "%", cls: "num" },
        { h: "Organization default", f: (r) => (r.applied_to_organization ? "yes" : "no") },
        { h: "", f: (r) => '<button type="button" data-del="' + esc(r.code) + '">' + icon("trash") + "Delete</button>" },
      ], data.taxes, { empty: "No tax rates configured." }));
    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete tax rate", "Delete tax " + b.dataset.del + "?", "Delete", async () => {
      await api.del("/api/v1/taxes/" + enc(b.dataset.del));
      return "Tax deleted";
    })));
    finish(view, {
      new: () => R.openForm({
        title: "New tax rate",
        fields: [
          { name: "name", label: "Name", required: true, placeholder: "VAT" },
          { name: "code", label: "Code", required: true, placeholder: "vat_20" },
          { name: "rate", label: "Rate (%)", type: "number", required: true, placeholder: "20" },
          { name: "description", label: "Description" },
          { name: "applied_to_organization", label: "Apply to every customer by default", type: "checkbox" },
        ],
        submitLabel: "Create tax rate",
        onSubmit: async (v) => { await api.post("/api/v1/taxes", { tax: v }); return "Tax rate created"; },
        after: R.render,
      }),
    });
  });

  // ---- webhooks ------------------------------------------------------------------------------------
  R.route("/webhooks", async (_p, q) => {
    const pg = page(q);
    const status = q.status || "";
    const [eps, del] = await Promise.all([
      api.get("/api/v1/webhook_endpoints"),
      api.get("/api/v1/webhook_deliveries?per_page=" + PER_PAGE + "&page=" + pg + (status ? "&status=" + enc(status) : "")),
    ]);
    const rows = del.webhook_deliveries || [];
    const view = $("#view");
    view.innerHTML = head("Webhooks", "Signed event notifications sent to your endpoints (X-Lago-Signature, HMAC-SHA256).", btn("new", "Add endpoint", "plus", "primary")) +
      card("Endpoints", table([
        { h: "URL", f: (r) => esc(r.webhook_url), cls: "mono" },
        { h: "Signature", f: (r) => badge(r.signature_algo) },
        { h: "", f: (r) => '<div class="actions"><button type="button" data-edit="' + esc(r.lago_id) + '" data-url="' + esc(r.webhook_url) + '">' + icon("edit") + 'Edit</button><button type="button" data-del="' + esc(r.lago_id) + '">' + icon("trash") + "Delete</button></div>" },
      ], eps.webhook_endpoints, { empty: "No endpoints. Add one to receive billing events." })) +
      '<div class="card"><h3>Deliveries<span class="spacer"></span><label for="dst" class="sub">Status</label> <select id="dst">' +
      [["", "All"], ["pending", "Pending"], ["retrying", "Retrying"], ["succeeded", "Succeeded"], ["failed", "Failed"]].map(([v, l]) => '<option value="' + v + '"' + (v === status ? " selected" : "") + ">" + l + "</option>").join("") +
      "</select></h3>" + table([
        { h: "Event", f: (r) => esc(r.event_type), cls: "mono" },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "HTTP", f: (r) => esc(r.http_status || "-") },
        { h: "Attempts", f: (r) => esc(r.attempts), cls: "num" },
        { h: "Endpoint", f: (r) => esc(r.endpoint), cls: "mono" },
        { h: "Last error", f: (r) => esc((r.last_error || "").slice(0, 80)) },
        { h: "Created", f: (r) => datetime(r.created_at) },
        { h: "", f: (r, i) => '<div class="actions"><button type="button" data-payload="' + esc(r.lago_id) + '">Payload</button>' + (r.status === "failed" ? '<button type="button" data-retry="' + esc(r.lago_id) + '">' + icon("retry") + "Retry</button>" : "") + "</div>" },
      ], rows, { empty: "No deliveries yet." }) + R.pager(del.meta, pg) + "</div>";
    $("#dst", view).onchange = (e) => R.setQuery({ status: e.target.value });
    const byId = Object.fromEntries(rows.map((r) => [r.lago_id, r]));
    R.$$("[data-payload]", view).forEach((b) => (b.onclick = () => R.showJson("Webhook payload", byId[b.dataset.payload].payload)));
    R.$$("[data-retry]", view).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api.post("/api/v1/webhook_deliveries/" + enc(b.dataset.retry) + "/retry"); R.toast("Delivery re-queued", "ok"); R.render(); }
      catch (e) { R.toast(e.message, "err"); b.disabled = false; }
    }));
    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete endpoint", "Stop sending events to this endpoint?", "Delete", async () => {
      await api.del("/api/v1/webhook_endpoints/" + enc(b.dataset.del));
      return "Endpoint deleted";
    })));
    R.$$("[data-edit]", view).forEach((b) => (b.onclick = () => R.openForm({
      title: "Edit endpoint",
      fields: [{ name: "webhook_url", label: "URL", required: true, value: b.dataset.url }],
      onSubmit: async (v) => { await api.put("/api/v1/webhook_endpoints/" + enc(b.dataset.edit), { webhook_endpoint: v }); return "Endpoint updated"; },
      after: R.render,
    })));
    finish(view, {
      new: () => R.openForm({
        title: "Add webhook endpoint",
        fields: [{ name: "webhook_url", label: "URL", required: true, type: "url", placeholder: "https://example.com/webhooks/rago" }],
        submitLabel: "Add endpoint",
        onSubmit: async (v) => {
          if (!/^https?:\/\//.test(v.webhook_url)) throw new Error("URL must start with https://");
          await api.post("/api/v1/webhook_endpoints", { webhook_endpoint: Object.assign(v, { signature_algo: "hmac" }) });
          return "Endpoint added";
        },
        after: R.render,
      }),
    });
  });

  // ---- dunning ----------------------------------------------------------------------------------------
  R.route("/dunning", async (_p, q) => {
    const pg = page(q);
    const [over, camps, reqs] = await Promise.all([
      api.get("/api/v1/dunning?per_page=" + PER_PAGE + "&page=" + pg),
      api.get("/api/v1/dunning_campaigns"),
      api.get("/api/v1/payment_requests"),
    ]);
    const view = $("#view");
    view.innerHTML = head("Dunning", "Overdue invoices and the campaigns that retry their collection.", btn("new", "New campaign", "plus", "primary")) +
      card("Overdue invoices", table([
        { h: "Invoice", f: (r) => '<a href="#/invoices/' + esc(r.lago_invoice_id) + '">' + esc(r.invoice_number) + "</a>" },
        { h: "Customer", f: (r) => '<a href="#/customers/' + enc(r.external_customer_id) + '">' + esc(r.external_customer_id) + "</a>" },
        { h: "Overdue", f: (r) => money(r.total_amount_cents, r.currency), cls: "num" },
        { h: "Due", f: (r) => esc(r.payment_due_date || "-") },
        { h: "Last attempt", f: (r) => datetime(r.last_dunning_campaign_attempt_at) },
        { h: "Excluded", f: (r) => (r.exclude_from_dunning_campaign ? "yes" : "no") },
      ], over.dunning, { empty: "Nothing overdue." }) + R.pager(over.meta, pg)) +
      card("Campaigns", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Attempts", f: (r) => esc(r.max_attempts), cls: "num" },
        { h: "Days between", f: (r) => esc(r.days_between_attempts), cls: "num" },
        { h: "Thresholds", f: (r) => (r.thresholds || []).map((t) => money(t.amount_cents, t.currency)).join(", ") || "-" },
        { h: "Default", f: (r) => (r.applied_to_organization ? "yes" : "no") },
        { h: "", f: (r) => '<button type="button" data-del="' + esc(r.code) + '">' + icon("trash") + "Delete</button>" },
      ], camps.dunning_campaigns, { empty: "No campaigns. Without one, overdue invoices are not retried automatically." })) +
      card("Payment requests", table([
        { h: "Customer", f: (r) => esc(r.external_customer_id), cls: "mono" },
        { h: "Email", f: (r) => esc(r.email) },
        { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        { h: "Status", f: (r) => badge(r.payment_status) },
      ], reqs.payment_requests, { empty: "No payment requests yet." }));
    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete campaign", "Delete campaign " + b.dataset.del + "?", "Delete", async () => {
      await api.del("/api/v1/dunning_campaigns/" + enc(b.dataset.del));
      return "Campaign deleted";
    })));
    finish(view, {
      new: () => R.openForm({
        title: "New dunning campaign",
        fields: [
          { name: "name", label: "Name", required: true },
          { name: "code", label: "Code", required: true },
          { name: "max_attempts", label: "Max attempts", type: "number", value: "3" },
          { name: "days_between_attempts", label: "Days between attempts", type: "number", value: "3" },
          { name: "threshold", label: "Minimum overdue amount", type: "money", required: true, value: "1.00" },
          { name: "currency", label: "Currency", required: true, value: "USD" },
          { name: "applied_to_organization", label: "Default campaign for all customers", type: "checkbox", value: true },
        ],
        submitLabel: "Create campaign",
        onSubmit: async (v) => {
          await api.post("/api/v1/dunning_campaigns", { dunning_campaign: {
            name: v.name, code: v.code, max_attempts: v.max_attempts, days_between_attempts: v.days_between_attempts,
            applied_to_organization: v.applied_to_organization, thresholds: [{ currency: v.currency, amount_cents: v.threshold }] } });
          return "Campaign created";
        },
        after: R.render,
      }),
    });
  });

  // ---- events ---------------------------------------------------------------------------------------
  R.route("/events", async (_p, q) => {
    const pg = page(q);
    const qs = "per_page=" + PER_PAGE + "&page=" + pg + (q.code ? "&code=" + enc(q.code) : "") + (q.sub ? "&external_subscription_id=" + enc(q.sub) : "");
    const data = await api.get("/api/v1/events?" + qs);
    const rows = data.events || [];
    const view = $("#view");
    view.innerHTML = head("Events", "Usage events ingested for billing.", btn("new", "Send test event", "events", "primary")) +
      '<div class="card"><form class="toolbar" id="ev-filter"><label for="ev-code">Code</label><input id="ev-code" value="' + esc(q.code || "") + '" />' +
      '<label for="ev-sub">Subscription</label><input id="ev-sub" value="' + esc(q.sub || "") + '" /><button type="submit">Filter</button></form>' +
      table([
        { h: "Transaction", f: (r) => esc(r.transaction_id), cls: "mono" },
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Subscription", f: (r) => esc(r.external_subscription_id), cls: "mono" },
        { h: "Properties", f: (r) => '<span class="mono">' + esc(JSON.stringify(r.properties).slice(0, 80)) + "</span>" },
        { h: "Timestamp", f: (r) => datetime(r.timestamp) },
      ], rows, { empty: "No events match." }) + R.pager(data.meta, pg) + "</div>";
    $("#ev-filter", view).onsubmit = (e) => { e.preventDefault(); R.setQuery({ code: $("#ev-code").value.trim(), sub: $("#ev-sub").value.trim() }); };
    finish(view, {
      new: () => R.openForm({
        title: "Send event",
        fields: [
          { name: "code", label: "Metric code", required: true },
          { name: "external_subscription_id", label: "Subscription external ID", required: true },
          { name: "transaction_id", label: "Transaction ID (idempotency key)", value: "evt_" + Date.now().toString(36) },
          { name: "properties", label: "Properties JSON", type: "json", value: "{}" },
        ],
        submitLabel: "Send",
        onSubmit: async (v) => {
          v.properties = v.properties || {};
          if (!v.transaction_id) v.transaction_id = "evt_" + Date.now().toString(36);
          await api.post("/api/v1/events", { event: v });
          return "Event accepted";
        },
        after: R.render,
      }),
    });
  });

  // ---- data exports ------------------------------------------------------------------------------------
  const EXPORTS_STORE = "rago_admin_exports";
  function recentExports() { try { return JSON.parse(sessionStorage.getItem(EXPORTS_STORE) || "[]"); } catch (_) { return []; } }
  function saveExports(list) { sessionStorage.setItem(EXPORTS_STORE, JSON.stringify(list.slice(0, 20))); }
  R.startExport = async function (resource) {
    const r = await api.post("/api/v1/data_exports", { data_export: { resource_type: resource } });
    saveExports([{ id: r.data_export.lago_id, resource, at: new Date().toISOString() }].concat(recentExports()));
    R.toast("Export started. It is generated in the background.", "ok");
    location.hash = "#/exports";
  };
  R.route("/exports", async () => {
    const list = recentExports();
    const statuses = await Promise.all(list.map((e) => api.get("/api/v1/data_exports/" + enc(e.id)).then((d) => d.data_export).catch((err) => ({ status: "error: " + err.message }))));
    const rows = list.map((e, i) => Object.assign({}, e, statuses[i]));
    const view = $("#view");
    view.innerHTML = head("Data exports", "CSV exports generated by the background worker.",
      ["invoices", "credit_notes", "customers", "events"].map((r) => btn("exp_" + r, "Export " + r.replace("_", " "), "download")).join("")) +
      card("Recent exports (this session)", table([
        { h: "Resource", f: (r) => esc(r.resource) },
        { h: "Requested", f: (r) => datetime(r.at) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Rows", f: (r) => esc(r.row_count === undefined ? "-" : r.row_count), cls: "num" },
        { h: "", f: (r) => (r.status === "completed" ? '<button type="button" data-dl="' + esc(r.id) + '" data-name="' + esc(r.resource) + '">' + icon("download") + "Download</button>" : "") },
      ], rows, { empty: "No exports requested in this session." }), btn("reload", "Refresh", "refresh"));
    R.$$("[data-dl]", view).forEach((b) => (b.onclick = async () => {
      try { await api.download("/api/v1/data_exports/" + enc(b.dataset.dl) + "/download", b.dataset.name + ".csv"); } catch (e) { R.toast(e.message, "err"); }
    }));
    const handlers = { reload: () => R.render() };
    ["invoices", "credit_notes", "customers", "events"].forEach((r) => (handlers["exp_" + r] = () => R.startExport(r)));
    finish(view, handlers);
  });

  // ---- settings -------------------------------------------------------------------------------------------
  const PROVIDER_LABEL = { stripe: "Stripe", gocardless: "GoCardless", adyen: "Adyen", creem: "Creem", dodo: "Dodo Payments", iyzico: "Iyzico" };
  R.route("/settings", async () => {
    const [org, gql, integ] = await Promise.all([
      api.get("/api/v1/organizations"),
      api.graphql("query { paymentProviders { id providerType code name secretRef webhookSecretRef successRedirectUrl settings } }").catch((e) => ({ error: e.message })),
      api.get("/api/v1/integrations").catch(() => ({ integrations: [] })),
    ]);
    const o = org.organization;
    const providers = gql.paymentProviders || [];
    function webhookUrl(kind) { return location.origin + "/webhooks/" + kind + "/" + o.lago_id; }
    const view = $("#view");
    view.innerHTML = head("Settings", "Organization profile, payment collection and integrations.") +
      card("Organization", '<div class="body">' + kv([
        ["Name", esc(o.name)], ["Default currency", esc(o.default_currency)], ["Billing email", esc(o.email)], ["Country", esc(o.country)],
        ["Timezone", esc(o.timezone)], ["Tax ID", esc(o.tax_identification_number)], ["Document locale", esc(o.document_locale)],
        ["Invoice numbers", '<span class="mono">' + esc((o.effective_document_number_prefix || "") + "-YYYYMM-001") + "</span>" + (o.document_number_prefix ? "" : " (default prefix)")],
        ["Organization ID", '<span class="mono">' + esc(o.lago_id) + "</span>"],
        ["Stripe webhook URL", '<span class="mono">' + esc(webhookUrl("stripe")) + "</span>"],
        ["GoCardless webhook URL", '<span class="mono">' + esc(webhookUrl("gocardless")) + "</span>"],
        ["Adyen webhook URL", '<span class="mono">' + esc(webhookUrl("adyen")) + "</span>"],
        ["Creem webhook URL", '<span class="mono">' + esc(webhookUrl("creem")) + "</span>"],
        ["Dodo Payments webhook URL", '<span class="mono">' + esc(webhookUrl("dodo")) + "</span>"],
        ["Iyzico callback and notification URL", '<span class="mono">' + esc(webhookUrl("iyzico")) + "</span>"],
      ]) + "</div>", btn("edit_org", "Edit", "edit")) +
      card("Payment providers", (gql.error ? '<div class="error-box">' + esc(gql.error) + "</div>" : "") + table([
        { h: "Provider", f: (r) => badge(r.providerType) },
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "API secret", f: (r) => esc(r.secretRef || "not set"), cls: "mono" },
        { h: "Webhook secret", f: (r) => esc(r.webhookSecretRef || "not set"), cls: "mono" },
        { h: "Return URL", f: (r) => esc(r.successRedirectUrl), cls: "mono" },
        { h: "", f: (r) => '<button type="button" data-edit-provider="' + esc(r.id) + '">' + icon("edit") + "Edit</button> " +
          '<button type="button" data-del-provider="' + esc(r.id) + '">' + icon("trash") + "Remove</button>" },
      ], providers, { empty: "No provider configured. Invoices are not collected automatically until you connect Stripe, GoCardless, Adyen, Creem, Dodo Payments or Iyzico." }),
      btn("stripe", "Connect Stripe", "pay", "primary") + btn("gocardless", "Connect GoCardless", "pay") + btn("adyen", "Connect Adyen", "pay") +
      btn("creem", "Connect Creem", "pay") + btn("dodo", "Connect Dodo Payments", "pay") + btn("iyzico", "Connect Iyzico", "pay")) +
      card("Integrations", table([
        { h: "Type", f: (r) => badge(r.integration_type) },
        { h: "Provider", f: (r) => esc(r.provider_name || r.provider_code) },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Status", f: (r) => badge(r.status === "connected" ? "active" : r.status === "needs_authorization" ? "pending" : "failed") + " " + esc((r.status || "").replace(/_/g, " ")) },
      ], integ.integrations, { href: (r) => "/integrations/" + r.lago_id, empty: "No accounting, CRM, tax or single sign-on integration connected." }),
      btn("manage_integrations", "Manage integrations", "link"));
    R.$$("[data-del-provider]", view).forEach((b) => (b.onclick = () => R.confirmAction("Remove provider", "Stop charging through this provider?", "Remove", async () => {
      await api.graphql("mutation($id: ID!) { destroyPaymentProvider(id: $id) }", { id: b.dataset.delProvider });
      return "Provider removed";
    })));
    R.$$("[data-edit-provider]", view).forEach((b) => (b.onclick = () => {
      const existing = providers.find((pr) => pr.id === b.dataset.editProvider);
      if (existing) providerForm(existing.providerType, existing);
    }));
    finish(view, {
      edit_org: () => R.openForm({
        title: "Edit organization",
        fields: [
          { name: "name", label: "Name", value: o.name, required: true },
          { name: "default_currency", label: "Default currency", value: o.default_currency },
          { name: "email", label: "Billing email", type: "email", value: o.email },
          { name: "country", label: "Country (ISO)", value: o.country },
          { name: "timezone", label: "Timezone", value: o.timezone },
          { name: "document_number_prefix", label: "Invoice number prefix (letters, digits, - or _; empty for default)", value: o.document_number_prefix || "", placeholder: o.effective_document_number_prefix || "" },
        ],
        onSubmit: async (v) => {
          const organization = Object.assign({}, v, { document_number_prefix: v.document_number_prefix === undefined ? null : v.document_number_prefix });
          await api.put("/api/v1/organizations", { organization });
          return "Organization updated";
        },
        after: R.render,
      }),
      manage_integrations: () => { location.hash = "#/integrations"; },
      stripe: () => providerForm("stripe"),
      gocardless: () => providerForm("gocardless"),
      adyen: () => providerForm("adyen"),
      creem: () => providerForm("creem"),
      dodo: () => providerForm("dodo"),
      iyzico: () => providerForm("iyzico"),
    });

    // Connect or edit a payment provider. Secrets are write-only: leaving a
    // secret empty on edit keeps the stored one.
    function providerForm(kind, existing) {
      const editing = !!existing;
      const settings = (existing && existing.settings) || {};
      const secretHint = editing ? "leave empty to keep the stored value" : "";
      const common = [
        { name: "code", label: "Code", required: true, value: existing ? existing.code : kind },
        { name: "name", label: "Name", required: true, value: existing ? existing.name : PROVIDER_LABEL[kind] },
      ];
      const spec = {
        stripe: {
          fields: [
            { name: "secretKey", label: "Secret key", type: "password", required: !editing, placeholder: editing ? secretHint : "sk_live_..." },
            { name: "webhookSecret", label: "Webhook signing secret", type: "password", placeholder: editing ? secretHint : "whsec_..." },
            { name: "successRedirectUrl", label: "Return URL after checkout", type: "url", value: existing ? existing.successRedirectUrl : "", placeholder: "https://your-app.example/billing" },
          ],
          mutation: "mutation($input: StripePaymentProviderInput!) { addStripePaymentProvider(input: $input) { id } }",
        },
        gocardless: {
          fields: [
            { name: "accessCode", label: "Access token", type: "password", required: !editing, placeholder: editing ? secretHint : "live_..." },
            { name: "webhookSecret", label: "Webhook endpoint secret", type: "password", placeholder: secretHint },
            { name: "successRedirectUrl", label: "Return URL after mandate setup", type: "url", value: existing ? existing.successRedirectUrl : "" },
          ],
          mutation: "mutation($input: GocardlessPaymentProviderInput!) { addGocardlessPaymentProvider(input: $input) { id } }",
        },
        adyen: {
          fields: [
            { name: "apiKey", label: "API key", type: "password", required: !editing, placeholder: secretHint },
            { name: "merchantAccount", label: "Merchant account", required: true, value: settings.merchant_account || "" },
            { name: "hmacKey", label: "Notification HMAC key (hex)", type: "password", placeholder: secretHint },
            { name: "livePrefix", label: "Live URL prefix", value: settings.live_prefix || "" },
            { name: "successRedirectUrl", label: "Return URL", type: "url", value: existing ? existing.successRedirectUrl : "" },
          ],
          mutation: "mutation($input: AdyenPaymentProviderInput!) { addAdyenPaymentProvider(input: $input) { id } }",
        },
        creem: {
          fields: [
            { name: "apiKey", label: "API key", type: "password", required: !editing, placeholder: editing ? secretHint : "creem_..." },
            { name: "productId", label: "One-time product ID (invoices are sold as this product at the invoice amount)", required: true, value: settings.product_id || "", placeholder: "prod_..." },
            { name: "currency", label: "Product currency", value: settings.currency || "USD" },
            { name: "mode", label: "Mode (live or test)", value: settings.mode || "live" },
            { name: "webhookSecret", label: "Webhook signing secret", type: "password", placeholder: secretHint },
            { name: "successRedirectUrl", label: "Return URL after payment", type: "url", value: existing ? existing.successRedirectUrl : "" },
          ],
          mutation: "mutation($input: CreemPaymentProviderInput!) { addCreemPaymentProvider(input: $input) { id } }",
        },
        dodo: {
          fields: [
            { name: "apiKey", label: "API key", type: "password", required: !editing, placeholder: secretHint },
            { name: "productId", label: "Pay-what-you-want product ID (invoices are sold as this product at the invoice amount)", required: true, value: settings.product_id || "", placeholder: "pdt_..." },
            { name: "mode", label: "Mode (live or test)", value: settings.mode || "live" },
            { name: "webhookSecret", label: "Webhook signing secret", type: "password", placeholder: editing ? secretHint : "whsec_..." },
            { name: "successRedirectUrl", label: "Return URL after payment", type: "url", value: existing ? existing.successRedirectUrl : "" },
          ],
          mutation: "mutation($input: DodoPaymentProviderInput!) { addDodoPaymentProvider(input: $input) { id } }",
        },
        iyzico: {
          fields: [
            { name: "apiKey", label: "API key", required: true, value: settings.api_key || "" },
            { name: "secretKey", label: "Secret key (also verifies notifications)", type: "password", required: !editing, placeholder: secretHint },
            { name: "mode", label: "Mode (live or sandbox)", value: settings.mode || "live" },
            { name: "successRedirectUrl", label: "Return URL after payment", type: "url", value: existing ? existing.successRedirectUrl : "" },
          ],
          mutation: "mutation($input: IyzicoPaymentProviderInput!) { addIyzicoPaymentProvider(input: $input) { id } }",
        },
      }[kind];
      R.openForm({
        title: (editing ? "Edit " : "Connect ") + PROVIDER_LABEL[kind],
        intro: "Credentials are encrypted at rest (requires RAGO_ENCRYPTION_KEY on the server); only their last 4 characters are shown afterwards. " +
          (kind === "iyzico"
            ? "Rago sends buyers to Iyzico's hosted form and receives them back at " + webhookUrl(kind) + " (set RAGO_PUBLIC_URL on the server). Add the same URL as the merchant notification URL in the Iyzico panel. Each customer needs a name, email, country, address, city and personal ID number on their record; invoices of incomplete customers are not sent to Iyzico."
            : "Create a webhook in " + PROVIDER_LABEL[kind] + " pointing at " + webhookUrl(kind) + " and paste its signing secret here so payment and refund events are verified for this organization.") +
          (kind === "creem" || kind === "dodo" ? " Invoices are collected through a hosted checkout link sent with the payment.requires_action webhook." : ""),
        fields: common.concat(spec.fields),
        submitLabel: editing ? "Save" : "Connect",
        onSubmit: async (v) => {
          const input = {};
          Object.keys(v).forEach((k) => { if (v[k] !== "" && v[k] !== null && v[k] !== undefined) input[k] = v[k]; });
          await api.graphql(spec.mutation, { input });
          return PROVIDER_LABEL[kind] + (editing ? " updated" : " connected");
        },
        after: R.render,
      });
    }
  });

  R.boot();
})();
