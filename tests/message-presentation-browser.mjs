// Actual app.js/HTML/CSS with a disposable local API. No ACP, microphone or provider.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const at=1794330000000, m=(id,text,extra={})=>({id,text,role:'user',actor:'owner',status:'voice_fragment',created_at:at,updated_at:at+1,...extra});
const envelope='Internal backend catalog: {"source_id":"minecraft-private","instruction":"do not display"}';
const latest=[m('word-1','Where'),m('word-2',' are'),m('word-3',' you?'),m('delegation',envelope,{status:'complete',presentation:{kind:'spoken_request',label:'From voice',text:'Where are you now?',excerpt:true,canonical_text_unchanged:true}}),m('card',"I'm **at spawn**.",{role:'assistant',actor:'Jarvis',status:'complete'}),m('typed','Typed JSON: {"custom":true}',{status:'complete'}),m('speech-1','I am',{role:'assistant',actor:'Jarvis'}),m('speech-2',' nearby.',{role:'assistant',actor:'Jarvis'})];
const earlier=[m('earlier-fragment',' earlier words')];
const canonical=JSON.stringify({latest,earlier}),writes=[],unexpected=[],huddleWrites=[],viewWrites=[];
const huddleViews=new Map();let mainViewId=null,holdHuddleRead=false,releaseHuddleRead=null;
const huddleMessages=[m('review-1','A private linked review.',{role:'assistant',actor:'Coach',agent_id:'coach',seq:1,status:'complete'})];
const huddleDescriptor=()=>({version:1,id:'review-room',conversation_id:'review-room',title:'Plan review',purpose:'Compare options before returning the result.',state:huddleRoom.huddle.state,revision:huddleRoom.huddle.revision,
 origin:{kind:'conversation',conversation_id:'room'},inspect_url:'/?conversation=review-room',members:[{username:'owner',role:'owner'}],participants:[{id:'coach',label:'Coach',connection_id:'minecraft-coach',delivery:'external'}],preference:{mode:'quiet'},outcome_summary:{count:0,latest:null}});
const defaultHuddleView=id=>({id,revision:0,state:{selected_conversation_id:null,panel:'conversation',draft:'',draft_request_id:'',draft_request_sha256:'',search:'',history_search:'',history_before:null,attachment_ids:[],target_agent_ids:[],elicitation_drafts:{}}});

let view={id:'browser-fixture',revision:1,state:{selected_conversation_id:'room',panel:'conversation',draft:'',search:'',history_search:'',history_before:null,attachment_ids:[],target_agent_ids:[],elicitation_drafts:{}}};
const conversation={id:'room',owner:'owner',title:'Saved voice conversation',status:'ready',updated_at:at,surface:'web',messages:latest,members:[{username:'owner',role:'owner'}],agents:[{id:'jarvis',label:'Jarvis'}],permissions:[],elicitations:[],deliveries:[{id:'pending-review',message_id:'word-2',agent_id:'jarvis',state:'unknown'}]};
const huddleRoom={id:'review-room',owner:'owner',title:'Plan review',status:'idle',updated_at:at,surface:'web',shared:true,huddle:{id:'review-room',state:'open',revision:1},messages:huddleMessages,members:[{username:'owner',role:'owner'}],agents:[{id:'coach',label:'Coach'}],permissions:[],elicitations:[],deliveries:[]};
const quietRoom={...conversation,id:'quiet-room',title:'Another conversation',messages:[],deliveries:[]};
const conversations=new Map([[conversation.id,conversation],[huddleRoom.id,huddleRoom],[quietRoom.id,quietRoom]]);
const server=createServer(async(req,res)=>{
 try{
  const url=new URL(req.url,'http://localhost'),p=url.pathname;
  res.setHeader('Cache-Control','no-store');
  const json=value=>{res.setHeader('Content-Type','application/json');res.end(JSON.stringify(value));};
  const viewPath=p.match(/^\/api\/v1\/views\/([^/]+)$/);
  if(viewPath){
   if(mainViewId===null)mainViewId=viewPath[1];
   const isMain=viewPath[1]===mainViewId;
   if(req.method==='PUT'){
    const body=JSON.parse(await Array.fromAsync(req).then(parts=>Buffer.concat(parts).toString()));
    if(isMain){writes.push(body);view={...view,revision:view.revision+1,state:body.state};return json(view);}
    const old=huddleViews.get(viewPath[1])||defaultHuddleView(viewPath[1]);assert.equal(body.expected_revision,old.revision);
    const saved={id:viewPath[1],revision:old.revision+1,state:body.state};viewWrites.push(body);huddleViews.set(viewPath[1],saved);return json(saved);
   }
   if(isMain)return json(view);
   if(!huddleViews.has(viewPath[1]))huddleViews.set(viewPath[1],defaultHuddleView(viewPath[1]));
   return json(huddleViews.get(viewPath[1]));
  }
  if(req.method==='POST'&&p==='/api/v1/conversations/review-room/messages'){
   const body=JSON.parse(await Array.fromAsync(req).then(parts=>Buffer.concat(parts).toString()));huddleWrites.push({method:req.method,path:p,body});
   huddleMessages.push(m(body.request_id,body.text,{seq:huddleMessages.length+1,status:'complete',role:'user',actor:'owner',agent_id:null,target_agent_ids:body.target_agent_ids}));return json({accepted:true,message_id:body.request_id});
  }
  if(req.method==='POST'&&p==='/api/v1/conversations/review-room/cancel'){
   huddleWrites.push({method:req.method,path:p});huddleRoom.huddle={...huddleRoom.huddle,state:'stopped',revision:2};return json({ok:true});
  }
  if(req.method==='PUT'&&p==='/api/v1/huddles/review-room/preference'){
   const body=JSON.parse(await Array.fromAsync(req).then(parts=>Buffer.concat(parts).toString()));huddleWrites.push({method:req.method,path:p,body});return json({mode:body.mode,membership_changed:false,work_authority_changed:false});
  }
  if(req.method!=='GET')throw Error(`Unexpected mutation ${req.method} ${p}`);
  if(p==='/auth/session')return json({authenticated:true,owner:'owner',csrf:'fixture'});
  if(p==='/api/v1/meta')return json({owner:'owner',agent:{label:'Jarvis'},provider:{id:'fixture'},version:'fixture',admin:false});
  if(p.startsWith('/api/v1/views/'))return json(view);
  if(p==='/api/v1/conversations')return json({items:[...conversations.values()]});
  if(p==='/api/v1/agents')return json({items:[{id:'jarvis',label:'Jarvis'}]});
  if(p==='/api/v1/people')return json({items:['owner']});
  const roomPath=p.match(/^\/api\/v1\/conversations\/([^/]+)$/);if(roomPath&&conversations.has(roomPath[1]))return json(conversations.get(roomPath[1]));
  const huddleList=p.match(/^\/api\/v1\/conversations\/([^/]+)\/huddles$/);
  if(huddleList)return json({version:1,items:huddleList[1]==='room'?[huddleDescriptor()]:[],current:huddleList[1]==='review-room'?huddleDescriptor():null});
  if(p==='/api/v1/huddles/review-room')return json(huddleDescriptor());
  if(p==='/api/v1/conversations/review-room/messages'){
   const data={items:structuredClone(huddleMessages),next_cursor:null};
   if(holdHuddleRead){holdHuddleRead=false;releaseHuddleRead=()=>json(data);return;}return json(data);
  }
  if(p==='/api/v1/conversations/quiet-room/messages')return json({items:[],next_cursor:null});

  if(p==='/api/v1/conversations/room/messages')return json({items:url.searchParams.has('before')?earlier:latest,next_cursor:url.searchParams.has('before')?null:100});
  if(/^\/api\/v1\/conversations\/[^/]+\/attachments$/.test(p))return json({items:[]});
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
 // A linked huddle stays secondary to the saved conversation until inspected.
 await page.setViewportSize({width:1280,height:900});
 await page.locator('#conversation-huddles:not([hidden])').waitFor();assert.equal(await page.locator('#conversation-huddles').evaluate(node=>node.open),false);
 const savedTranscript=await page.locator('#messages').innerText();
 await page.locator('#conversation-huddles > summary').click();
 await page.locator('#conversation-huddle-view').getByRole('button',{name:'Inspect',exact:true}).click();
 await page.locator('.huddles-message').waitFor();assert.equal(await page.locator('#messages').innerText(),savedTranscript);assert.equal(huddleWrites.length,0);
 await page.locator('#conversation-huddle-view').getByRole('button',{name:'Join discussion',exact:true}).click();
 await page.getByRole('textbox',{name:'Message the huddle'}).fill('Keep the source links with the answer.');
 await page.locator('#conversation-huddle-view').getByRole('button',{name:'Send message',exact:true}).click();
 await page.waitForFunction(()=>document.querySelector('#conversation-huddle-view').textContent.includes('Your message was saved'));
 assert.equal(huddleWrites.length,1);assert.equal(huddleWrites[0].path,'/api/v1/conversations/review-room/messages');assert.equal(viewWrites.length,2);
 assert.equal(await page.locator('#messages').innerText(),savedTranscript);
 // A late read cannot paint the previous room after selecting another one.
 holdHuddleRead=true;await page.locator('#conversation-huddle-view').getByRole('button',{name:'Refresh discussion',exact:true}).click();
 for(let i=0;i<50&&!releaseHuddleRead;i++)await page.waitForTimeout(20);assert(releaseHuddleRead);
 await page.locator('#conversation-list button').filter({hasText:'Another conversation'}).click();
 await page.waitForFunction(()=>document.querySelector('#conversation-title').textContent==='Another conversation');
 releaseHuddleRead();releaseHuddleRead=null;await page.waitForTimeout(100);
 assert(await page.locator('#conversation-huddles').isHidden());assert.doesNotMatch(await page.locator('#conversation-huddle-view').textContent(),/private linked review/);
 // Direct huddle rooms use explicit current metadata, not inferred labels.
 await page.locator('#conversation-list button').filter({hasText:'Plan review'}).click();
 await page.waitForFunction(()=>document.querySelector('#conversation-title').textContent==='Plan review');
 await page.locator('#conversation-huddles:not([hidden])').waitFor();assert.equal(await page.locator('#conversation-huddle-count').textContent(),'(1)');
 assert.equal(await page.locator('#stop').innerText(),'Stop huddle');assert(await page.locator('#stop').isVisible());assert.match(await page.locator('#turn-status-text').innerText(),/huddle is open/);
 assert(await page.locator('label[for="file-upload"]').isHidden());assert(await page.locator('#file-upload').isDisabled());
 await page.locator('#participants-toggle').click();await page.locator('#participants-panel:not([hidden])').waitFor();
 assert(await page.locator('#agent-invite-controls').isHidden());assert.equal(await page.locator('#participant-list .remove-participant').count(),0);assert(await page.locator('#invite-username').isVisible());
 await page.locator('#participants-toggle').click();
 await page.locator('#message').fill('é'.repeat(3001));await page.locator('#send').click();await page.locator('#global-error:not([hidden])').waitFor();assert.match(await page.locator('#global-error').innerText(),/shorter huddle message/);assert.equal(huddleWrites.length,1);
 await page.locator('#dismiss-error').click();await page.locator('#message').fill('Please continue with the agreed comparison.');await page.locator('#send').click();
 await page.waitForFunction(()=>document.querySelector('#message').value==='');assert.equal(huddleWrites.length,2);assert.equal(huddleWrites[1].body.text,'Please continue with the agreed comparison.');
 await page.locator('#stop').click();await page.waitForFunction(()=>document.querySelector('#turn-status-text').textContent.includes('huddle is stopped'));
 await page.locator('#message').fill('A stopped huddle is read-only here.');assert(await page.locator('#send').isDisabled());assert(await page.locator('#stop').isHidden());
 assert.equal(huddleWrites.filter(write=>write.path.endsWith('/cancel')).length,1);
 for(const width of [1280,390]){await page.setViewportSize({width,height:900});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));if(process.env.PRESENTATION_SCREENSHOTS)await page.screenshot({path:`${process.env.PRESENTATION_SCREENSHOTS}/huddle-room-${width}.png`,fullPage:true});}
 assert.deepEqual(errors,[]);assert.deepEqual(unexpected,[]);
 console.log(JSON.stringify({passed:true,scope:'actual app with disposable API, desktop/390px, canonical history, linked/current huddle inspect/join, stale context, explicit Stop, attachment/UTF8 restrictions and participant affordances; no provider or ACP'}));
}finally{await browser.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
