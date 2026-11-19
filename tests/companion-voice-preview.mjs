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
  await page.locator('#voice-choice:not([disabled])').waitFor();
  const saved=await page.locator('#voice-choice').inputValue();
  for(const voice of ['cedar','marin']){
    await page.locator('#voice-choice').selectOption(voice);
    await page.getByRole('button',{name:'Play voice sample'}).tap();
    await page.getByText('Playing a recorded sample.',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Stop sample'}).tap();
    await page.getByRole('button',{name:'Play voice sample'}).waitFor();
  }
  await page.locator('#close-voice-settings').tap();
  await page.getByRole('button',{name:/^Voice settings for /}).first().tap();
  await page.locator('#voice-choice:not([disabled])').waitFor();
  assert.equal(await page.locator('#voice-choice').inputValue(),saved);
  assert.equal(await page.evaluate(()=>window.mediaRequests),0);
  assert(traffic.every(r=>r.method==='GET'&&r.url.startsWith(origin+'/')));
  assert.deepEqual(traffic.filter(r=>r.url.includes('/voice-previews/')).map(r=>r.url.split('/').at(-1)),['cedar','marin']);
  console.log(JSON.stringify({voices:['cedar','marin'],playback:'both emitted playing and stopped',browser:browser.version(),touch:true,microphone:0,writes:0,external_requests:0}));
} finally {await browser.close();}
