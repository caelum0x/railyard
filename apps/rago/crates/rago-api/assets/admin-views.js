"use strict";
// Rago Billing console — views: overview, customers, subscriptions, plans,
// billable metrics, invoices, credit notes.
(function () {
  const R = window.Rago;
  const { $, esc, money, date, datetime, badge, api, enc, table, kv, btn, icon } = R;
  const PER_PAGE = 25;

  R.navItem("Revenue", "/overview", "Overview", "overview");
  R.navItem("Revenue", "/invoices", "Invoices", "invoices");
  R.navItem("Revenue", "/credit_notes", "Credit notes", "credit_notes");
  R.navItem("Customers", "/customers", "Customers", "customers");
  R.navItem("Customers", "/subscriptions", "Subscriptions", "subscriptions");
  R.navItem("Catalog", "/plans", "Plans", "plans");
  R.navItem("Catalog", "/metrics", "Billable metrics", "metrics");

  function head(title, sub, actionsHtml, crumb) {
    return (crumb ? '<div class="crumb">' + crumb + "</div>" : "") +
      '<div class="page-head"><div><h2>' + esc(title) + "</h2>" + (sub ? '<div class="sub">' + sub + "</div>" : "") +
      '</div><div class="spacer"></div><div class="actions">' + (actionsHtml || "") + "</div></div>";
  }
  function card(title, inner, actionsHtml) {
    return '<div class="card"><h3>' + esc(title) + '<span class="spacer"></span>' + (actionsHtml || "") + "</h3>" + inner + "</div>";
  }
  function page(q) { return Math.max(1, Number(q.page || 1)); }
  function finish(root, handlers) {
    R.bindRowLinks(root);
    R.bindPager(root, (p) => R.setQuery(Object.assign({}, R.query(), { page: p })));
    R.bindActions(root, handlers || {});
  }
  R.ui = { head, card, page, finish, PER_PAGE };

  // ---- overview -------------------------------------------------------------------
  function lastMonths(points, n) {
    const byMonth = {};
    for (const p of points) {
      const k = p.month + "|" + p.currency;
      byMonth[k] = (byMonth[k] || 0) + Number(p.amount_cents || 0);
    }
    const currencies = [...new Set(points.map((p) => p.currency))];
    const cur = currencies[0] || "USD";
    const months = [...new Set(points.filter((p) => p.currency === cur).map((p) => p.month))].sort().slice(-n);
    return { cur, series: months.map((m) => ({ label: m, short: m.slice(2, 7), value: byMonth[m + "|" + cur] || 0 })) };
  }
  R.route("/overview", async () => {
    const [mrr, rev, overdue, coll, custs, subs, failed] = await Promise.all([
      api.get("/api/v1/analytics/mrr"),
      api.get("/api/v1/analytics/gross_revenue"),
      api.get("/api/v1/analytics/overdue_balance"),
      api.get("/api/v1/analytics/invoice_collection"),
      api.get("/api/v1/customers?per_page=1"),
      api.get("/api/v1/subscriptions?status=active&per_page=1"),
      api.get("/api/v1/invoices?payment_status=failed&per_page=1"),
    ]);
    const m = lastMonths(mrr.mrrs || [], 12);
    const g = lastMonths(rev.gross_revenues || [], 12);
    const overdueTotal = (overdue.overdue_balances || []).reduce((a, p) => a + Number(p.amount_cents || 0), 0);
    const latest = (s) => (s.length ? s[s.length - 1].value : 0);
    const fmt = (cur) => (v) => money(v, cur).replace(/<[^>]+>/g, "");
    const view = $("#view");
    view.innerHTML = head("Overview", "Live figures from finalized invoices, payments and subscriptions.") +
      '<div class="kpis">' +
      kpi("MRR (latest month)", money(latest(m.series), m.cur), "subscription fees") +
      kpi("Revenue (latest month)", money(latest(g.series), g.cur), "finalized invoices") +
      kpi("Overdue balance", money(overdueTotal, (overdue.overdue_balances || [])[0] ? overdue.overdue_balances[0].currency : m.cur), '<a href="#/dunning">dunning</a>') +
      kpi("Active subscriptions", esc(((subs.meta || {}).total_count) || 0), '<a href="#/subscriptions">view</a>') +
      kpi("Customers", esc(((custs.meta || {}).total_count) || 0), '<a href="#/customers">view</a>') +
      kpi("Failed payments", esc(((failed.meta || {}).total_count) || 0), '<a href="#/invoices?payment_status=failed">review</a>') +
      "</div>" +
      '<div class="grid2">' +
      card("MRR by month (" + esc(m.cur) + ")", '<div class="body">' + R.bars(m.series, fmt(m.cur)) + "</div>") +
      card("Gross revenue by month (" + esc(g.cur) + ")", '<div class="body">' + R.bars(g.series, fmt(g.cur)) + "</div>") +
      "</div>" +
      card("Invoice collection", table([
        { h: "Month", f: (r) => esc(r.month) },
        { h: "Payment status", f: (r) => badge(r.payment_status) },
        { h: "Invoices", f: (r) => esc(r.invoices_count), cls: "num" },
        { h: "Amount", f: (r) => money(r.amount_cents, r.currency), cls: "num" },
      ], (coll.invoice_collections || []).slice(-24).reverse(), { empty: "No finalized invoices yet." }));
  });
  function kpi(label, value, hint) {
    return '<div class="kpi"><div class="label">' + esc(label) + '</div><div class="value">' + value + '</div><div class="hint">' + (hint || "") + "</div></div>";
  }

  // ---- customers ---------------------------------------------------------------------
  const CUSTOMER_FIELDS = (c) => [
    { name: "external_id", label: "External ID", required: true, value: c.external_id, placeholder: "your system's customer id" },
    { name: "name", label: "Name", value: c.name },
    { name: "email", label: "Billing email", type: "email", value: c.email },
    { name: "currency", label: "Currency", value: c.currency, placeholder: "USD" },
    { name: "country", label: "Country (ISO)", value: c.country, placeholder: "US" },
    { name: "timezone", label: "Timezone", value: c.timezone, placeholder: "UTC" },
    { name: "tax_identification_number", label: "Tax ID", value: c.tax_identification_number },
  ];
  function customerForm(existing, after) {
    R.openForm({
      title: existing ? "Edit customer" : "New customer",
      fields: CUSTOMER_FIELDS(existing || {}),
      submitLabel: existing ? "Save" : "Create customer",
      onSubmit: async (v) => {
        const res = await api.post("/api/v1/customers", { customer: v });
        return "Customer " + res.customer.external_id + " saved";
      },
      after: after || R.render,
    });
  }
  R.customerForm = customerForm;

  R.route("/customers", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/customers?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Customers", "Everyone you bill.", btn("new", "New customer", "plus", "primary")) +
      card("All customers", table([
        { h: "External ID", f: (r) => esc(r.external_id), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Email", f: (r) => esc(r.email) },
        { h: "Currency", f: (r) => badge(r.currency || "-") },
        { h: "Created", f: (r) => date(r.created_at) },
      ], data.customers, { href: (r) => "/customers/" + enc(r.external_id), empty: "No customers yet. Create one or send them through the API." }) + R.pager(data.meta, pg));
    finish(view, { new: () => customerForm(null) });
  });

  R.route("/customers/:ext", async (p) => {
    const ext = p.ext;
    const base = "/api/v1/customers/" + enc(ext);
    const [c, subs, invs, wallets, coupons, notes] = await Promise.all([
      api.get(base),
      api.get(base + "/subscriptions?per_page=50"),
      api.get(base + "/invoices?per_page=20"),
      api.get("/api/v1/wallets?external_customer_id=" + enc(ext)),
      api.get(base + "/applied_coupons"),
      api.get(base + "/credit_notes"),
    ]);
    const cu = c.customer;
    const view = $("#view");
    view.innerHTML = head(cu.name || cu.external_id, '<span class="mono">' + esc(cu.external_id) + "</span>",
      btn("checkout", "Payment method link", "link") + btn("portal", "Billing portal", "link") + btn("crm", "Sync to CRM", "refresh") +
      btn("edit", "Edit", "edit") + btn("delete", "Delete", "trash", "danger"), '<a href="#/customers">Customers</a>') +
      '<div class="grid2">' +
      card("Details", '<div class="body">' + kv([
        ["Lago ID", '<span class="mono">' + esc(cu.lago_id) + "</span>"], ["Email", esc(cu.email)], ["Currency", esc(cu.currency)],
        ["Country", esc(cu.country)], ["Timezone", esc(cu.timezone)], ["Tax ID", esc(cu.tax_identification_number)], ["Created", datetime(cu.created_at)],
      ]) + "</div>") +
      card("Current usage", '<div class="body" id="usage-box">' + usagePicker(subs.subscriptions || []) + "</div>") +
      "</div>" +
      card("Subscriptions", table([
        { h: "External ID", f: (r) => esc(r.external_id), cls: "mono" },
        { h: "Plan", f: (r) => esc(r.plan_code) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Amount", f: (r) => money(r.plan_amount_cents, r.plan_amount_currency), cls: "num" },
        { h: "Started", f: (r) => date(r.started_at) },
      ], subs.subscriptions, { href: (r) => "/subscriptions/" + enc(r.external_id), empty: "No subscriptions." }), btn("new_sub", "Subscribe to plan", "plus")) +
      card("Invoices", table(INVOICE_COLS, invs.invoices, { href: (r) => "/invoices/" + r.lago_id, empty: "No invoices yet." }), btn("one_off", "One-off invoice", "plus")) +
      '<div class="grid2">' +
      card("Wallets", table([
        { h: "Name", f: (r) => esc(r.name || "Wallet") },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Credits", f: (r) => esc(r.credits_balance), cls: "num" },
        { h: "Balance", f: (r) => money(r.balance_cents, r.currency), cls: "num" },
      ], wallets.wallets, { href: (r) => "/wallets/" + r.lago_id, empty: "No wallets." }), btn("new_wallet", "New wallet", "plus")) +
      card("Applied coupons", table([
        { h: "Coupon", f: (r) => esc(r.coupon_code || r.lago_coupon_id), cls: "mono" },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Amount", f: (r) => r.amount_cents != null ? money(r.amount_cents, r.amount_currency) : esc((r.percentage_rate || "") + "%"), cls: "num" },
        { h: "", f: (r) => r.status === "active" ? '<button type="button" data-remove-coupon="' + esc(r.lago_id) + '">' + icon("trash") + "Remove</button>" : "" },
      ], coupons.applied_coupons, { empty: "No coupons applied." }), btn("apply_coupon", "Apply coupon", "plus")) +
      "</div>" +
      card("Credit notes", table(CREDIT_NOTE_COLS, notes.credit_notes, { href: (r) => "/credit_notes/" + r.lago_id, empty: "No credit notes." }));

    R.$$("[data-remove-coupon]", view).forEach((b) => (b.onclick = () =>
      R.confirmAction("Remove coupon", "The coupon stops applying to future invoices.", "Remove", async () => {
        await api.del(base + "/applied_coupons/" + enc(b.dataset.removeCoupon));
        return "Coupon removed";
      })));
    const usageSel = $("#usage-sub", view);
    if (usageSel) usageSel.onchange = () => loadUsage(ext, usageSel.value);
    if (usageSel && usageSel.value) loadUsage(ext, usageSel.value);

    finish(view, {
      edit: () => customerForm(cu),
      delete: () => R.confirmAction("Delete customer", "Delete " + cu.external_id + "? Their subscriptions stop billing.", "Delete", async () => {
        await api.del(base);
        location.hash = "#/customers";
        return "Customer deleted";
      }),
      checkout: async () => {
        const r = await api.post(base + "/checkout_url");
        R.showJson("Payment method setup link", { checkout_url: r.customer.checkout_url, note: "Send this link to the customer to save a card." });
      },
      portal: async () => {
        const r = await api.get(base + "/portal_url");
        window.open(r.customer.portal_url, "_blank", "noopener");
      },
      crm: async () => {
        const r = await api.post(base + "/sync_crm");
        R.toast("Synced to CRM as " + r.integration_customer.external_id, "ok");
      },
      new_sub: () => R.subscriptionForm({ external_customer_id: ext }),
      one_off: () => R.oneOffInvoiceForm(ext),
      new_wallet: () => R.walletForm({ external_customer_id: ext, currency: cu.currency }),
      apply_coupon: () => R.applyCouponForm(ext),
    });
  });
  function usagePicker(subs) {
    const active = subs.filter((s) => s.status === "active");
    if (!active.length) return '<div class="empty">No active subscription.</div>';
    return '<div class="field"><label for="usage-sub">Subscription</label><select id="usage-sub">' +
      active.map((s) => '<option value="' + esc(s.external_id) + '">' + esc(s.external_id + " (" + s.plan_code + ")") + "</option>").join("") +
      '</select></div><div id="usage-result" class="loading">Loading...</div>';
  }
  async function loadUsage(ext, subId) {
    const box = $("#usage-result");
    if (!box) return;
    box.className = "loading";
    box.textContent = "Loading...";
    try {
      const u = (await api.get("/api/v1/customers/" + enc(ext) + "/current_usage?external_subscription_id=" + enc(subId))).customer_usage;
      box.className = "";
      box.innerHTML = kv([["Period", date(u.from_datetime) + " to " + date(u.to_datetime)], ["Usage so far", money(u.total_amount_cents, u.currency)]]) +
        table([
          { h: "Metric", f: (r) => esc((r.billable_metric || {}).code) },
          { h: "Units", f: (r) => esc(r.units), cls: "num" },
          { h: "Events", f: (r) => esc(r.events_count), cls: "num" },
          { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        ], u.charges_usage, { empty: "No usage-based charges on this plan." });
    } catch (e) { box.className = "error-box"; box.textContent = e.message; }
  }

  // ---- subscriptions -------------------------------------------------------------------
  R.subscriptionForm = function (preset) {
    R.openForm({
      title: "New subscription",
      fields: [
        { name: "external_customer_id", label: "Customer external ID", required: true, value: preset.external_customer_id },
        { name: "plan_code", label: "Plan code", required: true, value: preset.plan_code },
        { name: "external_id", label: "Subscription external ID", placeholder: "defaults to a generated id" },
        { name: "name", label: "Display name" },
        { name: "billing_time", label: "Billing time", type: "select", options: [["calendar", "Calendar (1st of period)"], ["anniversary", "Anniversary (start date)"]] },
      ],
      submitLabel: "Create subscription",
      onSubmit: async (v) => {
        const res = await api.post("/api/v1/subscriptions", { subscription: v });
        location.hash = "#/subscriptions/" + enc(res.subscription.external_id);
        return "Subscription created";
      },
    });
  };
  R.route("/subscriptions", async (_p, q) => {
    const pg = page(q);
    const status = q.status || "";
    const data = await api.get("/api/v1/subscriptions?per_page=" + PER_PAGE + "&page=" + pg + (status ? "&status=" + enc(status) : ""));
    const view = $("#view");
    view.innerHTML = head("Subscriptions", "Customers on a plan.", btn("new", "New subscription", "plus", "primary")) +
      '<div class="card"><div class="toolbar"><label for="st">Status</label><select id="st">' +
      [["", "All"], ["active", "Active"], ["pending", "Pending"], ["terminated", "Terminated"], ["canceled", "Canceled"]].map(([v, l]) => '<option value="' + v + '"' + (v === status ? " selected" : "") + ">" + l + "</option>").join("") +
      "</select></div>" + table([
        { h: "External ID", f: (r) => esc(r.external_id), cls: "mono" },
        { h: "Customer", f: (r) => esc(r.external_customer_id), cls: "mono" },
        { h: "Plan", f: (r) => esc(r.plan_code) },
        { h: "Amount", f: (r) => money(r.plan_amount_cents, r.plan_amount_currency), cls: "num" },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Started", f: (r) => date(r.started_at) },
      ], data.subscriptions, { href: (r) => "/subscriptions/" + enc(r.external_id), empty: "No subscriptions match." }) + R.pager(data.meta, pg) + "</div>";
    $("#st", view).onchange = (e) => R.setQuery({ status: e.target.value });
    finish(view, { new: () => R.subscriptionForm({}) });
  });
  R.route("/subscriptions/:ext", async (p) => {
    const base = "/api/v1/subscriptions/" + enc(p.ext);
    const s = (await api.get(base)).subscription;
    let lifetime = null;
    try { lifetime = (await api.get(base + "/lifetime_usage")).lifetime_usage; } catch (_) { lifetime = null; }
    const view = $("#view");
    view.innerHTML = head(s.name || s.external_id, "Plan " + esc(s.plan_code) + " for " + '<a href="#/customers/' + enc(s.external_customer_id) + '">' + esc(s.external_customer_id) + "</a>",
      (s.status === "active" ? btn("bill", "Bill current period", "invoices") + btn("preview", "Preview invoice", "invoices") : "") +
      btn("edit", "Edit", "edit") + (s.status !== "terminated" ? btn("terminate", "Terminate", "trash", "danger") : ""), '<a href="#/subscriptions">Subscriptions</a>') +
      '<div class="grid2">' +
      card("Details", '<div class="body">' + kv([
        ["External ID", '<span class="mono">' + esc(s.external_id) + "</span>"], ["Status", badge(s.status)], ["Billing time", esc(s.billing_time)],
        ["Plan amount", money(s.plan_amount_cents, s.plan_amount_currency)], ["Subscription at", datetime(s.subscription_at)],
        ["Started", datetime(s.started_at)], ["Ending", datetime(s.ending_at)], ["Terminated", datetime(s.terminated_at)],
      ]) + "</div>") +
      card("Lifetime usage", '<div class="body">' + (lifetime ? kv(Object.entries(lifetime).filter(([, v]) => typeof v !== "object").map(([k, v]) => [k.replace(/_/g, " "), /cents$/.test(k) ? money(v, s.plan_amount_currency) : esc(v)])) : '<div class="empty">No usage recorded.</div>') + "</div>") +
      "</div>";
    finish(view, {
      bill: () => R.confirmAction("Bill current period", "Generate and finalize the invoice for the current period now.", "Bill now", async () => {
        const r = await api.post(base + "/invoices");
        location.hash = "#/invoices/" + r.invoice.lago_id;
        return "Invoice " + r.invoice.number + " created";
      }),
      preview: async () => {
        const r = await api.post("/api/v1/invoices/preview", { subscriptions: { external_ids: [s.external_id] } });
        R.showJson("Invoice preview (not saved)", r.invoice);
      },
      edit: () => R.openForm({
        title: "Edit subscription",
        fields: [
          { name: "name", label: "Display name", value: s.name },
          { name: "ending_at", label: "Ending at (YYYY-MM-DD)", value: (s.ending_at || "").slice(0, 10) },
        ],
        onSubmit: async (v) => { await api.put(base, { subscription: v }); return "Subscription updated"; },
        after: R.render,
      }),
      terminate: () => R.confirmAction("Terminate subscription", "Terminate " + s.external_id + " now? Remaining usage is billed on its final invoice.", "Terminate", async () => {
        await api.del(base);
        return "Subscription terminated";
      }),
    });
  });

  // ---- plans ------------------------------------------------------------------------------
  const CHARGE_MODELS = [["standard", "Standard (per unit)"], ["package", "Package"], ["graduated", "Graduated"], ["volume", "Volume"], ["percentage", "Percentage"], ["graduated_percentage", "Graduated percentage"]];
  R.route("/plans", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/plans?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Plans", "What you sell: a recurring fee plus usage-based charges.", btn("new", "New plan", "plus", "primary")) +
      card("All plans", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Interval", f: (r) => badge(r.interval || "-") },
        { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        { h: "Charges", f: (r) => esc((r.charges || []).length), cls: "num" },
        { h: "Trial (days)", f: (r) => esc(r.trial_period || 0), cls: "num" },
      ], data.plans, { href: (r) => "/plans/" + enc(r.code), empty: "No plans yet." }) + R.pager(data.meta, pg));
    finish(view, { new: planForm });
  });
  function planForm() {
    R.openForm({
      title: "New plan",
      fields: [
        { name: "name", label: "Name", required: true },
        { name: "code", label: "Code", required: true, placeholder: "unique, e.g. startup_monthly" },
        { name: "interval", label: "Interval", type: "select", options: [["monthly", "Monthly"], ["yearly", "Yearly"], ["quarterly", "Quarterly"], ["semiannual", "Semiannual"], ["weekly", "Weekly"]] },
        { name: "amount", label: "Recurring fee", type: "money", required: true, placeholder: "49.00" },
        { name: "amount_currency", label: "Currency", required: true, value: "USD" },
        { name: "trial_period", label: "Trial days", type: "number" },
        { name: "pay_in_advance", label: "Charge the recurring fee in advance", type: "checkbox" },
        { name: "description", label: "Description" },
      ],
      submitLabel: "Create plan",
      onSubmit: async (v) => {
        const plan = { name: v.name, code: v.code, interval: v.interval, amount_cents: v.amount, amount_currency: v.amount_currency,
          trial_period: v.trial_period, pay_in_advance: v.pay_in_advance, description: v.description, charges: [] };
        await api.post("/api/v1/plans", { plan });
        location.hash = "#/plans/" + enc(v.code);
        return "Plan created. Add usage charges next.";
      },
    });
  }
  function chargeProps(model, v) {
    if (model === "standard") return { amount: String(v.amount || "0") };
    if (model === "package") return { amount: String(v.amount || "0"), package_size: Number(v.package_size || 1), free_units: Number(v.free_units || 0) };
    if (model === "percentage") return { rate: String(v.rate || "0") };
    return v.properties || {};
  }
  R.route("/plans/:code", async (p) => {
    const base = "/api/v1/plans/" + enc(p.code);
    const [plan, metrics] = await Promise.all([api.get(base), api.get("/api/v1/billable_metrics?per_page=100")]);
    const pl = plan.plan;
    const metricById = Object.fromEntries((metrics.billable_metrics || []).map((m) => [m.lago_id, m]));
    const view = $("#view");
    view.innerHTML = head(pl.name, '<span class="mono">' + esc(pl.code) + "</span>",
      btn("subscribe", "Subscribe a customer", "plus") + btn("edit", "Edit", "edit") + btn("delete", "Delete", "trash", "danger"), '<a href="#/plans">Plans</a>') +
      card("Details", '<div class="body">' + kv([
        ["Interval", esc(pl.interval)], ["Recurring fee", money(pl.amount_cents, pl.amount_currency)], ["Paid in advance", pl.pay_in_advance ? "yes" : "no"],
        ["Trial days", esc(pl.trial_period || 0)], ["Description", esc(pl.description)],
      ]) + "</div>") +
      card("Usage charges", table([
        { h: "Metric", f: (r) => esc((metricById[r.lago_billable_metric_id] || {}).code || r.lago_billable_metric_id), cls: "mono" },
        { h: "Model", f: (r) => badge(r.charge_model) },
        { h: "Properties", f: (r) => '<span class="mono">' + esc(JSON.stringify(r.properties)) + "</span>" },
        { h: "In advance", f: (r) => (r.pay_in_advance ? "yes" : "no") },
        { h: "", f: (r) => '<button type="button" data-del-charge="' + esc(r.lago_id) + '">' + icon("trash") + "Remove</button>" },
      ], pl.charges, { empty: "No usage charges. Add one to bill metered usage." }), btn("add_charge", "Add charge", "plus"));
    R.$$("[data-del-charge]", view).forEach((b) => (b.onclick = () => R.confirmAction("Remove charge", "Existing invoices keep their fees.", "Remove", async () => {
      await api.del(base + "/charges/" + enc(b.dataset.delCharge));
      return "Charge removed";
    })));
    finish(view, {
      subscribe: () => R.subscriptionForm({ plan_code: pl.code }),
      edit: () => R.openForm({
        title: "Edit plan",
        fields: [
          { name: "name", label: "Name", value: pl.name, required: true },
          { name: "amount", label: "Recurring fee", type: "money", value: (pl.amount_cents / 100).toFixed(2) },
          { name: "description", label: "Description", value: pl.description },
        ],
        onSubmit: async (v) => { await api.put(base, { plan: { name: v.name, amount_cents: v.amount, description: v.description } }); return "Plan updated"; },
        after: R.render,
      }),
      delete: () => R.confirmAction("Delete plan", "Delete plan " + pl.code + "?", "Delete", async () => {
        await api.del(base);
        location.hash = "#/plans";
        return "Plan deleted";
      }),
      add_charge: () => R.openForm({
        title: "Add usage charge",
        intro: "Standard: price per unit. Package: price per package of N units. Percentage: rate on the event amount. Other models take a properties JSON (Lago format).",
        fields: [
          { name: "billable_metric_code", label: "Billable metric", type: "select", required: true, options: (metrics.billable_metrics || []).map((m) => [m.code, m.name + " (" + m.code + ")"]) },
          { name: "charge_model", label: "Charge model", type: "select", options: CHARGE_MODELS },
          { name: "amount", label: "Unit / package price", placeholder: "0.02" },
          { name: "package_size", label: "Package size", type: "number" },
          { name: "free_units", label: "Free units per package", type: "number" },
          { name: "rate", label: "Percentage rate", placeholder: "2.5" },
          { name: "properties", label: "Properties JSON (graduated / volume / graduated percentage)", type: "json", placeholder: '{"graduated_ranges":[{"from_value":0,"to_value":100,"per_unit_amount":"0.1","flat_amount":"0"},{"from_value":101,"to_value":null,"per_unit_amount":"0.05","flat_amount":"0"}]}' },
          { name: "pay_in_advance", label: "Charge in advance (per event)", type: "checkbox" },
        ],
        submitLabel: "Add charge",
        onSubmit: async (v) => {
          await api.post(base + "/charges", { charge: { billable_metric_code: v.billable_metric_code, charge_model: v.charge_model, pay_in_advance: v.pay_in_advance, properties: chargeProps(v.charge_model, v) } });
          return "Charge added";
        },
        after: R.render,
      }),
    });
  });

  // ---- billable metrics -----------------------------------------------------------------
  R.route("/metrics", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/billable_metrics?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Billable metrics", "How events are aggregated into billable units.", btn("new", "New metric", "plus", "primary")) +
      card("All metrics", table([
        { h: "Code", f: (r) => esc(r.code), cls: "mono" },
        { h: "Name", f: (r) => esc(r.name) },
        { h: "Aggregation", f: (r) => badge(r.aggregation_type) },
        { h: "Field", f: (r) => esc(r.field_name), cls: "mono" },
        { h: "Recurring", f: (r) => (r.recurring ? "yes" : "no") },
        { h: "", f: (r) => '<button type="button" data-del-metric="' + esc(r.code) + '">' + icon("trash") + "Delete</button>" },
      ], data.billable_metrics, { empty: "No metrics yet. A metric turns raw events into billable units." }) + R.pager(data.meta, pg));
    R.$$("[data-del-metric]", view).forEach((b) => (b.onclick = () => R.confirmAction("Delete metric", "Delete metric " + b.dataset.delMetric + "? Charges using it stop billing.", "Delete", async () => {
      await api.del("/api/v1/billable_metrics/" + enc(b.dataset.delMetric));
      return "Metric deleted";
    })));
    finish(view, {
      new: () => R.openForm({
        title: "New billable metric",
        fields: [
          { name: "name", label: "Name", required: true },
          { name: "code", label: "Code (event code)", required: true, placeholder: "api_calls" },
          { name: "aggregation_type", label: "Aggregation", type: "select", options: [["count_agg", "Count events"], ["sum_agg", "Sum of a property"], ["max_agg", "Max of a property"], ["unique_count_agg", "Unique count of a property"], ["latest_agg", "Latest value"], ["weighted_sum_agg", "Weighted sum"]] },
          { name: "field_name", label: "Property to aggregate", placeholder: "required unless counting events" },
          { name: "recurring", label: "Recurring (value carries over between periods)", type: "checkbox" },
          { name: "description", label: "Description" },
        ],
        submitLabel: "Create metric",
        onSubmit: async (v) => { await api.post("/api/v1/billable_metrics", { billable_metric: v }); return "Metric created"; },
        after: R.render,
      }),
    });
  });

  // ---- invoices --------------------------------------------------------------------------------
  const INVOICE_COLS = [
    { h: "Number", f: (r) => esc(r.number), cls: "mono" },
    { h: "Issued", f: (r) => date(r.issuing_date) },
    { h: "Type", f: (r) => esc(r.invoice_type) },
    { h: "Status", f: (r) => badge(r.status) },
    { h: "Payment", f: (r) => badge(r.payment_status) },
    { h: "Total", f: (r) => money(r.total_amount_cents, r.currency), cls: "num" },
  ];
  R.INVOICE_COLS = INVOICE_COLS;
  R.oneOffInvoiceForm = function (ext) {
    R.openForm({
      title: "One-off invoice",
      intro: "Bill an add-on (by code) or a custom line. The invoice is finalized and emailed.",
      fields: [
        { name: "external_customer_id", label: "Customer external ID", required: true, value: ext },
        { name: "add_on_code", label: "Add-on code", placeholder: "leave empty for a custom line" },
        { name: "name", label: "Custom line name" },
        { name: "unit_amount", label: "Unit amount", type: "money", placeholder: "100.00" },
        { name: "units", label: "Units", type: "number", value: "1" },
        { name: "currency", label: "Currency", placeholder: "customer currency" },
      ],
      submitLabel: "Create invoice",
      onSubmit: async (v) => {
        const line = { units: String(v.units || 1) };
        if (v.add_on_code) line.add_on_code = v.add_on_code; else line.name = v.name;
        if (v.unit_amount !== undefined) line.unit_amount_cents = v.unit_amount;
        const r = await api.post("/api/v1/invoices", { invoice: { external_customer_id: v.external_customer_id, currency: v.currency, fees: [line] } });
        location.hash = "#/invoices/" + r.invoice.lago_id;
        return "Invoice " + r.invoice.number + " created";
      },
    });
  };
  R.route("/invoices", async (_p, q) => {
    const pg = page(q);
    const qs = "per_page=" + PER_PAGE + "&page=" + pg + (q.status ? "&status=" + enc(q.status) : "") + (q.payment_status ? "&payment_status=" + enc(q.payment_status) : "");
    const data = await api.get("/api/v1/invoices?" + qs);
    const sel = (id, cur, opts) => '<select id="' + id + '">' + opts.map(([v, l]) => '<option value="' + v + '"' + (v === (cur || "") ? " selected" : "") + ">" + l + "</option>").join("") + "</select>";
    const view = $("#view");
    view.innerHTML = head("Invoices", "Finalized, draft and voided invoices.", btn("export", "Export CSV", "download") + btn("one_off", "One-off invoice", "plus", "primary")) +
      '<div class="card"><div class="toolbar"><label for="f-status">Status</label>' +
      sel("f-status", q.status, [["", "All"], ["draft", "Draft"], ["finalized", "Finalized"], ["voided", "Voided"], ["failed", "Failed"]]) +
      '<label for="f-pay">Payment</label>' + sel("f-pay", q.payment_status, [["", "All"], ["pending", "Pending"], ["succeeded", "Succeeded"], ["failed", "Failed"]]) +
      "</div>" + table(INVOICE_COLS, data.invoices, { href: (r) => "/invoices/" + r.lago_id, empty: "No invoices match." }) + R.pager(data.meta, pg) + "</div>";
    $("#f-status", view).onchange = (e) => R.setQuery({ status: e.target.value, payment_status: q.payment_status });
    $("#f-pay", view).onchange = (e) => R.setQuery({ status: q.status, payment_status: e.target.value });
    finish(view, { one_off: () => R.oneOffInvoiceForm(""), export: () => R.startExport("invoices") });
  });
  R.route("/invoices/:id", async (p) => {
    const base = "/api/v1/invoices/" + enc(p.id);
    const inv = (await api.get(base)).invoice;
    let payments = [];
    try { payments = (await api.get("/api/v1/payments?invoice_id=" + enc(p.id))).payments || []; } catch (_) { payments = []; }
    const fin = inv.status === "finalized";
    const unpaid = fin && inv.payment_status !== "succeeded";
    const view = $("#view");
    view.innerHTML = head("Invoice " + inv.number, badge(inv.status) + " " + badge(inv.payment_status) +
      (inv.lago_customer_id ? ' <span class="mono">' + esc(inv.lago_customer_id) + "</span>" : ""),
      btn("pdf", "PDF", "download") + btn("xml", "XML", "download") +
      (inv.status === "draft" ? btn("finalize", "Finalize", "check", "primary") + btn("refresh", "Refresh totals", "refresh") : "") +
      (inv.status === "failed" ? btn("retry", "Retry finalization", "retry") : "") +
      (unpaid ? btn("charge", "Collect payment now", "pay", "primary") + btn("paylink", "Payment link", "link") + btn("mark_paid", "Mark as paid", "check") : "") +
      (fin ? btn("credit", "Issue credit note", "credit_notes") + btn("email", "Resend email", "refresh") : "") +
      (fin && inv.payment_status === "succeeded" ? btn("refund", "Refund", "retry", "danger") : "") +
      (fin ? btn("void", "Void", "trash", "danger") : ""), '<a href="#/invoices">Invoices</a>') +
      '<div class="grid2">' +
      card("Amounts", '<div class="body">' + kv([
        ["Fees", money(inv.fees_amount_cents, inv.currency)], ["Coupons", money(inv.coupons_amount_cents, inv.currency)],
        ["Subtotal excl. tax", money(inv.sub_total_excluding_taxes_amount_cents, inv.currency)], ["Taxes", money(inv.taxes_amount_cents, inv.currency)],
        ["Credit notes", money(inv.credit_notes_amount_cents, inv.currency)], ["Prepaid credits", money(inv.prepaid_credit_amount_cents, inv.currency)],
        ["Total due", "<strong>" + money(inv.total_amount_cents, inv.currency) + "</strong>"],
      ]) + "</div>") +
      card("Details", '<div class="body">' + kv([
        ["Lago ID", '<span class="mono">' + esc(inv.lago_id) + "</span>"], ["Issued", date(inv.issuing_date)], ["Type", esc(inv.invoice_type)], ["Created", datetime(inv.created_at)],
      ]) + "</div>") + "</div>" +
      card("Fees", table([
        { h: "Fee", f: (r) => esc(r.lago_charge_id ? "Usage charge" : "Subscription / line") },
        { h: "Units", f: (r) => esc(r.units), cls: "num" },
        { h: "Events", f: (r) => esc(r.events_count), cls: "num" },
        { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        { h: "Tax", f: (r) => money(r.taxes_amount_cents, r.amount_currency), cls: "num" },
        { h: "Total", f: (r) => money(r.total_amount_cents, r.amount_currency), cls: "num" },
      ], inv.fees, { empty: "No fees." })) +
      card("Payments", table([
        { h: "Provider", f: (r) => esc(r.payment_provider_type) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
        { h: "Lago ID", f: (r) => esc(r.lago_id), cls: "mono" },
      ], payments, { empty: "No payment attempts yet." }));
    finish(view, {
      pdf: () => api.download(base + "/download", inv.number + ".pdf"),
      xml: () => api.download(base + "/download.xml", inv.number + ".xml"),
      finalize: async () => { await api.put(base + "/finalize"); R.toast("Invoice finalized", "ok"); R.render(); },
      refresh: async () => { await api.put(base + "/refresh"); R.toast("Totals recomputed", "ok"); R.render(); },
      retry: async () => { await api.post(base + "/retry"); R.toast("Finalization retried", "ok"); R.render(); },
      charge: async () => {
        const r = await api.post(base + "/retry_payment");
        const res = r.invoice.result;
        R.toast(r.invoice.retried ? "Charge " + res : "Not charged: " + res.replace(/_/g, " "), r.invoice.retried && res !== "failed" ? "ok" : "err");
        R.render();
      },
      paylink: async () => {
        const r = await api.post(base + "/payment_url");
        R.showJson("Payment link", r.invoice_payment_details);
      },
      mark_paid: () => R.confirmAction("Mark as paid", "Record that this invoice was paid outside Rago (bank transfer, cash).", "Mark paid", async () => {
        await api.put(base, { invoice: { payment_status: "succeeded" } });
        return "Invoice marked as paid";
      }),
      email: async () => { await api.post(base + "/resend_email"); R.toast("Invoice email sent", "ok"); },
      refund: () => R.confirmAction("Refund invoice", "Mark the payment refunded.", "Refund", async () => { await api.post(base + "/refund"); return "Invoice refunded"; }),
      void: () => R.confirmAction("Void invoice", "Void " + inv.number + "? It will no longer be collectible.", "Void", async () => { await api.post(base + "/void"); return "Invoice voided"; }),
      credit: () => creditNoteForm(inv),
    });
  });
  function creditNoteForm(inv) {
    const fees = inv.fees || [];
    R.openForm({
      title: "Issue credit note on " + inv.number,
      intro: "Enter the amount to credit per fee (leave empty to skip a fee). Optionally refund part of it.",
      fields: fees.map((f, i) => ({ name: "fee_" + i, label: "Fee " + (i + 1) + " (max " + (f.amount_cents / 100).toFixed(2) + " " + f.amount_currency + ")", type: "money" }))
        .concat([{ name: "refund", label: "Refund amount", type: "money", placeholder: "0.00" }, { name: "reason", label: "Reason", type: "select", options: [["other", "Other"], ["duplicated_charge", "Duplicated charge"], ["product_unsatisfactory", "Product unsatisfactory"], ["order_change", "Order change"], ["order_cancellation", "Order cancellation"], ["fraudulent_charge", "Fraudulent charge"]] }]),
      submitLabel: "Issue credit note",
      onSubmit: async (v) => {
        const items = fees.map((f, i) => ({ fee_id: f.lago_id, amount_cents: v["fee_" + i] })).filter((it) => it.amount_cents);
        if (!items.length) throw new Error("Enter an amount for at least one fee.");
        const r = await api.post("/api/v1/credit_notes", { credit_note: { invoice_id: inv.lago_id, reason: v.reason, refund_amount_cents: v.refund, items } });
        location.hash = "#/credit_notes/" + r.credit_note.lago_id;
        return "Credit note " + r.credit_note.number + " issued";
      },
    });
  }

  // ---- credit notes ---------------------------------------------------------------------------
  const CREDIT_NOTE_COLS = [
    { h: "Number", f: (r) => esc(r.number), cls: "mono" },
    { h: "Issued", f: (r) => date(r.issuing_date) },
    { h: "Total", f: (r) => money(r.total_amount_cents, r.currency), cls: "num" },
    { h: "Credit", f: (r) => money(r.credit_amount_cents, r.currency), cls: "num" },
    { h: "Refund", f: (r) => money(r.refund_amount_cents, r.currency), cls: "num" },
    { h: "Remaining", f: (r) => money(r.balance_amount_cents, r.currency), cls: "num" },
  ];
  R.route("/credit_notes", async (_p, q) => {
    const pg = page(q);
    const data = await api.get("/api/v1/credit_notes?per_page=" + PER_PAGE + "&page=" + pg);
    const view = $("#view");
    view.innerHTML = head("Credit notes", "Credits and refunds issued against invoices.", btn("export", "Export CSV", "download")) +
      card("All credit notes", table(CREDIT_NOTE_COLS, data.credit_notes, { href: (r) => "/credit_notes/" + r.lago_id, empty: "No credit notes. Issue one from an invoice." }) + R.pager(data.meta, pg));
    finish(view, { export: () => R.startExport("credit_notes") });
  });
  R.route("/credit_notes/:id", async (p) => {
    const base = "/api/v1/credit_notes/" + enc(p.id);
    const cn = (await api.get(base)).credit_note;
    const view = $("#view");
    view.innerHTML = head("Credit note " + cn.number, "", btn("pdf", "PDF", "download") + btn("xml", "XML", "download") + btn("email", "Resend email", "refresh") +
      btn("refunded", "Mark refund succeeded", "check") + btn("void", "Void", "trash", "danger"), '<a href="#/credit_notes">Credit notes</a>') +
      card("Details", '<div class="body">' + kv([
        ["Invoice", '<a href="#/invoices/' + esc(cn.lago_invoice_id) + '">' + esc(cn.lago_invoice_id) + "</a>"], ["Issued", date(cn.issuing_date)],
        ["Reason", esc(cn.reason)], ["Total", money(cn.total_amount_cents, cn.currency)], ["Taxes", money(cn.taxes_amount_cents, cn.currency)],
        ["Credit", money(cn.credit_amount_cents, cn.currency)], ["Refund", money(cn.refund_amount_cents, cn.currency)], ["Remaining credit", money(cn.balance_amount_cents, cn.currency)],
      ]) + "</div>");
    finish(view, {
      pdf: () => api.download(base + "/download", cn.number + ".pdf"),
      xml: () => api.postDownload(base + "/download_xml", cn.number + ".xml"),
      email: async () => { await api.post(base + "/resend_email"); R.toast("Credit note email sent", "ok"); },
      refunded: async () => { await api.put(base, { credit_note: { refund_status: "succeeded" } }); R.toast("Refund marked succeeded", "ok"); R.render(); },
      void: () => R.confirmAction("Void credit note", "Void " + cn.number + "? Remaining credit is cancelled.", "Void", async () => { await api.put(base + "/void"); return "Credit note voided"; }),
    });
  });
})();
