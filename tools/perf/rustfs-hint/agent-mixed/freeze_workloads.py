"""Generate/audit paired FUSE workload plans; never run I/O or create cloud resources."""
from collections import Counter
import hashlib
import html
import json
from pathlib import Path, PurePosixPath
import random

from cloud import BASE, WORK, OUT, load_binaries
REPO = WORK.parents[3]
MIB = 1024**2
SIZES = [(4096, 40), (65536, 30), (262144, 15), (MIB, 10), (4*MIB+65536, 4), (16*MIB, 1)]
SCENES = [('browse', '浏览定位', 90), ('edit', '编辑验证', 70), ('generate', '生成产物', 30)]
ARMS = ['no-dynamic-block', 'dynamic-auto']
ORDERS = [ARMS, ARMS[::-1], ARMS[::-1], ARMS, ARMS, ARMS[::-1]]


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')) + '\n').encode()


def corpus(owner, size, count):
    return [f'{owner}/size-{size}/file-{i:04d}.bin' for i in range(count)]


def initial_corpus():
    return {path: size for size, percentage in SIZES
            for owner, count in [('shared', 10*percentage)] + [(f'worker-{w}', 5*percentage) for w in range(4)]
            for path in corpus(owner, size, count)}


def trace(round_number, scene_index, read_percent):
    rows = []
    initial = initial_corpus()
    for worker in range(4):
        rng = random.Random(202610200000 + round_number*100 + scene_index*10 + worker)
        schedule = []
        for size, percentage in SIZES:
            reads = 2000 * read_percent * percentage // 10000
            writes = 2000 * (100-read_percent) * percentage // 10000
            assert writes % 2 == 0
            schedule += [('read', size)] * reads
            schedule += [('replace', size)] * (writes//2)
            schedule += [('create', size)] * (writes//2)
        assert len(schedule) == 2000
        rng.shuffle(schedule)
        versions = {p: 'initial:' + p for p in initial}
        recent = {}
        for index, (op, size) in enumerate(schedule):
            percentage = dict(SIZES)[size]
            locality = 'new'
            if op == 'create':
                path = f'worker-{worker}/generated/op-{index:04d}.bin'
            elif op == 'read' and rng.random() < .2 and size in recent:
                path, locality = recent[size], 'read-after-write'
            else:
                shared = op == 'read' and rng.random() < .5
                candidates = corpus('shared' if shared else f'worker-{worker}', size,
                                    percentage*(10 if shared else 5))
                hot_count = len(candidates)//5
                hot = rng.random() < .8
                path = rng.choice(candidates[:hot_count] if hot else candidates[hot_count:])
                locality = ('shared-' if shared else 'private-') + ('hot' if hot else 'cold')
            version = versions[path] if op == 'read' else f'write:r{round_number}:s{scene_index}:w{worker}:op{index}'
            if op != 'read':
                versions[path] = version
                recent[size] = path
            rows.append(dict(worker=worker, index=index, op=op, bytes=size, path=path,
                             content_version=version, locality=locality))
    return rows


def audit(rows, read_percent):
    # Independently replay the complete namespace and content-version dependencies.
    states = {p: (size, 'initial:' + p) for p, size in initial_corpus().items()}
    counts = Counter()
    for row in rows:
        p = PurePosixPath(row['path'])
        assert not p.is_absolute() and '..' not in p.parts
        worker, op, size = row['worker'], row['op'], row['bytes']
        assert p.parts[0] in ['shared', f'worker-{worker}']
        counts[worker, 'read' if op == 'read' else 'write', size] += 1
        value = (size, row['content_version'])
        if op == 'read':
            assert states[row['path']] == value
        else:
            assert p.parts[0] == f'worker-{worker}'
            assert (row['path'] in states) == (op == 'replace')
            if op == 'replace':
                assert states[row['path']][0] == size
                assert states[row['path']][1] != row['content_version']
            states[row['path']] = value
    for worker in range(4):
        assert [r['index'] for r in rows if r['worker'] == worker] == list(range(2000))
        for size, percentage in SIZES:
            assert counts[worker, 'read', size] == 2000*read_percent*percentage//10000
            assert counts[worker, 'write', size] == 2000*(100-read_percent)*percentage//10000
    return dict(operations=len(rows), logical_bytes=sum(r['bytes'] for r in rows),
                operation_counts=dict(Counter(r['op'] for r in rows)),
                locality_counts=dict(Counter(r['locality'] for r in rows)),
                final_files=len(states), dependencies_and_ownership_passed=True)


def main():
    manifest = load_binaries()
    BASE.mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=False)
    (OUT/'traces').mkdir()
    index, jobs = [], []
    for round_number, order in enumerate(ORDERS, 1):
        for scene_index in [(round_number-1+i) % 3 for i in range(3)]:
            scene_id, label, read_percent = SCENES[scene_index]
            rows = trace(round_number, scene_index, read_percent)
            summary = audit(rows, read_percent)
            assert rows == trace(round_number, scene_index, read_percent)
            raw = b''.join(encoded(row) for row in rows)
            path = f'traces/round-{round_number}-{scene_id}.jsonl'
            (OUT/path).write_bytes(raw)
            entry = dict(path=path, sha256=hashlib.sha256(raw).hexdigest(), round=round_number,
                         scene=scene_id, label=label, read_percent=read_percent, **summary)
            index.append(entry)
            for arm in order:
                jobs.append(dict(run=len(jobs)+1, round=round_number, scene=scene_id, arm=arm,
                                 trace=path, trace_sha256=entry['sha256']))
    source = REPO/'innovations/brewfs-rustfs/agent-mixed-fuse-plan.md'
    (OUT/'测试方案.md').write_bytes(source.read_bytes())
    plan = dict(status='planned_not_executed', purpose='BrewFS FUSE agent-like mixed file I/O',
        primary_scene='edit', workers=4, ops_per_worker=2000, application_request_max_bytes=131072,
        corpus_files=3000, corpus_bytes=sum(initial_corpus().values()),
        sizes=[dict(bytes=s, operation_percent=p) for s,p in SIZES],
        scenes=[dict(id=i, label=l, read_percent=r, write_percent=100-r) for i,l,r in SCENES],
        paired_rounds=6, jobs=jobs, traces=index,
        source_commit=manifest['brewfs']['revision'],
        brewfs_sha256=manifest['brewfs']['sha256'],
        rustfs_commit=manifest['candidate']['revision'],
        rustfs_sha256=manifest['candidate']['sha256'],
        arms={'no-dynamic-block': dict(compiled_hint_feature=True, runtime_hint=None, server_accept_hint=False),
              'dynamic-auto': dict(compiled_hint_feature=True, runtime_hint='auto', server_accept_hint=True)},
        existing_code_comparison=True, actual_ec_layout_may_be_identical=True,
        product_block_bytes=4*MIB, product_chunk_bytes=64*MIB, compression='none',
        cache=dict(read_memory_bytes=64*MIB, read_ssd_bytes=0, write_memory_bytes=64*MIB,
                   write_stage_bytes=1024*MIB, memory_budget_bytes=512*MIB),
        fuse=dict(direct_io=0, keep_cache=1, kernel_writeback=False, workers=4, max_background=64),
        upload_permits=3, drain=dict(stable_zero_samples=3, interval_ms=20, timeout_seconds=120),
        statistics=dict(unit='paired round', bootstrap_percentiles=[2.5,97.5], bootstrap_samples=20000,
                        bootstrap_seed=20261020, throughput_ratio_ci_lower=.95, p95_ratio_ci_upper=1.05,
                        clear_gain_point_min=1.05, clear_gain_ci_lower_strict=1.0),
        resources=dict(created=False, region='cn-hangzhou', compute_class='performance',
                       client=dict(vcpu=4, memory_gib=8, compute_class='general-purpose'), store=dict(vcpu=16, memory_gib=32),
                       redis=dict(vcpu=1, memory_gib=2), disks=dict(count=4, gib_each=40, type='ESSD PL1')),
        before_measurement=['Implement the FUSE replay runner and audit its timing/namespace semantics',
                            'Verify all actual resource, mount, process, binary and layout identities',
                            'Freeze remaining tool hashes and pass smoke including fsync/rename/remount/hash'],
        generator_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        protocol_sha256=hashlib.sha256(source.read_bytes()).hexdigest())
    assert len(index) == 18 and len(jobs) == 36
    for i in range(0,len(jobs),2):
        assert jobs[i]['trace_sha256'] == jobs[i+1]['trace_sha256']
        assert {jobs[i]['arm'],jobs[i+1]['arm']} == set(ARMS)
    (OUT/'experiment-plan.json').write_text(json.dumps(plan,ensure_ascii=False,indent=2)+'\n')
    receipt = dict(passed=True, executed_io=False, resources_created=False, unique_traces=18,
                   compared_runs=36, unique_scheduled_operations=sum(r['operations'] for r in index),
                   paired_trace_identity=True, deterministic_generation=True,
                   namespace_content_dependencies=True, exact_size_and_rw_counts=True)
    (OUT/'plan-validation.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n')
    scene_rows = ''.join(f'<tr><td>{l}</td><td>{r}% / {100-r}%</td><td>8,000</td></tr>' for _,l,r in SCENES)
    names = ['4 KiB','64 KiB','256 KiB','1 MiB','4 MiB + 64 KiB','16 MiB']
    size_rows = ''.join(f'<tr><td>{n}</td><td>{p}%</td></tr>' for n,(_,p) in zip(names,SIZES))
    page = '''<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>BrewFS：Agent 混合读写测试方案</title><style>body{max-width:1000px;margin:40px auto;padding:0 24px;color:#203040;background:#fafbfd;font:17px/1.8 system-ui,"Microsoft YaHei",sans-serif}h1{line-height:1.4}h2{margin-top:36px}table{border-collapse:collapse;width:100%;background:white}th,td{padding:10px 16px;border-bottom:1px solid #dce3eb;text-align:left}th{background:#eaf0f7}aside{padding:18px 22px;border-left:5px solid #cb8b26;background:#fff7e9}.columns{display:grid;grid-template-columns:1fr 1fr;gap:24px}pre{white-space:pre-wrap;font:15px/1.9 system-ui}a{color:#2058a3}@media(max-width:700px){.columns{grid-template-columns:1fr}}</style>
<h1>BrewFS × RustFS<br>Agent 混合读写测试方案</h1>
<p>状态：方案与配对轨迹已生成；尚未运行性能测试，尚无新性能结论。</p>
<aside><b>对照定义：</b>同一 BrewFS / RustFS 二进制，比较运行时关闭动态联动的 no-dynamic-block 与开启 auto 的方案。当前两者 EC 大小可能相同；收益需按实际机制解释，不能直接归给块大小。</aside>
<h2>只保留三个端到端场景</h2><table><tr><th>场景</th><th>读 / 写操作数</th><th>每轮操作数</th></tr>''' + scene_rows + '''</table>
<div class="columns"><section><h2>同一轨迹内混合六种大小</h2><table><tr><th>完整文件大小</th><th>操作占比</th></tr>''' + size_rows + '''</table></section><section><h2>固定测试条件</h2>
<p>真实 FUSE 挂载目录，四并发，应用最大请求 128 KiB。BrewFS 数据块 4 MiB，chunk 64 MiB。缓存、上传并发、资源和文件内容一致。</p>
<p>一半写操作保存替换，一半创建产物；包含共享读取、热点复读和读取刚修改的文件。每轮 3,842.5 MiB 逻辑 I/O。</p>
<p>6 个配对轮次，36 次运行。两个方案重放相同轨迹；不与旧实验拼接。</p></section></div>
<h2>交付数据与图表</h2><p>每个场景给出两边的任务耗时、含远端完成总耗时、读 p95、保存 p95，列出绝对值、变化率和 95% 区间。再按六种文件大小展示延迟对比，并画完整写回债务曲线。</p>
<p>预设门槛：有效吞吐比区间下界 ≥0.95，p95 比区间上界 ≤1.05。明显收益要求点估计 ≥+5% 且区间下界 >1。所有不利维度完整报告。</p>
<p><a href="测试方案.md">完整方案</a> · <a href="experiment-plan.json">精确配置与配对清单</a> · <a href="plan-validation.json">规划校验回执</a></p>
<details><summary>展开全部方法、版本与限制</summary><pre>''' + html.escape(source.read_text()) + '</pre></details></html>'
    (OUT/'测试方案.html').write_text(page)
    print(json.dumps(receipt,ensure_ascii=False))


if __name__ == '__main__':
    main()
