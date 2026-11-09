// Browser media evidence, not a provider turn detector. Quiet intervals are
// candidate playback boundaries only; they never settle speech or backend work.
export class ConversationFloor {
  constructor({now=()=>Date.now(),id=()=>crypto.randomUUID(),changed=()=>{},sustainedMs=650,userQuietMs=1800,outputQuietMs=1200}={}) {
    Object.assign(this,{now,id,changed,sustainedMs,userQuietMs,outputQuietMs});
    this.available=false;this.availableSince=null;this.mic=null;this.output=null;this.lastMic=null;this.lastOutput=null;
    this.micStart=null;this.floor='unknown';this.suppressed=false;this.interruption=null;this.last=null;this.closed=false;
  }
  sample({mic,output,available=true}) {
    if(this.closed)return;
    const at=this.now(),wasAvailable=this.available;this.available=available && typeof mic==='boolean' && typeof output==='boolean';
    if(!this.available){this.availableSince=null;this.mic=null;this.output=null;this.micStart=null;this.floor='unknown';this.emit();return;}
    if(!wasAvailable)this.availableSince=at;
    const wasOutput=this.output;this.mic=mic;this.output=output;
    if(output)this.lastOutput=at;
    if(mic){
      if(this.lastMic===null || at-this.lastMic>250)this.micStart=at;
      this.lastMic=at;
      // Brief listener acknowledgments while the agent already speaks preserve
      // duplex audio. Sustained overlap takes the floor without an intent model.
      if(this.floor==='user' && output && wasOutput===false)this.interrupt('continued_explanation');
      else if(output && at-this.micStart>=this.sustainedMs)this.interrupt('sustained_speech');
      else if(!output && (this.lastOutput===null || at-this.lastOutput>=350))this.floor='user';
    }
    if(!mic && this.lastMic!==null && at-this.lastMic>=this.userQuietMs && !this.suppressed)this.floor='open';
    if(this.lastMic===null && !this.suppressed)this.floor='open';
    if(this.suppressed && at-this.availableSince>=Math.max(this.userQuietMs,this.outputQuietMs)
        && !mic && !output && this.lastMic!==null && at-this.lastMic>=this.userQuietMs
        && this.lastOutput!==null && at-this.lastOutput>=this.outputQuietMs){
      // Keep the stream running while muted: discarded audio is never queued
      // for replay. Release only at a locally measured quiet boundary, never a
      // provider instruction ack. This does not prove the old answer is done.
      this.suppressed=false;this.floor='open';this.interruption={...this.interruption,phase:'released'};
    }
    this.emit();
  }
  interrupt(reason='explicit_interrupt') {
    if(this.closed || !this.available || this.suppressed)return;
    this.floor='user';this.suppressed=true;this.lastMic=this.now();
    this.interruption={id:this.id(),reason,phase:'suppressed'};this.emit();
  }
  snapshot(){
    return {state:this.suppressed?'suppressed':this.available?'ready':'unknown',
      interruption_id:this.interruption?.id||null,output_active:this.output,
      floor:this.available?(this.mic===true?'user':this.floor):'unknown'};
  }
  emit(){
    const playback=this.snapshot();
    const value={playback,interruption:playback.state==='unknown' ? null : this.interruption};
    const key=JSON.stringify(value);if(key!==this.last){this.last=key;this.changed(value);}
  }
  close(){this.closed=true;}
}
