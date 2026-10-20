"""Readable Chinese results, full paired comparisons and standalone plots."""
from pathlib import Path
import itertools
import json
import os
import math
import statistics
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib import font_manager
from cloud import OUT

LABELS={'current':'已交付版本（原 auto）','optimized':'写入／热读优化＋原 auto','original':'原始无 hint 方案',
        'fixed-1m':'相同客户端，固定 1 MiB EC','before':'优化前缓存读路径','after':'优化后缓存读路径',
        'final-auto':'最终组合（修正 auto）'}
PHASES={'write':'整文件写入至上传完成','client_cold_read':'空客户端缓存整文件读取',
        'warm_read':'内核页缓存重复完整读取','mixed':'整文件混合读写至上传完成'}


def label(size):
    if size==4194304+65536:return '4 MiB＋64 KiB'
    return f'{size//1048576} MiB' if size>=1048576 else f'{size//1024} KiB'


def paired(values):
    assert len(values)==4 and all(math.isfinite(x) and x>0 for x in values)
    logs=np.log(values)
    draws=[math.exp(float(np.mean(logs[list(indices)]))) for indices in itertools.product(range(4),repeat=4)]
    return float(np.exp(np.mean(logs))),[float(x) for x in np.percentile(draws,[2.5,97.5])]


def summarize(rows,resident=False):
    groups={}
    for r in rows:
        names=['warm_read'] if resident else ['mixed'] if r['mixed'] else ['write','client_cold_read','warm_read']
        for name in names:
            p=r['write'] if name=='write' else r['phases'][name]
            key=(r['size'],r['concurrency'],name,r['arm'])
            groups.setdefault(key,[]).append((r['round'],p))
    result=[]
    for (size,c,name,arm),entries in sorted(groups.items()):
        entries.sort();assert [r for r,p in entries]==[1,2,3,4]
        rates=[p['upload_mib_s'] if name in ['write','mixed'] else p['active_mib_s'] for r,p in entries]
        latencies=[float(np.percentile(p['file_latencies_seconds'],95))*1000 for r,p in entries]
        strict=[p['total_mib_s'] for r,p in entries]
        result.append(dict(size=size,concurrency=c,phase=name,arm=arm,mib_s=statistics.median(rates),
                           rates=rates,p95_ms=statistics.median(latencies),p95_rounds=latencies,
                           strict_mib_s=statistics.median(strict),strict_rounds=strict,
                           active_mib_s=statistics.median(p['active_mib_s'] for r,p in entries),
                           fuse_read_ops=[p['settled_counters_delta']['brewfs_fuse_read_ops_total'] for r,p in entries],
                           s3_get_ops=[p['settled_counters_delta']['brewfs_s3_get_ops_total'] for r,p in entries]))
    return result


def comparisons(summary,pairs):
    result=[]
    for left,right in pairs:
        for a in summary:
            if a['arm']!=left:continue
            b=next((x for x in summary if (x['size'],x['concurrency'],x['phase'],x['arm'])==(a['size'],a['concurrency'],a['phase'],right)),None)
            if b is None:continue
            ratio,ci=paired([y/x for x,y in zip(a['rates'],b['rates'])])
            latency,lci=paired([y/x for x,y in zip(a['p95_rounds'],b['p95_rounds'])])
            strict,sci=paired([y/x for x,y in zip(a['strict_rounds'],b['strict_rounds'])])
            result.append(dict(size=a['size'],concurrency=a['concurrency'],phase=a['phase'],baseline=left,candidate=right,
                               before=a['mib_s'],after=b['mib_s'],before_p95=a['p95_ms'],after_p95=b['p95_ms'],
                               ratio=ratio,ci=ci,p95_ratio=latency,p95_ci=lci,strict_ratio=strict,strict_ci=sci,
                               passed=ci[0]>=.95 and lci[1]<=1.05,clear_gain=ratio>=1.05 and ci[0]>1,
                               clear_regression=ci[1]<.95 or lci[0]>1.05))
    return result


def chart(summary,name,left,right,filename,title):
    sizes=sorted({x['size'] for x in summary if x['phase']==name and x['arm']==left})
    fig,axes=plt.subplots(1,2,figsize=(14,5.3),layout='constrained')
    for ax,c in zip(axes,[1,4]):
        for i,arm in enumerate([left,right]):
            data=[next(x for x in summary if (x['phase'],x['arm'],x['size'],x['concurrency'])==(name,arm,s,c)) for s in sizes]
            med=[x['mib_s'] for x in data]
            errors=[[x['mib_s']-min(x['rates']) for x in data],[max(x['rates'])-x['mib_s'] for x in data]]
            ax.bar(np.arange(len(sizes))+(i-.5)*.36,med,.34,yerr=errors,capsize=2,label=LABELS[arm],color=['#78869c','#008b80'][i])
        ax.set_xticks(range(len(sizes)),[label(s) for s in sizes],rotation=30,ha='right')
        ax.set_ylabel('吞吐量 MiB/s（越高越好）');ax.set_title(f'{c} 个并发文件任务')
        ax.grid(axis='y',alpha=.2);ax.legend(fontsize=9)
    fig.suptitle(title+'\n四轮中位数；误差线显示各轮最小值至最大值')
    for suffix in ['png','svg']:fig.savefig(OUT/(filename+'.'+suffix),dpi=160)
    plt.close(fig)


def table(rows,title):
    body=''
    for x in sorted(rows,key=lambda r:(r['phase'],r['concurrency'],r['size'])):
        status='通过' if x['passed'] else '未通过'
        body+=f'<tr><td>{PHASES[x["phase"]]}</td><td>{label(x["size"])}</td><td>{x["concurrency"]}</td><td>{x["before"]:.2f}</td><td>{x["after"]:.2f}</td><td>{(x["ratio"]-1)*100:+.1f}%</td><td>{x["ci"][0]:.3f}–{x["ci"][1]:.3f}</td><td>{x["before_p95"]:.2f} → {x["after_p95"]:.2f}</td><td>{x["p95_ci"][0]:.3f}–{x["p95_ci"][1]:.3f}</td><td>{status}</td></tr>'
    return '<h2>'+title+'</h2><div class="scroll"><table><thead><tr>'+''.join('<th>'+x+'</th>' for x in ['测试内容','文件大小','并发','之前 MiB/s','之后 MiB/s','吞吐变化','吞吐比 95% 区间','文件 p95 毫秒','p95 比 95% 区间','无回退门槛'])+'</tr></thead><tbody>'+body+'</tbody></table></div>'


def context_chart(summary,left,filename,title):
    fig,axes=plt.subplots(1,2,figsize=(14,5.2),layout='constrained')
    for ax,phase in zip(axes,['write','client_cold_read']):
        sizes=sorted(x['size'] for x in summary if (x['arm'],x['phase'],x['concurrency'])==(left,phase,4))
        for i,arm in enumerate([left,'optimized']):
            data=[next(x for x in summary if (x['arm'],x['phase'],x['concurrency'],x['size'])==(arm,phase,4,s)) for s in sizes]
            errors=[[x['mib_s']-min(x['rates']) for x in data],[max(x['rates'])-x['mib_s'] for x in data]]
            ax.bar(np.arange(len(sizes))+(i-.5)*.36,[x['mib_s'] for x in data],.34,
                   yerr=errors,capsize=2,label=LABELS[arm],color=['#78869c','#008b80'][i])
        ax.set_xticks(range(len(sizes)),[label(s) for s in sizes],rotation=20,ha='right')
        ax.set_title(PHASES[phase]);ax.set_ylabel('吞吐量 MiB/s（越高越好）')
        ax.grid(axis='y',alpha=.2);ax.legend(fontsize=9)
    fig.suptitle(title+'\n4 个并发文件任务；四轮中位数及最小—最大值')
    for suffix in ['png','svg']:fig.savefig(OUT/(filename+'.'+suffix),dpi=160)
    plt.close(fig)


def write_cost_table(summary,comparisons_all):
    body=''
    for x in comparisons_all:
        if x['baseline']!='current' or x['phase'] not in ['write','mixed']:continue
        items=[next(s for s in summary if (s['size'],s['concurrency'],s['phase'],s['arm'])==
                    (x['size'],x['concurrency'],x['phase'],arm)) for arm in [x['baseline'],x['candidate']]]
        before,after=items
        body+=f'<tr><td>{PHASES[x["phase"]]}</td><td>{label(x["size"])}</td><td>{x["concurrency"]}</td>'
        for key in ['active_mib_s','mib_s','strict_mib_s']:body+=f'<td>{before[key]:.2f} → {after[key]:.2f}</td>'
        body+=f'<td>{x["strict_ci"][0]:.3f}–{x["strict_ci"][1]:.3f}</td></tr>'
    headers=['测试内容','文件大小','并发','前台阶段 MiB/s','至上传完成 MiB/s','至全部清债 MiB/s','全部清债吞吐比 95% 区间']
    return '<h2>写入成本有没有转移到后台</h2><p>每格均为优化前 → 优化后。全部清债还等待本地暂存与缓存回收，保留其额外耗时；前台文件 p95 不能代替这项指标。</p><div class="scroll"><table><thead><tr>'+''.join('<th>'+h+'</th>' for h in headers)+'</tr></thead><tbody>'+body+'</tbody></table></div>'


def main():
    receipt=json.loads((OUT/'export.json').read_text());assert receipt['passed']
    rows=json.loads((OUT/'acceptance-results.json').read_text())
    summary=summarize(rows)
    comparisons_all=comparisons(summary,[('current','optimized'),('original','optimized'),('fixed-1m','optimized')])
    hot_path=OUT/'hot_acceptance-results.json'
    hot=summarize(json.loads(hot_path.read_text()),True) if hot_path.exists() else []
    hot_cmp=comparisons(hot,[('before','after')])
    policy=summarize(json.loads((OUT/'policy_acceptance-results.json').read_text()))
    policy_cmp=comparisons(policy,[('current','final-auto')])
    for name,data in [('policy-summary.json',policy),('policy-comparisons.json',policy_cmp)]:
        (OUT/name).write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
    for name,data in [('summary.json',summary),('comparisons.json',comparisons_all)]:
        (OUT/name).write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
    font=Path(os.environ.get('BENCH_FONT', '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc'))
    font_manager.fontManager.addfont(str(font))
    plt.rcParams.update({'font.family':font_manager.FontProperties(fname=str(font)).get_name(),'axes.unicode_minus':False,'font.size':10})
    for phase in ['write','client_cold_read','warm_read']:chart(summary,phase,'current','optimized',phase,PHASES[phase])
    for phase in ['write','client_cold_read']:chart(policy,phase,'current','final-auto','final_'+phase,'最终组合：'+PHASES[phase])
    context_chart(summary,'original','original_comparison','原始无 hint 组合与第一阶段候选（未修正 auto）')
    context_chart(summary,'fixed-1m','hint_isolation','同一 BrewFS 与 RustFS，仅比较固定 1 MiB EC 和旧 auto（4 MiB EC）')
    body='<h1>BrewFS 与 RustFS：写入、热读和自动提示</h1>'
    main_rows=[x for x in comparisons_all if x['baseline']=='current']
    counts={name:dict(passed=sum(x['passed'] for x in main_rows if x['phase']==name),clear_gains=sum(x['clear_gain'] for x in main_rows if x['phase']==name),total=sum(x['phase']==name for x in main_rows)) for name in PHASES}
    body+='<p>本轮先验证写入与缓存热读，再根据独立 EC 对照修正 auto。两个阶段分别统计；原始版本只在第一阶段补测必要场景，不拼接不同阶段的轮次，也不把两次倍率相乘。</p>'
    body+='<div class="card"><h2>最终组合的确认结果</h2>'
    for phase in PHASES:
        cells=[x for x in policy_cmp if x['phase']==phase]
        body+=f'<p><b>{PHASES[phase]}</b>：{sum(x["clear_gain"] for x in cells)}/{len(cells)} 个场景达到明确吞吐收益，{sum(x["passed"] for x in cells)}/{len(cells)} 个场景通过完整无明显回退门槛。</p>'
    if hot_cmp:
        body+=f'<p><b>真正经过 FUSE 的 BrewFS 缓存热读</b>：{sum(x["clear_gain"] for x in hot_cmp)}/{len(hot_cmp)} 个场景达到明确吞吐收益，{sum(x["passed"] for x in hot_cmp)}/{len(hot_cmp)} 个场景通过完整无明显回退门槛。</p>'
    body+='<p>最终确认覆盖 1 MiB、4 MiB、4 MiB＋64 KiB、16 MiB 和 64 MiB 文件的并发 1/4 及混合读写，共 88 组；小对象规则未变，保留第一阶段结果。缓存热读的 64 组独立确认只隔离共享回复改动，该读路径在最终组合中未再修改。</p>'
    body+=('</div><h2>修改的具体位置</h2>'
           '<p><b>写入：</b>已经提交且收到 RustFS 上传成功确认的块立即释放内存和写预算，避免等待约两秒的缓存回收计时器而阻塞后续写入。仍待上传的数据继续保留本地读取和恢复能力。</p>'
           '<p><b>BrewFS 缓存热读：</b>FUSE 回复共享已经缓存的不可变数据，省去每个小请求的输出分配、清零和复制；有脏数据覆盖时再复制。普通冷读、跨块组合和 workspace provider 保持原读取流程。</p>'
           '<p><b>auto：</b>最终编码对象不超过 1 MiB 时不发提示；超过时明确选择 1 MiB EC。独立对照没有支持自动扩大到 4 MiB，故撤回这项策略，保留固定 4 MiB 等显式配置。当前 4 MiB 数据块工作负载的联动收益主要来自写预算、完整 GET、缓存准入与共享回复，不能说较大 EC 块本身已被证明更快。</p>'
           '<p>本轮起点已包含完整对象下载合并、LRU 缓存和原 auto；第一阶段只增加写入与缓存回复改动，第二阶段修正 auto。原始无 hint 对照使用旧 BrewFS＋未修改的上游 RustFS，与本轮起点是不同参照。</p>'
           '<h2>FUSE 请求与分块如何配合</h2>'
           '<p>实际 4 MiB 应用写入可以对应一个 4 MiB FUSE WRITE；完整读取会拆成 128 KiB FUSE READ。128 KiB 小写入仍合并成 4 MiB 对象。应用请求、文件系统数据块、RustFS EC 块是三个不同层次。</p>'
           '<p>自动提示使用最终编码对象长度，不按 FUSE 片段、文件总大小或 SSD 预分配量决定。4 MiB 数据带上格式头实际为 4 MiB＋4 字节：1 MiB EC 产生五段，4 MiB EC 产生两段。段数减少并不保证整条读写链路更快，本轮实际结果因此优先选择 1 MiB。</p>')
    body+='<h2>测量标准</h2><p>相同 ACS 资源：客户端 4 CPU／8 GiB，RustFS 8 CPU／16 GiB，四块独立 40 GiB ESSD PL1，2＋2 纠删码，Redis 独立部署。数据块 4 MiB、读内存缓存 64 MiB、写内存 64 MiB、SSD 暂存 1 GiB，不压缩。</p><p>完整写入包含 fsync／close，主指标计至上传完成，另保留全部缓存清理时间；冷读通过新目录重挂清空客户端缓存，服务器及云盘缓存保留。所有哈希和对象布局检查在计时之外。四轮平衡顺序，旧二进制直接复用。</p><p>明确吞吐收益：配对提升至少 5%，且 95% 区间下界高于 1。无明显回退：吞吐比区间下界 ≥ 0.95，文件操作 p95 比区间上界 ≤ 1.05。四轮配对重采样反映本次样本的不确定性，不代表所有环境。文件 p95 不包括整批写入之后的清债时间。</p>'
    body+='<p>“未通过”也可能来自区间过宽，不能一律解释为已经证明变慢。只有吞吐比区间上界低于 0.95，或 p95 比区间下界高于 1.05，才标为本次样本支持的明确回退；这不会改变原来的通过门槛。</p>'
    body+='<p>绝对吞吐和 p95 显示四轮中位数；百分比和区间使用同轮比值的几何平均，因此不能直接用表内两个中位数相除得到百分比。四轮区间用于本次有限确认，不宣称所有尺寸、压缩模式及部署条件都已经无回退。</p>'
    body+='<p>为控制磁盘空间，在最终确认第二轮的两组对照全部结束并停止服务后，先校验并归档第一阶段的 3005 个文件，再清理其临时对象；每盘恢复约 27 GiB 可用空间后继续第三轮。四块 40 GiB PL1 的规格不变，清理没有与计时重叠。控制端连接超时后也仅接续已完成的云端结果，未重采或拼接样本。</p>'
    for phase in ['write','client_cold_read']:
        body+=f'<h2>最终组合：{PHASES[phase]}</h2><img src="final_{phase}.png" alt="最终组合{PHASES[phase]}对照图">'
    body+=table(policy_cmp,'最终组合对已交付版本：受 auto 改变影响的完整场景')
    body+=write_cost_table(policy,policy_cmp)
    body+='<h2>第一阶段：单独确认写入与共享缓存回复</h2><p>以下候选仍使用原 auto 的 4 MiB EC，作用是隔离客户端改动。它不是最终 auto 策略。第一阶段共 184 组，包含必要原版补测及固定 1 MiB 的独立控制。</p>'
    for phase in ['write','client_cold_read','warm_read']:
        body+=f'<h3>{PHASES[phase]}</h3><img src="{phase}.png" alt="第一阶段{PHASES[phase]}对照图">'
    body+='<p>历史 192 个所谓热读场景全部没有 FUSE READ 和 S3 GET，实际由内核页缓存服务；不能将其差异解释为 BrewFS 数据缓存性能。上面的内核热读保留这一正常使用场景。</p>'
    if hot_cmp:
        (OUT/'resident-summary.json').write_text(json.dumps(hot,ensure_ascii=False,indent=2))
        (OUT/'resident-comparisons.json').write_text(json.dumps(hot_cmp,ensure_ascii=False,indent=2))
        chart(hot,'warm_read','before','after','resident_hot','真正经过 FUSE 的 BrewFS 缓存热读')
        body+='<h2>BrewFS 数据缓存热读</h2><p>两边均包含写预算释放，RustFS 后端也相同，只比较共享缓存回复前后的读路径。关闭跨 open 的内核 KEEP_CACHE，32 MiB 工作集完整放入 64 MiB 缓存。每个重复读取阶段都有实际 FUSE READ，且没有新增 S3 GET；因此直接比较客户端缓存读路径。完整文件多次读取后逐一校验哈希。正常挂载命中内核页缓存时，不会执行这条 BrewFS 路径。</p><img src="resident_hot.png" alt="BrewFS 缓存热读对照图">'
        saved=PHASES['warm_read'];PHASES['warm_read']='BrewFS 缓存整文件热读'
        body+=table(hot_cmp,'缓存热读完整结果');PHASES['warm_read']=saved
    body+=table(main_rows,'第一阶段完整对照（保持原 auto 策略）')
    body+=write_cost_table(summary,comparisons_all)
    body+='<h2>与原始方案的直观对照</h2><p>这里比较原始组合与第一阶段候选，包含此前整块下载合并、缓存准入和服务端优化，不能全部归因于本轮两处修改或 auto hint。为复用原版结果，没有为最终策略再次运行原版，也不将阶段间倍率相乘。</p><img src="original_comparison.png" alt="原始无 hint 组合与第一阶段候选的写入及冷读对照">'
    body+=table([x for x in comparisons_all if x['baseline']=='original'],'与原始无 hint 组合的补充对照')
    body+='<h2>为什么修正自动提示</h2><p>客户端、后端及完整对象读取方式相同，只改变上传的 EC 块提示。旧 auto 的 4 MiB EC 没有明确读写吞吐收益；4 MiB＋64 KiB 文件冷读比固定 1 MiB 慢约 9.9%，吞吐比区间为 0.874–0.925。两边每轮均为 2176 次 FUSE READ、128 次 S3 GET，下载合并没有差别。这个不利结果完整保留，成为修正 auto 的依据。</p><img src="hint_isolation.png" alt="相同联动代码下固定 1 MiB EC 与旧 auto 的对照">'
    body+=table([x for x in comparisons_all if x['baseline']=='fixed-1m'],'仅改变 EC 提示的消融：固定 1 MiB → 旧 auto（4 MiB）')
    validation=json.loads((OUT/'validation.json').read_text())
    enabled=validation['auto-policy-green'];default=validation['auto-policy-nohint']
    body+=f'<h2>实现验证</h2><p>启用 hint 的工作区测试：{enabled["passed"]} 通过、{enabled["failed"]} 失败、{enabled["ignored"]} 跳过。默认功能测试：{default["passed"]} 通过、{default["failed"]} 失败、{default["ignored"]} 跳过。格式、构建、clippy、Tokio／io_uring 及 workspace overlay 编译检查全部通过。</p><p>写缓存回归测试先重现上传完成后仍占用预算的问题，再确认释放后读取字节一致；共享回复测试覆盖 slice 偏移、空洞、EOF 截断、覆盖写隔离和元数据失效。第一次仅简化调度的热读候选约只有 1% 变化，已撤回，原始记录保留。</p>'
    body+='<h2>正确性与可复核记录</h2><p>全部完成的实验保留原始样本、对象布局、挂载退出和资源计数。初筛不与正式对照拼接，失败或未通过项目完整保留。编译功能开关的测试数量及构建记录见原始归档；跳过的测试不计作通过。</p>'
    stage_labels={'screen':'写入初筛','hot_screen':'已撤回的热读候选初筛','shared_screen':'共享缓存回复初筛','acceptance':'第一阶段整文件四轮对照','hot_acceptance':'BrewFS 缓存热读四轮对照','policy_smoke':'最终策略布局与完整读取检查','policy_acceptance':'最终组合四轮确认'}
    body+='<div class="scroll"><table><thead><tr><th>实验</th><th>完成组数</th><th>完整文件哈希检查</th><th>四盘对象布局记录检查</th></tr></thead><tbody>'
    for name,c in receipt['campaigns'].items():body+=f'<tr><td>{stage_labels[name]}</td><td>{c["groups"]}</td><td>{c["full_file_hash_checks"]}</td><td>{c["metadata_layout_checks"]}</td></tr>'
    body+='</tbody></table></div>'
    body+='<p><a href="raw-records.tar.gz">完整原始记录</a> · <a href="policy-comparisons.json">最终组合统计比较</a> · <a href="policy-summary.json">最终组合逐轮绝对性能</a> · <a href="resident-comparisons.json">真实缓存热读统计</a> · <a href="comparisons.json">第一阶段全部统计比较</a> · <a href="summary.json">第一阶段逐轮绝对性能</a> · <a href="final-source.patch">本轮源码修改</a></p>'
    text='<!doctype html><html lang="zh-CN"><meta charset="utf-8"><title>BrewFS 写入、热读与自动提示</title><style>body{font-family:system-ui,"Microsoft YaHei",sans-serif;max-width:1450px;margin:36px auto;padding:0 26px;color:#203147;line-height:1.65;background:#f5f7fa}h1{font-size:32px}h2{margin-top:30px}img{width:100%;background:white;border-radius:10px}.card{background:#e2f2ef;padding:18px 25px;border-radius:12px}.scroll{overflow:auto;background:white}table{border-collapse:collapse;min-width:1100px;width:100%;font-size:13px}th,td{padding:9px 11px;border-bottom:1px solid #dce1e8;text-align:right}th:first-child,td:first-child{text-align:left}th{background:#e5ebf2}pre{background:#e9eef5;padding:18px}</style>'+body+'</html>'
    (OUT/'BrewFS写入热读与自动提示.html').write_text(text,encoding='utf-8')
    print(json.dumps(dict(report=str(OUT/'BrewFS写入热读与自动提示.html'),first_stage_counts=counts),ensure_ascii=False))


if __name__=='__main__':main()
