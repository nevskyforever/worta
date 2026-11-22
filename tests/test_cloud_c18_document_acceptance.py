"""Real PostgreSQL + production C11 + native file-backed document authority acceptance."""
from __future__ import annotations
import json, os, sqlite3
from datetime import datetime, timezone
from uuid import uuid4
import pytest
from sqlalchemy import text
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard
from test_cloud_auth import cloud_client, migrated_database, create_user, login
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_structural_cross_runtime import event, structural, structural_publish, structural_sync
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _native_bridge, _headers
NOW='2026-10-03T00:00:00.000000Z'
def document(tmp,database_path,identity,user,step,**values):
    return _native_bridge(tmp,dict(action='document',database_path=str(database_path),local_account_id=identity['local_account_id'],device_id=identity['device_id'],canonical_user_id=user,project_id=PROJECT_ID,step=step,**values))
def content(value):return dict(type='doc',content=[dict(type='paragraph',content=[dict(type='text',text=value)])])
def support(client,headers,device):
    r=client.put('/api/v3/sync/encrypted/document-reader-capabilities',headers=headers,json=dict(device_id=device,frame_version=1,codec_id=10,codec_version=1,reader_version=1,compression_zero=True));assert r.status_code==204,r.text

def emit(client,tmp,token,path,identity,user,amk,lose=False):
    sent=[]
    for item in document(tmp,path,identity,user,'pending'):
        h=item['event']['header'];sealed=_crypto_bridge(tmp,dict(action='document_seal',frame=item['frame'],amk=amk))
        document(tmp,path,identity,user,'seal',event_id=h['event_id'],frame=item['frame'],nonce=sealed['nonce'],ciphertext=sealed['ciphertext'])
        wire={k:h[k] for k in ('event_id','project_id','entity_id','revision','updated_at')};wire.update(entity_type='document',operation='event',deleted_at=None)
        request=dict(protocol_version=3,encrypted_sync_version=3,device_id=identity['device_id'],items=[dict(event=wire,object=sealed['object'])])
        response=client.post('/api/v3/sync/encrypted/push',headers=_headers(token),json=request);assert response.status_code==200,response.text
        replay=client.post('/api/v3/sync/encrypted/push',headers=_headers(token),json=request);assert replay.status_code==200 and replay.json()['results'][0]['duplicate'];assert replay.json()['results'][0]['server_sequence']==response.json()['results'][0]['server_sequence']
        if not lose:
            r=response.json()['results'][0];document(tmp,path,identity,user,'receipt',event_id=h['event_id'],server_sequence=r['server_sequence'],duplicate=r['duplicate'])
        else:
            frozen=next(x for x in document(tmp,path,identity,user,'pending',sealed=True) if x['event']['header']['event_id']==h['event_id']);assert frozen['nonce']==sealed['nonce'] and frozen['ciphertext']==sealed['ciphertext'] and frozen['frame']==item['frame']
        sent.append(request)
    return sent

def receive(client,tmp,token,path,identity,user,amk):
    with sqlite3.connect(path) as db:cursor=db.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
    response=client.get('/api/v3/sync/encrypted/pull',headers=_headers(token),params=dict(device_id=identity['device_id'],since=cursor,limit=100,protocol_version=3,encrypted_sync_version=3));assert response.status_code==200,response.text
    outcomes=[]
    for item in response.json()['items']:
        assert item['event']['entity_type']=='document'
        opened=_crypto_bridge(tmp,dict(action='document_open',canonical_user_id=user,amk=amk,item=item))
        row=dict(item['event']);row['source_device_id']=row.pop('device_id');row['envelope']=item['object'];row['updated_at']=datetime.fromisoformat(row['updated_at'].replace('Z','+00:00')).astimezone(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z')
        command=dict(account_id=identity['local_account_id'],device_id=identity['device_id'],canonical_user_id=user,expected_cursor=cursor,next_cursor=row['server_sequence'],has_more=False,items=[row])
        outcomes.append(document(tmp,path,identity,user,'receive',command=command,opened=opened));cursor=row['server_sequence']
    return outcomes

def stored(path,id):
    with sqlite3.connect(path) as db:
        r=db.execute('SELECT id,project_id,stage_id,title,content_json,extensions_json FROM documents WHERE id=?',(id,)).fetchone()
        return None if r is None else dict(id=r[0],project_id=r[1],stage_id=r[2],title=r[3],content_json=json.loads(r[4]),extensions=json.loads(r[5]))
def heads(tmp,path,identity,user,id):return document(tmp,path,identity,user,'expected',document_id=id)
def owner(tmp,path,identity,user,id):return next(o for o in document(tmp,path,identity,user,'view')['owners'] if o['entity_id']==id)
def choose(tmp,path,identity,user,id,selected):
    o=owner(tmp,path,identity,user,id);return document(tmp,path,identity,user,'decide',decision=dict(project_id=PROJECT_ID,document_id=id,expected_tips=o['tips'],expected_local=o['local'],selected_event_id=selected))

def test_documents_real_two_devices_moves_conflicts_extensions_and_gate(cloud_client,tmp_path):
    client,engine=cloud_client;user=str(create_user(engine));token=login(client).json()['access_token'];headers=_headers(token)
    a,ia=provision(tmp_path,'document-A',user,True);b,ib=provision(tmp_path,'document-B',user,False)
    for path,identity in ((a,ia),(b,ib)):
        assert client.put(f"/api/v1/sync/devices/{identity['device_id']}",headers=headers).status_code==200
        document(tmp_path,path,identity,user,'init')
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.work_method','app')")
    boot=bootstrap(tmp_path,a,ia,'bootstrap_prepare')['bootstrap_id'];assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap',headers=headers,json=dict(bootstrap_id=boot,device_id=ia['device_id'])).status_code==200
    bootstrap(tmp_path,a,ia,'bootstrap_prepare_capture',bootstrap_id=boot,remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete',headers=headers,json=dict(bootstrap_id=boot,device_id=ia['device_id'],initial_event_count=0,initial_max_server_sequence=0)).status_code==200
    bootstrap(tmp_path,a,ia,'bootstrap_confirm_active',bootstrap_id=boot,remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover',headers=headers,json=dict(expected_cutover_epoch=0)).status_code==200
    for identity in (ia,ib):assert client.post('/api/v3/sync/encrypted/reader-ready',headers=headers,json=dict(device_id=identity['device_id'],reader_transport_version=3)).status_code==204
    assert client.post('/api/v3/sync/encrypted/cutover',headers=headers,json=dict(expected_cutover_epoch=1)).status_code==200
    native(tmp_path,a,ia,user,'begin');amk=list(os.urandom(32));meta,_=publish(client,tmp_path,token,a,ia,user,amk);assert sync(client,tmp_path,token,a,ia,user,amk)['view']['state']=='active'
    bootstrap(tmp_path,b,ib,'bootstrap_import',bootstrap_id=boot,remote_high_water=1,display_name=meta['metadata']['name'],authenticated_metadata=meta['metadata']);assert sync(client,tmp_path,token,b,ib,user,amk)['view']['state']=='active'
    stage_roots={}
    for i in (1,2,3):
        s=event(ia,user,boot,meta['header']['event_id'],entity=f'S{i}');stage=s['stage'];stage['work_method']='app';stage_roots[f'S{i}']=s
        with sqlite3.connect(a) as db:
            db.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',(f'S{i}',PROJECT_ID,stage['name'],stage['goal'],int(stage['infinite']),stage['unit'],stage['status'],stage['created_at'],json.dumps(stage)));db.execute('INSERT INTO stage_order VALUES(?,?,?)',(f'S{i}',PROJECT_ID,i-1))
        structural(tmp_path,a,ia,user,'capture',stage_id=f'S{i}');structural_publish(client,tmp_path,token,a,ia,user,amk,s)
    for path,identity in ((a,ia),(b,ib)):structural_sync(client,tmp_path,token,path,identity,user,amk)
    ordering=event(ia,user,boot,meta['header']['event_id'],order=['S1','S2','S3'],heads={id:[stage_roots[id]['header']['event_id']] for id in ('S1','S2','S3')})
    structural_publish(client,tmp_path,token,a,ia,user,amk,ordering)
    for path,identity in ((a,ia),(b,ib)):structural_sync(client,tmp_path,token,path,identity,user,amk)
    p=document(tmp_path,a,ia,user,'save',content=content('Secret portable manuscript'));assert 'error' not in p,p;pid=p['document_id']
    stage=document(tmp_path,a,ia,user,'save',stage_id='S3',content=content('Stage private text'));assert 'error' not in stage,stage;sid=stage['document_id']
    renamed=document(tmp_path,a,ia,user,'rename',title='Private document heading',expected=heads(tmp_path,a,ia,user,pid));assert 'error' not in renamed,renamed
    assert document(tmp_path,a,ia,user,'pending')==[]
    for identity in (ia,ib):
        assert client.put('/api/v3/sync/encrypted/note-reader-capabilities',headers=headers,json=capabilities(identity['device_id'])).status_code==204;support(client,headers,identity['device_id'])
    third=str(uuid4());assert client.put(f'/api/v1/sync/devices/{third}',headers=headers).status_code==200;assert client.put('/api/v3/sync/encrypted/note-reader-capabilities',headers=headers,json=capabilities(third)).status_code==204
    assert client.get('/api/v3/sync/encrypted/document-reader-capabilities',headers=headers).json()==dict(ready=False,missing_devices=1)
    view=document(tmp_path,a,ia,user,'begin');assert len(view['owners'])==2
    pending=document(tmp_path,a,ia,user,'pending');item=pending[0];h=item['event']['header'];sealed=_crypto_bridge(tmp_path,dict(action='document_seal',frame=item['frame'],amk=amk));wire={k:h[k] for k in ('event_id','project_id','entity_id','revision','updated_at')};wire.update(entity_type='document',operation='event',deleted_at=None)
    assert client.post('/api/v3/sync/encrypted/push',headers=headers,json=dict(protocol_version=3,encrypted_sync_version=3,device_id=ia['device_id'],items=[dict(event=wire,object=sealed['object'])])).status_code==409
    support(client,headers,third);assert document(tmp_path,a,ia,user,'begin')==view
    assert len(emit(client,tmp_path,token,a,ia,user,amk,lose=True))==2
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied','applied']
    assert stored(a,pid)==stored(b,pid) and stored(a,sid)==stored(b,sid)
    with sqlite3.connect(b) as db:assert db.execute('SELECT count(*) FROM document_bindings').fetchone()[0]==0
    # C18.6.02: each device explicitly compares its own file; no implicit copy.
    import hashlib, io, zipfile
    def local_docx(value, tag):
        out=io.BytesIO()
        with zipfile.ZipFile(out,'w') as z:
            z.writestr('word/document.xml',f'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{value}</w:t></w:r></w:p></w:body></w:document>')
            z.writestr('local-container-note',tag)
        return out.getvalue()
    paths=[tmp_path/'C18_SECRET_PATH_A.docx',tmp_path/'C18_SECRET_PATH_B.docx']
    text_value='Secret portable manuscript'
    for path,identity,word,tag in ((a,ia,paths[0],'A'),(b,ib,paths[1],'B')):
        word.write_bytes(local_docx(text_value,tag));raw=word.read_bytes();before=heads(tmp_path,path,identity,user,pid)
        linked=document(tmp_path,path,identity,user,'reattach',path=str(word));assert 'error' not in linked,linked
        assert linked['sync_state']=='synced';assert word.read_bytes()==raw;assert heads(tmp_path,path,identity,user,pid)==before;assert document(tmp_path,path,identity,user,'pending')==[]
    for path,other in ((a,paths[1]),(b,paths[0])):
        with sqlite3.connect(path) as db:assert str(other) not in json.dumps(db.execute('SELECT * FROM document_bindings').fetchall())
    paths[1].write_bytes(local_docx('B external explicit import','B2'));before=heads(tmp_path,b,ib,user,pid);raw=paths[1].read_bytes()
    proposal=document(tmp_path,b,ib,user,'resolve_binding',choice='compare');assert proposal['sync_state']=='external_proposal';assert heads(tmp_path,b,ib,user,pid)==before;assert paths[1].read_bytes()==raw
    imported=document(tmp_path,b,ib,user,'resolve_binding',choice='import');assert imported['sync_state']=='external_import_pending',imported
    pending=document(tmp_path,b,ib,user,'pending')
    for item in pending:
        canonical=bytes(item['frame']).decode('utf8',errors='ignore');assert 'C18_SECRET_PATH' not in canonical;assert 'SOURCE-ID-C18-LOCAL-ONLY' not in canonical;assert 'document_bindings' not in canonical
    requests=emit(client,tmp_path,token,b,ib,user,amk)
    assert 'C18_SECRET_PATH' not in json.dumps(requests)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
    assert document(tmp_path,b,ib,user,'poll_binding')['sync_state']=='synced'
    paths[1].write_bytes(local_docx('Preserve this external edit','B3'));document(tmp_path,b,ib,user,'resolve_binding',choice='compare');raw=paths[1].read_bytes()
    # Proposal has a frozen authenticated head and cannot overwrite a later edit.
    document(tmp_path,a,ia,user,'save',content=content('Cloud newer'),expected=heads(tmp_path,a,ia,user,pid));emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):receive(client,tmp_path,token,path,identity,user,amk)
    assert document(tmp_path,b,ib,user,'resolve_binding',choice='import')['error']=='external_head_stale';assert paths[1].read_bytes()==raw
    document(tmp_path,b,ib,user,'resolve_binding',choice='compare');paths[1].write_bytes(local_docx('Changed after comparison','B4'));raw=paths[1].read_bytes()
    assert document(tmp_path,b,ib,user,'resolve_binding',choice='cloud')['error']=='external_hash_stale';assert paths[1].read_bytes()==raw
    document(tmp_path,b,ib,user,'resolve_binding',choice='compare');before=heads(tmp_path,b,ib,user,pid)
    written=document(tmp_path,b,ib,user,'resolve_binding',choice='cloud');assert written['sync_state']=='synced',written;assert heads(tmp_path,b,ib,user,pid)==before;assert document(tmp_path,b,ib,user,'pending')==[]
    for path,identity in ((a,ia),(b,ib)):document(tmp_path,path,identity,user,'resolve_binding',choice='unlink')
    with engine.connect() as db:
        for table in ('sync_events','encrypted_objects','sync_devices','cloud_projects','encrypted_blobs'):
            rows=db.execute(text(f'SELECT row_to_json(t)::text FROM {table} t')).scalars().all();assert not any('C18_SECRET_PATH' in r or 'SOURCE-ID-C18-LOCAL-ONLY' in r for r in rows)
    # A frozen document reference remains valid through an accepted Stage rename.
    from test_cloud_c18_structural_integration import publish_pending,pull_structure
    saved=document(tmp_path,a,ia,user,'save',stage_id='S3',content=content('Before Stage rename'),expected=heads(tmp_path,a,ia,user,sid));assert 'error' not in saved,saved
    with sqlite3.connect(a) as db:proposed=json.loads(db.execute("SELECT payload_json FROM stages WHERE id='S3'").fetchone()[0])
    proposed['name']='Renamed live Stage';assert structural(tmp_path,a,ia,user,'edit',stage_id='S3',proposed=proposed)
    publish_pending(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):pull_structure(client,tmp_path,token,path,identity,user,amk)
    emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
    assert stored(a,sid)==stored(b,sid)
    # Production editor writers on both devices and all same-ID scope moves.
    for path,identity,value in ((a,ia,'A ordinary'),(b,ib,'B ordinary')):
        result=document(tmp_path,path,identity,user,'save',stage_id=stored(path,pid)['stage_id'],content=content(value),expected=heads(tmp_path,path,identity,user,pid));assert 'error' not in result,result
        emit(client,tmp_path,token,path,identity,user,amk)
        for peer,peer_id in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,peer,peer_id,user,amk)==['applied']
    for target in ('S1','S2',None):
        assert document(tmp_path,a,ia,user,'move',document_id=pid,stage_id=target,expected=heads(tmp_path,a,ia,user,pid)) is True
        emit(client,tmp_path,token,a,ia,user,amk)
        for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied'];assert stored(path,pid)['stage_id']==target
    before=stored(a,pid);assert document(tmp_path,a,ia,user,'move',document_id=pid,stage_id='S3',expected=heads(tmp_path,a,ia,user,pid))['error']=='document_scope_occupied';assert stored(a,pid)==before and stored(a,sid) is not None
    assert document(tmp_path,a,ia,user,'move',document_id=pid,stage_id='fabricated',expected=heads(tmp_path,a,ia,user,pid)).get('error')
    # Whole document concurrent edits; explicit full-tip choice and stale decision.
    for path,identity,value in ((a,ia,'A concurrent'),(b,ib,'B concurrent')):
        result=document(tmp_path,path,identity,user,'save',content=content(value),expected=heads(tmp_path,path,identity,user,pid));assert 'error' not in result,result
    for path,identity in ((a,ia),(b,ib)):emit(client,tmp_path,token,path,identity,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert 'conflict_preserved' in receive(client,tmp_path,token,path,identity,user,amk)
    o=owner(tmp_path,a,ia,user,pid);assert len(o['tips'])==2;decision=dict(project_id=PROJECT_ID,document_id=pid,expected_tips=o['tips'],expected_local=o['local'],selected_event_id=o['tips'][0]);assert isinstance(document(tmp_path,a,ia,user,'decide',decision=decision),str)
    emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
    assert stored(a,pid)==stored(b,pid);assert document(tmp_path,a,ia,user,'decide',decision=decision)['error']=='document_resolution_stale'
    # Scope/content/delete races use the same original causal head on two devices.
    for left,right in [('edit','S1'),('S1','S2'),('delete','edit'),('delete','S2')]:
        for path,identity,operation in ((a,ia,left),(b,ib,right)):
            expected=heads(tmp_path,path,identity,user,pid)
            if operation=='edit':
                result=document(tmp_path,path,identity,user,'save',stage_id=stored(path,pid)['stage_id'],content=content(f'{left}/{right}/{identity["device_id"]}'),expected=expected)
            elif operation=='delete':result=document(tmp_path,path,identity,user,'delete',document_id=pid,expected=expected)
            else:result=document(tmp_path,path,identity,user,'move',document_id=pid,stage_id=operation,expected=expected)
            assert result is True or 'error' not in result,result
        for path,identity in ((a,ia),(b,ib)):emit(client,tmp_path,token,path,identity,user,amk)
        for path,identity in ((a,ia),(b,ib)):assert 'conflict_preserved' in receive(client,tmp_path,token,path,identity,user,amk)
        o=owner(tmp_path,a,ia,user,pid);assert len(o['tips'])==2
        selected=next(v['event_id'] for v in o['versions'] if v['document'] is not None)
        assert isinstance(choose(tmp_path,a,ia,user,pid,selected),str)
        emit(client,tmp_path,token,a,ia,user,amk)
        for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
        assert stored(a,pid)==stored(b,pid)
        if stored(a,pid)['stage_id'] is not None:
            assert document(tmp_path,a,ia,user,'move',document_id=pid,stage_id=None,expected=heads(tmp_path,a,ia,user,pid)) is True
            emit(client,tmp_path,token,a,ia,user,amk)
            for path,identity in ((a,ia),(b,ib)):receive(client,tmp_path,token,path,identity,user,amk)
    # Explicit Word proposal is read again; bindings remain device-local.
    import hashlib,io,zipfile
    word=tmp_path/'private-only-A.docx';buf=io.BytesIO()
    with zipfile.ZipFile(buf,'w') as z:
        z.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Explicit external manuscript</w:t></w:r></w:p></w:body></w:document>')
    word.write_bytes(buf.getvalue());digest=hashlib.sha256(word.read_bytes()).hexdigest()
    with sqlite3.connect(a) as db:db.execute("INSERT INTO document_bindings(id,document_id,binding_type,external_path,payload_json) VALUES('word-local',?,'word',?,'{}')",(pid,str(word)))
    accepted=document(tmp_path,a,ia,user,'external',content=content('Explicit external manuscript'),source_hash=digest,expected=heads(tmp_path,a,ia,user,pid));assert 'error' not in accepted,accepted
    emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
    assert stored(a,pid)==stored(b,pid)
    word.write_bytes(b'changed proposal')
    assert document(tmp_path,a,ia,user,'external',content=content('Explicit external manuscript'),source_hash=digest,expected=heads(tmp_path,a,ia,user,pid))['error']=='document_external_proposal_stale'
    # A causal tombstone removes only canonical/local binding rows, never the file.
    assert document(tmp_path,a,ia,user,'delete',document_id=pid,expected=heads(tmp_path,a,ia,user,pid)) is True
    emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied'];assert stored(path,pid) is None
    assert word.read_bytes()==b'changed proposal'
    # Stage tombstone retains the child manuscript and blocks both edit and move.
    assert document(tmp_path,a,ia,user,'move',document_id=sid,stage_id='S1',expected=heads(tmp_path,a,ia,user,sid)) is True
    emit(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):assert receive(client,tmp_path,token,path,identity,user,amk)==['applied']
    tombstone=event(ia,user,boot,meta['header']['event_id'],entity='S1',parent=stage_roots['S1']);tombstone['header']['operation']='delete';tombstone['stage']=None;tombstone['deleted_at']=tombstone['header']['updated_at']
    structural(tmp_path,a,ia,user,'prepare',event=tombstone,expected_tips=tombstone['header']['parent_event_ids']);publish_pending(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        pull_structure(client,tmp_path,token,path,identity,user,amk);before=stored(path,sid)
        assert document(tmp_path,path,identity,user,'move',document_id=sid,stage_id=None,expected=heads(tmp_path,path,identity,user,sid)).get('error')
        assert document(tmp_path,path,identity,user,'save',stage_id='S1',content=content('Rejected after tombstone'),expected=heads(tmp_path,path,identity,user,sid)).get('error')
        assert stored(path,sid)==before;assert owner(tmp_path,path,identity,user,sid)['blocker']=='stage_tombstone_child_manifest_incomplete'
    # Extensions and migration evidence never become lossy cloud payloads.
    with sqlite3.connect(a) as db:
        db.execute("INSERT INTO document_migration_orphans VALUES('unresolved','{\"legacy_flag\":true}','missing ownership')")
    blocked=document(tmp_path,a,ia,user,'begin');assert blocked['blocker']=='document_migration_orphan';assert document(tmp_path,a,ia,user,'pending')==[]
    with sqlite3.connect(a) as db:assert db.execute('SELECT payload_json FROM document_migration_orphans').fetchone()[0]=='{"legacy_flag":true}'
    with engine.connect() as db:
        columns=db.execute(text('SELECT entity_id,entity_type,operation FROM sync_events')).fetchall();assert all('Secret portable manuscript' not in str(row) and 'Private document heading' not in str(row) for row in columns)
        cipher=db.execute(text('SELECT ciphertext FROM encrypted_objects')).fetchall();assert all(b'Secret portable manuscript' not in bytes(row[0]) and str(word).encode() not in bytes(row[0]) for row in cipher)


@pytest.mark.parametrize('stage_id', [None, 'chapter'])
def test_web_document_save_applies_in_native_sqlite(cloud_client, tmp_path, stage_id):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    path, identity = provision(tmp_path, 'document-native-web', user, True)
    device, web = identity['device_id'], str(uuid4())
    document(tmp_path, path, identity, user, 'init')
    with sqlite3.connect(path) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.work_method','app')")
    base = '/api/v3/sync/encrypted'
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    boot = bootstrap(tmp_path, path, identity, 'bootstrap_prepare')['bootstrap_id']
    bootstrap_url = f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap'
    assert client.post(bootstrap_url, headers=headers, json={'bootstrap_id': boot, 'device_id': device}).status_code == 200
    bootstrap(tmp_path, path, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(bootstrap_url + '/complete', headers=headers, json={
        'bootstrap_id': boot, 'device_id': device, 'initial_event_count': 0, 'initial_max_server_sequence': 0,
    }).status_code == 200
    bootstrap(tmp_path, path, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post(base + '/reader-ready', headers=headers, json={
        'device_id': device, 'reader_transport_version': 3,
    }).status_code == 204
    assert client.post(base + '/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    assert client.put(base + '/note-reader-capabilities', headers=headers, json=capabilities(device)).status_code == 204
    support(client, headers, device)
    amk = list(os.urandom(32))
    native(tmp_path, path, identity, user, 'begin')
    metadata, _ = publish(client, tmp_path, token, path, identity, user, amk)
    assert sync(client, tmp_path, token, path, identity, user, amk)['view']['state'] == 'active'

    stage_refs = []
    if stage_id:
        stage_event = event(identity, user, boot, metadata['header']['event_id'], entity=stage_id)
        stage = stage_event['stage']
        stage['work_method'] = 'app'
        with sqlite3.connect(path) as db:
            db.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',
                       (stage_id, PROJECT_ID, stage['name'], stage['goal'], int(stage['infinite']), stage['unit'],
                        stage['status'], stage['created_at'], json.dumps(stage)))
            db.execute('INSERT INTO stage_order VALUES(?,?,?)', (stage_id, PROJECT_ID, 0))
        structural(tmp_path, path, identity, user, 'capture', stage_id=stage_id)
        structural_publish(client, tmp_path, token, path, identity, user, amk, stage_event)
        structural_sync(client, tmp_path, token, path, identity, user, amk)
        stage_refs = [stage_event['header']['event_id']]
    import hashlib
    doc_id = 'document-' + hashlib.sha256(f"{PROJECT_ID}:{stage_id or 'project'}".encode()).hexdigest()[:40]
    header = {
        'account_id': user, 'device_id': web, 'project_id': PROJECT_ID, 'stage_id': stage_id,
        'entity_id': doc_id, 'event_id': str(uuid4()), 'bootstrap_id': boot,
        'metadata_event_id': metadata['header']['event_id'], 'stage_event_ids': stage_refs,
        'parents': [], 'revision': 1, 'generation': 1, 'operation': 'create', 'updated_at': NOW,
    }
    portable = {
        'id': doc_id, 'project_id': PROJECT_ID, 'stage_id': stage_id, 'title': 'Manuscript',
        'content_json': content('First Web draft'), 'content_format': 'tiptap-json/v1',
        'created_at': NOW, 'extensions': {},
    }
    for revision, text_value in [(1, 'First Web draft'), (2, 'Revised Web draft')]:
        portable['content_json'] = content(text_value)
        portable['content_json']['content'][0]['content'][0]['marks'] = [{'type': 'bold'}]
        wire = _crypto_bridge(tmp_path, {
            'action': 'web_document_seal', 'amk': amk,
            'payload': {'version': 1, 'header': header, 'document': portable, 'mutation': 'upsert', 'deleted_at': None},
        })
        pin = {'web_document_version': 1, 'expected_cursor': revision + len(stage_refs), 'expected_epoch': 2}
        if header['parents']:
            pin['document_parent'] = header['parents'][0]
        response = client.post(base + '/push', headers=headers, params=pin, json=wire)
        assert response.status_code == 200, response.text
        assert receive(client, tmp_path, token, path, identity, user, amk) == ['applied']
        assert stored(path, doc_id)['content_json'] == portable['content_json']
        header.update(event_id=str(uuid4()), parents=[header['event_id']], revision=2, generation=2, operation='update')
