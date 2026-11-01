#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Fixed 1 ps dual-PDK RC waveform acceptance. All tools are black boxes."""
import argparse
import fcntl
import gzip
import io
import json
from pathlib import Path
import sys
import time
from spef_acceptance import Harness, require, read, fingerprint, save, reference


def native(h,tag,tools,files):
    ws=h.root/(tag+'_workspace');common=['--workspace',ws,'--threads','2','--format','json','--color','never']
    a=h.command(tag+'_init',[tools['rusteda'],*common,'init'])
    a=h.command(tag+'_import',[tools['rusteda'],*common,'import','--base-rev',a['new_rev'],'--lib',files['library'],'--corner','tt','--netlist',files['netlist'],'--sdc',files['sdc'],'--spef',files['spef'],'--spef-corner','tt'])
    r=h.command(tag+'_run',[tools['ruststa'],*common,'run','--base-rev',a['new_rev'],'--corner','tt','--precision','f64','--max-state-gib','1','--max-paths','16'])
    require(r['scope']['wire_model']=='spef_rc' and r['scope']['delay_calculator']=='nldm_ceff_ramp','wrong RC waveform scope')
    require(r['completeness']['unconstrained_endpoints']==0,'unconstrained endpoints')
    report=h.command(tag+'_report',[tools['ruststa'],*common,'report','--rev',r['new_rev'],'--section','checks','--section','paths','--limit','128'])
    check=h.command(tag+'_full',[tools['ruststa'],*common,'check','--base-rev',r['new_rev'],'--method','full','--tolerance','1e-9'])
    require(check['verdict']=='agree','full recomputation differs')
    update=h.command(tag+'_update',[tools['ruststa'],*common,'update','--base-rev',check['new_rev'],'--precision','f64','--max-state-gib','1'])
    again=h.command(tag+'_full_updated',[tools['ruststa'],*common,'check','--base-rev',update['new_rev'],'--method','full','--tolerance','1e-9'])
    require(again['verdict']=='agree','update differs from full')
    return dict(scope=r['scope'],metrics=r['metrics'],completeness=r['completeness'],diagnostic_codes=sorted({d['code'] for d in r['diagnostics']}),checks=report['sections']['checks']['rows'],paths=report['sections']['paths']['rows'],full=check['verdict'],incremental_noop=again['verdict'],workspace=str(ws),rev=again['new_rev'])


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--workspace',type=Path,required=True);p.add_argument('--base-rev',required=True);p.add_argument('--cases',type=Path,required=True)
    for n in ['rusteda','ruststa','opensta']:p.add_argument('--'+n,type=Path,required=True)
    a=p.parse_args();require(a.base_rev=='empty','REVISION_CONFLICT: use fresh evidence workspace')
    root=a.workspace.resolve();root.mkdir(parents=True,exist_ok=False)
    lock=(root/'workspace.lock').open('wb');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);(root/'HEAD').write_text('rev0\n')
    started=time.monotonic();h=Harness(root);tools={n:getattr(a,n).resolve() for n in ['rusteda','ruststa','opensta']}
    pins={};inputs={n:fingerprint(v) for n,v in [('harness',Path(__file__).resolve()),('cases',a.cases.resolve())]}
    summary=dict(schema='ruststa.rc-waveforms.v1',status='unverified',tolerance_ns=0.001,pdks=[],tools=pins,inputs=inputs)
    try:
        missing=[n for n,v in tools.items() if not v.is_file()]
        if missing:
            summary.update(status='unverified',reason='missing reference/tool: '+','.join(missing))
            raise FileNotFoundError(summary['reason'])
        pins.update({n:fingerprint(v) for n,v in tools.items()})
        h.run('memory_preflight',['free','-h'])
        for n,v in tools.items():
            r=h.run(n+'_version',[v,'-version' if n=='opensta' else '--version']);pins[n]['version']=read(Path(r['stdout'])).strip()
        cases=json.loads(read(a.cases,65536))['cases'];require(len(cases)==2 and {c['tag'] for c in cases}=={'sky130','gf180'},'both PDKs required')
        for c in cases:
            tag=c['tag'];files={k:Path(c[k]).resolve() for k in ['library','netlist','sdc','spef']};source_pins={k:fingerprint(v) for k,v in files.items()}
            require(source_pins['spef']['bytes']<=32*1024**2,'source budget')
            data=files['spef'].read_bytes()
            if data.startswith(b'\x1f\x8b'):
                with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:data=stream.read(32*1024**2+1)
            require(len(data)<=32*1024**2,'decoded source budget')
            plain=root/(tag+'.spef');plain.write_bytes(data)
            n=native(h,tag,tools,files)
            ref=reference(h,tag+'_reference',tools['opensta'],files['library'],files['netlist'],files['sdc'],plain,n)
            require(ref['tolerance_ns']==0.001,'accuracy gate changed')
            require(all(fingerprint(files[k])['sha256']==v['sha256'] for k,v in source_pins.items()),'input changed')
            summary['pdks'].append(dict(pdk=tag,inputs=source_pins,native=n,reference=ref))
        summary['status']='passed' if all(c['reference']['status']=='passed' for c in summary['pdks']) else 'different'
    except FileNotFoundError as e:summary.update(status='unverified',reason=str(e))
    except (RuntimeError,ValueError,KeyError,OSError) as e:summary.update(status='failed',reason=str(e))
    finally:
        summary['identity_stable']=all(fingerprint(Path(t['path']))['sha256']==t['sha256'] for t in [*pins.values(),*inputs.values()])
        if not summary['identity_stable']:summary['status']='failed'
        summary.update(wall_seconds=time.monotonic()-started,peak_rss_kib=max((r['peak_rss_kib'] for r in h.receipts),default=None),receipts=h.receipts)
        save(root/'summary.json',summary);(root/'HEAD').write_text('rev1\n')
    print(json.dumps(dict(status=summary['status'],summary=str(root/'summary.json'))))
    return 0 if summary['status']=='passed' else 3 if summary['status']=='unverified' else 1


if __name__=='__main__':sys.exit(main())
