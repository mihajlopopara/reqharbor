(function(){
// Proof of work for the login form (06-go-server.md §5.9): find a decimal nonce such that
// SHA-256(challenge || nonce) has d leading zero bits. SHA-256 is computed here in plain JS rather
// than with WebCrypto, whose digest exists only in secure contexts, because an http PUBLIC_URL
// on a LAN address is supported. The message is at most 36 bytes, so one block suffices.
var f=document.querySelector("form[data-challenge]");if(!f)return;
var o=f.querySelector("input[name=pow]"),btn=f.querySelector("button"),st=document.getElementById("pow-status");
function say(t){if(st)st.textContent=t}
if(btn)btn.disabled=true;
try{
var c=f.getAttribute("data-challenge"),d=+f.getAttribute("data-difficulty");
var raw=atob(c.replace(/-/g,"+").replace(/_/g,"/")),cb=[];
for(var i=0;i<raw.length;i++)cb.push(raw.charCodeAt(i)&255);
if(!o||!(d>0)||cb.length>35)throw new Error("bad challenge");
var K=[0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
var H=[0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
var W=[];
// lz returns the leading zero bits of SHA-256(m), m an array of at most 55 byte values.
var lz=function(m){
var i,x,y,n=m.length;
for(i=0;i<16;i++)W[i]=0;
for(i=0;i<n;i++)W[i>>2]|=m[i]<<(24-8*(i&3));
W[n>>2]|=0x80<<(24-8*(n&3));
W[15]=n*8;
for(i=16;i<64;i++){x=W[i-15];y=W[i-2];
W[i]=(W[i-16]+(((x>>>7)|(x<<25))^((x>>>18)|(x<<14))^(x>>>3))+W[i-7]+(((y>>>17)|(y<<15))^((y>>>19)|(y<<13))^(y>>>10)))|0}
var a=H[0],b=H[1],cc=H[2],dd=H[3],e=H[4],ff=H[5],g=H[6],h=H[7],t1,t2;
for(i=0;i<64;i++){
t1=(h+(((e>>>6)|(e<<26))^((e>>>11)|(e<<21))^((e>>>25)|(e<<7)))+((e&ff)^(~e&g))+K[i]+W[i])|0;
t2=((((a>>>2)|(a<<30))^((a>>>13)|(a<<19))^((a>>>22)|(a<<10)))+((a&b)^(a&cc)^(b&cc)))|0;
h=g;g=ff;ff=e;e=(dd+t1)|0;dd=cc;cc=b;b=a;a=(t1+t2)|0}
var out=[a,b,cc,dd,e,ff,g,h],z=0;
for(i=0;i<8;i++){x=(out[i]+H[i])>>>0;if(x===0){z+=32;continue}while(!(x&0x80000000)){z++;x=(x<<1)>>>0}break}
return z};
var k=0,m=cb.slice();
var step=function(){
var t0=Date.now();
while(Date.now()-t0<50){
for(var j=0;j<4096;j++,k++){
var ns=String(k);m.length=cb.length;
for(var q=0;q<ns.length;q++)m.push(ns.charCodeAt(q));
if(lz(m)>=d){o.value=c+"."+ns;if(btn)btn.disabled=false;say("Ready. Enter your password to continue.");return}}}
setTimeout(step,0)};
setTimeout(step,0);
}catch(err){say("This browser could not finish the check, so signing in is paused. Try another browser.")}
})();
