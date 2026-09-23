"use strict";
// Rago Billing console — billing panels mounted inside other views:
// progressive-billing usage thresholds (plan + subscription), the pending
// SCA / 3-D Secure payment action of an invoice, and payment receipts.
(function () {
  const R = window.Rago;
  const { $, esc, money, datetime, badge, api, btn, icon, centsFromInput } = R;

  function panel(title, inner, actionsHtml) {
    return '<div class="card"><h3>' + esc(title) + '<span class="spacer"></span>' + (actionsHtml || "") + "</h3>" + inner + "</div>";
  }

  // ---- usage thresholds (progressive billing) ------------------------------------------
  // Fixed thresholds: one amount per line, optionally followed by a label,
  // e.g. "100.00 Starter cap". The recurring threshold bills again every time
  // usage advances by that amount past the largest fixed threshold.
  function parseFixed(text) {
    const out = [];
    (text || "").split("\n").map((l) => l.trim()).filter(Boolean).forEach((line, i) => {
      const m = line.match(/^([0-9]+(?:[.,][0-9]{1,2})?)\s*(.*)$/);
      if (!m) throw new Error("Line " + (i + 1) + ': start with an amount, e.g. "100.00 Starter cap"');
      const cents = centsFromInput(m[1].replace(",", "."));
      if (!cents || cents <= 0) throw new Error("Line " + (i + 1) + ": amount must be positive");
      out.push({ amount_cents: cents, recurring: false, threshold_display_name: m[2] || undefined });
    });
    return out;
  }

  R.thresholdsPanel = async function (box, url, currency, opts) {
    const o = opts || {};
    if (!box) return;
    box.innerHTML = panel("Progressive billing", '<div class="body"><div class="empty">Loading thresholds...</div></div>');
    let data;
    try { data = await api.get(url); } catch (e) {
      box.innerHTML = panel("Progressive billing", '<div class="body"><div class="error-box">Thresholds could not be loaded: ' + esc(e.message) + "</div></div>");
      return;
    }
    const rows = data.usage_thresholds || [];
    const inherited = !!data.inherited_from_plan;
    const fixed = rows.filter((t) => !t.recurring);
    const recurring = rows.find((t) => t.recurring);
    const intro = rows.length
      ? (inherited ? '<div class="help">Inherited from the plan. Saving here overrides them for this subscription only.</div>' : "")
      : '<div class="empty">No thresholds. Usage is billed only at the end of each period.</div>';
    const list = rows.length ? R.table([
      { h: "Threshold", f: (r) => money(r.amount_cents, currency), cls: "num" },
      { h: "Type", f: (r) => badge(r.recurring ? "recurring" : "fixed") },
      { h: "Label", f: (r) => esc(r.threshold_display_name || "") },
    ], rows, {}) : "";
    const actions = btn("edit_thresholds", rows.length ? "Edit thresholds" : "Add thresholds", "edit") +
      (o.subscription && rows.length ? btn("check_thresholds", "Check usage now", "refresh") : "") +
      (o.subscription && !inherited && rows.length ? btn("reset_thresholds", "Use plan thresholds", "retry") : "");
    box.innerHTML = panel("Progressive billing", '<div class="body">' + intro + "</div>" + list, actions);
    R.bindActions(box, {
      edit_thresholds: () => R.openForm({
        title: "Usage thresholds",
        intro: "When the period's usage crosses a threshold, an invoice is issued right away for the usage so far; the period-end invoice credits it back.",
        fields: [
          { name: "fixed", label: "Fixed thresholds (one per line: amount then optional label)", type: "textarea",
            value: fixed.map((t) => (t.amount_cents / 100).toFixed(2) + (t.threshold_display_name ? " " + t.threshold_display_name : "")).join("\n"),
            placeholder: "100.00 Starter cap\n500.00" },
          { name: "recurring", label: "Recurring threshold (optional)", type: "money", value: recurring ? (recurring.amount_cents / 100).toFixed(2) : "" },
          { name: "recurring_name", label: "Recurring threshold label", value: recurring ? recurring.threshold_display_name || "" : "" },
        ],
        submitLabel: "Save thresholds",
        onSubmit: async (v) => {
          const items = parseFixed(v.fixed);
          if (v.recurring !== undefined) {
            if (v.recurring <= 0) throw new Error("Recurring threshold must be positive");
            items.push({ amount_cents: v.recurring, recurring: true, threshold_display_name: v.recurring_name });
          }
          await api.put(url, { usage_thresholds: items });
          return items.length ? "Thresholds saved" : "Thresholds cleared";
        },
        after: R.render,
      }),
      reset_thresholds: () => R.confirmAction("Use plan thresholds", "Remove this subscription's own thresholds and fall back to the plan's.", "Reset", async () => {
        await api.put(url, { usage_thresholds: [] });
        return "Subscription now uses the plan thresholds";
      }),
      check_thresholds: async () => {
        const r = await api.post(url + "/check");
        const inv = r.progressive_invoice;
        if (inv) {
          R.toast("Threshold crossed: invoice " + inv.number + " issued", "ok");
          location.hash = "#/invoices/" + inv.lago_id;
        } else {
          R.toast("No threshold crossed yet", "ok");
        }
      },
    });
  };

  // ---- pending payment action (SCA / 3-D Secure) ---------------------------------------
  R.paymentActionPanel = async function (box, invoice) {
    if (!box) return;
    let data;
    try { data = await api.get("/api/v1/invoices/" + R.enc(invoice.lago_id) + "/payment_action"); } catch (e) {
      box.innerHTML = panel("Customer action", '<div class="body"><div class="error-box">Payment status could not be loaded: ' + esc(e.message) + "</div></div>");
      return;
    }
    const a = data.payment_action;
    if (!a) { box.innerHTML = ""; return; }
    const link = a.payment_url
      ? '<a href="' + esc(a.payment_url) + '" target="_blank" rel="noopener noreferrer">' + icon("link") + "Open confirm link</a>"
      : '<span class="help">No confirm link yet. Send the email to generate one.</span>';
    box.innerHTML = panel("Customer action required",
      '<div class="body"><div class="help">The customer\'s bank requires them to authenticate this payment (3-D Secure). The invoice is paid as soon as they confirm.</div>' +
      R.kv([
        ["Status", badge("pending") + " " + esc((a.provider_status || "requires_action").replace(/_/g, " "))],
        ["Action", esc((a.next_action_type || "authentication").replace(/_/g, " "))],
        ["Provider payment", '<span class="mono">' + esc(a.provider_payment_id || "-") + "</span>"],
        ["Confirm link", link],
      ]) + "</div>",
      btn("notify_action", "Email confirm link to customer", "refresh", "primary"));
    R.bindActions(box, {
      notify_action: async () => {
        try {
          const r = await api.post("/api/v1/invoices/" + R.enc(invoice.lago_id) + "/payment_action/notify");
          R.toast("Confirm link emailed to " + r.email_sent_to, "ok");
          R.render();
        } catch (e) { R.toast(e.message, "err"); }
      },
    });
  };

  // ---- payment receipts --------------------------------------------------------------------
  R.receiptsPanel = async function (box, invoice) {
    if (!box) return;
    let rows = [];
    try { rows = (await api.get("/api/v1/payment_receipts?invoice_id=" + R.enc(invoice.lago_id))).payment_receipts || []; } catch (e) {
      box.innerHTML = panel("Payment receipts", '<div class="body"><div class="error-box">Receipts could not be loaded: ' + esc(e.message) + "</div></div>");
      return;
    }
    if (!rows.length) { box.innerHTML = ""; return; }
    box.innerHTML = panel("Payment receipts", R.table([
      { h: "Receipt", f: (r) => esc(r.number), cls: "mono" },
      { h: "Amount", f: (r) => money(r.amount_cents, r.amount_currency), cls: "num" },
      { h: "Issued", f: (r) => datetime(r.created_at) },
    ], rows, {}), btn("receipt_pdf", "Download receipt", "download") + btn("receipt_email", "Email receipt", "refresh"));
    R.bindActions(box, {
      receipt_pdf: () => api.download("/api/v1/invoices/" + R.enc(invoice.lago_id) + "/receipt", rows[rows.length - 1].number + ".pdf"),
      receipt_email: async () => {
        try {
          await api.post("/api/v1/payment_receipts/" + R.enc(invoice.lago_id) + "/resend_email");
          R.toast("Receipt emailed", "ok");
        } catch (e) { R.toast(e.message, "err"); }
      },
    });
  };
})();
