/* Click map page. The page named in data-page loads in a frame (its scripts
   stay off unless the viewer turns them on, so viewing does not count as a
   visit), and the clicks from data-data are drawn on top with the vendored
   heat layer (vendor/simpleheat, BSD-2-Clause). Scroll depth is drawn as a
   line at each 10% of the page, labelled with the share of views that
   reached it. */
(function () {
  "use strict";
  var body = document.body;
  var stage = document.getElementById("stage");
  var status = document.getElementById("status");
  var state = { ready: false, points: 0, total: 0, error: null };
  window.__heatmap = state;
  var width = Number(body.getAttribute("data-width") || "1280");

  function el(tag, attrs, text) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) { e.setAttribute(k, attrs[k]); });
    if (text) e.textContent = text;
    return e;
  }

  fetch(body.getAttribute("data-data"), { credentials: "omit", cache: "no-store" })
    .then(function (res) {
      if (!res.ok) throw new Error(res.status === 403 ? "This link has expired. Open the map again from the console." : "The map could not be loaded.");
      return res.json();
    })
    .then(function (data) {
      status.remove();
      var height = Math.max(800, data.scroll && data.scroll.page_height ? data.scroll.page_height : 0);
      var column = el("div", {});
      var bar = el("div", { class: "heat-bar" });
      var info = el("span", {}, data.total + " interactions on this page. " + (data.scroll ? data.scroll.views + " page views with scroll depth. " : "") + "A page that does not allow framing stays blank here.");
      var toggle = el("button", { type: "button", id: "run-scripts" }, "Run the page's scripts");
      bar.appendChild(info);
      bar.appendChild(toggle);
      var wrap = el("div", { class: "heat-wrap", style: "width:" + width + "px;height:" + height + "px" });
      var frame = el("iframe", { src: body.getAttribute("data-page"), width: String(width), height: String(height), sandbox: "allow-same-origin", referrerpolicy: "no-referrer", title: "Page" });
      var canvas = el("canvas", { width: String(width), height: String(height), id: "heat" });
      wrap.appendChild(frame);
      wrap.appendChild(canvas);
      (data.scroll ? data.scroll.bands : []).forEach(function (b) {
        if (!data.scroll.views) return;
        var line = el("div", { class: "scroll-band", style: "top:" + Math.round((b.percent / 100) * height) + "px" });
        line.appendChild(el("span", {}, Math.round(b.share * 100) + "% reached " + b.percent + "%"));
        wrap.appendChild(line);
      });
      column.appendChild(bar);
      column.appendChild(wrap);
      stage.appendChild(column);
      toggle.addEventListener("click", function () {
        frame.setAttribute("sandbox", "allow-same-origin allow-scripts");
        frame.src = body.getAttribute("data-page");
        toggle.disabled = true;
      });
      var max = 1;
      var pts = data.points.map(function (p) {
        if (p.count > max) max = p.count;
        return [Math.round(p.x * width), p.y, p.count];
      });
      var heat = window.simpleheat(canvas);
      heat.data(pts).max(max).radius(18, 14);
      heat.draw(0.05);
      state.points = pts.length;
      state.total = data.total;
      state.ready = true;
    })
    .catch(function (err) {
      state.error = err.message;
      state.ready = true;
      status.textContent = err.message;
    });
})();
