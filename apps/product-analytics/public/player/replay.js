/* Recording player page. Reads the recording's lines from the signed URL in
   data-events, keeps the browser window with a full page snapshot and the
   most events, and plays it with the vendored open-source player
   (vendor/rrweb-player, MIT). data-offset starts playback at that point. */
(function () {
  "use strict";
  var body = document.body;
  var stage = document.getElementById("stage");
  var status = document.getElementById("status");
  var state = { ready: false, events: 0, windows: 0, error: null };
  window.__replay = state;

  function say(text) {
    status.textContent = text;
  }

  function pickWindow(lines) {
    var byWindow = {};
    for (var i = 0; i < lines.length; i += 1) {
      var w = lines[i].window_id || "main";
      (byWindow[w] = byWindow[w] || []).push(lines[i].data);
    }
    var best = null;
    Object.keys(byWindow).forEach(function (w) {
      var list = byWindow[w];
      var full = list.some(function (e) { return e && e.type === 2; });
      if (!full) return;
      if (!best || list.length > best.length) best = list;
    });
    state.windows = Object.keys(byWindow).length;
    return best || [];
  }

  fetch(body.getAttribute("data-events"), { credentials: "omit", cache: "no-store" })
    .then(function (res) {
      if (!res.ok) throw new Error(res.status === 403 ? "This link has expired. Open the recording again from the console." : "The recording could not be loaded.");
      return res.text();
    })
    .then(function (text) {
      var lines = [];
      text.split("\n").forEach(function (l) {
        if (!l) return;
        try { lines.push(JSON.parse(l)); } catch (e) { /* skip a damaged line */ }
      });
      var events = pickWindow(lines).filter(function (e) { return e && typeof e.timestamp === "number"; });
      events.sort(function (a, b) { return a.timestamp - b.timestamp; });
      state.events = events.length;
      if (events.length < 2) {
        say("This recording has nothing to play yet.");
        state.ready = true;
        return;
      }
      status.remove();
      var width = Math.max(320, Math.min(window.innerWidth - 16, 1280));
      var height = Math.max(240, window.innerHeight - 96);
      var Player = window.rrwebPlayer && (window.rrwebPlayer.default || window.rrwebPlayer);
      var player = new Player({ target: stage, props: { events: events, width: width, height: height, autoPlay: false, showController: true, skipInactive: true } });
      var offset = Number(body.getAttribute("data-offset") || "0");
      if (offset > 0) player.goto(offset, false);
      state.ready = true;
      state.player = player;
    })
    .catch(function (err) {
      state.error = err.message;
      state.ready = true;
      say(err.message);
    });
})();
