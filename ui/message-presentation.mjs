// Presentation copies only. Original IDs, text, times and evidence stay intact.
export function presentMessages(messages,{hasOlder=false,hasNewer=false,filtered=false}={}){
  const groups=[];
  for(const message of messages){
    const previous=groups.at(-1),fragment=message.status==='voice_fragment';
    // Search results may omit intervening speech. Never join across those gaps,
    // normal messages, speakers or page boundaries, or hide attached artifacts.
    if(!filtered&&fragment&&previous?.status==='voice_fragment'&&previous.role===message.role&&previous.actor===message.actor&&!previous.attachments?.length&&!message.attachments?.length){
      previous.text+=message.text;previous.source_messages.push(message);previous.updated_at=message.updated_at;
    }else groups.push({...message,source_messages:[message]});
  }
  return groups.map((message,index)=>({
    ...message,
    voice_page_excerpt:message.status==='voice_fragment'&&(filtered||(index===0&&hasOlder)||(index===groups.length-1&&hasNewer)),
    display_text:message.presentation?.kind==='spoken_request'?message.presentation.text:message.text
  }));
}

export function appendMessageDetails(body,message){
  const spoken=message.presentation?.kind==='spoken_request',fragment=message.status==='voice_fragment';
  if(!spoken&&!fragment)return;
  const details=document.createElement('details');details.className='message-details';
  details.dataset.messageId=message.id;
  const summary=document.createElement('summary');summary.textContent=spoken?'Technical details':'Transcript details';details.append(summary);
  const text=document.createElement('pre');text.textContent=spoken?message.text:JSON.stringify(message.source_messages.map(m=>({id:m.id,role:m.role,actor:m.actor,text:m.text,created_at:m.created_at,updated_at:m.updated_at})),null,2);
  details.append(text);body.append(details);
}

export function voiceLabel(message){
  if(message.presentation?.kind==='spoken_request')return 'From voice · request excerpt';
  if(message.status!=='voice_fragment')return null;
  const parts=['Voice transcript'];
  if(message.voice_page_excerpt)parts.push('excerpt on this page');
  if(message.role==='assistant')parts.push('playback unconfirmed');
  return parts.join(' · ');
}
