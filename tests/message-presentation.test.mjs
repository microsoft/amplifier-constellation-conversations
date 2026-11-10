import test from 'node:test';
import assert from 'node:assert/strict';
import {presentMessages,voiceLabel} from '../ui/message-presentation.mjs';
const m=(id,text,extras={})=>({id,text,role:'user',actor:'owner',status:'voice_fragment',created_at:10,updated_at:11,...extras});

test('consecutive fragments join verbatim and preserve every original identity and timestamp',()=>{
 const input=[m('a','Where'),m('b',' are'),m('c',' you?')],before=structuredClone(input);
 const output=presentMessages(input);
 assert.equal(output.length,1);assert.equal(output[0].display_text,'Where are you?');
 assert.deepEqual(output[0].source_messages,input);assert.deepEqual(input,before);
 assert.equal(voiceLabel(output[0]),'Voice transcript');
});

test('normal messages, exact speaker/role changes and attachments break grouping',()=>{
 const input=[m('a','A'),m('b',' typed',{status:'complete'}),m('c','C'),m('d','D',{actor:'robin'}),m('e','E',{actor:'robin',role:'assistant'}),m('f','F',{actor:'robin',role:'assistant',attachments:[{id:'file'}]}),m('g','G',{actor:'robin',role:'assistant'})];
 assert.deepEqual(presentMessages(input).map(x=>x.source_messages.map(m=>m.id)),input.map(x=>[x.id]));
 assert.match(voiceLabel(presentMessages(input)[4]),/playback unconfirmed/);
 assert.equal(voiceLabel(presentMessages(input)[1]),null);
});

test('page boundaries and search never imply one complete utterance',()=>{
 const first=presentMessages([m('a','Where')],{hasOlder:true,hasNewer:true});
 const next=presentMessages([m('b',' are'),m('c',' you?')],{hasNewer:true});
 assert.equal(first[0].display_text,'Where');assert.equal(next[0].display_text,' are you?');
 assert.match(voiceLabel(first[0]),/excerpt on this page/);assert.match(voiceLabel(next[0]),/excerpt on this page/);
 const searched=presentMessages([m('a','Where'),m('c',' you?')],{filtered:true});
 assert.equal(searched.length,2);assert(searched.every(x=>x.voice_page_excerpt));
});

test('only explicit presentation metadata changes visible text; canonical evidence remains intact',()=>{
 const raw='Internal catalog: {"source":"private"}';
 const input=[m('linked',raw,{status:'complete',presentation:{kind:'spoken_request',text:'Where are you now?',excerpt:true}}),m('typed',raw,{status:'complete'})],before=structuredClone(input);
 const output=presentMessages(input);
 assert.equal(output[0].display_text,'Where are you now?');assert.equal(output[0].text,raw);
 assert.equal(output[1].display_text,raw);assert.equal(voiceLabel(output[0]),'From voice · request excerpt');
 assert.deepEqual(input,before);assert.deepEqual(output.map(x=>x.source_messages.map(m=>m.id)),[['linked'],['typed']]);
});
