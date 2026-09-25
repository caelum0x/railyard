"use strict";
// Rago Billing console — API keys: list the organization's live keys, create,
// rotate (expire + replace) and delete them through the GraphQL mutations
// (apiKeys, createApiKey, rotateApiKey, destroyApiKey). A new key value is
// shown once in a dialog; the list shows keys masked.
(function () {
  const R = window.Rago;
  const { $, esc, datetime, badge, api, table, btn, icon } = R;

  // Register next to the other Operations entries (views2.js registers the
  // group; this script loads before it, so add ours right before boot).
  const boot = R.boot;
  R.boot = function () {
    R.navItem("Operations", "/api_keys", "API keys", "key");
    boot();
  };

  const LIST = "query { apiKeys { id name value createdAt } }";
  const CREATE = "mutation($input: CreateApiKeyInput!) { createApiKey(input: $input) { id name value createdAt } }";
  const ROTATE = "mutation($id: ID!) { rotateApiKey(id: $id) { id name value createdAt } }";
  const DESTROY = "mutation($id: ID!) { destroyApiKey(id: $id) }";

  function mask(value) {
    const v = String(value || "");
    if (v.length <= 12) return "********";
    return v.slice(0, 5) + "..." + v.slice(-4);
  }

  // Show a freshly minted key once, with a copy button.
  function showNewKey(title, key, note) {
    const root = $("#overlay-root");
    root.innerHTML =
      '<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="modal-title"><div class="modal">' +
      '<header><h3 id="modal-title">' + esc(title) + '</h3><button type="button" class="ghost" data-close aria-label="Close">' + icon("close") + "</button></header>" +
      '<div class="content"><div class="help">Copy this key now and store it in the consumer\'s secret store. Anyone holding it has full API access to this organization.' +
      (note ? " " + esc(note) : "") + "</div>" +
      '<pre class="json" id="new-key-value">' + esc(key.value) + "</pre>" +
      '<div class="form-error" role="status" id="copy-status"></div></div>' +
      '<footer><button type="button" data-copy class="primary">Copy key</button><button type="button" data-close>Done</button></footer>' +
      "</div></div>";
    R.$$("[data-close]", root).forEach((b) => (b.onclick = () => { R.closeOverlay(); R.render(); }));
    $("[data-copy]", root).onclick = async () => {
      const status = $("#copy-status", root);
      try {
        await navigator.clipboard.writeText(key.value);
        status.textContent = "Copied to clipboard.";
      } catch (_) {
        status.textContent = "Copy failed. Select the key above and copy it manually.";
      }
    };
  }

  R.route("/api_keys", async () => {
    const data = await api.graphql(LIST);
    const keys = data.apiKeys || [];
    const current = R.getKey();
    const view = $("#view");
    const rows = keys.map((k) => Object.assign({}, k, { inUse: k.value === current }));
    view.innerHTML = R.ui.head(
      "API keys",
      "Bearer keys for the REST and GraphQL APIs (<code>Authorization: Bearer &lt;key&gt;</code>). Every key has full access to this organization. Rotating a key expires it immediately and issues a replacement.",
      btn("new", "Create key", "plus", "primary"),
    ) + R.ui.card("Active keys", table([
      { h: "Name", f: (r) => esc(r.name || "Unnamed") + (r.inUse ? " " + badge("this console") : "") },
      { h: "Key", f: (r) => esc(mask(r.value)), cls: "mono" },
      { h: "Created", f: (r) => datetime(r.createdAt) },
      { h: "", f: (r) => '<div class="actions"><button type="button" data-rotate="' + esc(r.id) + '">' + icon("retry") + 'Rotate</button><button type="button" data-del="' + esc(r.id) + '">' + icon("trash") + "Delete</button></div>" },
    ], rows, { empty: "No active keys. Create one to call the API." }));

    const byId = Object.fromEntries(rows.map((r) => [r.id, r]));

    R.$$("[data-rotate]", view).forEach((b) => (b.onclick = () => {
      const k = byId[b.dataset.rotate];
      const warn = k.inUse ? " This console is signed in with it and will switch to the new key." : "";
      R.openForm({
        title: "Rotate key",
        intro: "Expire \"" + (k.name || "Unnamed") + "\" now and issue a replacement with the same name. Services still using the old key will get 401 until they are updated." + warn,
        fields: [],
        submitLabel: "Rotate",
        danger: true,
        onSubmit: async () => {
          const res = await api.graphql(ROTATE, { id: k.id });
          const fresh = res.rotateApiKey;
          if (k.inUse) R.setKey(fresh.value);
          setTimeout(() => showNewKey("Key rotated", fresh, k.inUse ? "This console now uses the new key." : ""), 0);
          return "";
        },
      });
    }));

    R.$$("[data-del]", view).forEach((b) => (b.onclick = () => {
      const k = byId[b.dataset.del];
      if (rows.length === 1) {
        R.toast("This is the organization's last active key. Create another key before deleting it.", "err");
        return;
      }
      const warn = k.inUse ? " This console is signed in with it and will sign out." : "";
      R.confirmAction(
        "Delete key",
        "Revoke \"" + (k.name || "Unnamed") + "\" (" + mask(k.value) + ") now. Requests using it will get 401." + warn,
        "Delete",
        async () => {
          await api.graphql(DESTROY, { id: k.id });
          if (k.inUse) setTimeout(() => $("#logout").click(), 0);
          return "Key deleted";
        },
      );
    }));

    R.ui.finish(view, {
      new: () => R.openForm({
        title: "Create API key",
        intro: "Name the key after the service that will use it (for example relay or autogtm-web) so it can be rotated later.",
        fields: [{ name: "name", label: "Name", required: true, placeholder: "relay" }],
        submitLabel: "Create key",
        onSubmit: async (v) => {
          const name = String(v.name || "").trim();
          if (!name) throw new Error("Enter a name for the key.");
          if (name.length > 255) throw new Error("Keep the name under 255 characters.");
          const res = await api.graphql(CREATE, { input: { name } });
          setTimeout(() => showNewKey("Key created", res.createApiKey), 0);
          return "";
        },
      }),
    });
  });
})();
