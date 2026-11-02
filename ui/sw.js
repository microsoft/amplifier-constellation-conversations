// A private conversation is never cached. The installed app needs a connection.
self.addEventListener('install',()=>self.skipWaiting());
self.addEventListener('activate',event=>event.waitUntil(self.clients.claim()));
self.addEventListener('fetch',event=>{
  if(event.request.mode==='navigate') event.respondWith(fetch(event.request).catch(()=>new Response(
    '<!doctype html><html lang="en"><meta name="viewport" content="width=device-width"><title>Conversations · Offline</title><h1>You’re offline</h1><p>Reconnect to your network or Tailscale, then reload to reach your agents. Messages have not been sent or stored offline.</p><button onclick="location.reload()">Try again</button></html>',
    {status:503,headers:{'Content-Type':'text/html; charset=utf-8','Cache-Control':'no-store'}})));
});
