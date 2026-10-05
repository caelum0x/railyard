// The page tag served at /s.js: page views on load and on history changes,
// custom events through window.railyard.track(name, data), and
// window.railyard.identify(userId, traits) for a signed-in person (PY3.2; kept in
// memory for the page only, so call it on each load). No cookies, no storage,
// nothing sent when Do Not Track is on.
//
// Page speed (QY1.2, P-Y12): each page view's LCP, INP and CLS (and FCP and
// TTFB) are sent once, as one `performance` event, when the page is hidden or
// left, or before a client-side route change. The numbers follow the metric
// definitions of GoogleChrome/web-vitals (Apache-2.0, see NOTICE) with the two
// corrections @railyard/insights-web makes so they match Chrome's own field
// reporting: LCP stops at the first input, and INP skips one interaction per
// 50 of the page's total. `data-vitals="off"` on the script tag turns it off.
// Budget: 3 KB gzip for the whole tag (test/vitals.test.mjs).
export const TRACKER_JS = `(function(){var s=document.currentScript;if(!s)return;var site=s.getAttribute("data-site");if(!site)return;
var endpoint=new URL("/api/send",s.src).href;if(navigator.doNotTrack==="1")return;
var user=null,last=null,E=[],done=0,P=typeof PerformanceObserver=="function"&&s.getAttribute("data-vitals")!=="off"?PerformanceObserver:null;
function base(u){var p={website:site,url:u||location.pathname+location.search,hostname:location.hostname,referrer:document.referrer||null,title:document.title,language:navigator.language,screen:screen.width+"x"+screen.height};if(user)p.user=user;return p;}
function post(type,p){var body=JSON.stringify({type:type,payload:p});
if(navigator.sendBeacon&&navigator.sendBeacon(endpoint,new Blob([body],{type:"text/plain"})))return;
fetch(endpoint,{method:"POST",body:body,keepalive:true,headers:{"content-type":"text/plain"}}).catch(function(){});}
function send(name,data,type){var p=base();if(type==="identify"){if(data)p.data=data;}else if(name){p.name=String(name).slice(0,50);if(data)p.data=data;}post(type||"event",p);}
function vitals(){if(done||!P)return;done=1;var v={},nav,act=0,first=1/0,lcp,ix={},d=[],w=0,wf,wl,cls,inp=/^(pointer(down|up)|mousedown|click|key(down|up)|touchstart)$/,
r=function(x,k){k=Math.pow(10,k||0);return Math.round(Math.max(x,0)*k)/k;},es=E.slice().sort(function(a,b){return a.startTime-b.startTime;});
es.forEach(function(e){var t=e.entryType,ev=t==="event"||t==="first-input";if(t==="navigation"&&!nav){nav=e;act=e.activationStart||0;}
if(ev&&inp.test(e.name)&&e.startTime<first)first=e.startTime;if(ev&&e.interactionId)ix[e.interactionId]=Math.max(ix[e.interactionId]||0,e.duration);});
if(nav&&typeof nav.responseStart=="number")v.ttfb=r(nav.responseStart-act);
es.forEach(function(e){var t=e.entryType;if(t==="paint"&&e.name==="first-contentful-paint"&&v.fcp==null)v.fcp=r(e.startTime-act);
if(t==="largest-contentful-paint"&&e.startTime<first)lcp=e;
if(t==="layout-shift"&&!e.hadRecentInput){if(wf&&e.startTime-wl.startTime<1e3&&e.startTime-wf.startTime<5e3){w+=e.value||0;}else{w=e.value||0;wf=e;}wl=e;cls=Math.max(cls||0,w);}});
if(lcp)v.lcp=r(lcp.startTime-act);
if(cls!=null)v.cls=r(cls,4);else if((P.supportedEntryTypes||[]).indexOf("layout-shift")>=0)v.cls=0;
for(var k in ix)d.push(ix[k]);d.sort(function(a,b){return b-a;});
if(d.length){var n=Math.max(performance.interactionCount||0,d.length);v.inp=r(d[Math.min(Math.floor(n/50),d.length-1)]);}
for(k in v)return post("performance",Object.assign(base(last),v));}
var page=function(){var u=location.pathname+location.search;if(u===last)return;if(last!==null){vitals();E=[];done=0;}last=u;send();};
var ps=history.pushState;history.pushState=function(){ps.apply(this,arguments);page();};addEventListener("popstate",page);
if(P)["navigation","paint","largest-contentful-paint","layout-shift","event","first-input"].forEach(function(t){try{new P(function(l){E=E.concat(l.getEntries());}).observe(t==="event"?{type:t,buffered:true,durationThreshold:40}:{type:t,buffered:true});}catch(x){}});
addEventListener("visibilitychange",function(){if(document.visibilityState==="hidden")vitals();});addEventListener("pagehide",vitals);
function identify(id,traits){if(typeof id!=="string"||!/^[A-Za-z0-9@._:+=-]{1,50}$/.test(id))return;user=id;send(null,traits,"identify");}
window.railyard={track:function(n,d){send(n,d);},identify:identify,vitals:vitals};if(document.readyState==="complete")page();else addEventListener("load",page);})();`;
