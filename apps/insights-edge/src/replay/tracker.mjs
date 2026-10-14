// The opt-in replay tag, served at /replay.js. Separate from the main /s.js
// tag on purpose: it is not part of that tag's 3 KB budget (test/vitals.test.mjs),
// it is only ever added by a site that turned replay on from its dashboard,
// and it only records anything for the sampled-in fraction of visits.
//
// Order of checks, every one of them before a single byte leaves the page:
//   1. Do Not Track / Global Privacy Control -> nothing happens, ever.
//   2. Ask the Worker whether replay is on for this site (site owners can
//      turn it off from the dashboard at any time; this always reflects
//      that, with no separate cache to go stale).
//   3. Sample: skip this visit unless Math.random() < sample_rate.
//   4. Only then is the ~80 KB rrweb recorder bundle fetched at all.
//
// The recorder's own options come straight from the site's settings:
// maskAllInputs and maskTextSelector are never hard-coded here.
export const REPLAY_JS = `(function(){var s=document.currentScript;if(!s)return;var site=s.getAttribute("data-site");if(!site)return;
if(navigator.doNotTrack==="1"||navigator.doNotTrack==="yes"||(window.doNotTrack==="1")||navigator.globalPrivacyControl===true)return;
var origin=new URL(s.src).origin;
fetch(origin+"/api/replay/config?site="+encodeURIComponent(site)).then(function(r){return r.ok?r.json():null;}).then(function(cfg){
if(!cfg||!cfg.enabled)return;if(Math.random()>=(cfg.sample_rate||0))return;
var sid=(crypto.randomUUID?crypto.randomUUID():(Date.now().toString(36)+Math.random().toString(36).slice(2)));
var seq=0,bytes=0,batch=[],stopped=false,stopFn=null;
var maxMs=cfg.max_session_ms||1800000,maxBytes=cfg.max_bytes||5000000;
function send(body,beacon){
var ok=beacon&&navigator.sendBeacon&&body.length<60000&&navigator.sendBeacon(origin+"/api/replay",new Blob([body],{type:"text/plain"}));
if(!ok)fetch(origin+"/api/replay",{method:"POST",body:body,keepalive:true,headers:{"content-type":"text/plain"}}).catch(function(){});
}
function flush(done){
if(!batch.length&&!done)return;
var payload=JSON.stringify({site:site,session:sid,seq:seq,url:location.pathname+location.search,hostname:location.hostname,events:batch,ended:!!done});
seq+=1;batch=[];send(payload,!done);
}
function stop(){if(stopped)return;stopped=true;if(stopFn)try{stopFn();}catch(e){}clearTimeout(durTimer);clearInterval(flushTimer);flush(true);}
var durTimer=setTimeout(stop,maxMs);
var flushTimer=setInterval(function(){flush(false);},5000);
var rec=document.createElement("script");rec.src=origin+"/replay-recorder.js";
rec.onload=function(){
if(stopped||!window.rrweb||!window.rrweb.record)return;
stopFn=window.rrweb.record({
emit:function(ev){if(stopped)return;batch.push(ev);bytes+=JSON.stringify(ev).length;
if(bytes>=maxBytes){stop();}else if(batch.length>=50){flush(false);}},
maskAllInputs:cfg.mask_all_inputs!==false,
maskTextSelector:cfg.mask_text_selector||"*",
sampling:{scroll:150,input:"last"}
});
};
document.head.appendChild(rec);
addEventListener("pagehide",stop);
addEventListener("visibilitychange",function(){if(document.visibilityState==="hidden")flush(false);});
})["catch"](function(){});
})();`;

// The dashboard player page's own wiring, served at /replay-viewer.js so it
// runs under the private dashboard's strict script-src 'self' CSP (no inline
// <script> there). Reads the events URL off the player container's own data
// attribute instead of being templated per page.
export const REPLAY_VIEWER_JS = `(function(){
var el=document.getElementById("ry-player");if(!el)return;
var url=el.getAttribute("data-events-url");var status=document.getElementById("ry-player-status");
function fail(e){if(status)status.textContent="Could not load this session.";if(window.console&&console.error)console.error("railyard replay viewer:",e);}
if(!url){fail(new Error("no events url"));return;}
fetch(url).then(function(r){if(!r.ok)throw new Error("fetch failed");return r.json();}).then(function(events){
if(!events||!events.length){if(status)status.textContent="No events stored for this session.";return;}
if(status)status.textContent="";
// The UMD bundle exposes the Svelte component as window.rrwebPlayer.Player (or .default on some builds), not the namespace object itself.
var Player=window.rrwebPlayer&&(window.rrwebPlayer.Player||window.rrwebPlayer.default||window.rrwebPlayer);
new Player({target:el,props:{events:events,showController:true,autoPlay:false}});
})["catch"](fail);
})();`;
