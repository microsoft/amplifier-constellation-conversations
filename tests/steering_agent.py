#!/usr/bin/env python3
"""Independent duplex ACP fixture. File release gates never call a model."""
import json, os, select, sys, time, uuid
from pathlib import Path

mode=os.environ.get('STEERING_MODE','native')
log=Path(os.environ['FIXTURE_LOG']); release=Path(os.environ['STEERING_RELEASE'])
receipt_file=log.with_suffix('.receipts.json')
receipts=json.loads(receipt_file.read_text()) if receipt_file.exists() else {}
active=None; sid=None; approved=False; inputs=[]; buffer=b''

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
def complete():
    global active,inputs
    if active is None:return
    for id in inputs:
        if mode in ('unsettled','regress-queued','received-only'):continue
        r=receipt(id,'incorporated')
        write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':''},'_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipt':r}}}}})
    write({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Completed the work once.'}}}})
    write({'id':active,'result':{'stopReason':'end_turn','_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipts':[] if mode=='regress-queued' else [receipts[id] for id in inputs]}}}})
    active=None;inputs=[]

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
        if mode!='generic' and not mode.startswith('cleanup-'):result['_meta']={'steering':{'supported':True,'version':1,'idleBehavior':['promptRequired'],'receiptsMethod':'_session/input_receipts','acknowledgeMethod':'_session/acknowledge_inputs'}}
        if mode.startswith('attention_'):result['_meta']['com.microsoft.amplifier.attention']={'version':1,'enabled':True,'method':'_amplifier/attention'}
        write({'id':value['id'],'result':result})
    elif method=='session/load' and log.with_name('reject-load').exists():
        write({'id':value['id'],'error':{'code':-32602,'message':'Fixture cannot restore this session'}})
    elif method in ('session/new','session/load'):
        sid=p.get('sessionId',str(uuid.uuid4()));write({'id':value['id'],'result':{'sessionId':sid}})
    elif method=='session/prompt':
        assert active is None,'overlapping prompts'
        active=value['id'];approved=not os.environ.get('STEERING_APPROVAL')
        if not approved:
            write({'id':'permission','method':'session/request_permission','params':{'sessionId':sid,'toolCall':{'toolCallId':'write-once','title':'Write once','kind':'edit'},'options':[{'optionId':'yes','name':'Allow once','kind':'allow_once'},{'optionId':'no','name':'Reject once','kind':'reject_once'}]}})
    elif method=='_amplifier/attention':
        write({'id':value['id'],'result':{'action':'hold' if mode=='attention_hold' else 'engage','id':'fixture-offer','ticket':'fixture-ticket'}})
    elif method=='_session/steering':
        assert p['_meta']['steering']['idleBehavior']=='promptRequired'
        id=p['_meta']['steering']['inputId']
        if mode=='generic':raise AssertionError('Generic peer received steering')
        if mode=='held-steer':
            while not log.with_name('steering-ack-release').exists():time.sleep(.01)
        if mode=='disconnect':receipt(id,'unknown');sys.exit(0)
        if mode in ('idle','attention_idle') and active is not None:
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
