import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const ui = path.join(process.cwd(), 'ui');
const script = await fs.readFile(path.join(ui, 'markdown.js'), 'utf8') + await fs.readFile(path.join(ui, 'markdown-renderer.js'), 'utf8');
const css = await fs.readFile(path.join(ui, 'markdown.css'), 'utf8');
const server = http.createServer((req,res) => {
  if(req.url === '/markdown.js') {res.setHeader('Content-Type','text/javascript');res.end(script);}
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
} finally { await browser.close(); server.close(); }
