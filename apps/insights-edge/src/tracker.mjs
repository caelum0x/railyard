// The page tag served at /s.js: page views on load and on history changes,
// custom events through window.railyard.track(name, data). No cookies, no
// storage, nothing sent when Do Not Track is on.
export const TRACKER_JS = `(function(){var s=document.currentScript;if(!s)return;var site=s.getAttribute("data-site");if(!site)return;
var endpoint=new URL("/api/send",s.src).href;if(navigator.doNotTrack==="1")return;
function send(name,data){var p={website:site,url:location.pathname+location.search,hostname:location.hostname,referrer:document.referrer||null,title:document.title,language:navigator.language,screen:screen.width+"x"+screen.height};
if(name){p.name=String(name).slice(0,50);if(data)p.data=data;}var body=JSON.stringify({type:"event",payload:p});
if(navigator.sendBeacon&&navigator.sendBeacon(endpoint,new Blob([body],{type:"text/plain"})))return;
fetch(endpoint,{method:"POST",body:body,keepalive:true,headers:{"content-type":"text/plain"}}).catch(function(){});}
var last=null;function page(){var u=location.pathname+location.search;if(u===last)return;last=u;send();}
var ps=history.pushState;history.pushState=function(){ps.apply(this,arguments);page();};addEventListener("popstate",page);
window.railyard={track:send};if(document.readyState==="complete")page();else addEventListener("load",page);})();`;
