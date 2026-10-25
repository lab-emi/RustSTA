#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Bounded SPEF acceptance: Ngspice nodal moments and OpenSTA dual-PDK comparisons.

The full-R reference waveform model is not asserted identical to the declared
first-moment model. Its unchanged 1 ps gate is recorded separately. No reference
implementation is inspected. All transient design data stays in the workspace.
"""
import argparse
import fcntl
import gzip
import io
import hashlib
import json
import math
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[1]
NUMBER=r'[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?'


def require(ok,message):
    if not ok:raise RuntimeError(message)


def read(path,cap=32*1024**2):
    require(path.stat().st_size<=cap,'input/log byte budget exceeded')
    return path.read_text()


def fingerprint(path):
    require(path.is_file() and path.stat().st_size<=256*1024**2,'identity file absent/too large')
    with path.open('rb') as f:digest=hashlib.file_digest(f,'sha256').hexdigest()
    return dict(path=str(path),bytes=path.stat().st_size,sha256=digest)


def save(path,value):path.write_text(json.dumps(value,indent=2,allow_nan=False)+'\n')


class Harness:
    def __init__(self,root):self.root=root;self.receipts=[]
    def run(self,name,command,expected=0):
        d=self.root/'receipts'/name;d.mkdir(parents=True)
        def cap():
            resource.setrlimit(resource.RLIMIT_AS,(2*1024**3,2*1024**3))
            resource.setrlimit(resource.RLIMIT_FSIZE,(32*1024**2,32*1024**2))
            resource.setrlimit(resource.RLIMIT_CORE,(0,0))
        args=['/usr/bin/time','-v','-o',str(d/'time.txt'),'timeout','--kill-after=2s','40s',*map(str,command)]
        with (d/'stdout.txt').open('wb') as out,(d/'stderr.txt').open('wb') as err:
            r=subprocess.run(args,cwd=self.root,stdin=subprocess.DEVNULL,stdout=out,stderr=err,preexec_fn=cap,env={**os.environ,'RAYON_NUM_THREADS':'2','LC_ALL':'C'})
        t=read(d/'time.txt');m=re.search(r'Maximum resident set size \(kbytes\):\s*(\d+)',t)
        item=dict(label=name,command=list(map(str,command)),exit_code=r.returncode,peak_rss_kib=int(m[1]) if m else None,stdout=str(d/'stdout.txt'),stderr=str(d/'stderr.txt'))
        self.receipts.append(item)
        if expected is not None:require(r.returncode==expected,name+' failed; see receipt')
        require(m is not None and int(m[1])<2*1024**2,'RSS missing/over budget')
        return item
    def command(self,name,command):
        r=self.run(name,command);j=json.loads(read(Path(r['stdout'])))
        require(j.get('ok') is True,name+' envelope failed');return j


def native(h,tag,tools,lib,net,sdc,spef):
    common=['-w',str(h.root/(tag+'_ws')),'--threads','2','--format','json','--color','never']
    a=h.command(tag+'_init',[tools['rusteda'],*common,'init'])
    imported=h.command(tag+'_import',[tools['rusteda'],*common,'import','--base-rev',a['new_rev'],'--lib',lib,'--corner','tt','--netlist',net,'--sdc',sdc,'--spef',spef,'--spef-corner','tt','--max-rc-nodes','100000','--max-rc-elements','200000'])
    a=h.command(tag+'_run',[tools['ruststa'],*common,'run','--base-rev',imported['new_rev'],'--corner','tt','--max-state-gib','1','--max-paths','16','--precision','f64'])
    report=h.command(tag+'_report',[tools['ruststa'],*common,'report','--rev',a['new_rev'],'--section','checks','--section','paths','--limit','128'])
    check=h.command(tag+'_check',[tools['ruststa'],*common,'check','--base-rev',a['new_rev'],'--method','full','--tolerance','1e-9'])
    update=h.command(tag+'_update',[tools['ruststa'],*common,'update','--base-rev',check['new_rev'],'--max-state-gib','1','--max-paths','16','--precision','f64'])
    again=h.command(tag+'_recheck',[tools['ruststa'],*common,'check','--base-rev',update['new_rev'],'--method','full','--tolerance','1e-9'])
    require(check.get('verdict')=='agree' and again.get('verdict')=='agree','full/incremental mismatch')
    require(a['scope']['wire_model']=='spef_elmore' and a['completeness']['unconstrained_endpoints']==0,'unexpected model/coverage')
    return dict(imported=imported,scope=a['scope'],metrics=a['metrics'],completeness=a['completeness'],diagnostics=a['diagnostics'],checks=report['sections']['checks']['rows'],paths=report['sections']['paths']['rows'],full=check,incremental_noop=again)


def transport_controls(h,tag,tools,result,good):
    ws=h.root/(tag+'_ws');base=result['incremental_noop']['new_rev'];common=['-w',str(ws),'--format','json','--threads','2','--color','never']
    original=good.read_bytes();bad=bytearray(original);bad[-8]^=1
    crc=h.root/'bad-crc.spef.gz';crc.write_bytes(bad)
    bomb=h.root/'over-budget.spef.gz';bomb.write_bytes(gzip.compress(b'x'*(2<<20),mtime=0))
    cases=[('bad_crc',crc,[]),('decoded_budget',bomb,['--max-spef-mib','1'])]
    if hasattr(os,'mkfifo'):
        fifo=h.root/'input-fifo.spef';os.mkfifo(fifo);cases.append(('fifo',fifo,[]))
    for name,path,extra in cases:
        r=h.run(name,[tools['rusteda'],*common,'import','--base-rev',base,'--spef',path,'--spef-corner','tt',*extra],expected=None)
        j=json.loads(read(Path(r['stdout'])));require(r['exit_code']!=0 and j.get('ok') is False,'invalid transport accepted')
        state=h.command(name+'_head',[tools['rusteda'],*common,'show']);require(state['rev']==base,'invalid import changed HEAD')
    return {name:'rejected' for name,_,_ in cases}


def nodal_cases(h,tools):
    cases=json.loads(read(ROOT/'verification/fixtures/synthetic/spef/moments.json',65536))['cases']
    require(0<len(cases)<=16,'case budget');result=[]
    lib=ROOT/'verification/fixtures/synthetic/sta/lib/wt_15.lib'
    for case in cases:
        tag='moment_'+case['name'];caps=case['caps_pf'];edges=case['edges'];out=case['output'];load=case['port_cap_pf']
        require(len(caps)<=64 and len(edges)<=128 and 0<out<len(caps),'nodal budget')
        require(all(math.isfinite(c) and c>=0 for c in caps+[load]),'invalid capacitance')
        names=['a' if i==0 else 'y' if i==out else 'a:'+str(i) for i in range(len(caps))]
        net=h.root/(tag+'.v');net.write_text('module top(input a,output y);assign y=a;endmodule\n')
        sdc=h.root/(tag+'.sdc');sdc.write_text('create_clock -name virtual -period 100\nset_input_delay 0 -clock virtual [get_ports a]\nset_output_delay 0 -clock virtual [get_ports y]\nset_input_transition 0.1 [get_ports a]\nset_load '+str(load)+' [get_ports y]\n')
        text='*SPEF "ieee 1481-1999"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n*T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n*D_NET a '+str(sum(caps))+'\n*CONN\n*P a I\n*P y O\n*CAP\n'
        text+=''.join(f'{i+1} {names[i]} {c}\n' for i,c in enumerate(caps))+'*RES\n'
        for i,(a,b,r) in enumerate(edges):
            require(0<=a<len(caps) and 0<=b<len(caps) and math.isfinite(r) and r>=0,'invalid resistor')
            text+=f'{i+1} {names[a]} {names[b]} {r}\n'
        text+='*END\n';spef=h.root/(tag+'.spef.gz');spef.write_bytes(gzip.compress(text.encode(),mtime=0))
        n=native(h,tag,tools,lib,net,sdc,spef)
        arrivals=[r['arrival_ns'] for r in n['paths']];require(arrivals and max(arrivals)==min(arrivals),'unexpected transition dependence')
        # Independent nodal solve: G*tau=C. Normalize kOhm/pF to numerical ns.
        labels=['a' if i==0 else 'y' if i==out else 'n'+str(i) for i in range(len(caps))]
        circuit=['first moment nodal solve','VROOT a 0 DC 0']
        for i,(a,b,r) in enumerate(edges):
            circuit.append(f'R{i} {labels[a]} {labels[b]} {r}' if r else f'VZERO{i} {labels[a]} {labels[b]} DC 0')
        for i,c in enumerate(caps):
            if i:circuit.append(f'I{i} 0 {labels[i]} DC {c+(load if i==out else 0)}')
        circuit+=['.control','set numdgt=17','op','print v(y)','quit','.endc','.end']
        cir=h.root/(tag+'.cir');cir.write_text('\n'.join(circuit)+'\n')
        r=h.run(tag+'_nodal',[tools['ngspice'],'-b',cir]);m=re.search(r'v\(y\)\s*=\s*('+NUMBER+')',read(Path(r['stdout'])))
        require(m is not None,'nodal value absent');reference=float(m[1]);difference=abs(arrivals[0]-reference)
        require(difference<=1e-9,'first moment disagrees with independent nodal solver')
        entry=dict(case=case['name'],ruststa_ns=arrivals[0],nodal_ns=reference,difference_ns=difference,status='passed',compression=n['imported']['metrics'].get('spef_compression'))
        if not result:entry['transport_controls']=transport_controls(h,tag,tools,n,spef)
        result.append(entry)
    return result


def tcl(path):
    require(not any(c in str(path) for c in '{}\n\r'),'unsupported path character');return '{'+str(path)+'}'


def reference(h,tag,tool,lib,net,sdc,spef,native_result):
    script=h.root/(tag+'.tcl')
    script.write_text(f'''read_liberty {tcl(lib)}
read_verilog {tcl(net)}
link_design lane_smoke
read_sdc {tcl(sdc)}
read_spef {tcl(spef)}
report_units
puts SETUP_BEGIN
report_checks -path_delay max -group_path_count 128 -endpoint_path_count 1 -format end -digits 12
puts SETUP_END
puts HOLD_BEGIN
report_checks -path_delay min -group_path_count 128 -endpoint_path_count 1 -format end -digits 12
puts HOLD_END
exit
''')
    r=h.run(tag,[tool,'-no_init','-no_splash','-exit',script]);text=read(Path(r['stdout']))+read(Path(r['stderr']))
    require(not re.search(r'(?mi)^\s*Error(?:\s+\d+)?:',text),'reference input error')
    require(re.search(r'(?mi)^\s*Time\s+1ns\s*$',text),'reference time unit not verified')
    rows={};diff=[]
    for kind in ('SETUP','HOLD'):
        require(text.count(kind+'_BEGIN')==1 and text.count(kind+'_END')==1,'missing reference markers')
        block=text.split(kind+'_BEGIN')[1].split(kind+'_END')[0]
        pattern=rf'^(\S+)\s+\([^\n]*?\)\s+({NUMBER})\s+({NUMBER})\s+({NUMBER})\s+\((?:MET|VIOLATED)\)\s*$'
        rows[kind.lower()]={m[1]:float(m[4]) for m in re.finditer(pattern,block,re.M)}
    names={r['endpoint'] for r in native_result['checks']}
    require(names and all(set(r)==names for r in rows.values()),'endpoint coverage mismatch')
    for row in native_result['checks']:
        for kind in ('setup','hold'):
            a=row[kind+'_slack_ns'];b=rows[kind][row['endpoint']]
            require(math.isfinite(a) and math.isfinite(b),'nonfinite reference timing')
            diff.append(dict(endpoint=row['endpoint'],kind=kind,native_ns=a,reference_ns=b,difference_ns=abs(a-b)))
    maximum=max(r['difference_ns'] for r in diff)
    return dict(max_difference_ns=maximum,tolerance_ns=0.001,status='passed' if maximum<=0.001 else 'different',comparisons=diff,reference_delay_calculator='default')


def pdks(h,tools,path):
    data=json.loads(read(path,65536));cases=data['cases'];require(len(cases)==2 and {c['tag'] for c in cases}=={'sky130','gf180'},'both baseline PDKs required')
    results=[]
    for c in cases:
        tag=c['tag'];files={k:Path(c[k]).resolve() for k in ('library','netlist','sdc','spef')}
        pins={k:fingerprint(p) for k,p in files.items()}
        # Real extraction is retained unchanged. The R=0 projection is a separate
        # calibration of naming, capacitance, NLDM and propagation, not signoff.
        require(files['spef'].stat().st_size<=32*1024**2,'encoded source budget')
        raw=files['spef'].read_bytes()
        if raw.startswith(b'\x1f\x8b'):
            with gzip.GzipFile(fileobj=io.BytesIO(raw)) as stream:decoded=stream.read(32*1024**2+1)
        else:decoded=raw
        text=decoded.decode()
        require(len(text)<=32*1024**2,'decoded source budget')
        full=h.root/(tag+'_full.spef');full.write_text(text)
        zero=[];res=False
        for line in text.splitlines():
            if line.strip()=='*RES':res=True
            elif line.startswith('*'):res=False
            if res and not line.startswith('*') and line.strip():
                parts=line.split();require(len(parts)==4,'unexpected R record');line=' '.join(parts[:3]+['0'])
            zero.append(line)
        zero_path=h.root/(tag+'_zero.spef');zero_path.write_text('\n'.join(zero)+'\n')
        real=native(h,tag+'_real',tools,files['library'],files['netlist'],files['sdc'],files['spef'])
        real_ref=reference(h,tag+'_reference',tools['opensta'],files['library'],files['netlist'],files['sdc'],full,real)
        z=native(h,tag+'_zero',tools,files['library'],files['netlist'],files['sdc'],zero_path)
        zref=reference(h,tag+'_zero_reference',tools['opensta'],files['library'],files['netlist'],files['sdc'],zero_path,z)
        require(zref['status']=='passed','zero-R capacitance/model calibration failed')
        require(all(fingerprint(files[k])['sha256']==p['sha256'] for k,p in pins.items()),'input changed during validation')
        results.append(dict(pdk=tag,inputs=pins,real=real,reference=real_ref,zero_resistance_calibration=zref))
    return results


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--workspace',type=Path,required=True);p.add_argument('--base-rev',required=True)
    for n in ('rusteda','ruststa','opensta','ngspice'):p.add_argument('--'+n,type=Path,required=True)
    p.add_argument('--cases',type=Path,required=True);a=p.parse_args()
    require(a.base_rev=='empty','REVISION_CONFLICT: fresh evidence requires empty')
    root=a.workspace.resolve();root.mkdir(parents=True,exist_ok=False);lock=(root/'workspace.lock').open('wb');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);(root/'HEAD').write_text('rev0\n')
    inputs={name:fingerprint(path) for name,path in [('harness',Path(__file__).resolve()),('moments',ROOT/'verification/fixtures/synthetic/spef/moments.json'),('cases',a.cases.resolve())]}
    h=Harness(root);started=time.monotonic();summary=dict(status='unverified',model_status='unverified',pdks=[],nodal=[],tools={},inputs=inputs)
    try:
        h.run('memory_preflight',['free','-h']);tools={n:getattr(a,n).resolve() for n in ('rusteda','ruststa','opensta','ngspice')}
        for n,path in tools.items():
            version=h.run(n+'_version',[path,'-version' if n=='opensta' else '--version'])
            summary['tools'][n]={**fingerprint(path),'version':read(Path(version['stdout'])).strip()}
        summary['nodal']=nodal_cases(h,tools);summary['pdks']=pdks(h,tools,a.cases)
        summary['model_status']='passed'
        summary['status']='passed' if all(c['reference']['status']=='passed' for c in summary['pdks']) else 'partial'
    except (RuntimeError,ValueError,KeyError,OSError) as e:summary['status']='failed';summary['reason']=str(e)
    finally:
        summary['identity_stable']=all(fingerprint(Path(t['path']))['sha256']==t['sha256'] for t in [*summary['tools'].values(),*inputs.values()])
        if not summary['identity_stable']:summary['status']='failed'
        summary['wall_seconds']=time.monotonic()-started;summary['peak_rss_kib']=max((r['peak_rss_kib'] for r in h.receipts if r['peak_rss_kib'] is not None),default=None);summary['receipts']=h.receipts
        save(root/'summary.json',summary);(root/'HEAD').write_text('rev1\n')
    print(json.dumps(dict(status=summary['status'],model_status=summary['model_status'],summary=str(root/'summary.json'))))
    return {'passed':0,'partial':3,'unverified':3,'failed':1}[summary['status']]


if __name__=='__main__':sys.exit(main())
