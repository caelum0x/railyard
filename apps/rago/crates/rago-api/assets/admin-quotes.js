"use strict";
// Rago Billing console — quotes, order forms and orders: the quote → order
// form → order → invoice flow (REST: /api/v1/quotes, /order_forms, /orders).
(function () {
  const R = window.Rago;
  const { $, $$, esc, money, date, datetime, badge, api, enc, table, kv, btn, icon } = R;
  const { head, card, page, finish, PER_PAGE } = R.ui;

  R.navItem("Revenue", "/quotes", "Quotes", "credit_notes");
  R.navItem("Revenue", "/orders", "Orders", "invoices");

  const QUOTE_STATUSES = [["", "All statuses"], ["draft", "Open"], ["accepted", "Accepted"], ["voided", "Voided"], ["expired", "Expired"]];
  const ORDER_STATUSES = [["", "All statuses"], ["pending", "Pending"], ["executed", "Executed"], ["voided", "Voided"]];
  const LINES_HELP = "One line per item: description ; unit price ; quantity. Use @add_on_code as the description to bill an add-on " +
    "(leave the price empty to use the add-on's price). Example: Onboarding package ; 1500 ; 1";

  // ---- line items <-> text ------------------------------------------------------------
  function parseLines(text) {
    const rows = String(text || "").split("\n").map((l) => l.trim()).filter(Boolean);
    if (!rows.length) throw new Error("Add at least one line item.");
    return rows.map((row, i) => {
      const parts = row.split(";").map((p) => p.trim());
      const [label, price, qty] = [parts[0] || "", parts[1] || "", parts[2] || ""];
      const units = qty === "" ? "1" : qty;
      if (Number.isNaN(Number(units)) || Number(units) <= 0) throw new Error("Line " + (i + 1) + ": quantity must be a positive number.");
      let cents;
      if (price !== "") {
        cents = R.centsFromInput(price);
        if (cents === undefined || cents < 0) throw new Error("Line " + (i + 1) + ": unit price is not a valid amount.");
      }
      if (label.startsWith("@")) {
        const code = label.slice(1).trim();
        if (!code) throw new Error("Line " + (i + 1) + ": add-on code is missing after @.");
        const line = { add_on_code: code, units };
        if (cents !== undefined) line.unit_amount_cents = cents;
        return line;
      }
      if (!label) throw new Error("Line " + (i + 1) + ": description is required.");
      if (cents === undefined) throw new Error("Line " + (i + 1) + ": unit price is required.");
      return { name: label, unit_amount_cents: cents, units };
    });
  }
  function linesToText(items) {
    return (Array.isArray(items) ? items : []).map((l) =>
      (l.name || (l.add_on_code ? "@" + l.add_on_code : "")) + " ; " +
      (l.unit_amount_cents !== undefined && l.unit_amount_cents !== null ? (Number(l.unit_amount_cents) / 100).toFixed(2) : "") + " ; " +
      (l.units || "1")).join("\n");
  }
  function linesHtml(items, currency) {
    const list = Array.isArray(items) ? items : [];
    if (!list.length) return '<span class="muted">No lines</span>';
    return list.map((l) => esc(l.name || l.add_on_code || "Line") + ' <span class="muted">' + esc(l.units || "1") + " x " +
      money(l.unit_amount_cents, currency) + "</span>").join("<br>");
  }
  function customerLink(ext, name) {
    if (!ext) return "-";
    return '<a href="#/customers/' + enc(ext) + '">' + esc(name || ext) + "</a>" + (name ? ' <span class="muted mono">' + esc(ext) + "</span>" : "");
  }

  // ---- forms ------------------------------------------------------------------------------
  R.quoteForm = function (preset) {
    preset = preset || {};
    R.openForm({
      title: "New quote",
      intro: "Totals are priced with your organization's taxes. The quote starts as draft version 1, which you can edit before approving.",
      fields: [
        { name: "external_customer_id", label: "Customer external ID", required: true, value: preset.external_customer_id },
        { name: "currency", label: "Currency", placeholder: "defaults to the customer's currency", value: preset.currency },
        { name: "lines", label: "Line items", type: "textarea", required: true, help: LINES_HELP, placeholder: "Onboarding package ; 1500 ; 1\n@priority_support ; ; 12" },
      ],
      submitLabel: "Create quote",
      onSubmit: async (v) => {
        const line_items = parseLines(v.lines);
        const r = await api.post("/api/v1/quotes", { quote: { external_customer_id: v.external_customer_id, currency: v.currency, line_items } });
        location.hash = "#/quotes/" + r.quote.lago_id;
        return "Quote " + r.quote.number + " created";
      },
    });
  };
  function signForm(form) {
    R.openForm({
      title: "Mark order form " + form.number + " as signed",
      intro: "Signing creates the order. Execute immediately to invoice the quote now; otherwise execute it from the Orders page.",
      fields: [
        { name: "signed_document", label: "Signed document reference", placeholder: "URL or document ID of the countersigned form" },
        { name: "execution_mode", label: "Execution", type: "select", options: [["manual", "Create the order; execute it later"], ["immediate", "Create and execute the order now (issues the invoice)"]] },
      ],
      submitLabel: "Mark signed",
      onSubmit: async (v) => {
        const r = await api.post("/api/v1/order_forms/" + enc(form.lago_id) + "/sign", { order_form: v });
        return r.lago_invoice_id ? "Signed; order executed and invoice issued" : "Signed; order " + r.order.number + " created";
      },
      after: R.render,
    });
  }
  function executeOrder(order) {
    R.confirmAction("Execute order " + order.number, "Bills the quote's approved version on a one-off invoice and queues collection. This happens once.", "Execute", async () => {
      const r = await api.post("/api/v1/orders/" + enc(order.lago_id) + "/execute", {});
      return "Order executed; invoice " + String(r.lago_invoice_id).slice(0, 8) + " issued";
    });
  }
  function voidOrder(order) {
    R.confirmAction("Void order " + order.number, "The order is cancelled and will not be invoiced.", "Void order", async () => {
      await api.post("/api/v1/orders/" + enc(order.lago_id) + "/void", {});
      return "Order voided";
    });
  }
  function voidOrderForm(form) {
    R.confirmAction("Void order form " + form.number, "The customer can no longer sign this form.", "Void form", async () => {
      await api.post("/api/v1/order_forms/" + enc(form.lago_id) + "/void", {});
      return "Order form voided";
    });
  }

  const ORDER_COLS = [
    { h: "Number", f: (r) => esc(r.number), cls: "mono" },
    { h: "Customer", f: (r) => customerLink(r.external_customer_id, r.customer_name) },
    { h: "Quote", f: (r) => (r.lago_quote_id ? '<a href="#/quotes/' + esc(r.lago_quote_id) + '">' + esc(r.quote_number || "quote") + "</a>" : "-") },
    { h: "Status", f: (r) => badge(r.status) },
    { h: "Executed", f: (r) => datetime(r.executed_at) },
    { h: "Invoice", f: (r) => (r.lago_invoice_id ? '<a href="#/invoices/' + esc(r.lago_invoice_id) + '">' + money(r.invoice_total_amount_cents, r.invoice_currency) + "</a>" : "-") },
    { h: "", f: (r) => (r.status === "pending"
      ? '<button type="button" data-exec-order="' + esc(r.lago_id) + '" class="primary">' + icon("check") + "Execute</button> " +
        '<button type="button" data-void-order="' + esc(r.lago_id) + '">' + icon("close") + "Void</button>"
      : "") },
  ];
  const FORM_COLS = [
    { h: "Number", f: (r) => esc(r.number), cls: "mono" },
    { h: "Customer", f: (r) => customerLink(r.external_customer_id, r.customer_name) },
    { h: "Quote", f: (r) => (r.lago_quote_id ? '<a href="#/quotes/' + esc(r.lago_quote_id) + '">' + esc(r.quote_number || "quote") + "</a>" : "-") },
    { h: "Status", f: (r) => badge(r.status === "draft" ? "awaiting signature" : r.status) },
    { h: "Execution", f: (r) => esc(r.execution_mode || "-") },
    { h: "Signed", f: (r) => datetime(r.signed_at) + (r.signed_document ? '<div class="muted mono">' + esc(r.signed_document) + "</div>" : "") },
    { h: "", f: (r) => (r.status === "draft"
      ? '<button type="button" data-sign-form="' + esc(r.lago_id) + '" class="primary">' + icon("check") + "Mark signed</button> " +
        '<button type="button" data-void-form="' + esc(r.lago_id) + '">' + icon("close") + "Void</button>"
      : "") },
  ];
  function bindFlowButtons(view, orders, forms) {
    const byId = (list, id) => (list || []).find((x) => x.lago_id === id);
    $$("[data-exec-order]", view).forEach((b) => (b.onclick = (e) => { e.stopPropagation(); executeOrder(byId(orders, b.dataset.execOrder)); }));
    $$("[data-void-order]", view).forEach((b) => (b.onclick = (e) => { e.stopPropagation(); voidOrder(byId(orders, b.dataset.voidOrder)); }));
    $$("[data-sign-form]", view).forEach((b) => (b.onclick = (e) => { e.stopPropagation(); signForm(byId(forms, b.dataset.signForm)); }));
    $$("[data-void-form]", view).forEach((b) => (b.onclick = (e) => { e.stopPropagation(); voidOrderForm(byId(forms, b.dataset.voidForm)); }));
  }
  function statusFilter(id, options, current) {
    return '<div class="toolbar"><label for="' + id + '">Status</label><select id="' + id + '">' +
      options.map(([v, l]) => '<option value="' + esc(v) + '"' + (v === (current || "") ? " selected" : "") + ">" + esc(l) + "</option>").join("") +
      '</select><span class="spacer"></span></div>';
  }

  // ---- quotes list ----------------------------------------------------------------------------
  R.route("/quotes", async (_p, q) => {
    const pg = page(q);
    const params = new URLSearchParams({ per_page: PER_PAGE, page: pg });
    if (q.status) params.set("status", q.status);
    if (q.customer) params.set("external_customer_id", q.customer);
    const data = await api.get("/api/v1/quotes?" + params.toString());
    const view = $("#view");
    view.innerHTML = head("Quotes", "Price a deal, get it approved, issue an order form, and invoice it once signed.", btn("new", "New quote", "plus", "primary")) +
      card("All quotes", statusFilter("quote-status", QUOTE_STATUSES, q.status) + table([
        { h: "Number", f: (r) => esc(r.number), cls: "mono" },
        { h: "Customer", f: (r) => customerLink(r.external_customer_id, r.customer_name) },
        { h: "Status", f: (r) => badge(r.status === "draft" ? "open" : r.status) },
        { h: "Latest version", f: (r) => (r.versions_count ? "v" + esc(r.versions_count) + " " + badge(r.latest_version_status) : '<span class="muted">none</span>') },
        { h: "Total", f: (r) => money(r.total_amount_cents, r.currency), cls: "num" },
        { h: "Created", f: (r) => date(r.created_at) },
      ], data.quotes, { href: (r) => "/quotes/" + r.lago_id, empty: q.status ? "No quotes with this status." : "No quotes yet. Create one to price a deal for a customer." }) + R.pager(data.meta, pg));
    $("#quote-status", view).onchange = (e) => R.setQuery(Object.assign({}, q, { status: e.target.value, page: undefined }));
    finish(view, { new: () => R.quoteForm({ external_customer_id: q.customer }) });
  });

  // ---- quote detail ------------------------------------------------------------------------------
  R.route("/quotes/:id", async (p) => {
    const base = "/api/v1/quotes/" + enc(p.id);
    const qt = (await api.get(base)).quote;
    const versions = qt.versions || [];
    const forms = qt.order_forms || [];
    const orders = qt.orders || [];
    const open = qt.status === "draft";
    const hasDraft = versions.some((v) => v.status === "draft");
    const approved = versions.filter((v) => v.status === "approved").pop();
    const openForm = forms.find((f) => f.status !== "voided");
    const actions =
      (open && approved && !openForm ? btn("issue", "Issue order form", "exports", "primary") : "") +
      (open && !hasDraft ? btn("new_version", "New version", "plus") : "") +
      (open && !versions.length ? btn("accept", "Accept and invoice", "check", "primary") : "") +
      (open ? btn("void", "Void quote", "trash", "danger") : "");
    const view = $("#view");
    const nextStep = !open ? "" :
      !versions.length ? "This quote predates versioning: accept it to invoice directly, or start a version to use order forms." :
      hasDraft && !approved ? "Next: review the draft version and approve it." :
      approved && !openForm ? "Next: issue an order form from the approved version." :
      openForm && openForm.status === "draft" ? "Next: mark the order form as signed once the customer countersigns." :
      orders.some((o) => o.status === "pending") ? "Next: execute the pending order to issue the invoice." : "";
    view.innerHTML = head("Quote " + qt.number, "Customer " + customerLink(qt.external_customer_id, qt.customer_name), actions, '<a href="#/quotes">Quotes</a>') +
      (nextStep ? '<div class="card"><div class="body">' + icon("alert") + " " + esc(nextStep) + "</div></div>" : "") +
      '<div class="grid2">' +
      card("Details", '<div class="body">' + kv([
        ["Status", badge(qt.status === "draft" ? "open" : qt.status)],
        ["Currency", esc(qt.currency)],
        ["Sub-total", money(qt.sub_total_excluding_taxes_amount_cents, qt.currency)],
        ["Taxes", money(qt.taxes_amount_cents, qt.currency)],
        ["Total", money(qt.total_amount_cents, qt.currency)],
        ["Invoice", qt.accepted_invoice_id ? '<a href="#/invoices/' + esc(qt.accepted_invoice_id) + '">view invoice</a>' : "-"],
        ["Created", datetime(qt.created_at)],
      ]) + "</div>") +
      card(approved ? "Approved lines (v" + approved.version_number + ")" : "Quoted lines", '<div class="body">' + linesHtml(approved ? approved.line_items : qt.line_items, qt.currency) + "</div>") +
      "</div>" +
      card("Versions", table([
        { h: "Version", f: (r) => "v" + esc(r.version_number) },
        { h: "Status", f: (r) => badge(r.status) },
        { h: "Lines", f: (r) => linesHtml(r.line_items, r.currency) },
        { h: "Total (incl. taxes)", f: (r) => money(r.total_amount_cents, r.currency), cls: "num" },
        { h: "Expires", f: (r) => datetime(r.expires_at) },
        { h: "Approved", f: (r) => datetime(r.approved_at) },
        { h: "", f: (r) => !open ? "" :
          (r.status === "draft" ? '<button type="button" data-edit-version="' + esc(r.lago_id) + '">' + icon("edit") + "Edit</button> " +
            '<button type="button" data-approve-version="' + esc(r.lago_id) + '" class="primary">' + icon("check") + "Approve</button> " : "") +
          (!hasDraft ? '<button type="button" data-clone-version="' + esc(r.lago_id) + '">' + icon("plus") + "New draft from this</button> " : "") +
          (r.status !== "voided" ? '<button type="button" data-void-version="' + esc(r.lago_id) + '">' + icon("close") + "Void</button>" : "") },
      ], versions, { empty: "No versions. Start one to edit, approve and issue this quote as an order form." })) +
      card("Order forms", table(FORM_COLS, forms, { empty: "No order form issued yet." })) +
      card("Orders", table(ORDER_COLS, orders, { empty: "No order yet. Signing an order form creates one." }));

    const versionById = (id) => versions.find((v) => v.lago_id === id);
    $$("[data-edit-version]", view).forEach((b) => (b.onclick = () => {
      const v = versionById(b.dataset.editVersion);
      R.openForm({
        title: "Edit version " + v.version_number,
        intro: "Only draft versions can be edited. The total is re-priced with your organization's taxes.",
        fields: [{ name: "lines", label: "Line items", type: "textarea", required: true, value: linesToText(v.line_items), help: LINES_HELP }],
        onSubmit: async (vals) => {
          await api.put(base + "/versions/" + enc(v.lago_id), { quote_version: { line_items: parseLines(vals.lines) } });
          return "Version " + v.version_number + " updated";
        },
        after: R.render,
      });
    }));
    $$("[data-approve-version]", view).forEach((b) => (b.onclick = () => {
      const v = versionById(b.dataset.approveVersion);
      R.openForm({
        title: "Approve version " + v.version_number,
        intro: "The approved version is what the order form and invoice use. A previously approved version is superseded.",
        fields: [{ name: "expires_at", label: "Offer expires on", type: "date", help: "Optional. After this date the version can no longer be issued or executed." }],
        submitLabel: "Approve",
        onSubmit: async (vals) => {
          await api.post(base + "/versions/" + enc(v.lago_id) + "/approve", { quote_version: { expires_at: vals.expires_at } });
          return "Version " + v.version_number + " approved";
        },
        after: R.render,
      });
    }));
    $$("[data-clone-version]", view).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try {
        const r = await api.post(base + "/versions", { from_version_id: b.dataset.cloneVersion });
        R.toast("Draft version " + r.quote_version.version_number + " created", "ok");
        R.render();
      } catch (err) { R.toast(err.message, "err"); } finally { b.disabled = false; }
    }));
    $$("[data-void-version]", view).forEach((b) => (b.onclick = () => {
      const v = versionById(b.dataset.voidVersion);
      R.confirmAction("Void version " + v.version_number, "A voided version cannot be approved or issued.", "Void version", async () => {
        await api.post(base + "/versions/" + enc(v.lago_id) + "/void", {});
        return "Version voided";
      });
    }));
    bindFlowButtons(view, orders, forms);
    finish(view, {
      issue: async () => {
        const r = await api.post(base + "/order_forms", {});
        R.toast("Order form " + r.order_form.number + " issued", "ok");
        R.render();
      },
      new_version: async () => {
        const r = await api.post(base + "/versions", {});
        R.toast("Draft version " + r.quote_version.version_number + " created", "ok");
        R.render();
      },
      accept: () => R.confirmAction("Accept quote " + qt.number, "Issues a finalized one-off invoice for the quoted lines.", "Accept and invoice", async () => {
        const r = await api.post(base + "/accept", {});
        location.hash = "#/invoices/" + r.invoice.lago_id;
        return "Quote accepted; invoice issued";
      }),
      void: () => R.confirmAction("Void quote " + qt.number, "Voids the quote, its open versions and any unsigned order form.", "Void quote", async () => {
        await api.post(base + "/void", {});
        return "Quote voided";
      }),
    });
  });

  // ---- orders + order forms ---------------------------------------------------------------------
  R.route("/orders", async (_p, q) => {
    const params = new URLSearchParams();
    if (q.status) params.set("status", q.status);
    const [od, fd] = await Promise.all([
      api.get("/api/v1/orders" + (params.toString() ? "?" + params.toString() : "")),
      api.get("/api/v1/order_forms"),
    ]);
    const orders = od.orders || [];
    const forms = fd.order_forms || [];
    const awaiting = forms.filter((f) => f.status === "draft");
    const view = $("#view");
    view.innerHTML = head("Orders", "Signed order forms become orders; executing an order invoices the quote.", btn("quotes", "Quotes", "credit_notes")) +
      card("Orders", statusFilter("order-status", ORDER_STATUSES, q.status) + table(ORDER_COLS, orders, { empty: q.status ? "No orders with this status." : "No orders yet. Orders are created when an order form is signed." })) +
      card("Order forms awaiting signature", table(FORM_COLS, awaiting, { empty: "No order form is waiting for a signature." })) +
      card("All order forms", table(FORM_COLS, forms, { empty: "No order forms issued yet. Issue one from an approved quote." }));
    $("#order-status", view).onchange = (e) => R.setQuery(Object.assign({}, q, { status: e.target.value }));
    bindFlowButtons(view, orders, forms);
    finish(view, { quotes: () => { location.hash = "#/quotes"; } });
  });
})();
