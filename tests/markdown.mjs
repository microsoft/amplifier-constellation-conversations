import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const ui = path.join(process.cwd(), 'ui');
const script = await fs.readFile(path.join(ui, 'markdown.js'), 'utf8') + await fs.readFile(path.join(ui, 'markdown-renderer.js'), 'utf8');
const css = await fs.readFile(path.join(ui, 'markdown.css'), 'utf8');
const companionCards = await fs.readFile(path.join(ui, 'companion-cards.mjs'), 'utf8');
const server = http.createServer((req,res) => {
  if(req.url === '/markdown.js') {res.setHeader('Content-Type','text/javascript');res.end(script);}
  else if(req.url === '/companion-cards.mjs') {res.setHeader('Content-Type','text/javascript');res.end(companionCards);}
  else res.end(`<style>body{margin:12px}.markdown{max-width:100%}${css}</style><main class="markdown"></main><script src="/markdown.js"></script>`);
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true,args:['--no-sandbox']});
try {
  const page=await browser.newPage({viewport:{width:390,height:844}});
  const external=[]; page.on('request', r=>{if(!r.url().startsWith('http://127.0.0.1:'))external.push(r.url());});
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  const sample = '# Context\n\n| Connection | Update |\n|---|---|\n| **Slack** | [Thread](https://example.com/thread) |\n\n- First\n  - Nested *detail*\n\n1. Next\n2. Then\n\n> Quoted\n\n- [x] Checked\n\n```js\nconst example = "<script>";\n'+'a'.repeat(250)+'\n```\n\n![Image](https://example.com/tracker.png)\n\n<script>globalThis.compromised=true</script>\n<img src=x onerror="globalThis.compromised=true">\n\n[Unsafe](javascript:alert%281%29)\n\n[Encoded](java&#x73;cript:alert%281%29)\n\n[Local](file:///etc/passwd)';
  await page.evaluate(text=>{document.querySelector('main').innerHTML=renderMarkdown(text);},sample);
  assert.equal(await page.locator('table tbody tr').count(),1);
  assert.equal(await page.locator('table strong').innerText(),'Slack');
  assert.equal(await page.locator('ul ul li em').innerText(),'detail');
  assert.equal(await page.locator('ol li').count(),2);
  assert.equal(await page.locator('blockquote').innerText(),'Quoted');
  assert(await page.locator('input[type=checkbox]').isChecked());
  assert(await page.locator('input[type=checkbox]').isDisabled());
  assert((await page.locator('pre code').innerText()).includes('<script>'));
  assert.equal(await page.locator('main script, main img, main iframe, main [onerror]').count(),0);
  assert.equal(await page.locator('main a').count(),2);
  assert.equal(await page.getByRole('link',{name:'Thread'}).getAttribute('rel'),'noopener noreferrer');
  assert.equal(await page.evaluate(()=>globalThis.compromised),undefined);
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  assert.deepEqual(external,[]);
  console.log('PASS Markdown: tables, nested lists, links, quotes, code, checklists, hostile markup, no remote image fetching, narrow layout');

  // Exercise the actual Companion module, including its narrower link policy.
  await page.evaluate(async()=>{globalThis.cards = await import('/companion-cards.mjs');});
  const cardSource = '**Message:** I sent the request.\n\n- **Posted by:** Jarvis\n- **Timestamp:** 20:34\n\nThe tool returned **chat_sent**. Delivery is still unknown; no recipient acknowledgment was observed.\n\n[Attachment](/api/v1/conversations/room-jarvis/attachments/photo.png)\n\n[Wrong room](/api/v1/conversations/room-coach/attachments/private.png) [Account](/auth/logout) [Query](/api/v1/conversations/room-jarvis/attachments/photo.png?download=1) [Fragment](/api/v1/conversations/room-jarvis/attachments/photo.png#x) [Credentials](https://user:secret@example.com/) [Protocol-relative](//example.com/private) [Email](mailto:someone@example.com) [Script](javascript:alert%281%29) [Data](data:text/html,hello)\n\n![External image](https://example.com/image.png) [Reference](https://example.com/reference)\n\n<script>globalThis.cardInjected=true</script><img src="https://example.com/tracker.png" onerror="globalThis.cardInjected=true">';
  const rendered = await page.evaluate(text=>{
    const main = document.querySelector('main'); main.replaceChildren();
    const source = text;
    const links = cards.renderCardText(main,text,'room-jarvis',location.origin);
    return {links,source};
  },cardSource);
  assert.equal(rendered.source,cardSource,'presentation must not rewrite saved source text');
  assert.equal(await page.locator('.card-text > p > strong').first().innerText(),'Message:');
  assert.equal(await page.locator('.card-text li').count(),2);
  assert.match(await page.locator('.card-text').innerText(),/Delivery is still unknown; no recipient acknowledgment was observed\./);
  assert.equal(await page.locator('.card-text a').count(),3,'only own attachment and explicit HTTPS links survive');
  assert.deepEqual(rendered.links.map(link=>link.name),['Attachment','External image','Reference']);
  assert.match(rendered.links[0].url,/\/conversations\/room-jarvis\/attachments\/photo\.png$/);
  assert.equal(await page.locator('.card-text script,.card-text img,.card-text iframe,.card-text [onerror]').count(),0);
  assert.equal(await page.evaluate(()=>globalThis.cardInjected),undefined);
  for (const link of await page.locator('.card-text a').all()) {
    assert.equal(await link.getAttribute('target'),'_blank');
    assert.equal(await link.getAttribute('rel'),'noopener noreferrer');
  }
  const denied = await page.evaluate(()=>[
    '/api/v1/conversations/room-jarvis/attachments/nested/file.png',
    '/api/v1/conversations/room-jarvis/attachments/../private.png',
    'https://example.com/has space','https://example.com/\ncontrol',
    'file:///etc/passwd', 'javascript:alert(1)', '/auth/logout',
  ].map(url=>cards.safeCardLink(url,'room-jarvis',location.origin)));
  assert(denied.every(value=>value===null));
  assert.equal(await page.evaluate(()=>cards.safeCardLink('/api/v1/conversations/room-jarvis/attachments/photo.png',null,location.origin)),null);
  assert.equal(await page.evaluate(()=>{
    const html=renderMarkdown('[Unsafe](https://example.com)',{resolveLink:()=> 'javascript:alert(1)'});
    const node=document.createElement('div');node.innerHTML=html;return node.querySelectorAll('a').length;
  }),0,'even an erroneous host resolver cannot authorize an executable URL');
  const times = await page.evaluate(()=>{
    const stamp='2026-11-08T20:42:17.003Z', first=cards.savedCardTime(stamp), RealDate=Date;
    // Replaying the same card at another wall-clock time must preserve its label.
    globalThis.Date=class extends RealDate {constructor(...args){super(...(args.length?args:['2099-01-01T00:00:00Z']));}static now(){return 4070908800000;}};
    try {
      const replay=cards.savedCardTime(stamp);
      return {first:first.outerHTML,replay:replay.outerHTML,iso:replay.querySelector('time').dateTime,
        numeric:cards.savedCardTime(RealDate.parse(stamp)).outerHTML,
        missing:[undefined,null,0,-1,NaN,Infinity,'bad','2026-99-99T00:00:00Z'].map(value=>cards.savedCardTime(value).outerHTML)};
    } finally {globalThis.Date=RealDate;}
  });
  assert.equal(times.iso,'2026-11-08T20:42:17.003Z');
  assert.equal(times.first,times.replay);assert.equal(times.numeric,times.first);
  assert.match(times.first,/>Saved /);
  for(const value of times.missing){assert.match(value,/Saved time unavailable/);assert(!value.includes('<time'));}
  assert.deepEqual(external,[],'rendering must not fetch linked images or wrong-conversation attachments');
  console.log('PASS Companion result cards: real Markdown, complete caveat/source preservation, strict conversation-scoped links, no HTML/media loads, durable saved timestamp and unknown absence');
} finally { await browser.close(); server.close(); }
