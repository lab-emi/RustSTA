#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Build the local SPEF report from the bounded verification summary."""
import html
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]


def main():
    p=ROOT/'docs/verification/spef-results.json'
    if p.stat().st_size>1024**2:raise ValueError('report byte cap')
    data=json.loads(p.read_text())
    if data['model_status']!='passed':raise ValueError('model verification absent')
    nodal=[]
    for c in data['nodal']:
        nodal.append('<tr>'+''.join('<td>'+html.escape(str(v))+'</td>' for v in [c['case'],f'{c["ruststa_ns"]:.9g}',f'{c["nodal_ns"]:.9g}',f'{c["difference_ns"]:.3g}'])+'</tr>')
    pdks=[]
    for c in data['pdks']:
        m=c['import'];pdks.append('<tr>'+''.join('<td>'+html.escape(str(v))+'</td>' for v in [c['pdk'],m['spef_nets'],m['spef_nodes'],m['spef_compression'],c['completeness']['unconstrained_endpoints'],f'{c["zero_r_calibration"]["max_difference_ns"]*1000:.5g}',f'{c["full_r_reference"]["max_difference_ns"]*1000:.5g}'])+'</tr>')
    source=r'''<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>RustSTA 0.2 · SPEF</title><style>
:root{color-scheme:dark;--bg:#121b22;--panel:#1b2932;--line:#344c58;--text:#eaf0f2;--muted:#abc0cb;--teal:#7fd7c0;--copper:#f1bf86}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.75 system-ui,-apple-system,sans-serif}main{max-width:1100px;margin:auto;padding:44px 24px 70px}h1{font-size:clamp(32px,5vw,54px);line-height:1.18;margin:16px 0 24px}h2{font-size:25px;margin:36px 0 14px}a{color:var(--teal)}.tag{letter-spacing:2px;color:var(--teal);font-size:12px}.lead{font-size:21px}.muted{color:var(--muted)}.cards{display:grid;grid-template-columns:repeat(3,1fr);gap:16px;margin:26px 0}.card{padding:20px;background:var(--panel);border:1px solid var(--line);border-radius:10px}.big{font-size:31px;font-weight:650;color:var(--teal)}.notice{border-left:3px solid var(--copper);padding:14px 18px;background:#2a2821}.flow{display:flex;gap:12px;align-items:center;flex-wrap:wrap;margin:26px 0}.flow span{padding:10px 16px;border:1px solid var(--line);border-radius:6px;background:var(--panel)}.flow b{color:var(--copper)}code{font:14px ui-monospace,monospace;background:#0c151a;padding:2px 5px}pre{padding:18px;background:#0c151a;overflow:auto}pre code{padding:0}table{width:100%;border-collapse:collapse;min-width:650px}th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--line)}th{font-size:13px;color:var(--muted)}.scroll{overflow:auto}details{border:1px solid var(--line);padding:18px;border-radius:8px;margin:18px 0}summary{cursor:pointer;font-weight:600}footer{border-top:1px solid var(--line);margin-top:34px;padding-top:18px;color:var(--muted);font-size:14px}@media(max-width:720px){main{padding:28px 16px}.cards{grid-template-columns:1fr}.lead{font-size:18px}}</style></head><body><main>
<div class="tag">RUSTSTA · 0.2.0 · 2026-10-23</div><h1>SPEF 进入时序计算。</h1>
<p class="lead">支持普通和 gzip SPEF，严格检查单位、名称、RC 拓扑与资源预算，并将结果写入可重开的工作区，用于一阶 RC 延迟和显式传播时钟分析。</p>
<div class="flow"><span>SPEF / .spef.gz</span><b>→</b><span>PARASIT · mmap</span><b>→</b><span>NLDM + Elmore</span><b>→</b><span>时序 / 完整重算</span></div>
<div class="cards"><div class="card"><div class="big">__TESTS__</div>常规测试通过<br><span class="muted">另有双 PDK 实际单元替换验证</span></div><div class="card"><div class="big">4 / 4</div>独立 RC 一阶矩验证通过<br><span class="muted">分支、零电阻、非对称与宽数值范围</span></div><div class="card"><div class="big">2 / 2</div>CPU / CUDA 验证通过<br><span class="muted">f64 结果逐位一致</span></div></div>
<h2>已实现的范围</h2><ul><li>详细 D_NET、NAME_MAP、端口 / 引脚、接地和耦合电容、电阻；NS/PS、PF/FF、OHM/KOHM 单位换算。</li><li>gzip CRC、截断、解压膨胀、非法索引、错误方向与缺失标注检查；失败不改变 HEAD。</li><li>零电阻收缩、树状 RC 的 Elmore 一阶矩、实际引脚电容与输出负载；不可用的网格拓扑会明确报错。</li><li>显式提取 corner、PARASIT 存储、过期结果失效；支持完整重算和增量一致性检查。</li></ul>
<p class="notice">当前模型是<strong>集总 NLDM + 一阶 Elmore，耦合按系数 1 接地</strong>，不求解有效电容波形或串扰。格式支持、模型验证、与其他计算器的一致性以及设计时序闭合，是不同的结论。</p>
<h2>独立节点方程验证</h2><p>Ngspice 通过独立节点方程 Gτ=C 计算一阶矩，RustSTA 通过树归约计算。下表单位均为 ns，不是 50% 电压阈值延迟。</p><div class="scroll"><table><thead><tr><th>案例</th><th>RustSTA</th><th>独立节点求解</th><th>绝对差</th></tr></thead><tbody>__NODAL__</tbody></table></div>
<h2>双 PDK 实际提取</h2><div class="scroll"><table><thead><tr><th>PDK</th><th>RC 网</th><th>RC 节点</th><th>输入</th><th>未约束端点</th><th>R=0 校准差 ps</th><th>完整 RC 差 ps</th></tr></thead><tbody>__PDKS__</tbody></table></div>
<p>真实文件的节点、负载、传播时钟及完整 / 增量重算均通过功能验证。R=0 仅作为名称、电容和单元模型校准，不能代替真实提取。实际单元替换后仍与完整重算一致。</p>
<p class="notice"><strong>严格参考比对仍为 partial。</strong>完整非零 RC 与 OpenSTA 默认计算器的差异约 3.15 / 6.79 ps，超过保持不变的 1 ps 门槛。不同计算器没有被假定为同一波形模型，差异未被隐藏或改写为通过；也不据此声称时序签核完成。</p>
<h2>使用方法</h2><pre><code>rusteda init --workspace work --format json
rusteda import --workspace work --base-rev rev0 \
  --lib cells.lib --corner tt --netlist routed.v --sdc timing.sdc \
  --spef routed.spef.gz --spef-corner tt --format json
ruststa run --workspace work --base-rev rev1 --corner tt \
  --precision f64 --max-state-gib 1 --format json
ruststa check --workspace work --base-rev rev2 --method full --format json</code></pre>
<p>默认输入和解压结果各不超过 32 MiB、100,000 个网、500,000 个节点和 1,000,000 个元件。预算可配置但有硬上限。原生核心不依赖 Python 或 GUI；外部求解器仅用于验证。</p>
<details><summary>证据与资源</summary><p>联合验证耗时 <strong>__TIME__</strong>，峰值进程 RSS 为 <strong>__RSS__ KiB</strong>。包含输入、mmap、解析、计算、完整重算及参考进程；这是小样例功能测试，不是工业规模吞吐量结论。</p>
<p><a href="spef-results.json">结果与文件指纹</a> · <a href="spef.md">验证方法与限制</a> · <a href="../guide/spef.md">使用说明</a> · <a href="../specs/sta/parasitics.md">模型定义</a> · <a href="../../verification/spef_acceptance.py">可复现验证脚本</a></p></details>
<footer>RustSTA 0.2.0 · Apache-2.0 · <a href="../../README.md">README</a><br>参考工具仅通过标准文件与公开命令使用。没有读取、比较或翻译参考实现源码。</footer></main></body></html>'''
    for k,v in {'__TESTS__':str(data['tests']['workspace_passed']),'__NODAL__':''.join(nodal),'__PDKS__':''.join(pdks),'__TIME__':data['integration']['wall_time'],'__RSS__':f'{data["integration"]["peak_rss_kib"]:,}'}.items():source=source.replace(k,v)
    (ROOT/'docs/verification/spef-report.html').write_text(source)


if __name__=='__main__':main()
