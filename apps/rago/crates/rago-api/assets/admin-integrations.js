"use strict";
// Rago Billing console — integrations: accounting (Xero, NetSuite), CRM
// (Salesforce, HubSpot), tax (Anrok, Avalara) and single sign-on (Google, Okta,
// Microsoft Entra ID). Connect / authorize / edit / remove connections, fetch
// the provider catalog, map billable metrics, add-ons and document concepts to
// provider items and accounts, link customers to existing provider contacts,
// and the per-resource sync panels used by the invoice, credit note and
// customer pages. Also completes console SSO sign-in (/admin/sso/callback).
(function () {
  const R = window.Rago;
  const { $, esc, datetime, badge, api, enc, table, kv, btn, icon } = R;
  const { head, card, finish } = R.ui;

  // Nav entries render grouped in registration order; this script loads before
  // views2.js registers the rest, so add ours right before boot.
  const boot = R.boot;
  R.boot = function () {
    R.navItem("Operations", "/integrations", "Integrations", "link");
    boot();
  };

  const TYPE_LABEL = { accounting: "Accounting", crm: "CRM", tax: "Tax", auth: "Single sign-on" };
  const STATUS_TEXT = {
    connected: "connected",
    needs_authorization: "needs authorization",
    incomplete: "incomplete",
  };
  function statusBadge(st) {
    const t = STATUS_TEXT[st] || st;
    const cls = st === "connected" ? " ok" : st === "needs_authorization" ? " warn" : " err";
    return '<span class="badge' + cls + '">' + esc(t) + "</span>";
  }

  // Provider catalog: form fields per provider. Secrets are write-only; on
  // edit an empty secret keeps the stored one.
  const PROVIDERS = {
    xero: {
      type: "accounting", label: "Xero", oauth: true,
      intro: "Invoices, credit notes and contacts sync to Xero's Accounting API. After saving you are sent to Xero to authorize access; tokens are refreshed automatically.",
      fields: [
        { name: "client_id", label: "Xero app client ID", help: "Optional. Leave empty to use the server's Xero app (RAGO_XERO_OAUTH_CLIENT_ID)." },
        { name: "client_secret", label: "Xero app client secret", type: "password", secret: true },
        { name: "sync_invoices", label: "Sync invoices automatically when finalized", type: "checkbox" },
        { name: "sync_credit_notes", label: "Sync credit notes automatically", type: "checkbox" },
      ],
    },
    netsuite: {
      type: "accounting", label: "NetSuite", oauth: false,
      intro: "Invoices, credit memos and customers are written through your deployed RESTlet using Token-Based Authentication; the item, account and subsidiary catalog is read with SuiteQL.",
      fields: [
        { name: "account_id", label: "Account ID", required: true, placeholder: "1234567 or 1234567_SB1" },
        { name: "client_id", label: "Consumer key", required: true },
        { name: "client_secret", label: "Consumer secret", type: "password", secret: true, required: true },
        { name: "token_id", label: "Token ID", required: true },
        { name: "token_secret", label: "Token secret", type: "password", secret: true, required: true },
        { name: "script_endpoint_url", label: "RESTlet URL", type: "url", required: true, placeholder: "https://1234567.restlets.api.netsuite.com/app/site/hosting/restlet.nl?script=..&deploy=1" },
        { name: "subsidiary_id", label: "Subsidiary for new customers", help: "Pick from the catalog after the first fetch, or enter the internal ID." },
        { name: "sync_invoices", label: "Sync invoices automatically when finalized", type: "checkbox" },
        { name: "sync_credit_notes", label: "Sync credit notes automatically", type: "checkbox" },
      ],
    },
    salesforce: {
      type: "crm", label: "Salesforce", oauth: true,
      intro: "Customers sync to Salesforce as contacts. After saving you are sent to Salesforce to authorize access.",
      fields: [
        { name: "client_id", label: "Connected app consumer key", help: "Optional. Leave empty to use the server's app (RAGO_SALESFORCE_OAUTH_CLIENT_ID)." },
        { name: "client_secret", label: "Connected app consumer secret", type: "password", secret: true },
        { name: "sandbox", label: "Sandbox org (test.salesforce.com)", type: "checkbox" },
        { name: "login_url", label: "My Domain login URL", type: "url", placeholder: "https://acme.my.salesforce.com" },
      ],
    },
    hubspot: {
      type: "crm", label: "HubSpot", oauth: true,
      intro: "Customers sync to HubSpot as contacts or companies. After saving you are sent to HubSpot to authorize access.",
      fields: [
        { name: "client_id", label: "HubSpot app client ID", help: "Optional. Leave empty to use the server's app (RAGO_HUBSPOT_OAUTH_CLIENT_ID)." },
        { name: "client_secret", label: "HubSpot app client secret", type: "password", secret: true },
        { name: "default_targeted_object", label: "Sync customers as", type: "select", options: [["contacts", "Contacts"], ["companies", "Companies"]] },
      ],
    },
    anrok: {
      type: "tax", label: "Anrok", oauth: false,
      intro: "Invoice taxes are calculated by Anrok with this organization's API key (requires RAGO_TAX_BASE_URL on the server).",
      fields: [{ name: "api_key", label: "API key", type: "password", secret: true, required: true }],
    },
    avalara: {
      type: "tax", label: "Avalara", oauth: false,
      intro: "Invoice taxes are calculated by Avalara with this organization's license key (requires RAGO_TAX_BASE_URL on the server).",
      fields: [
        { name: "account_id", label: "Account ID", required: true },
        { name: "license_key", label: "License key", type: "password", secret: true, required: true },
        { name: "company_code", label: "Company code", required: true },
      ],
    },
    google: {
      type: "auth", label: "Google", oauth: false,
      intro: "Members whose email is on this domain can sign in to the console with Google.",
      fields: [
        { name: "domain", label: "Email domain", required: true, placeholder: "acme.com" },
        { name: "client_id", label: "Google OAuth client ID", help: "Optional. Leave empty to use the server's Google app (RAGO_GOOGLE_CLIENT_ID)." },
        { name: "client_secret", label: "Google OAuth client secret", type: "password", secret: true },
        { name: "enforce_sso", label: "Require SSO (disable password sign-in for members)", type: "checkbox" },
      ],
    },
    okta: {
      type: "auth", label: "Okta", oauth: false,
      intro: "Members whose email is on this domain sign in through your Okta organization.",
      fields: [
        { name: "domain", label: "Email domain", required: true, placeholder: "acme.com" },
        { name: "organization_name", label: "Okta organization", required: true, placeholder: "acme (for acme.okta.com)" },
        { name: "client_id", label: "Client ID", required: true },
        { name: "client_secret", label: "Client secret", type: "password", secret: true, required: true },
        { name: "enforce_sso", label: "Require SSO (disable password sign-in for members)", type: "checkbox" },
      ],
    },
    entra_id: {
      type: "auth", label: "Microsoft Entra ID", oauth: false,
      intro: "Members whose email is on this domain sign in through Microsoft Entra ID (Azure AD).",
      fields: [
        { name: "domain", label: "Email domain", required: true, placeholder: "acme.com" },
        { name: "tenant_id", label: "Directory (tenant) ID", required: true },
        { name: "client_id", label: "Application (client) ID", required: true },
        { name: "client_secret", label: "Client secret", type: "password", secret: true, required: true },
        { name: "enforce_sso", label: "Require SSO (disable password sign-in for members)", type: "checkbox" },
      ],
    },
  };
  const COLLECTION_LABEL = {
    fallback_item: "Fallback item (any unmapped fee)",
    subscription_fee: "Subscription fees",
    minimum_commitment: "Minimum commitment",
    coupon: "Coupons",
    tax: "Taxes",
    prepaid_credit: "Prepaid credits",
    credit_note: "Credit notes",
    account: "Default account",
  };

  async function startAuthorize(id) {
    const r = await api.get("/api/v1/integrations/" + enc(id) + "/oauth/authorize");
    location.href = r.authorize_url;
  }

  // Create or edit a connection.
  function connectionForm(provider, existing) {
    const spec = PROVIDERS[provider];
    const editing = !!existing;
    const settings = (existing && existing.settings) || {};
    const fields = [
      { name: "name", label: "Name", required: true, value: existing ? existing.name : spec.label },
      { name: "code", label: "Code", required: true, value: existing ? existing.code : provider },
    ].concat(spec.fields.map((f) => {
      const copy = Object.assign({}, f);
      if (f.secret) {
        copy.required = f.required && !editing;
        copy.placeholder = editing ? (settings[f.name + "_ref"] ? "stored " + settings[f.name + "_ref"] + " (leave empty to keep)" : "not set") : (f.placeholder || "");
      } else if (f.type === "checkbox") {
        copy.value = !!settings[f.name];
      } else {
        copy.value = settings[f.name] !== undefined && settings[f.name] !== null ? settings[f.name] : (f.type === "select" ? f.options[0][0] : "");
      }
      delete copy.secret;
      return copy;
    }));
    R.openForm({
      title: (editing ? "Edit " : "Connect ") + spec.label,
      intro: spec.intro + " Secrets are encrypted at rest (requires RAGO_ENCRYPTION_KEY) and only their last four characters are shown afterwards.",
      fields,
      submitLabel: editing ? "Save" : (spec.oauth ? "Save and authorize" : "Connect"),
      onSubmit: async (v) => {
        const s = {};
        spec.fields.forEach((f) => {
          const val = v[f.name];
          if (f.type === "checkbox") s[f.name] = !!val;
          else if (f.secret) { if (val) s[f.name] = val; }
          else s[f.name] = val === undefined ? "" : val;
        });
        if (editing) {
          await api.put("/api/v1/integrations/" + enc(existing.lago_id), { integration: { name: v.name, code: v.code, settings: s } });
          R.render();
          return spec.label + " updated";
        }
        const r = await api.post("/api/v1/integrations", { integration: { provider_code: provider, name: v.name, code: v.code, settings: s } });
        const created = r.integration;
        if (spec.oauth) {
          try { await startAuthorize(created.lago_id); return ""; }
          catch (e) {
            location.hash = "#/integrations/" + created.lago_id;
            throw new Error("Saved, but authorization could not start: " + e.message);
          }
        }
        location.hash = "#/integrations/" + created.lago_id;
        return spec.label + " connected";
      },
    });
  }
  R.integrationConnectionForm = connectionForm;

  // ---- list ------------------------------------------------------------------------
  R.route("/integrations", async () => {
    const data = await api.get("/api/v1/integrations");
    const rows = data.integrations || [];
    const view = $("#view");
    const byType = (t) => rows.filter((r) => r.integration_type === t);
    const cols = [
      { h: "Provider", f: (r) => esc(r.provider_name) },
      { h: "Name", f: (r) => esc(r.name) },
      { h: "Code", f: (r) => esc(r.code), cls: "mono" },
      { h: "Status", f: (r) => statusBadge(r.status) },
      { h: "Connected", f: (r) => datetime(r.settings && r.settings.connected_at ? r.settings.connected_at : r.created_at) },
    ];
    const section = (type, providers, empty) => card(TYPE_LABEL[type], table(cols, byType(type), { href: (r) => "/integrations/" + r.lago_id, empty }),
      providers.map((p) => btn("connect_" + p, "Connect " + PROVIDERS[p].label, "plus")).join(""));
    view.innerHTML = head("Integrations", "Accounting, CRM, tax and single sign-on connections for this organization.") +
      section("accounting", ["xero", "netsuite"], "No accounting system connected. Invoices and credit notes are not synced.") +
      section("crm", ["salesforce", "hubspot"], "No CRM connected.") +
      section("tax", ["anrok", "avalara"], "No tax provider connected. Taxes come from the organization's tax rates.") +
      section("auth", ["google", "okta", "entra_id"], "Single sign-on is not enabled. Members sign in with an API key or password.");
    const handlers = {};
    Object.keys(PROVIDERS).forEach((p) => (handlers["connect_" + p] = () => connectionForm(p)));
    finish(view, handlers);
  });

  // ---- detail ----------------------------------------------------------------------
  function itemOptions(items, type, current, blankLabel) {
    const list = items.filter((i) => i.item_type === type);
    const opts = [["", blankLabel]].concat(list.map((i) => [i.external_id, (i.external_name || i.external_id) + (i.external_account_code ? " (" + i.external_account_code + ")" : "") + " - " + i.external_id]));
    if (current && !list.some((i) => i.external_id === current)) opts.push([current, current + " (not in catalog)"]);
    return opts;
  }

  R.route("/integrations/:id", async (p, q) => {
    const base = "/api/v1/integrations/" + enc(p.id);
    const d = await api.get(base);
    const it = d.integration;
    const spec = PROVIDERS[it.provider_code] || { label: it.provider_name, fields: [], oauth: false };
    const accounting = it.integration_type === "accounting";
    const crmOrAcc = accounting || it.integration_type === "crm";
    let metrics = [];
    let addOns = [];
    if (accounting) {
      const [m, a] = await Promise.all([
        api.get("/api/v1/billable_metrics?per_page=100").catch(() => ({ billable_metrics: [] })),
        api.get("/api/v1/add_ons?per_page=100").catch(() => ({ add_ons: [] })),
      ]);
      metrics = m.billable_metrics || [];
      addOns = a.add_ons || [];
    }
    const items = d.items || [];
    const settings = it.settings || {};
    const view = $("#view");

    const settingPairs = Object.keys(settings).filter((k) => k !== "available_tenants").sort().map((k) => [k.replace(/_ref$/, " (stored)").replace(/_/g, " "), '<span class="mono">' + esc(typeof settings[k] === "object" ? JSON.stringify(settings[k]) : settings[k]) + "</span>"]);
    const statusHelp = it.status === "needs_authorization" ? '<div class="body"><div class="error-box" style="padding:0">' + icon("alert") + " Authorize access at " + esc(spec.label) + " to start syncing.</div></div>" :
      it.status === "incomplete" ? '<div class="body"><div class="error-box" style="padding:0">' + icon("alert") + " Missing settings: " + esc((it.missing_settings || []).join(", ")) + "</div></div>" : "";

    let html = head(it.name || spec.label, esc(spec.label) + " " + badge(TYPE_LABEL[it.integration_type] || it.integration_type) + " " + statusBadge(it.status),
      (it.oauth ? btn("authorize", it.has_access_token ? "Reconnect" : "Authorize", "link", it.has_access_token ? "" : "primary") : "") +
      (it.has_refresh_token ? btn("refresh_token", "Refresh token", "refresh") : "") +
      (PROVIDERS[it.provider_code] ? btn("edit", "Edit", "edit") : "") + btn("remove", "Remove", "trash", "danger"),
      '<a href="#/integrations">Integrations</a>') + statusHelp;

    html += '<div class="grid2">' + card("Connection", '<div class="body">' + kv([
      ["Provider", esc(spec.label)], ["Code", '<span class="mono">' + esc(it.code) + "</span>"], ["Status", statusBadge(it.status)],
      ["Token expires", it.oauth ? datetime(it.expires_at) : "-"], ["Created", datetime(it.created_at)],
      ["Integration ID", '<span class="mono">' + esc(it.lago_id) + "</span>"],
    ].concat(it.oauth ? [["OAuth redirect URI", d.oauth_redirect_uri ? '<span class="mono">' + esc(d.oauth_redirect_uri) + "</span>" : '<span class="muted">not configured: set RAGO_PUBLIC_URL on the server</span>']] : [])
      .concat(it.integration_type === "auth" ? [["IdP redirect URI", d.sso_redirect_uri ? '<span class="mono">' + esc(d.sso_redirect_uri) + "</span>" : '<span class="muted">not configured: set RAGO_PUBLIC_URL on the server</span>']] : [])) + "</div>") +
      card("Settings", '<div class="body">' + (settingPairs.length ? kv(settingPairs) : '<span class="muted">No settings.</span>') + "</div>") + "</div>";

    if (it.integration_type === "auth") {
      html += card("Sign-in", '<div class="body">' + kv([
        ["Email domain", esc(settings.domain)],
        ["Password sign-in", settings.enforce_sso ? "Disabled for members (SSO required)" : "Allowed"],
        ["How members sign in", "Open " + esc(location.origin + "/admin") + ", choose <strong>Sign in with SSO</strong> and enter their @" + esc(settings.domain || "domain") + " email."],
      ]) + "</div>");
    }

    if (accounting) {
      const counts = {};
      items.forEach((i) => (counts[i.item_type] = (counts[i.item_type] || 0) + 1));
      html += card("Provider catalog", '<div class="body">' + kv([
        ["Items", esc(counts.item || 0)], ["Accounts", esc(counts.account || 0)],
      ].concat(it.provider_code === "netsuite" ? [["Subsidiaries", esc(counts.subsidiary || 0)]] : [])) +
        (items.length ? "" : '<div class="help" style="margin-top:8px">Fetch the catalog to pick items and accounts from ' + esc(spec.label) + " instead of typing their IDs.</div>") + "</div>",
        btn("catalog", "Refresh catalog", "refresh"));

      const mappingFor = (type, id) => (d.mappings || []).find((m) => m.mappable_id === id && m.mappable_type.replace(/_/g, "").toLowerCase() === type.toLowerCase());
      const objects = metrics.map((m) => ({ type: "BillableMetric", kind: "Billable metric", id: m.lago_id, name: m.name, code: m.code }))
        .concat(addOns.map((a) => ({ type: "AddOn", kind: "Add-on", id: a.lago_id, name: a.name, code: a.code })));
      html += card("Item mappings", table([
        { h: "Type", f: (o) => esc(o.kind) },
        { h: "Name", f: (o) => esc(o.name || o.code) },
        { h: "Code", f: (o) => esc(o.code), cls: "mono" },
        { h: "Provider item", f: (o) => { const m = mappingFor(o.type, o.id); return m ? '<span class="mono">' + esc(m.external_name || m.external_id) + "</span>" : '<span class="muted">uses fallback item</span>'; } },
        { h: "Account", f: (o) => { const m = mappingFor(o.type, o.id); return m && m.external_account_code ? '<span class="mono">' + esc(m.external_account_code) + "</span>" : "-"; } },
        { h: "", f: (o) => { const m = mappingFor(o.type, o.id); return '<button type="button" data-map="' + esc(o.type + "|" + o.id) + '">' + icon("edit") + "Map</button>" + (m ? ' <button type="button" data-unmap="' + esc(m.lago_id) + '">' + icon("trash") + "Clear</button>" : ""); } },
      ], objects, { empty: "No billable metrics or add-ons yet." }));

      const coll = (type) => (d.collection_mappings || []).find((m) => m.mapping_type === type);
      html += card("Document mappings", table([
        { h: "Concept", f: (t) => esc(COLLECTION_LABEL[t] || t) },
        { h: "Provider item", f: (t) => { const m = coll(t); return m && m.external_id ? '<span class="mono">' + esc(m.external_name || m.external_id) + "</span>" : '<span class="muted">not set</span>'; } },
        { h: "Account", f: (t) => { const m = coll(t); return m && m.external_account_code ? '<span class="mono">' + esc(m.external_account_code) + "</span>" : "-"; } },
        { h: "", f: (t) => { const m = coll(t); return '<button type="button" data-coll="' + esc(t) + '">' + icon("edit") + "Set</button>" + (m ? ' <button type="button" data-uncoll="' + esc(m.lago_id) + '">' + icon("trash") + "Clear</button>" : ""); } },
      ], d.collection_mapping_types || []));
    }

    if (crmOrAcc) {
      html += card("Linked customers", table([
        { h: "Customer", f: (c) => '<a href="#/customers/' + esc(enc(c.external_customer_id)) + '">' + esc(c.customer_name || c.external_customer_id) + "</a>" },
        { h: "External ID", f: (c) => esc(c.external_customer_id), cls: "mono" },
        { h: spec.label + " ID", f: (c) => esc(c.provider_customer_id), cls: "mono" },
        { h: "", f: (c) => '<button type="button" data-unlink="' + esc(c.lago_id) + '">' + icon("trash") + "Unlink</button>" },
      ], d.customers || [], { empty: "No customers linked yet. Customers are created at " + spec.label + " on their first sync, or link an existing record." }),
        btn("link_customer", "Link customer", "link"));
    }

    view.innerHTML = html;
    if (q.connected) R.toast(spec.label + " authorized", "ok");

    R.$$("[data-map]", view).forEach((b) => (b.onclick = () => {
      const [type, id] = b.dataset.map.split("|");
      const m = (d.mappings || []).find((x) => x.mappable_id === id && x.mappable_type.replace(/_/g, "").toLowerCase() === type.toLowerCase()) || {};
      const hasCatalog = items.some((i) => i.item_type === "item");
      R.openForm({
        title: "Map to a " + spec.label + " item",
        intro: hasCatalog ? "Pick the item fees for this object are booked to." : "The catalog is empty: enter the " + spec.label + (it.provider_code === "xero" ? " item code." : " item internal ID.") + " Use Refresh catalog to pick from a list.",
        fields: [
          hasCatalog ? { name: "external_id", label: "Item", type: "select", required: true, value: m.external_id, options: itemOptions(items, "item", m.external_id, "Select an item") }
            : { name: "external_id", label: "Item ID", required: true, value: m.external_id },
          items.some((i) => i.item_type === "account")
            ? { name: "external_account_code", label: "Account", type: "select", value: m.external_account_code, options: [["", "Item default"]].concat(items.filter((i) => i.item_type === "account").map((i) => [i.external_account_code || i.external_id, (i.external_name || i.external_id) + (i.external_account_code ? " (" + i.external_account_code + ")" : "")])) }
            : { name: "external_account_code", label: "Account code", value: m.external_account_code },
        ],
        submitLabel: "Save mapping",
        onSubmit: async (v) => {
          const picked = items.find((i) => i.item_type === "item" && i.external_id === v.external_id);
          await api.post(base + "/mappings", { mapping: { mappable_type: type, mappable_id: id, external_id: v.external_id, external_account_code: v.external_account_code || "", external_name: picked ? picked.external_name || "" : "" } });
          return "Mapping saved";
        },
        after: R.render,
      });
    }));
    R.$$("[data-unmap]", view).forEach((b) => (b.onclick = () => R.confirmAction("Clear mapping", "Fees for this object will use the fallback item.", "Clear", async () => {
      await api.del(base + "/mappings/" + enc(b.dataset.unmap));
      return "Mapping cleared";
    })));
    R.$$("[data-coll]", view).forEach((b) => (b.onclick = () => {
      const t = b.dataset.coll;
      const m = (d.collection_mappings || []).find((x) => x.mapping_type === t) || {};
      const hasItems = items.some((i) => i.item_type === "item");
      const hasAccounts = items.some((i) => i.item_type === "account");
      R.openForm({
        title: COLLECTION_LABEL[t] || t,
        intro: "Set the item and/or account used for this concept on synced documents.",
        fields: [
          hasItems ? { name: "external_id", label: "Item", type: "select", value: m.external_id, options: itemOptions(items, "item", m.external_id, "None") }
            : { name: "external_id", label: "Item ID", value: m.external_id },
          hasAccounts ? { name: "external_account_code", label: "Account", type: "select", value: m.external_account_code, options: [["", "None"]].concat(items.filter((i) => i.item_type === "account").map((i) => [i.external_account_code || i.external_id, (i.external_name || i.external_id) + (i.external_account_code ? " (" + i.external_account_code + ")" : "")])) }
            : { name: "external_account_code", label: "Account code", value: m.external_account_code },
        ],
        submitLabel: "Save",
        onSubmit: async (v) => {
          if (!v.external_id && !v.external_account_code) throw new Error("Choose an item or an account.");
          const picked = items.find((i) => i.item_type === "item" && i.external_id === v.external_id);
          await api.post(base + "/collection_mappings", { collection_mapping: { mapping_type: t, external_id: v.external_id || "", external_account_code: v.external_account_code || "", external_name: picked ? picked.external_name || "" : "" } });
          return "Mapping saved";
        },
        after: R.render,
      });
    }));
    R.$$("[data-uncoll]", view).forEach((b) => (b.onclick = () => R.confirmAction("Clear mapping", "Remove this document mapping?", "Clear", async () => {
      await api.del(base + "/collection_mappings/" + enc(b.dataset.uncoll));
      return "Mapping cleared";
    })));
    R.$$("[data-unlink]", view).forEach((b) => (b.onclick = () => R.confirmAction("Unlink customer", "The next sync creates a new " + spec.label + " record for this customer unless you link another one.", "Unlink", async () => {
      await api.del(base + "/customers/" + enc(b.dataset.unlink));
      return "Customer unlinked";
    })));

    finish(view, {
      authorize: () => startAuthorize(it.lago_id),
      refresh_token: async () => { await api.post(base + "/refresh"); R.toast("Token refreshed", "ok"); R.render(); },
      edit: () => connectionForm(it.provider_code, it),
      remove: () => R.confirmAction("Remove integration", "Disconnect " + (it.name || spec.label) + "? Syncs, tax calculation and sign-in through it stop immediately.", "Remove", async () => {
        await api.del(base);
        location.hash = "#/integrations";
        return "Integration removed";
      }),
      catalog: async () => {
        const r = await api.post(base + "/catalog");
        R.toast("Catalog refreshed: " + (r.catalog || []).map((c) => c.count + " " + c.item_type + (c.count === 1 ? "" : "s")).join(", "), "ok");
        R.render();
      },
      link_customer: () => R.openForm({
        title: "Link customer to " + spec.label,
        intro: "Use an existing " + spec.label + " record for this customer instead of creating one on the next sync.",
        fields: [
          { name: "external_customer_id", label: "Customer external ID", required: true },
          { name: "provider_customer_id", label: spec.label + " record ID", required: true },
        ],
        submitLabel: "Link",
        onSubmit: async (v) => {
          await api.post(base + "/customers", { integration_customer: v });
          return "Customer linked";
        },
        after: R.render,
      }),
    });
  });

  // ---- sync panels for invoices, credit notes and customers ---------------------------
  // kind: "invoice" | "credit_note" | "customer"; obj: the resource JSON;
  // el: the container to fill.
  R.integrationPanel = async function (kind, obj, el) {
    if (!el) return;
    const lagoId = obj.lago_id;
    let st;
    try {
      st = await api.get("/api/v1/integration_sync_status?resource_type=" + enc(kind) + "&lago_id=" + enc(lagoId));
    } catch (e) {
      el.innerHTML = card("Integrations", '<div class="body muted">Integration status could not be loaded: ' + esc(e.message) + "</div>");
      return;
    }
    const acc = st.integrations.accounting;
    const crm = st.integrations.crm;
    const tax = st.integrations.tax;
    const ready = (i) => i && i.status === "connected";
    const records = st.sync_records || [];
    let actions = "";
    const finalized = obj.status === "finalized";
    if (kind === "invoice") {
      if (ready(acc) && finalized) actions += btn("int_sync", (records.some((r) => r.integration_type === "accounting") ? "Re-sync to " : "Sync to ") + acc.provider_name, "refresh");
      if (ready(tax) && obj.status === "draft") actions += btn("int_tax", "Apply " + tax.provider_name + " taxes", "taxes");
      if (ready(tax) && obj.status === "voided") actions += btn("int_void", "Retry " + tax.provider_name + " voiding", "retry");
    } else if (kind === "credit_note") {
      if (ready(acc)) actions += btn("int_sync", "Sync to " + acc.provider_name, "refresh");
      if (ready(tax)) actions += btn("int_report", "Report to " + tax.provider_name, "retry");
    } else if (kind === "customer") {
      if (ready(crm)) actions += btn("int_crm", "Sync to " + crm.provider_name, "refresh");
    }
    const connectedList = [["Accounting", acc], ["CRM", crm], ["Tax", tax]]
      .map(([l, i]) => [l, i ? '<a href="#/integrations/' + esc(i.lago_id) + '">' + esc(i.provider_name) + "</a> " + statusBadge(i.status) : '<span class="muted">not connected</span> <a href="#/integrations">Connect</a>']);
    const rows = records.map((r) => ({ label: r.provider_name, id: r.external_id, at: r.synced_at }))
      .concat((st.customer_links || []).filter((l) => !records.some((r) => r.external_id === l.provider_customer_id))
        .map((l) => ({ label: l.provider_name + " (linked)", id: l.provider_customer_id, at: null })));
    el.innerHTML = card("Integrations", '<div class="body">' + kv(connectedList) + "</div>" + table([
      { h: "Synced to", f: (r) => esc(r.label) },
      { h: "Provider ID", f: (r) => esc(r.id), cls: "mono" },
      { h: "When", f: (r) => datetime(r.at) },
    ], rows, { empty: kind === "customer" ? "Not synced to a CRM or accounting system yet." : "Not synced yet." }), actions);
    const invBase = "/api/v1/invoices/" + enc(lagoId);
    R.bindActions(el, {
      int_sync: async () => {
        const path = kind === "invoice" ? invBase + "/sync_accounting" : "/api/v1/credit_notes/" + enc(lagoId) + "/sync_accounting";
        const r = await api.post(path);
        const rec = r.integration_invoice || r.integration_credit_note || {};
        R.toast("Synced to " + acc.provider_name + " as " + rec.external_id, "ok");
        R.render();
      },
      int_tax: async () => {
        await api.post(invBase + "/apply_provider_taxes?provider=" + enc(tax.provider_code));
        R.toast(tax.provider_name + " taxes applied", "ok");
        R.render();
      },
      int_void: async () => {
        await api.graphql("mutation($id: ID!) { retryTaxProviderVoiding(id: $id) { id } }", { id: lagoId });
        R.toast("Voiding queued at " + tax.provider_name, "ok");
      },
      int_report: async () => {
        await api.graphql("mutation($id: ID!) { retryTaxReporting(id: $id) { id } }", { id: lagoId });
        R.toast("Credit note reporting queued at " + tax.provider_name, "ok");
      },
      int_crm: async () => {
        const r = await api.post("/api/v1/customers/" + enc(obj.external_id) + "/sync_crm");
        R.toast("Synced to " + crm.provider_name + " as " + r.integration_customer.external_id, "ok");
        R.render();
      },
    });
  };

  // ---- console SSO sign-in ----------------------------------------------------------------
  function bindSsoGate() {
    const form = $("#sso-form");
    if (!form) return;
    form.onsubmit = async (e) => {
      e.preventDefault();
      const email = $("#sso-email").value.trim();
      const errBox = $("#sso-error");
      errBox.textContent = "";
      if (!email || email.indexOf("@") < 1) { errBox.textContent = "Enter your work email."; return; }
      const b = $('button[type="submit"]', form);
      b.disabled = true;
      try {
        const res = await fetch("/auth/sso/authorize", { method: "POST", headers: { "Content-Type": "application/json", "Accept": "application/json" }, body: JSON.stringify({ email }) });
        const body = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(ssoMessage(body));
        location.href = body.authorize_url;
      } catch (err) {
        errBox.textContent = err.message;
        b.disabled = false;
      }
    };
  }
  function ssoMessage(body) {
    const raw = body && (body.error_details ? Object.values(body.error_details).flat().join(", ") : body.code || body.error) || "Sign-in failed";
    if (/sso_not_enabled_for_domain/.test(raw)) return "Single sign-on is not enabled for this email domain.";
    if (/sso_not_enabled_for_user|user_not_found/.test(raw)) return "This account is not a member of an organization that uses this sign-in method.";
    if (/idp_not_configured/.test(raw)) return "Single sign-on is not fully configured: " + raw.replace(/.*idp_not_configured:\s*/, "");
    if (/sso_state_invalid/.test(raw)) return "The sign-in link expired. Start again.";
    return raw;
  }
  async function completeSsoCallback() {
    if (location.pathname !== "/admin/sso/callback") return;
    const params = new URLSearchParams(location.search);
    const gateErr = $("#sso-error") || $("#gate-error");
    const fail = (msg) => {
      history.replaceState(null, "", "/admin");
      if (gateErr) gateErr.textContent = msg;
    };
    if (params.get("error")) { fail("Sign-in was cancelled: " + (params.get("error_description") || params.get("error"))); return; }
    const code = params.get("code");
    const state = params.get("state");
    if (!code || !state) { fail("The sign-in response is incomplete. Start again."); return; }
    try {
      const res = await fetch("/auth/sso/callback", { method: "POST", headers: { "Content-Type": "application/json", "Accept": "application/json" }, body: JSON.stringify({ code, state }) });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(ssoMessage(body));
      R.setKey(body.token);
      location.replace("/admin#/overview");
    } catch (err) {
      fail(err.message);
    }
  }
  bindSsoGate();
  completeSsoCallback();
})();
