"""Packaged-runtime regression through synthetic ACP and native CLI protocols.

No provider requests or developer configuration are used. Hand-authored frames
exercise integration handling; they are not evidence of a vendor's live behavior.
"""
import json, os, signal, socket, sqlite3, subprocess, sys, tempfile, time
from pathlib import Path
import urllib.request, urllib.error

import argparse
parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
parser.add_argument("--report", type=Path)
args = parser.parse_args()
BINARY = args.binary.resolve()
REPORT = args.report
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
report = {'runtime_version': '0.2.2+layabox.9', 'source_commit': '57566db2bfb29331a022d2e78cc8e06e3144d4d2',
          'scope': 'packaged runtime preflight + synthetic ACP protocol fixtures; no real provider prompts',
          'catalog': [], 'fixtures': []}
NATIVE_ONLY=False

STUB = r'''
import sys,json,time
from pathlib import Path
scenario,capture=sys.argv[1:3]
def send(x):
 print(json.dumps(x),flush=True)
for line in sys.stdin:
 try:r=json.loads(line)
 except ValueError:continue
 with open(capture,'a') as f:f.write(json.dumps(r)+'\n')
 method=r.get('method');id=r.get('id')
 if method=='initialize':
  result={'protocolVersion':1,'agentCapabilities':{'loadSession':False,'mcpCapabilities':{'http':scenario!='downgrade','sse':scenario!='downgrade'}},'authMethods':[],'agentInfo':{'name':'Audit Fixture','version':'1.0.0'}}
 elif method in ('session/new','session/load'):
  result={'sessionId':'audit-session'}
 elif method=='session/prompt':
  if scenario=='rpc_error':
   send({'jsonrpc':'2.0','id':id,'error':{'code':-32603,'message':'HTTP 429 rate limit exceeded'}});continue
  if scenario in ('silent_stderr','empty_billing','empty_billing_delayed','process_exit'):
   print('ERROR HTTP 402: Insufficient account balance',file=sys.stderr,flush=True)
  if scenario=='empty_billing_delayed':time.sleep(.25)
  if scenario=='process_exit':sys.exit(3)
  if scenario in ('silent_stderr','silent_internal'):continue
  result={'stopReason':'end_turn'}
 elif method=='session/cancel':continue
 else:result={}
 if id is not None:send({'jsonrpc':'2.0','id':id,'result':result})
'''

NATIVE_STUB = r'''
import json,sys
from pathlib import Path
scenario=Path(__file__).stem
is_claude=scenario.startswith('claude')
if '--version' in sys.argv:
 print('2.1.287' if is_claude else 'codex-cli 0.151.0');sys.exit(0)
sid='audit-native-thread'
def capture(direction,x):
 with open(str(Path(__file__))+'.ndjson','a') as f:f.write(json.dumps({'direction':direction,'frame':x})+'\n')
def send(x):
 capture('tx',x);print(json.dumps(x),flush=True)
if is_claude:
 send({'type':'system','subtype':'init','session_id':sid,'tools':[],'model':'audit-model','permissionMode':'default'})
for line in sys.stdin:
 try:r=json.loads(line)
 except ValueError:continue
 capture('rx',r)
 if is_claude:
  if r.get('type')=='control_request':
   send({'type':'control_response','response':{'subtype':'success','request_id':r.get('request_id'),'response':{'models':[],'commands':[]}}})
  elif r.get('type')=='user':
   if scenario.endswith('retry'):
    send({'type':'system','subtype':'api_retry','session_id':sid,'attempt':1,'max_retries':5,'error_status':429,'retry_delay_ms':1000})
   else:
    send({'type':'result','subtype':'error_during_execution','is_error':True,'session_id':sid,'result':'HTTP 429 native fixture failure','num_turns':1,'usage':{'input_tokens':0,'output_tokens':0}})
 else:
  method=r.get('method');id=r.get('id')
  if method=='initialize':result={'userAgent':'codex-cli/0.161.0'}
  elif method in ('thread/start','thread/resume'):
   result={'thread':{'id':sid,'status':{'type':'idle'}},'model':'audit-model','modelProvider':'openai','cwd':str(Path.cwd()),'approvalPolicy':'untrusted','sandbox':{'type':'workspaceWrite'},'reasoningEffort':None}
  elif method=='turn/start':result={'turn':{'id':'audit-turn','items':[],'status':'inProgress'}}
  elif method=='model/list':result={'data':[],'nextCursor':None}
  else:result={}
  if id is not None:send({'jsonrpc':'2.0','id':id,'result':result})
  if method in ('thread/start','thread/resume'):
   send({'jsonrpc':'2.0','method':'thread/started','params':{'thread':{'id':sid,'status':{'type':'idle'},'cwd':str(Path.cwd())}}})
  if method=='turn/start':
   send({'jsonrpc':'2.0','method':'turn/started','params':{'threadId':sid,'turn':{'id':'audit-turn','items':[],'status':'inProgress'}}})
   send({'jsonrpc':'2.0','method':'error','params':{'threadId':sid,'turnId':'audit-turn','willRetry':scenario.endswith('retry'),'error':{'message':'Reconnecting 1/5' if scenario.endswith('retry') else 'HTTP 429 native fixture failure','additionalDetails':'Synthetic provider rate limit'}}})
'''

with tempfile.TemporaryDirectory(prefix='laya-agent-audit-') as tmp:
 root=Path(tmp);data=root/'data';out=None;child=None;base=None
 stub=root/'stub.py';stub.write_text(STUB)
 def stop():
  global child,out
  if child and child.poll() is None:
   child.terminate()
   try:child.wait(timeout=8)
   except subprocess.TimeoutExpired:
    os.killpg(child.pid,signal.SIGKILL);child.wait(timeout=3)
  child=None
  if out:out.close();out=None
 def start():
  global child,out,base
  with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
  base=f'http://127.0.0.1:{port}'
  out=(root/'process.log').open('a')
  child=subprocess.Popen([str(BINARY),'--host','127.0.0.1','--port',str(port),'--data-dir',str(data),
    '--log-dir',str(root/'logs'),'--work-dir',str(root),'--parent-pid',str(os.getpid()),
    '--local','--managed-resources-mode','download'],cwd=root,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
  deadline=time.monotonic()+30
  while time.monotonic()<deadline and child.poll() is None:
   try:
    request('GET','/health');return
   except Exception:time.sleep(.1)
  raise RuntimeError((root/'process.log').read_text(errors='replace')[-2500:])
 def request(method,route,body=None):
  payload=None if body is None else json.dumps(body).encode()
  req=urllib.request.Request(base+route,data=payload,method=method,headers={'Content-Type':'application/json'})
  try:
   with opener.open(req,timeout=12) as response:d=json.load(response)
  except urllib.error.HTTPError as e:raise RuntimeError(f'{method} {route}: {e.code} {e.read().decode()[:700]}')
  return d.get('data',d) if isinstance(d,dict) else d
 def create(id,backend,typ='acp'):
  return request('POST','/api/conversations',{'type':typ,'name':'Isolated compatibility audit',
   'extra':{'workspace':str(root),'custom_workspace':True,'agent_id':id,'backend':backend,
    'selected_mcp_server_ids':[], 'selected_session_mcp_servers':[{'id':'audit-http','name':'audit-http',
      'transport':{'type':'http','url':'http://127.0.0.1:9/mcp','headers':{}}}]}})
 try:
  start()
  with sqlite3.connect(data/'aionui-backend.db') as c:
   rows=c.execute('SELECT id,name,backend,agent_type,agent_capabilities FROM agent_metadata ORDER BY sort_order').fetchall()
  for id,name,b,typ,caps in ([] if NATIVE_ONLY else rows):
   d=json.loads(caps) if caps else {}
   entry={'id':id,'name':name,'backend':b,'type':typ,'cached_mcp':d.get('mcp_capabilities',d.get('mcpCapabilities'))}
   if typ in ('acp','aionrs','antigravity'):
    try:
     conv=create(id,b,typ);entry['http_preflight']=conv.get('extra',{}).get('mcp_statuses',[])
    except Exception as e:entry['error']=str(e)
   else:entry['http_preflight']='separate non-ACP transport; source review only'
   report['catalog'].append(entry)
  print('CATALOG',len(rows),'entries; retaining previous preflight' if NATIVE_ONLY else 'entries checked; no real agent started',flush=True)
  stop()
  scenarios=['cold','warm','downgrade','rpc_error','empty_billing','empty_billing_delayed','empty_clean','process_exit','silent_stderr','silent_internal']
  with sqlite3.connect(data/'aionui-backend.db') as c:
   for scenario in scenarios:
    caps=None if scenario=='cold' else json.dumps({'mcp_capabilities':{'http':True,'sse':True}})
    c.execute('INSERT INTO agent_metadata (id,agent_id,name,backend,agent_type,agent_source,agent_source_info,enabled,command,args,env,behavior_policy,agent_capabilities,sort_order,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
     ('audit-'+scenario,'audit-'+scenario,'Audit '+scenario,'audit-'+scenario,'acp','custom','{}',1,sys.executable,
      json.dumps(['-u',str(stub),scenario,str(root/(scenario+'.ndjson'))]),'[]','{}',caps,9999,1,1))
   for scenario in ['claude-retry','claude-error','codex-retry','codex-error']:
    native_stub=root/(scenario+'.py')
    native_stub.write_text('#!'+sys.executable+'\n'+NATIVE_STUB)
    native_stub.chmod(0o700)
    backend=scenario.split('-')[0]
    c.execute('INSERT INTO agent_metadata (id,agent_id,name,backend,agent_type,agent_source,agent_source_info,enabled,command,command_override,args,env,behavior_policy,sort_order,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
     ('audit-'+scenario,'audit-'+scenario,'Audit '+scenario,backend,'acp','custom','{}',1,str(native_stub),str(native_stub),'[]','[]','{}',9999,1,1))
  start()
  for scenario in ([] if NATIVE_ONLY else scenarios):
   id='audit-'+scenario;conv=create(id,id);cid=conv['id']
   entry={'scenario':scenario,'before':conv.get('extra',{}).get('mcp_statuses')}
   try:
    entry['ensure']=request('POST',f'/api/conversations/{cid}/runtime/ensure')
    capfile=root/(scenario+'.ndjson')
    captured=[json.loads(x) for x in capfile.read_text().splitlines()]
    entry['injected_mcp']=[r.get('params',{}).get('mcpServers',[]) for r in captured if r.get('method')=='session/new']
    time.sleep(.15)
    after=request('GET',f'/api/conversations/{cid}')
    entry['after_handshake']=after.get('extra',{}).get('mcp_statuses')
    with sqlite3.connect(data/'aionui-backend.db') as c:
     entry['persisted_capabilities']=json.loads(c.execute('SELECT agent_capabilities FROM agent_metadata WHERE id=?',(id,)).fetchone()[0])
    if scenario not in ('cold','warm','downgrade'):
     entry['send']=request('POST',f'/api/conversations/{cid}/messages',{'content':'isolated synthetic failure probe'})
     time.sleep(21 if scenario=='silent_internal' else 3)
     entry['conversation_after_prompt']=request('GET',f'/api/conversations/{cid}')
     entry['messages']=request('GET',f'/api/conversations/{cid}/messages')
   except Exception as e:entry['error']=str(e)
   finally:
    try:request('DELETE',f'/api/conversations/{cid}')
    except Exception as e:entry['cleanup_error']=str(e)
   report['fixtures'].append(entry)
   print('FIXTURE',scenario,'ERROR '+entry['error'] if 'error' in entry else 'complete',flush=True)
  report['native_fixtures']=[]
  for scenario in ['claude-retry','claude-error','codex-retry','codex-error']:
   conv=create('audit-'+scenario,scenario.split('-')[0]);cid=conv['id']
   entry={'scenario':scenario}
   try:
    entry['ensure']=request('POST',f'/api/conversations/{cid}/runtime/ensure')
    entry['send']=request('POST',f'/api/conversations/{cid}/messages',{'content':'synthetic native status probe'})
    time.sleep(1.3)
    entry['conversation']=request('GET',f'/api/conversations/{cid}')
    entry['messages']=request('GET',f'/api/conversations/{cid}/messages')
    capture_file=root/(scenario+'.py.ndjson')
    entry['wire']=[json.loads(x) for x in capture_file.read_text().splitlines()] if capture_file.exists() else []
   except Exception as e:entry['error']=str(e)
   finally:
    try:request('DELETE',f'/api/conversations/{cid}')
    except Exception as e:entry['cleanup_error']=str(e)
   report['native_fixtures'].append(entry)
   print('NATIVE',scenario,'ERROR '+entry['error'] if 'error' in entry else 'complete',flush=True)
  for p in (root/'logs').rglob('*.log'):
   if p.is_file():
    for line in p.read_text(errors='replace').splitlines():
     if any(x in line for x in ['transport unsupported by ACP','empty turn classified','without visible output','ACP send_message failed']):
      report.setdefault('diagnostic_log',[]).append(line)
  by_scenario={f['scenario']:f for f in report['fixtures']}
  assert not any('error' in e for e in report['catalog']), 'catalog preflight contained request errors'
  assert not any('error' in e for e in report['fixtures']), 'protocol fixture failed to execute'
  assert by_scenario['cold']['injected_mcp'][0], 'live HTTP capability must include cold MCP candidates'
  assert by_scenario['cold']['after_handshake'][0]['status']=='loaded'
  assert by_scenario['downgrade']['injected_mcp']==[[]]
  assert by_scenario['downgrade']['after_handshake'][0]['status']=='unsupported'
  assert by_scenario['cold']['persisted_capabilities']['mcp_capabilities']['http'] is True
  assert by_scenario['warm']['injected_mcp'][0], 'warm HTTP injection must work'
  for scenario in ['rpc_error','empty_clean','empty_billing_delayed','process_exit']:
   assert by_scenario[scenario]['conversation_after_prompt']['runtime']['is_processing'] is False, scenario
  for scenario in ['silent_stderr','silent_internal']:
   assert by_scenario[scenario]['conversation_after_prompt']['runtime']['is_processing'] is True, scenario
  native={f['scenario']:f for f in report['native_fixtures']}
  for scenario,f in native.items():
   assert 'error' not in f, (scenario,f.get('error'))
   if scenario.startswith('codex'):
    assert any(w['direction']=='rx' and w['frame'].get('method')=='turn/start' for w in f['wire']), 'Codex fixture did not reach a turn'
   assert f['conversation']['runtime']['is_processing'] is scenario.endswith('retry'), scenario
  assert any(m.get('content',{}).get('code')=='CODEX_RETRYING' for m in native['codex-retry']['messages']['items'])
  assert any('retrying' in m.get('content',{}).get('content','') for m in native['claude-retry']['messages']['items'])
  for scenario in ['silent_stderr','silent_internal']:
   assert any(m.get('content',{}).get('type')=='warning' for m in by_scenario[scenario]['messages']['items']), scenario
  for scenario in ['empty_billing','empty_billing_delayed']:
   assert any(m.get('status')=='error' for m in by_scenario[scenario]['messages']['items']), scenario
  report['harness_assertions']='passed'
 finally:
  stop()
  if REPORT is not None:
   REPORT.write_text(json.dumps(report,ensure_ascii=False,indent=2))
  print('Agent MCP negotiation and wait feedback smoke completed.',flush=True)
