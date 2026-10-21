"""Readable whole-workflow comparisons; never substitute smoke for formal data."""
from pathlib import Path
import html
import json
import os
import shutil
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib import font_manager
from cloud import BASE, OUT

SCENES={'browse':'浏览定位（90%读）','edit':'编辑验证（70%读）','generate':'生成产物（30%读）'}
COLORS=['#446d9c','#d57930']
LABELS=['无动态联动（no-dynamic-block）','动态联动（auto）']


def size(n):
    if n==4259840:return '4 MiB + 64 KiB'
    return f'{n//1048576} MiB' if n>=1048576 else f'{n//1024} KiB'


def table(headers,rows):
    return '<div class="scroll"><table><thead><tr>'+''.join('<th>'+html.escape(h)+'</th>' for h in headers)+'</tr></thead><tbody>'+''.join('<tr>'+''.join('<td>'+html.escape(str(v))+'</td>' for v in r)+'</tr>' for r in rows)+'</tbody></table></div>'


def main():
    data=json.loads((BASE/'formal-analysis.json').read_text())
    comparisons=data['comparisons']
    font=os.environ.get('BENCH_FONT')
    if font:
        font_manager.fontManager.addfont(font)
    family=font_manager.FontProperties(fname=font).get_name() if font else 'Noto Sans CJK SC'
    plt.rcParams.update({'font.family':family,
        'axes.unicode_minus':False,'font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    def get(scene,metric,n=None):
        return next(r for r in comparisons if (r['scene'],r['metric'],r['file_bytes'])==(scene,metric,n))
    fig,axes=plt.subplots(1,2,figsize=(13,4.8),layout='constrained')
    for ax,metric,title in zip(axes,['task_seconds','remote_complete_seconds'],['文件任务完成时间（含内容校验）','包含后台上传排空的总时间']):
        rows=[get(s,metric) for s in SCENES];x=np.arange(3)
        for a,(key,color,label) in enumerate(zip(['baseline','dynamic'],COLORS,LABELS)):
            values=[r[key+'_geomean'] for r in rows]
            bars=ax.bar(x+(-.18 if a==0 else .18),values,.36,color=color,label=label)
            ax.bar_label(bars,fmt='%.2f',padding=3,fontsize=8)
            for i,r in enumerate(rows):
                jitter=np.linspace(-.07,.07,6)
                ax.scatter(i+(-.18 if a==0 else .18)+jitter,r[key+'_values'],s=9,color='#26364a',alpha=.5,zorder=3)
        ax.set(xticks=x,xticklabels=list(SCENES.values()),ylabel='秒，越低越好',title=title)
        ax.grid(axis='y',alpha=.18);ax.legend(fontsize=8)
    fig.savefig(OUT/'任务耗时对比.png',dpi=170);plt.close(fig)
    sizes=[4096,65536,262144,1048576,4259840,16777216]
    fig,axes=plt.subplots(3,2,figsize=(14,12),layout='constrained')
    for i,scene in enumerate(SCENES):
        for j,(metric,verb) in enumerate([('read_p95_ms','读取'),('save_p95_ms','保存（含 fsync 与 rename）')]):
            ax=axes[i,j];rows=[get(scene,metric,n) for n in sizes];x=np.arange(6)
            ax.bar(x-.18,[r['baseline_geomean'] for r in rows],.36,color=COLORS[0],label=LABELS[0])
            ax.bar(x+.18,[r['dynamic_geomean'] for r in rows],.36,color=COLORS[1],label=LABELS[1])
            ax.set(xticks=x,xticklabels=[size(n) for n in sizes],yscale='log',ylabel='文件 p95，毫秒（对数轴）',title=SCENES[scene]+' · '+verb)
            ax.tick_params(axis='x',rotation=20);ax.grid(axis='y',alpha=.18)
            if i==0:ax.legend(fontsize=8)
    fig.savefig(OUT/'按文件大小的延迟对比.png',dpi=170);plt.close(fig)
    # Every round is shown. No fastest/median run selection or tail trimming.
    fig,axes=plt.subplots(6,2,figsize=(14,17),layout='constrained')
    jobs=json.loads((BASE/'formal-plan.json').read_text())['jobs']
    for round_number in range(1,7):
        for arm,color,label in zip(['no-dynamic-block','dynamic-auto'],COLORS,LABELS):
            job=next(j for j in jobs if j['scene']=='edit' and j['round']==round_number and j['arm']['id']==arm)
            folder=BASE/'formal/raw'/job['id']/'client'
            result=json.loads((folder/'results.json').read_text())[0]
            operations=[r for w in json.loads((folder/'operations.json').read_text())['workers'] for r in w]
            operations.sort(key=lambda r:r['end_ns'])
            x=[0]+[(r['end_ns']-result['start_ns'])/1e9 for r in operations]
            y=np.concatenate(([0],np.cumsum([r['bytes']/1048576 for r in operations])))
            axes[round_number-1,0].plot(x,y,color=color,label=label,linewidth=1.4)
            samples=[json.loads(l) for l in (folder/'timeline.jsonl').read_text().splitlines()]
            start=result['start_ns']/1e9
            selected=[s for s in samples if start<=s['monotonic']<=result['drained_end_ns']/1e9]
            debt_keys=['brewfs_writeback_dirty_bytes','brewfs_writeback_live_dirty_bytes',
                       'brewfs_writeback_recent_pending_upload_bytes','brewfs_writeback_remote_upload_inflight_bytes']
            # The counters overlap. Plot their maximum, never sum them as bytes owed.
            dx=[s['monotonic']-start for s in selected]
            dy=[max(s['counters'][k] for k in debt_keys)/1048576 for s in selected]
            dx.extend(s['time']-start for s in result['drain_samples'])
            dy.extend(max(s['values'].values())/1048576 for s in result['drain_samples'])
            points=sorted(zip(dx,dy))
            axes[round_number-1,1].plot([p[0] for p in points],[p[1] for p in points],color=color,label=label,linewidth=1.2)
            for ax in axes[round_number-1]:
                ax.axvline(result['total_seconds'],color=color,linestyle=':',alpha=.6)
        for j,ax in enumerate(axes[round_number-1]):
            ax.set(xlabel='自开始后的秒数',ylabel='MiB',title=f'编辑验证，第{round_number}轮 · '+('累计完整读写字节' if j==0 else '四项写回债务中的最大值'))
            ax.grid(alpha=.18)
            if round_number==1:ax.legend(fontsize=8)
    fig.savefig(OUT/'编辑验证完整时间线.png',dpi=160);plt.close(fig)
    bottlenecks=json.loads((BASE/'formal-bottlenecks.json').read_text())['rows']
    components=json.loads((BASE/'formal-components.json').read_text())['rows']
    # Means preserve the sum of the critical worker's disjoint time spans.
    # These are explicitly separate from the geometric paired performance table.
    groups=[('完整读取',['read_io_seconds'],'#446d9c'),
        ('保存：打开与写入',['save_open_seconds','save_write_seconds'],'#75a6aa'),
        ('保存：fsync',['save_fsync_seconds'],'#cc793e'),
        ('保存：关闭与改名',['save_close_seconds','save_rename_seconds'],'#d6b04e'),
        ('内容校验与其他',['post_io_check_seconds','other_worker_seconds','save_wrapper_seconds','join_and_finish_seconds'],'#939bad'),
        ('最后排空',['drain_seconds'],'#b44e5a')]
    fig,axes=plt.subplots(1,3,figsize=(14,5.5),layout='constrained')
    for ax,scene in zip(axes,SCENES):
        bottom=np.zeros(2)
        for label,keys,color in groups:
            values=[np.mean([sum(r['critical_worker_seconds'][k] for k in keys) for r in bottlenecks
                            if r['scene']==scene and r['arm']==arm]) for arm in ['no-dynamic-block','dynamic-auto']]
            ax.bar([0,1],values,.6,bottom=bottom,label=label,color=color)
            bottom+=values
        ax.set(xticks=[0,1],xticklabels=['无动态联动','动态联动 auto'],ylabel='秒，六轮算术平均',title=SCENES[scene])
        ax.grid(axis='y',alpha=.18)
    axes[-1].legend(fontsize=8,loc='upper left',bbox_to_anchor=(1.01,1))
    fig.savefig(OUT/'任务关键路径.png',dpi=170);plt.close(fig)
    verdict=data['verdict']
    headline='主比较通过既定性能门槛。' if verdict['primary_passed'] else '本轮尚未通过整套性能门槛，所有不利维度保留如下。'
    body='<h1>BrewFS：Agent 混合读写对比</h1><p class="lead">'+headline+'</p>'
    body+='<p>同一 BrewFS 与 RustFS 二进制，在实际 FUSE 挂载目录重放相同文件轨迹。对照是运行时关闭动态联动的 no-dynamic-block；auto 同时启用当前整对象下载和缓存策略。两组实际 EC 大小均为 1 MiB，不能把收益单独归因于块大小变化。</p>'
    body+='<h2>先看这三项结果</h2><p>以下总时间包含文件任务、内容校验和后台上传排空。每行是六轮配对结果，两个方案完成相同的文件操作和字节数。</p>'
    body+=table(['场景 / 读写比例','no-dynamic-block 总时间','auto 总时间','有效吞吐变化','吞吐变化的95%区间'],
        [[SCENES[s]+' / '+ratio,
          f'{get(s,"remote_complete_seconds")["baseline_geomean"]:.2f} 秒',
          f'{get(s,"remote_complete_seconds")["dynamic_geomean"]:.2f} 秒',
          f'{(get(s,"effective_mib_s")["ratio"]-1)*100:+.1f}%',
          f'[{(get(s,"effective_mib_s")["ci_low"]-1)*100:+.1f}%, {(get(s,"effective_mib_s")["ci_high"]-1)*100:+.1f}%]']
         for s,ratio in zip(SCENES,['90/10','70/30','30/70'])])
    body+=f'<p>有效吞吐按应用完成的逻辑字节计算，包含缓存命中，不代表实际网络或 S3 传输带宽。强制性能门槛通过 {verdict["required_comparisons"]-len(verdict["failed_required"])}/{verdict["required_comparisons"]} 项。未通过项见后文；不能把吞吐提升等同于全部验收通过。</p>'
    body+='<h2>如何测试</h2><p>三个场景分别为 90/10、70/30、30/70 读写操作比例；四并发，每次 8,000 操作、3,842.5 MiB 逻辑读写。六种文件大小在同一轨迹内混合，含共享读取、热点、保存替换、创建产物与写后读取。六轮配对、共 36 次运行；smoke 未混入统计。读后完整哈希进入任务总时间，不进入单文件 I/O 延迟。</p>'
    body+=table(['混合文件大小','操作占比'],[[size(n),share] for n,share in zip(sizes,['40%','30%','15%','10%','4%','1%'])])
    body+='<p>应用读写缓冲区上限 128 KiB；BrewFS 数据块 4 MiB、chunk 64 MiB。读内存与写内存各 64 MiB、读磁盘缓存关闭、写暂存容量 1 GiB、后台上传并发 3，两组一致。每个文件完整读写，不测对象局部读取。保存包含临时文件 write、fsync、close、rename；预填充后重挂载再计时。</p>'
    body+='<p>实际环境：杭州 ACS，客户端通用型 4 CPU/8 GiB、RustFS 性能型 16 CPU/32 GiB、Redis 1 CPU/2 GiB、四块 40 GiB ESSD PL1。两种 16 CPU 客户端的 FUSE 设备预检均失败，因此测量前固定为已验证可挂载的规格。客户端名义出入总带宽 1.5 Gbit/s；完整保留传输限制及尾部影响。</p>'
    recoveries=sorted(BASE.glob('formal-recovery*/reattachment.json'))
    if recoveries:
        resumed=[json.loads(p.read_text())['job'] for p in recoveries]
        numbers='、'.join(str(int(j.split('-')[2])) for j in resumed)
        body+=f'<p>采集过程中，管理接口出现 {len(recoveries)} 次连接故障，控制器在第 {numbers} 次作业的观察或布局检查阶段退出。各远端客户端均独立正常完成，控制器接回原始结果后继续原定顺序。测量程序、输入、二进制及门槛未变，没有重跑或拼接样本；连接失败与接续记录随归档保留。</p>'
    body+='<p>吞吐比 95% 区间下界至少 0.95，读/保存 p95 比区间上界最多 1.05。明确收益要求吞吐点估计至少 +5% 且区间下界大于 1；编辑验证为主场景。区间以六个配对轮次计算，不把文件操作当作独立实验。区间按单项比较解释。</p>'
    primary=get('edit','effective_mib_s')
    body+=f'<p><strong>主场景编辑验证：</strong>含排空有效吞吐变化 {(primary["ratio"]-1)*100:+.1f}%，95% 区间 [{(primary["ci_low"]-1)*100:+.1f}%，{(primary["ci_high"]-1)*100:+.1f}%]；'+('达到' if primary['clear_gain'] else '未达到')+'预定明确收益标准。其余无回退门槛分别见下表。</p>'
    body+='<img src="任务耗时对比.png" alt="两方案三个场景的任务完成及上传排空耗时，显示每轮点">'
    metrics={'task_seconds':'任务完成（秒）','remote_complete_seconds':'含远端完成（秒）','read_p95_ms':'读 p95（毫秒）','save_p95_ms':'保存 p95（毫秒）','effective_mib_s':'含排空有效吞吐（MiB/s）'}
    def values(r):
        if r['no_regression']:
            decision='通过无回退门槛'
        else:
            adverse=r['ci_high']<.95 if r['higher_is_better'] else r['ci_low']>1.05
            decision='未通过：明确回退' if adverse else '未通过：证据不足'
        return [f'{r["baseline_geomean"]:.3f}',f'{r["dynamic_geomean"]:.3f}',
                f'{(r["ratio"]-1)*100:+.1f}%',f'[{(r["ci_low"]-1)*100:+.1f}%, {(r["ci_high"]-1)*100:+.1f}%]',
                decision]
    body+='<h2>绝对值与完整对照</h2>'+table(['场景','指标','无动态联动','动态联动','变化','变化的95%区间','判定'],
        [[SCENES[s],metrics[m],*values(get(s,m))] for s in SCENES for m in metrics])
    body+='<p>时间和延迟越低越好，吞吐越高越好。柱高及表格为六轮几何平均；散点为每轮原值。保存延迟包括 fsync 与 rename，不能冒充每文件远端持久化延迟。最终四项债务连续三次为零后才结束总计时。</p>'
    body+='<h2>不同文件大小的读写延迟</h2><img src="按文件大小的延迟对比.png" alt="六档文件大小的完整读取和保存p95对比">'
    body+=table(['场景','操作','文件大小','无动态联动 ms','动态联动 ms','变化','变化的95%区间','判定'],
        [[SCENES[s],'读' if m=='read_p95_ms' else '保存',size(n),*values(get(s,m,n))] for s in SCENES for m in ['read_p95_ms','save_p95_ms'] for n in sizes])
    body+='<h2>完整时间线</h2><p>六轮编辑验证全部展示。四项债务含重叠记账，图中取最大值而不是相加；每项原值在原始数据中保留。虚线标记含排空结束，慢尾段没有删除。</p><img src="编辑验证完整时间线.png" alt="编辑验证六轮累计完成量与完整写回债务曲线">'
    body+='<h2>时间花在哪里</h2><p>取每次最后完成的 worker，拆分它自身按顺序发生的时间，再加最终排空。下图使用六轮算术平均，分段之和等于总耗时；它说明观测到的等待位置，不把 fsync 等同于纯磁盘 I/O。保存写入的是临时文件，随后 fsync、关闭、原子改名。</p><img src="任务关键路径.png" alt="两组在三个场景的关键worker耗时分解">'
    def mechanism(scene,arm):
        bs=[r for r in bottlenecks if r['scene']==scene and r['arm']==arm]
        cs=[r for r in components if r['scene']==scene and r['arm']==arm]
        mean=lambda key:np.mean([r['counters'][key] for r in bs])
        return [SCENES[scene],LABELS[0 if arm=='no-dynamic-block' else 1],
            f'{mean("brewfs_s3_get_ops_total"):.0f}',f'{mean("brewfs_s3_put_ops_total"):.0f}',
            f'{mean("brewfs_writeback_stage_lat_us_total")/1e6:.3f}',
            f'{np.mean([r["save_components"]["fsync"]["sum_ms"] for r in cs])/1000:.3f}',
            f'{np.mean([r["fuse_average_read_request_bytes"] for r in bs])/1024:.1f}',
            f'{np.mean([r["fuse_average_write_request_bytes"] for r in bs])/1024:.1f}']
    body+=table(['场景','方案','GET 次数','PUT 次数','stage 累计秒','应用 fsync 累计秒','平均 FUSE 读请求 KiB','平均 FUSE 写请求 KiB'],
        [mechanism(s,a) for s in SCENES for a in ['no-dynamic-block','dynamic-auto']])
    body+='<p>以上为六轮算术平均。stage 包含落盘、封存记录和元数据提交；它与 fsync、上传等存在重叠，跨 worker 的累计秒数不能相加成任务耗时。FUSE 请求数按实际计数计算，仅有平均值，没有逐请求大小直方图。应用请求上限为 128 KiB，BrewFS 数据块固定 4 MiB；两者不是同一个大小。</p>'
    body+='<p>客户端暂存目录以 mount-observation.json 的实测设备为准，不能将未核实的物理介质称为本地 SSD。进程、容器 CPU、内存、重传和采样时段全部保存在机制数据中，不根据这些值剔除慢样本。</p>'
    profile=BASE/'stage-profile/raw/agent-diagnostic-stage-edit/client/stage-syscalls.txt'
    if profile.exists():
        body+='<h2>保存路径的下一步优化</h2><p>写入较多时收益较小；关键路径显示保存的 fsync、打开、写入与改名占主导。独立 syscall 诊断记录 4,800 次 fsync、278,270 次 openat、278,005 次 close 和 446,147 次 read。源码中的脏数据恢复回退只先检查全局是否存在脏切片，随后扫描目录并读取元数据，即使请求的是另一个 inode。这是值得优先核实的调用放大来源，当前汇总没有文件路径，尚不能确认这些调用全部来自该处。</p>'
        body+='<p>后续先做路径归因，再分别验证按 inode/chunk 缩小恢复扫描范围、合并单次暂存的阻塞文件操作；每次只验证一个假设。保持 fsync 和目录同步语义，使用同一三场景协议比较新旧 auto，再展示与 no-dynamic-block 的对照。诊断中 readv 的等待包含 FUSE 接收线程等待请求；跨线程 syscall 累计时间不能当作任务关键路径，也不能据此把 readv 排成首要 CPU 瓶颈。插桩数据未计入 36 次性能验收。</p>'
        body+='<details><summary>独立 syscall 诊断原文（不作为性能结果）</summary><pre>'+html.escape(profile.read_text())+'</pre></details>'
    audit=json.loads((BASE/'independent-audit.json').read_text())
    body+='<h2>结果完整性</h2><p>36 次作业共 288,000 个操作、182,400 次即时完整读取校验、160,800 次重挂载完整文件校验、108 次挂载正常退出；'+str(audit['totals']['physical_layout_records'])+' 份物理布局记录核验通过。正确性核验通过与性能门槛通过分别判断。</p>'
    assert audit['passed'] and audit['complete_campaign']
    failures=[r for r in comparisons if r['required'] and not r['no_regression']]
    body+='<h2>不利维度</h2>'
    if failures:
        body+=table(['场景','指标','文件大小','无动态联动','动态联动','变化','变化的95%区间','判定'],
            [[SCENES[r['scene']],metrics[r['metric']],size(r['file_bytes']) if r['file_bytes'] else '混合整体',*values(r)] for r in failures])
    else:
        body+='<p>45 项强制性能比较均通过预定无回退门槛。明确收益仍单独按主场景吞吐的要求判断。</p>'
    body+='<details><summary>展开全部六轮原始值</summary>'+table(['场景','指标','文件大小','方案','第1轮','第2轮','第3轮','第4轮','第5轮','第6轮'],
        [[SCENES[r['scene']],metrics[r['metric']],size(r['file_bytes']) if r['file_bytes'] else '混合整体',LABELS[i],
          *[f'{v:.4f}' for v in r[key+'_values']]] for r in comparisons for i,key in enumerate(['baseline','dynamic'])])+'</details>'
    body+='<p>这里是合成 agent 文件负载，不包含模型推理或编译；prepared 内容的前 128 KiB 随版本变化，大文件其余部分为同大小档共享的伪随机字节，两边相同且均完整写入。压缩关闭，未配置后端内容去重。完整校验会消耗客户端 CPU，因此同时提供不含哈希的文件 I/O 延迟。</p>'
    body+='<p><a href="测试方案.html">测量前冻结方案</a> · <a href="formal-analysis.json">全部原值与统计</a> · <a href="formal-bottlenecks.json">机制与资源数据</a> · <a href="execution-bindings.json">实际部署与执行绑定</a> · <a href="independent-audit.json">完整核验记录</a> · <a href="raw-records.tar.gz">下载原始记录</a></p>'
    css='body{max-width:1300px;margin:32px auto;padding:0 24px;background:#f7f9fb;color:#23354a;font:16px/1.8 system-ui,"Microsoft YaHei",sans-serif}.lead{background:#fff4df;padding:18px;border-left:5px solid #cf8c24}img{width:100%;background:white;margin:18px 0}.scroll{overflow:auto}table{border-collapse:collapse;background:white;font-size:13px;width:100%}td,th{padding:10px;border-bottom:1px solid #dae1e8;text-align:left}th{background:#e8eef5}h2{margin-top:36px}a{color:#215c91}'
    (OUT/'Agent混合读写结果.html').write_text('<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>BrewFS Agent混合读写对比</title><style>'+css+'</style>'+body+'</html>')
    for name in ['formal-analysis.json','execution-bindings.json']:
        shutil.copyfile(BASE/name,OUT/name)
    print('Chinese report and4figures generated; all51comparisons retained.',flush=True)


if __name__=='__main__':
    main()
