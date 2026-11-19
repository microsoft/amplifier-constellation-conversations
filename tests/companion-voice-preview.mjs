// Invoked by the opt-in real-server preview test, with disposable owner data.
import assert from 'node:assert/strict';
const {chromium,devices}=await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const [origin,cookieName,cookieValue]=process.argv.slice(2);
const browser=await chromium.launch({executablePath:process.env.CHROMIUM_EXECUTABLE || undefined,headless:true,args:['--no-sandbox']});
try {
  const context=await browser.newContext({...devices['Pixel 7'],viewport:{width:390,height:844}});
  await context.addCookies([{url:origin,name:cookieName,value:cookieValue}]);
  const page=await context.newPage(), traffic=[];
  await page.addInitScript(()=>{
    window.mediaRequests=0;
    navigator.mediaDevices.getUserMedia=async()=>{window.mediaRequests++;throw Error('Unexpected microphone request');};
  });
  page.on('request',r=>traffic.push({method:r.method(),url:r.url()}));
  await page.goto(origin+'/companion');
  await page.getByRole('button',{name:/^Voice settings for /}).first().tap();
  const checked=page.locator('#voice-choice input[type=radio]:checked');
  await checked.waitFor();
  const saved=await checked.inputValue();
  const screenshot=process.env.COMPANION_VOICE_SCREENSHOT;
  assert(await page.getByText('English · British · Masculine',{exact:true}).isVisible());
  await page.setViewportSize({width:320,height:740});
  const dialog=page.locator('#voice-settings');
  const withinDialog=await dialog.evaluate(node=>{
    const {left,right}=node.getBoundingClientRect();
    return node.scrollWidth<=node.clientWidth && left>=0 && right<=innerWidth
      && [...node.querySelectorAll('.voice-filters select, .voice-footer button')].every(control=>{
        const bounds=control.getBoundingClientRect();
        return bounds.left>=left && bounds.right<=right;
      });
  });
  assert(withinDialog,'voice filters or actions overflowed a 320px screen');
  const labelsFit=await page.locator('.voice-filters label').evaluateAll(labels=>labels.every(label=>{
    const text=document.createRange();text.selectNodeContents(label.firstChild);
    const cell=label.getBoundingClientRect();
    return [...text.getClientRects()].every(line=>line.left>=cell.left-1 && line.right<=cell.right+1);
  }));
  assert(labelsFit,'a filter label spills into the next column at 320px');
  if(screenshot)await page.screenshot({path:screenshot.replace('.png','-320.png')});
  await page.setViewportSize({width:390,height:844});
  if(screenshot){
    await page.screenshot({path:screenshot.replace('.png','-mobile.png')});
    await page.setViewportSize({width:1280,height:900});
    await page.screenshot({path:screenshot.replace('.png','-desktop.png')});
    await page.setViewportSize({width:390,height:844});
  }
  const unfilteredBounds=await dialog.boundingBox();
  await page.locator('#voice-filter-region').selectOption('British');
  const filteredBounds=await dialog.boundingBox();
  assert(Math.abs(unfilteredBounds.y-filteredBounds.y)<=2,'changing a filter moved the dialog');
  const filtered=page.locator('#voice-choice input[type=radio]:visible');
  assert.deepEqual(await filtered.evaluateAll(radios=>radios.map(radio=>radio.value)),['vesper']);
  if(screenshot)await page.screenshot({path:screenshot.replace('.png','-filtered.png')});
  await page.locator('#voice-filter-region').selectOption('');
  assert.equal(await checked.inputValue(),saved);
  for(const voice of ['cedar','marin']){
    await page.getByRole('button',{name:`Play sample of ${voice}`,exact:true}).tap();
    await page.getByText('Playing a recorded sample.',{exact:true}).waitFor();
    assert.equal(await checked.inputValue(),saved);
    await page.getByRole('button',{name:`Stop sample of ${voice}`,exact:true}).tap();
    await page.getByRole('button',{name:`Play sample of ${voice}`,exact:true}).waitFor();
  }
  await page.locator('#close-voice-settings').tap();
  await page.getByRole('button',{name:/^Voice settings for /}).first().tap();
  await checked.waitFor();
  assert.equal(await checked.inputValue(),saved);
  assert.equal(await page.evaluate(()=>window.mediaRequests),0);
  assert(traffic.every(r=>r.method==='GET'&&r.url.startsWith(origin+'/')));
  assert.deepEqual(traffic.filter(r=>r.url.includes('/voice-previews/')).map(r=>r.url.split('/').at(-1)),['cedar','marin']);
  console.log(JSON.stringify({voices:['cedar','marin'],playback:'both emitted playing and stopped',browser:browser.version(),touch:true,microphone:0,writes:0,external_requests:0}));
} finally {await browser.close();}
