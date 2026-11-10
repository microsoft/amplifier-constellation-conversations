// Actual app.js/HTML/CSS with a disposable local API. No ACP, microphone or provider.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const at=1794330000000, m=(id,text,extra={})=>({id,text,role:'user',actor:'owner',status:'voice_fragment',created_at:at,updated_at:at+1,...extra});
const envelope='Internal backend catalog: {"source_id":"minecraft-private","instruction":"do not display"}';
const latest=[m('word-1','Where'),m('word-2',' are'),m('word-3',' you?'),m('delegation',envelope,{status:'complete',presentation:{kind:'spoken_request',label:'From voice',text:'Where are you now?',excerpt:true,canonical_text_unchanged:true}}),m('card',"I'm **at spawn**.",{role:'assistant',actor:'Jarvis',status:'complete'}),m('typed','Typed JSON: {"custom":true}',{status:'complete'}),m('speech-1','I am',{role:'assistant',actor:'Jarvis'}),m('speech-2',' nearby.',{role:'assistant',actor:'Jarvis'})];
const earlier=[m('earlier-fragment',' earlier words')];
const canonical=JSON.stringify({latest,earlier}),writes=[],unexpected=[];
let view={id:'browser-fixture',revision:1,state:{selected_conversation_id:'room',panel:'conversation',draft:'',search:'',history_search:'',history_before:null,attachment_ids:[],target_agent_ids:[],elicitation_drafts:{}}};
const conversation={id:'room',owner:'owner',title:'Saved voice conversation',status:'ready',updated_at:at,surface:'web',messages:latest,members:[{username:'owner',role:'owner'}],agents:[{id:'jarvis',label:'Jarvis'}],permissions:[],elicitations:[],deliveries:[{id:'pending-review',message_id:'word-2',agent_id:'jarvis',state:'unknown'}]};
const server=createServer(async(req,res)=>{
 try{
  const url=new URL(req.url,'http://localhost'),p=url.pathname;
  res.setHeader('Cache-Control','no-store');
  const json=value=>{res.setHeader('Content-Type','application/json');res.end(JSON.stringify(value));};
  if(req.method==='PUT'&&p.startsWith('/api/v1/views/')){
   const body=JSON.parse(await Array.fromAsync(req).then(parts=>Buffer.concat(parts).toString()));
   assert.equal(body.state.selected_conversation_id,'room');writes.push(body);view={...view,revision:view.revision+1,state:body.state};return json(view);
  }
  if(req.method!=='GET')throw Error(`Unexpected mutation ${req.method} ${p}`);
  if(p==='/auth/session')return json({authenticated:true,owner:'owner',csrf:'fixture'});
  if(p==='/api/v1/meta')return json({owner:'owner',agent:{label:'Jarvis'},provider:{id:'fixture'},version:'fixture',admin:false});
  if(p.startsWith('/api/v1/views/'))return json(view);
  if(p==='/api/v1/conversations')return json({items:[conversation]});
  if(p==='/api/v1/agents')return json({items:[{id:'jarvis',label:'Jarvis'}]});
  if(p==='/api/v1/people')return json({items:['owner']});
  if(p==='/api/v1/conversations/room')return json(conversation);
  if(p==='/api/v1/conversations/room/messages')return json({items:url.searchParams.has('before')?earlier:latest,next_cursor:url.searchParams.has('before')?null:100});
  if(p==='/api/v1/conversations/room/attachments')return json({items:[]});
  if(p==='/api/v1/events'){res.writeHead(200,{'Content-Type':'text/event-stream'});res.write(': fixture ready\n\n');return;}
  const name=p==='/'?'index.html':p.slice(1);
  if(!/^[a-z0-9.-]+$/.test(name))throw Error(`Unexpected path ${p}`);
  let data=await readFile(new URL(`../ui/${name}`,import.meta.url));
  if(name==='markdown.js')data=Buffer.concat([data,await readFile(new URL('../ui/markdown-renderer.js',import.meta.url))]);
  res.setHeader('Content-Type',name.endsWith('.js')||name.endsWith('.mjs')?'text/javascript':name.endsWith('.css')?'text/css':name.endsWith('.svg')?'image/svg+xml':name.endsWith('.webmanifest')?'application/manifest+json':'text/html');res.end(data);
 }catch(e){unexpected.push(e.message);res.writeHead(500);res.end(JSON.stringify({error:e.message}));}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE||undefined,args:['--no-sandbox']});
try{
 const page=await browser.newPage(),errors=[];page.on('pageerror',e=>errors.push(e.message));
 const base=`http://127.0.0.1:${server.address().port}`;
 await page.route('**/*',route=>route.request().url().startsWith(base)?route.continue():route.abort());
 await page.goto(base);await page.locator('article[data-message-id=card]').waitFor();
 assert.deepEqual(await page.locator('#messages article').evaluateAll(nodes=>nodes.flatMap(n=>JSON.parse(n.dataset.messageIds))),latest.map(m=>m.id));
 assert.equal(await page.locator('#messages article').count(),5);
 assert.equal(await page.locator('article[data-message-id=word-1] .message-content').innerText(),'Where are you?');
 assert.match(await page.locator('article[data-message-id=word-1] .message-source').innerText(),/Voice transcript.*excerpt on this page/);
 assert.equal(await page.locator('[data-delivery-id=pending-review]').count(),1,'delivery controls survive grouping even for a later member');
 assert.equal(await page.locator('[data-delivery-id=pending-review] button').count(),1);
 const request=page.locator('article[data-message-id=delegation]');
 assert.equal(await request.locator('.message-content').innerText(),'Where are you now?');
 assert.match(await request.locator('.message-source').innerText(),/From voice.*request excerpt/);
 assert.equal(await request.locator('details pre').isVisible(),false,'canonical orchestration is collapsed by default');
 assert(!await page.locator('#messages').innerText().then(text=>text.includes('minecraft-private')));
 await request.locator('summary').click();assert.equal(await request.locator('details pre').innerText(),envelope);
 await request.locator('summary').click();
 assert.equal(await page.locator('article[data-message-id=typed] .message-content').innerText(),'Typed JSON: {"custom":true}');
 assert.equal(await page.locator('article[data-message-id=typed] details').count(),0);
 assert.equal(await page.locator('article[data-message-id=card]').getAttribute('data-message-ids'),'["card"]');
 assert.equal(await page.locator('article[data-message-id=card] strong').last().innerText(),'at spawn');
 assert.match(await page.locator('article[data-message-id=speech-1] .message-source').innerText(),/playback unconfirmed/);
 await page.locator('article[data-message-id=word-1] summary').click();
 assert.deepEqual(JSON.parse(await page.locator('article[data-message-id=word-1] pre').innerText()),latest.slice(0,3).map(({id,role,actor,text,created_at,updated_at})=>({id,role,actor,text,created_at,updated_at})));
 await page.locator('article[data-message-id=word-1] summary').click();
 for(const width of [1280,390]){
  await page.setViewportSize({width,height:900});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  if(process.env.PRESENTATION_SCREENSHOTS){await mkdir(process.env.PRESENTATION_SCREENSHOTS,{recursive:true});await page.screenshot({path:`${process.env.PRESENTATION_SCREENSHOTS}/history-${width}.png`,fullPage:true});}
 }
 await page.locator('#older-messages').click();await page.locator('article[data-message-id=earlier-fragment]').waitFor();
 assert.match(await page.locator('article[data-message-id=earlier-fragment] .message-source').innerText(),/excerpt on this page/);
 assert.deepEqual(await page.locator('#messages article').evaluateAll(nodes=>nodes.flatMap(n=>JSON.parse(n.dataset.messageIds))),['earlier-fragment']);
 await page.locator('#latest-messages').click();await page.locator('article[data-message-id=card]').waitFor();
 assert.deepEqual(await page.locator('#messages article').evaluateAll(nodes=>nodes.flatMap(n=>JSON.parse(n.dataset.messageIds))),latest.map(m=>m.id));
 assert.equal(JSON.stringify({latest,earlier}),canonical);assert(writes.length===2&&writes.every(w=>w.state.selected_conversation_id==='room'));
 assert.deepEqual(errors,[]);assert.deepEqual(unexpected,[]);
 console.log(JSON.stringify({passed:true,scope:'actual app with disposable API, desktop/390px, canonical details, exact fragment membership, delivery controls and Older/Latest navigation; no provider or ACP'}));
}finally{await browser.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
