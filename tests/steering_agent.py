#!/usr/bin/env python3
"""Independent duplex ACP fixture. File release gates never call a model."""
import json, os, select, sys, time, uuid
from pathlib import Path

mode=os.environ.get('STEERING_MODE','native')
log=Path(os.environ['FIXTURE_LOG']); release=Path(os.environ['STEERING_RELEASE'])
receipt_file=log.with_suffix('.receipts.json')
receipts=json.loads(receipt_file.read_text()) if receipt_file.exists() else {}
active=None; sid=None; approved=False; inputs=[]; buffer=b''
source_mode=mode.startswith('source-')
source_meta='com.microsoft.amplifier.source-delivery'
source_file=log.with_suffix('.source-receipts.json')
source_receipts=json.loads(source_file.read_text()) if source_file.exists() else {}
active_source=None

def record(value):
    with log.open('a') as f:f.write(json.dumps(value)+'\n')
def write(value):
    data=json.dumps({'jsonrpc':'2.0',**value})+'\n'
    if os.environ.get('STEERING_FRAGMENT'):
        for offset in range(0,len(data),17):sys.stdout.write(data[offset:offset+17]);sys.stdout.flush();time.sleep(.001)
    else:sys.stdout.write(data);sys.stdout.flush()
def receipt(id,state):
    receipts[id]={'inputId':id,'state':state,'turnId':'fixture-turn','recorded':state=='incorporated'}
    receipt_file.write_text(json.dumps(receipts))
    return receipts[id]
def save_sources():
    source_file.write_text(json.dumps(source_receipts))
def register_source(params,kind):
    metadata=params.get('_meta',{}).get(source_meta)
    assert metadata is not None,'Negotiated input omitted source metadata'
    assert metadata['version']==1 and metadata['inputId']
    assert params['sessionId']==sid,'Source input changed loaded session'
    assert len(metadata['sources'])==1,'Fixture expects only the new source, not history'
    source=metadata['sources'][0]
    assert set(source)=={'id','revision','occurredAt'}
    assert source['id'] and source['revision'] and source['occurredAt']>0
    input_id=metadata['inputId']
    if kind=='steering':
        assert params['_meta']['steering']['inputId']==input_id
    old=source_receipts.get(input_id)
    if old is not None:
        assert old['sessionId']==sid and old['metadata']==metadata,'Input provenance changed'
        assert old['kind']=='steering' and old['state']=='promptRequired' and kind=='prompt','Input replayed'
    source_receipts[input_id]={'metadata':metadata,'sessionId':sid,'kind':kind,'state':'submitted','response':None}
    save_sources()
    return input_id
def source_query(params):
    assert params['sessionId']==sid,'Receipt query changed loaded session'
    rows=[]
    for source in params['sources']:
        matches=[(key,row) for key,row in source_receipts.items() if row['sessionId']==sid and source in row['metadata']['sources']]
        if not matches:continue
        input_id,row=matches[-1]
        count=3 if mode=='source-many' else 1 if mode in ('source-one','source-work-only','source-disconnect') else 0
        work=[{'id':f'00000000-0000-4000-8000-{n+1:012d}','ownerAgentId':'11111111-1111-4111-8111-111111111111','revision':n+1,'state':'proposed' if n==0 else 'accepted','inputId':input_id if n<2 else '22222222-2222-4222-8222-222222222222'} for n in range(count)]
        item={'sourceId':source['id'],'revision':source['revision'],'occurredAt':source['occurredAt'],'inputId':input_id,'state':row['state'],'incorporated':False,'response':row['response'],'observedAt':int(time.time()*1000),'completion':'not_established','work':work,'workCoverage':'retained_session_records'}
        if mode=='source-work-only':item.update(inputId=None,state='unknown',response=None)
        if mode=='source-wrong-revision':item['revision']='different-source-revision'
        if mode=='source-wrong-time':item['occurredAt']+=1
        rows.append(item)
    return {'version':1,'receipts':rows,'coverage':{'delivery':'recent_session_records','work':'retained_session_records','missing':'unknown'}}
def complete():
    global active,inputs,active_source
    if active is None:return
    if active_source is not None:
        source_receipts[active_source]['response']={'kind':'result','stopReason':'end_turn'}
        save_sources()
    for id in inputs:
        if mode in ('unsettled','regress-queued','received-only'):continue
        r=receipt(id,'incorporated')
        write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':''},'_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipt':r}}}}})
    write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Completed the work once.'}}}})
    write({'id':active,'result':{'stopReason':'end_turn','_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipts':[] if mode=='regress-queued' else [receipts[id] for id in inputs]}}}})
    active=None;inputs=[];active_source=None

while True:
    if active is not None and release.exists() and approved:complete()
    if b'\n' not in buffer:
        ready,_,_=select.select([0],[],[],.01)
        if not ready:continue
        chunk=os.read(0,65536)
        if not chunk:break
        buffer+=chunk
        if b'\n' not in buffer:continue
    line,buffer=buffer.split(b'\n',1)
    value=json.loads(line);record(value)
    method=value.get('method');p=value.get('params',{})
    if method=='initialize':
        result={'protocolVersion':1,'agentCapabilities':{'loadSession':True},'agentInfo':{'name':'Steering fixture','version':'1'}}
        if mode!='generic' and not mode.startswith('cleanup-') and (not source_mode or mode in ('source-idle','source-idle-loss')):result['_meta']={'steering':{'supported':True,'version':1,'idleBehavior':['promptRequired'],'receiptsMethod':'_session/input_receipts','acknowledgeMethod':'_session/acknowledge_inputs'}}
        if source_mode and not (mode=='source-idle-loss' and log.with_name('profile-unavailable').exists()):
            result.setdefault('_meta',{})['io.github.bkrabach.agent-instance']={'inputContinuity':{'sourceDelivery':{'version':1,'metadata':source_meta,'receiptsMethod':'_amplifier/source_receipts','incorporationIsCompletion':False,'ordinaryPrompts':True,'promptRetry':'inspectOnly','workReferences':True,'retentionMs':86400000,'maxRecords':8192,'queryRecords':256,'workQueryRecords':1000,'workReferencesPerSource':64}}}
        if mode.startswith('attention_'):result['_meta']['com.microsoft.amplifier.attention']={'version':1,'enabled':True,'method':'_amplifier/attention'}
        write({'id':value['id'],'result':result})
    elif method=='session/load' and log.with_name('reject-load').exists():
        write({'id':value['id'],'error':{'code':-32602,'message':'Fixture cannot restore this session'}})
    elif method in ('session/new','session/load'):
        sid=p.get('sessionId',str(uuid.uuid4()));write({'id':value['id'],'result':{'sessionId':sid}})
    elif method=='session/prompt':
        assert active is None,'overlapping prompts'
        if source_mode:
            active_source=register_source(p,'prompt')
            if mode=='source-disconnect':
                source_receipts[active_source]['state']='unknown';save_sources();sys.exit(0)
        active=value['id'];approved=not os.environ.get('STEERING_APPROVAL')
        if not approved:
            write({'id':'permission','method':'session/request_permission','params':{'sessionId':sid,'toolCall':{'toolCallId':'write-once','title':'Write once','kind':'edit'},'options':[{'optionId':'yes','name':'Allow once','kind':'allow_once'},{'optionId':'no','name':'Reject once','kind':'reject_once'}]}})
    elif method=='_amplifier/attention':
        write({'id':value['id'],'result':{'action':'hold' if mode=='attention_hold' else 'engage','id':'fixture-offer','ticket':'fixture-ticket'}})
    elif method=='_session/steering':
        assert p['_meta']['steering']['idleBehavior']=='promptRequired'
        id=p['_meta']['steering']['inputId']
        if source_mode:assert register_source(p,'steering')==id
        if mode=='generic':raise AssertionError('Generic peer received steering')
        if mode=='held-steer':
            while not log.with_name('steering-ack-release').exists():time.sleep(.01)
        if mode=='disconnect':receipt(id,'unknown');sys.exit(0)
        if mode in ('idle','attention_idle','source-idle','source-idle-loss') and active is not None:
            if source_mode:source_receipts[id]['state']='promptRequired';save_sources()
            if mode=='source-idle-loss':log.with_name('profile-unavailable').write_text('profile no longer advertised')
            complete();write({'id':value['id'],'result':{'outcome':'promptRequired','_meta':{'com.microsoft.amplifier.attention':{'ticketState':'restored'}}}})
        elif active is None:write({'id':value['id'],'result':{'outcome':'promptRequired'}})
        elif mode.startswith('bad-ack-'):
            result={'outcome':'injected','_meta':{'steering':{'inputId':id}}}
            if mode=='bad-ack-silent':
                pass
            elif mode in ('bad-ack-type','bad-ack-id'):
                result['_meta']['steering']['inputId']=42 if mode=='bad-ack-type' else 'different-input'
                write({'id':value['id'],'result':result})
            else:
                write({'id':value['id'],'result':result,'error':{'code':-32601 if mode=='bad-ack-conflict' else -32000,'message':'Conflicting acknowledgment'}})
        else:
            if id not in inputs:inputs.append(id)
            receipt(id,'remoteQueued' if mode in ('unsettled','regress-queued') else 'received')
            if mode=='regress-queued':
                for state in ['remoteQueued','received','submitted']:
                    write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':''},'_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipt':{'inputId':id,'state':state}}}}}})
            if mode=='preack':
                r=receipt(id,'incorporated')
                write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':''},'_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipt':r}}}}})
            write({'id':value['id'],'result':{'outcome':'injected','_meta':{'steering':{'inputId':id,'state':'remoteQueued' if mode=='unsettled' else 'received','turnId':'fixture-turn'}}}})
    elif method=='_amplifier/source_receipts' and source_mode:
        if mode=='source-query-hold' and log.with_name('hold-source-query').exists():
            record({'inspection':'query-held','pid':os.getpid(),'sessionId':sid,'requestId':value['id']})
            deadline=time.monotonic()+30
            while not log.with_name('source-query-release').exists():
                assert time.monotonic()<deadline,'Source-query fixture gate was never released or disposed'
                time.sleep(.01)
        if mode=='source-query-error':write({'id':value['id'],'error':{'code':-32603,'message':'Fixture receipt query unavailable'}})
        else:write({'id':value['id'],'result':source_query(p)})
    elif method=='_session/input_receipts':write({'id':value['id'],'result':{'inputReceipts':list(receipts.values())}})
    elif method=='_session/acknowledge_inputs' and log.with_name('reject-ack').exists():
        write({'id':value['id'],'result':{}})
    elif method=='_session/acknowledge_inputs':
        for id in p['inputIds']:receipts[id]['reconciled']=True
        receipt_file.write_text(json.dumps(receipts));write({'id':value['id'],'result':{'inputReceipts':list(receipts.values())}})
    elif method=='session/cancel':
        if mode=='held-cancel':
            log.with_name('cancel-observed').write_text('cancel received')
            while not log.with_name('cancel-release').exists():time.sleep(.01)
        if active is not None:write({'id':active,'result':{'stopReason':'cancelled'}});active=None
    elif value.get('id')=='permission' and method is None:
        approved=True
        if value.get('result',{}).get('outcome',{}).get('optionId')=='yes':record({'effect':'write-once'})
    elif method:write({'id':value.get('id'),'error':{'code':-32601,'message':'Unsupported'}})

if mode.startswith('cleanup-'):
    record({'cleanup':'eof','pid':os.getpid()})
    # These must never become new answer text, approval, or host work after end_turn.
    write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'LATE ANSWER'}}}})
    write({'id':'late-write','method':'fs/write_text_file','params':{'sessionId':sid,'path':str(log.with_name('late-write')),'content':'not admitted'}})
    write({'id':'late-permission','method':'session/request_permission','params':{'sessionId':sid,'toolCall':{'toolCallId':'late','title':'Must not be admitted','kind':'edit'},'options':[{'optionId':'yes','kind':'allow_once','name':'Allow'}]}})
    if mode=='cleanup-early-eof':
        import subprocess
        child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        record({'cleanup':'descendant','pid':child.pid})
        os.close(1)
        time.sleep(120)
    elif mode=='cleanup-error':
        # Exceed the existing frame bound: terminal drain fails, not the prompt.
        os.write(1,b'x'*(24*1024*1024+1)+b'\n')
        time.sleep(120)
    else:
        while not log.with_name('cleanup-release').exists():time.sleep(.01)
        record({'cleanup':'finished','pid':os.getpid()})
