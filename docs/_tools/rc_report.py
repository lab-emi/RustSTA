#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Render the bounded RC verification receipt as a standalone Chinese report."""
import html
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]

def main():
    path=ROOT/'docs/verification/rc-results.json'
    if path.stat().st_size>1024**2:raise ValueError('report byte cap')
    d=json.loads(path.read_text());require=d['status']=='passed'
    if not require:raise ValueError('RC acceptance did not pass')
    rows=[]
    for p in d['pdks']:
        vals=[p['pdk'],p['scope']['clock_model'],p['endpoints'],f'{p["max_difference_ns"]*1000:.6f}',f'{p["metrics"]["worst_hold_slack_ns"]*1000:.6f}',p['timing_closure']]
        rows.append('<tr>'+''.join('<td>'+html.escape(str(v))+'</td>' for v in vals)+'</tr>')
    source=r'''<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>RustSTA 0.3 · RC 时序</title><style>
:root{color-scheme:dark;--bg:#121b22;--panel:#1b2932;--line:#344c58;--text:#eaf0f2;--muted:#abc0cb;--teal:#7fd7c0;--copper:#f1bf86}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.75 system-ui,-apple-system,sans-serif}main{max-width:1100px;margin:auto;padding:44px 24px 70px}h1{font-size:clamp(32px,5vw,54px);line-height:1.18;margin:16px 0 24px}h2{font-size:25px;margin:34px 0 14px}a{color:var(--teal)}.tag{letter-spacing:2px;color:var(--teal);font-size:12px}.lead{font-size:21px}.muted{color:var(--muted)}.cards{display:grid;grid-template-columns:repeat(3,1fr);gap:16px;margin:26px 0}.card{padding:20px;background:var(--panel);border:1px solid var(--line);border-radius:10px}.big{font-size:31px;font-weight:650;color:var(--teal)}.notice{border-left:3px solid var(--copper);padding:14px 18px;background:#2a2821}.flow{display:flex;gap:12px;align-items:center;flex-wrap:wrap;margin:24px 0}.flow span{padding:10px 16px;border:1px solid var(--line);border-radius:6px;background:var(--panel)}.flow b{color:var(--copper)}code{font:14px ui-monospace,monospace;background:#0c151a;padding:2px 5px}pre{padding:18px;background:#0c151a;overflow:auto}pre code{padding:0}table{width:100%;border-collapse:collapse;min-width:650px}th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--line)}th{font-size:13px;color:var(--muted)}.scroll{overflow:auto}details{border:1px solid var(--line);padding:18px;border-radius:8px;margin:18px 0}summary{cursor:pointer;font-weight:600}footer{border-top:1px solid var(--line);margin-top:34px;padding-top:18px;color:var(--muted);font-size:14px}@media(max-width:720px){main{padding:28px 16px}.cards{grid-template-columns:1fr}.lead{font-size:18px}}</style></head><body><main>
<div class="tag">RUSTSTA · 0.3.0 · 2026-10-23</div><h1>RC 时序通过 1 ps 对照门槛。</h1>
<p class="lead">在真实 sky130 / gf180 提取网络上加入有效电容、驱动端非线性波形和接收端 slew。使用相同 Liberty、SDC、完整非零 RC SPEF，与 OpenSTA 默认计算器对照；保持原有 1 ps 门槛。</p>
<div class="cards"><div class="card"><div class="big">__SKY__ ps</div>sky130 最大端点差<br><span class="muted">此前 3.148661 ps</span></div><div class="card"><div class="big">__GF__ ps</div>gf180 最大端点差<br><span class="muted">此前 6.792814 ps</span></div><div class="card"><div class="big">9 / 9</div>独立电路案例通过<br><span class="muted">Ngspice 电荷与瞬态阈值验证</span></div></div>
<h2>模型补齐了什么</h2><div class="flow"><span>真实 RC 树</span><b>→</b><span>三阶导纳矩 / π</span><b>→</b><span>电荷匹配 Ceff</span><b>→</b><span>驱动波形与接收端 slew</span></div>
<p>单元驱动采用有限斜坡 Thevenin 模型，求解有效电容并保留非线性波形尾部。接收端用逐负载 Elmore 一阶矩建立单极点阈值模型；输入端口采用显式指数响应约定。一阶矩和电压阈值延迟分别处理。</p>
<p>修复了已解析 Liberty 元数据的传递：GF180 的 30%–70% 阈值和 0.5 slew derate 会进入计算。阈值不再由库名推断。驱动电阻仍是 NLDM 延迟敏感度的代理值，属于明确的近似；没有声称获得晶体管直流参数、CCS 或串扰分析能力。</p>
<h2>双 PDK 结果</h2><div class="scroll"><table><thead><tr><th>PDK</th><th>时钟</th><th>约束端点</th><th>最大差 ps</th><th>最差 hold ps</th><th>该基准 setup/hold</th></tr></thead><tbody>__ROWS__</tbody></table></div>
<p>每个 PDK 比对 8 个端点的 setup / hold，未约束端点为 0。完整重算与更新检查一致；实际时钟缓冲单元替换和恢复也通过增量 / 完整一致性验证。</p>
<p class="notice"><strong>精度验收 passed，完整流程仍为 partial。</strong>GF180 保留 4 个 hold 违例，最差约 −635.5 ps。现有 sky130 基准的名义 setup / hold 满足约束，但 hold 裕量仅约 0.247 ps。通过 1 ps 对照门槛不等于签核，后续仍需物理时序修复及更完整的验证。</p>
<h2>后布线反馈链路</h2><div class="flow"><span>RTL / 逻辑等价</span><b>→</b><span>放置 / CTS / 信号布线</span><b>→</b><span>外部提取 SPEF</span><b>→</b><span>RustSTA / 参考检查</span></div>
<p>重新运行双 PDK 的真实布线与提取，包含 CEC、512 向量仿真、原始 GDS / DEF 几何检查、供电及 tap 校验、时钟偏斜检查和负向控制。每个 PDK 19 个信号网、46 个信号端子均被覆盖；没有用零电阻文件替代真实寄生。完整 DRC / LVS 与完整 RTL-to-GDS 签核仍未完成。</p>
<h2>证据与资源</h2><p>常规测试 <strong>__TESTS__</strong> 项通过；另有 <strong>3</strong> 项 CPU / CUDA f64 比对逐位一致。非线性 RC 波形在 CPU 上预计算成连续记录，两类传播后端使用相同记录；资源统计包含这一步。</p>
<p>独立电路最大时间差 <strong>__ORACLE__ ps</strong>。STA 双 PDK 验收耗时 <strong>__STA_TIME__</strong>，峰值 RSS <strong>__STA_RSS__ KiB</strong>。包含综合、物理实现、外部提取与时序的联合验证耗时 <strong>__FLOW_TIME__</strong>，峰值主机进程 RSS <strong>__FLOW_RSS__ KiB</strong>。这些是共享机器上小基准的完整流程测量，不是工业规模吞吐量结论。</p>
<details><summary>复现与模型边界</summary><p>输入 / 元件 / 名称 / 状态大小均有上限。数值迭代、工作量与电荷残差受限；超预算和不兼容模型会明确失败，不会替换成理想连线。独立检查器尚不支持提取 RC，因此仍返回 undecided。</p><p><a href="rc-results.json">结果、工具与输入指纹</a> · <a href="rc-waveforms.md">验证方法</a> · <a href="../specs/sta/rc-waveforms.md">模型定义</a> · <a href="../guide/spef.md">使用方法</a> · <a href="../../verification/rc_waveforms.py">可复现脚本</a></p></details>
<footer>RustSTA 0.3.0 · Apache-2.0 · <a href="../../README.md">README</a><br>参考工具仅作为外部黑盒执行。未读取、比较或翻译参考实现源码。</footer></main></body></html>'''
    values={'__SKY__':f'{d["pdks"][0]["max_difference_ns"]*1000:.3f}','__GF__':f'{d["pdks"][1]["max_difference_ns"]*1000:.3f}','__ROWS__':''.join(rows),'__TESTS__':str(d['tests']['passed']),'__ORACLE__':f'{d["oracle"]["max_time_error_ns"]*1000:.7f}','__STA_TIME__':d['acceptance']['wall_time'],'__STA_RSS__':f'{d["acceptance"]["peak_rss_kib"]:,}','__FLOW_TIME__':d['pipeline']['measurement']['wall_time'],'__FLOW_RSS__':f'{d["pipeline"]["measurement"]["peak_rss_kib"]:,}'}
    for k,v in values.items():source=source.replace(k,html.escape(v) if k!='__ROWS__' else v)
    (ROOT/'docs/verification/rc-report.html').write_text(source)

if __name__=='__main__':main()
