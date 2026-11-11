// Actual Companion HTML/CSS and callback-bound huddle module. No server, model,
// microphone or remote request. Pointer controls and wheel events only: never
// scrollIntoView/scrollTop, which can move inaccessible overflow:hidden areas.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const [html,css,huddleCss,module]=await Promise.all(['companion.html','companion.css','huddles.css','huddles.mjs'].map(name=>readFile(new URL('../ui/'+name,import.meta.url),'utf8')));

async function pixels(locator){return locator.evaluate(node=>{
 const rect=node.getBoundingClientRect(),clip={left:Math.max(0,rect.left),right:Math.min(innerWidth,rect.right),top:Math.max(0,rect.top),bottom:Math.min(innerHeight,rect.bottom)};
 for(let parent=node.parentElement;parent;parent=parent.parentElement){
  const style=getComputedStyle(parent),bounds=parent.getBoundingClientRect();
  if(['auto','scroll','hidden','clip'].includes(style.overflowY)){clip.top=Math.max(clip.top,bounds.top+parent.clientTop);clip.bottom=Math.min(clip.bottom,bounds.top+parent.clientTop+parent.clientHeight);}
  if(['auto','scroll','hidden','clip'].includes(style.overflowX)){clip.left=Math.max(clip.left,bounds.left+parent.clientLeft);clip.right=Math.min(clip.right,bounds.left+parent.clientLeft+parent.clientWidth);}
 }
 const inside=rect.width>0&&rect.height>0&&rect.left>=clip.left-1&&rect.right<=clip.right+1&&rect.top>=clip.top-1&&rect.bottom<=clip.bottom+1;
 return inside&&[.1,.5,.9].every(x=>[.1,.5,.9].every(y=>{const hit=document.elementFromPoint(rect.left+x*rect.width,rect.top+y*rect.height);return hit&&(hit===node||node.contains(hit));}));
});}
async function reveal(page,locator){
 for(let n=0;n<8;n++){
  if(await pixels(locator))return;
  const move=await locator.evaluate(node=>{
   const target=node.getBoundingClientRect();
   for(let current=node.parentElement;current;current=current.parentElement){
    const style=getComputedStyle(current);if(!['auto','scroll'].includes(style.overflowY)||current.scrollHeight<=current.clientHeight+1)continue;
    const bounds=current.getBoundingClientRect(),clip={left:Math.max(0,bounds.left+current.clientLeft),right:Math.min(innerWidth,bounds.left+current.clientLeft+current.clientWidth),top:Math.max(0,bounds.top+current.clientTop),bottom:Math.min(innerHeight,bounds.top+current.clientTop+current.clientHeight)};
    for(let parent=current.parentElement;parent;parent=parent.parentElement){const s=getComputedStyle(parent),r=parent.getBoundingClientRect();if(['auto','scroll','hidden','clip'].includes(s.overflowY)){clip.top=Math.max(clip.top,r.top+parent.clientTop);clip.bottom=Math.min(clip.bottom,r.top+parent.clientTop+parent.clientHeight);}}
    if(clip.bottom-clip.top<4)continue;
    const delta=(target.top+target.bottom-clip.top-clip.bottom)/2;
    if(Math.abs(delta)<1||(delta<0&&current.scrollTop<=0)||(delta>0&&current.scrollTop>=current.scrollHeight-current.clientHeight-1))continue;
    const y=(clip.top+clip.bottom)/2;
    for(const x of [clip.left+2,clip.right-2,(clip.left+clip.right)/2]){
     const hit=document.elementFromPoint(x,y);if(!hit||!(hit===current||current.contains(hit)))continue;
     let nested=false;for(let p=hit;p&&p!==current;p=p.parentElement)if(['auto','scroll'].includes(getComputedStyle(p).overflowY)){nested=true;break;}
     if(!nested)return {x,y,delta:Math.max(-600,Math.min(600,delta))};
    }
   }return null;
  });
  if(!move)break;await page.mouse.move(move.x,move.y);await page.mouse.wheel(0,move.delta);await page.waitForTimeout(60);
 }
 assert(await pixels(locator),'Content/control must be reachable inside all clips and unoccluded by normal scrolling');
}
async function click(page,locator){await reveal(page,locator);await locator.click();}

for(const width of [390,1280])test(`Companion huddle controls and History use normal scrolling at ${width}px`,async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
 try{
  const context=await browser.newContext({viewport:{width,height:844}}),requests=[];
  await context.route('**/*',route=>{requests.push(route.request().url());return route.abort();});
  const page=await context.newPage();
  await page.setContent(html.replace(/<script\b[^>]*>[\s\S]*?<\/script>/g,'').replace(/<link\b[^>]*>/g,'').replace('</head>',`<style>${css}\n${huddleCss}</style></head>`));
  await page.evaluate(async source=>{
   const {createHuddles}=await import('data:text/javascript;base64,'+btoa(unescape(encodeURIComponent(source))));
   document.body.classList.add('call-active');for(const id of ['loading','login','phonebook'])document.getElementById(id).hidden=true;
   for(const id of ['app','call-screen','call-huddles'])document.getElementById(id).hidden=false;
   document.getElementById('space-empty').hidden=true;document.getElementById('board-items').textContent='Keep this board unchanged.';
   window.fixture={reads:0,sends:0,preferences:0,messages:[],peer:{},audio:document.querySelector('#remote-audio')};window.originalPeer=fixture.peer;
   const room={id:'layout-room',title:'Entry review',purpose:'Compare the entry width and materials list before reporting the useful finding. This discussion must leave every connected world and current task unchanged.',status:'Open',participants:[{id:'reviewer',label:'Reviewer',kind:'agent'}],following:false,canSend:true,before:null};
   window.view=createHuddles({root:document.getElementById('call-huddle-view'),list:async()=>({items:[{...room}]}),read:async()=>{fixture.reads++;return {...room,messages:structuredClone(fixture.messages)};},setFollowing:async({following})=>{fixture.preferences++;room.following=following;return {following};},send:async()=>{fixture.sends++;throw new Error('No sends in layout fixture');}});
   document.getElementById('call-huddles').ontoggle=()=>{if(!document.getElementById('call-huddles').open)view.close();else view.refresh({automatic:true});};
   const card=document.createElement('article');card.className='output-card';
   const title=document.createElement('h2');title.textContent='Review result';card.append(title);
   for(const text of ['Reviewer reported this update.','Evidence: first retained observation.','Evidence: independently retained comparison.']){const p=document.createElement('p');p.textContent=text;card.append(p);}
   const inspect=document.createElement('button');inspect.textContent='Inspect huddle';card.append(inspect);
   const answer=document.createElement('div');answer.className='card-answer';answer.textContent='The measured entry is clear, but the materials list still needs its final check. These are saved observations, not evidence that any world action was performed. Keep the original assignment and its limits unchanged.';card.append(answer);
   document.getElementById('cards').append(card);
   await view.setContext({key:'layout',owner:'reader',parentId:'original'});
  },module);
  const mediaBefore=await page.locator('.call-controls').boundingBox(),tabsBefore=await page.locator('.shared-tabs').boundingBox(),board=await page.locator('#board-items').innerHTML();
  await click(page,page.locator('#call-huddles > summary'));
  await click(page,page.getByRole('button',{name:'Inspect',exact:true}));
  await page.getByRole('button',{name:'Refresh discussion',exact:true}).waitFor();
  const reads=await page.evaluate(()=>fixture.reads);
  await page.evaluate(async()=>{fixture.messages=[{id:'finding',actor:'Reviewer',text:'The main entry fits the measured clearance. The materials list still needs an independent check before anyone changes the world. This is a saved review of the supplied observations, not confirmation of physical work or a completed assignment.',createdAt:'2026-01-02T03:04:05Z'}];await view.refresh({automatic:true});});
  assert.equal(await page.evaluate(()=>fixture.reads),reads);assert.equal(await page.locator('.huddles-message').count(),0);
  await click(page,page.getByRole('button',{name:'Refresh discussion',exact:true}));await reveal(page,page.locator('.huddles-text'));
  assert.equal(await page.locator('.huddles-message strong').innerText(),'Reviewer');assert.equal(await page.locator('.huddles-message time').getAttribute('datetime'),'2026-01-02T03:04:05.000Z');
  await click(page,page.getByRole('button',{name:'Join discussion',exact:true}));
  const composer=page.getByRole('textbox',{name:'Message the huddle'});await reveal(page,composer);assert.equal(await composer.inputValue(),'');assert(await page.getByRole('button',{name:'Send message',exact:true}).isDisabled());
  await click(page,page.getByRole('button',{name:'Hide message box',exact:true}));
  await click(page,page.locator('#call-huddles > summary'));assert.equal(await page.locator('#call-huddles').evaluate(node=>node.open),false);
  await click(page,page.locator('#board-history > summary'));await reveal(page,page.locator('.card-answer'));
  assert.deepEqual(await page.locator('.call-controls').boundingBox(),mediaBefore);assert.deepEqual(await page.locator('.shared-tabs').boundingBox(),tabsBefore);
  assert.deepEqual(await page.evaluate(()=>[fixture.sends,fixture.preferences,fixture.peer===originalPeer,fixture.audio===document.querySelector('#remote-audio')]),[0,0,true,true]);
  assert.equal(await page.locator('#board-items').innerHTML(),board);assert.deepEqual(requests,[]);
  assert.equal(await page.evaluate(()=>[document.body,document.querySelector('.shared-panel'),document.getElementById('call-screen')].every(node=>node.scrollTop===0)),true,'No hidden ancestor was scrolled to manufacture visibility');
 }finally{await browser.close();}
});
