(function(){var d=document,live=d.getElementById("live"),keep=d.getElementById("keep-vars");
if(keep){keep.addEventListener("change",function(){d.body.classList.toggle("keep-vars",keep.checked)})}
function fallback(el){var det=el.closest("details");if(det)det.open=true;var r=d.createRange();r.selectNodeContents(el);var s=getSelection();s.removeAllRanges();s.addRange(r);try{return d.execCommand("copy")}catch(e){return false}}
d.addEventListener("click",function(e){var b=e.target&&e.target.closest?e.target.closest("button[data-copy]"):null;if(!b)return;
var id=b.getAttribute("data-copy"),raw=d.body.classList.contains("keep-vars");
var el=(raw&&d.getElementById(id+"-curl-raw"))||d.getElementById(id+"-curl");if(!el)return;
function done(ok){b.textContent=ok?"Copied":"Press Ctrl+C to copy";if(live)live.textContent=ok?"cURL copied to clipboard":"";setTimeout(function(){b.textContent="Copy as cURL"},1600)}
if(navigator.clipboard&&navigator.clipboard.writeText){navigator.clipboard.writeText(el.textContent).then(function(){done(true)},function(){done(fallback(el))})}else{done(fallback(el))}})})();