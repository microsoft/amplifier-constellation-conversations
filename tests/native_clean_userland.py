#!/usr/bin/env python3
"""Build/CI only: qualify one native Linux package in clean Ubuntu userland.

Requires an already acquired exact Docker image and locked test fixture. It never
pulls images, installs packages, calls providers, or changes a live installation.
Python and Docker run outside the container and are not application dependencies.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time
import uuid

ROOT=Path(__file__).resolve().parents[1]
IMAGE='ubuntu@sha256:561618e2c15bf2397621dd04f96926663a3b5616c189cf7e38db7e82f5c538ea'
LABEL='io.constellation.native-userland'
PLATFORMS={'linux-x64':('linux/amd64','amd64'), 'linux-arm64':('linux/arm64','arm64')}
TOOLS=('python','python3','pip','pip3','node','npm','npx','cargo','rustc','cc','gcc','g++','clang','make','cmake','wsl')


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(value, message):
    if not value:
        raise ValueError(message)


def package_module():
    spec=importlib.util.spec_from_file_location('native_package',ROOT/'scripts/package-native.py')
    module=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def command(name: str, run_id: str, target: str, bundle: Path, fixture: Path, args: list[str]) -> list[str]:
    require(target in PLATFORMS,'Clean userland qualification only supports native Linux x64/ARM64')
    return ['docker','run','--rm','--name',name,'--label',LABEL+'='+run_id,'--pull','never',
        '--platform',PLATFORMS[target][0],'--network','none','--read-only','--cap-drop','ALL',
        '--security-opt','no-new-privileges','--user','1000:1000','--pids-limit','64','--memory','2g','--cpus','2',
        '--tmpfs','/home/fixture:rw,exec,nosuid,nodev,uid=1000,gid=1000,mode=0700,size=1024m',
        '--tmpfs','/tmp:rw,nosuid,nodev,mode=1777,size=128m',
        '--mount','type=bind,src='+str(bundle)+',dst=/bundle,readonly',
        '--mount','type=bind,src='+str(fixture)+',dst=/fixture,readonly',
        '--env','HOME=/home/fixture','--env','USER=fixture','--env','LANG=C.UTF-8',
        '--env','PATH=/usr/sbin:/usr/bin:/sbin:/bin',IMAGE,*args]


def capture(args: list[str], timeout=180) -> dict:
    try:
        result=subprocess.run(args,text=True,capture_output=True,timeout=timeout)
        return {'returncode':result.returncode,'stdout':result.stdout,'stderr':result.stderr}
    except subprocess.TimeoutExpired as error:
        def text(value):
            return value.decode(errors='replace') if isinstance(value,bytes) else value or ''
        return {'returncode':None,'timeout':True,'stdout':text(error.stdout),'stderr':text(error.stderr)}


def cleanup(name: str, run_id: str) -> dict:
    result=capture(['docker','inspect',name,'--format','{{json .Config.Labels}}'],15)
    if result['returncode']!=0:
        require(any(message in result.get('stderr','').lower() for message in ['no such object','no such container']),
            'Cannot prove container absence after cleanup; Docker inspection failed')
        return {'name':name,'absent':True}
    labels=json.loads(result['stdout'])
    require(labels.get(LABEL)==run_id,'Container owner changed; refusing cleanup')
    removed=capture(['docker','rm','-f',name],30)
    require(removed['returncode']==0,'Owned container cleanup failed')
    return {'name':name,'owner_matched':True,'removed':True}


def qualify(archive: Path, receipt_path: Path, output: Path) -> dict:
    start=time.time()
    run_id='companion-userland-'+uuid.uuid4().hex[:12]
    output=output.resolve()
    require(not output.exists(),'Preserve previous qualification; select a new output path')
    output.parent.mkdir(parents=True,exist_ok=True)
    result={'schema_version':1,'run_id':run_id,'started_at_unix':start,'image':IMAGE,
        'scope':'native Linux isolated Ubuntu 24.04 userland; shares host kernel',
        'status':'failed','model_requests':0,'containers':[],'cleanup':[],
        'limitations':['Not a fresh desktop VM or Windows/macOS result',
            'No OS password login, browser certificate enrollment, physical microphone/camera, or startup registration',
            'No signing/notarization or external service connectivity qualification']}
    owned=[]
    expected=None
    try:
        require(platform.system()=='Linux','Run on native Linux; emulated/cross-platform launch is not qualification')
        receipt=json.loads(receipt_path.read_text())
        expected=receipt['archive_sha256']
        packager=package_module()
        manifest,files=packager.inspect_archive(archive,expected)
        target=manifest['target']
        require(target in PLATFORMS,'Unsupported clean userland target')
        require(receipt['target']==target and receipt['source_revision']==manifest['source_revision'],
            'Archive receipt identity mismatch')
        manifest_sha=hashlib.sha256(files['manifest.json']).hexdigest()
        require(manifest_sha==receipt['manifest_sha256'],'Archive receipt manifest hash mismatch')
        result.update({'target':target,'source_revision':manifest['source_revision'],
            'archive_sha256':expected,'manifest_sha256':manifest_sha,'package_receipt_sha256':sha(receipt_path),
            'harness_sha256':sha(Path(__file__)),'node_harness_sha256':sha(ROOT/'tests/native_clean_userland.mjs'),
            'archive_inspector_sha256':sha(ROOT/'scripts/package-native.py')})
        info=capture(['docker','info','--format','{{json .}}'],30)
        require(info['returncode']==0,'Docker is unavailable')
        engine=json.loads(info['stdout'])
        actual_arch={'x86_64':'amd64','aarch64':'arm64'}.get(engine.get('Architecture'),engine.get('Architecture'))
        require(engine.get('OSType')=='linux' and actual_arch==PLATFORMS[target][1],
            'Docker engine is not the native target architecture; emulation is not qualification')
        inspected=capture(['docker','image','inspect',IMAGE],30)
        require(inspected['returncode']==0,'Exact Ubuntu image must be acquired by CI first; harness never pulls')
        image=json.loads(inspected['stdout'])[0]
        require(IMAGE in image.get('RepoDigests',[]) and image.get('Os')=='linux'
            and image.get('Architecture')==PLATFORMS[target][1],'Cached image identity/architecture mismatch')
        result['image_identity']={key:image.get(key) for key in ['Id','RepoDigests','Os','Architecture']}
        with tempfile.TemporaryDirectory(prefix='companion-userland-') as temporary:
            base=Path(temporary)
            bundle=base/'Relocated Companion 日本語'; bundle.mkdir(mode=0o755)
            for name,data in files.items():
                dest=bundle/name; dest.parent.mkdir(parents=True,exist_ok=True); dest.write_bytes(data)
            for entry in manifest['files']:
                (bundle/entry['path']).chmod(0o755 if entry['executable'] else 0o644)
            fixture=base/'fixture'; fixture.mkdir(mode=0o755)
            fixture_files=packager.tree_files(ROOT/'tests/native/node_modules')
            for name,data in fixture_files.items():
                dest=fixture/'node_modules'/name; dest.parent.mkdir(parents=True,exist_ok=True); dest.write_bytes(data)
            for name in ['fixture.mjs','package.json','package-lock.json']:
                (fixture/name).write_bytes((ROOT/'tests/native'/name).read_bytes())
            (fixture/'check.mjs').write_bytes((ROOT/'tests/native_clean_userland.mjs').read_bytes())
            result['fixture']={'files':{name:sha(fixture/name) for name in
                ['fixture.mjs','package.json','package-lock.json','check.mjs']},
                'node_modules_sha256':packager.tree_digest(fixture_files)}
            probe_script='''missing=0
for x in '''+' '.join(TOOLS)+'''; do
  if command -v "$x" >/dev/null 2>&1; then printf 'UNEXPECTED_TOOL %s\\n' "$x"; missing=1; fi
done
printf 'IDENTITY '; id
printf 'OS '; cat /etc/os-release
/bundle/agent-conversations --version
service_status=$?
/bundle/runtime/node --version
node_status=$?
printf 'RESULT tools=%s service=%s node=%s\\n' "$missing" "$service_status" "$node_status"
test "$missing" -eq 0 && test "$service_status" -eq 0 && test "$node_status" -eq 0
'''
            for suffix,args,timeout in [('probe',['/bin/sh','-c',probe_script],60),
                    ('full',['/bundle/runtime/node','/fixture/check.mjs'],180)]:
                name=run_id+'-'+suffix; owned.append(name)
                invocation=command(name,run_id,target,bundle,fixture,args)
                captured=capture(invocation,timeout)
                result['containers'].append({'name':name,'command':invocation,**captured})
                require(captured['returncode']==0,f'{suffix} qualification failed; see retained output')
                if suffix=='full':
                    lines=[line for line in captured['stdout'].splitlines() if line.startswith('{')]
                    require(bool(lines),'Native harness did not emit a receipt')
                    result['native_result']=json.loads(lines[-1])
                    require(result['native_result']['status']=='passed','Native harness reported failure')
            result['status']='passed'
        result['temporary_extraction_removed']=True
    except Exception as error:
        result['error']=str(error)
    finally:
        for name in owned:
            try:
                result['cleanup'].append(cleanup(name,run_id))
            except Exception as error:
                result['cleanup'].append({'name':name,'error':str(error)})
                result['status']='failed'
        if expected is not None:
            result['archive_unchanged']=sha(archive)==expected
            if not result['archive_unchanged']:
                result['status']='failed'
        result['elapsed_seconds']=time.time()-start
        output.write_text(json.dumps(result,indent=2)+'\n')
    return result


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive',required=True,type=Path)
    parser.add_argument('--receipt',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    result=qualify(args.archive.resolve(),args.receipt.resolve(),args.output)
    print(json.dumps({'status':result['status'],'receipt':str(args.output.resolve()),'error':result.get('error')}))
    raise SystemExit(0 if result['status']=='passed' else 1)
