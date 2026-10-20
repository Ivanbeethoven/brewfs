"""Low-rate resource sampling; cgroup and host-scope pressure stay distinct."""
from pathlib import Path
import json
import sys
import time

pid=int(sys.argv[1]); folder=Path(sys.argv[2]); interval=.2
def identity():
    return Path(f'/proc/{pid}/stat').read_text().split(') ',1)[1].split()[19]
starttime=identity()
names=['memory.stat','memory.current','memory.max','memory.events','memory.pressure','cpu.pressure','io.pressure','cpu.stat','io.stat',
       'memory/memory.stat','memory/memory.usage_in_bytes','memory/memory.limit_in_bytes','memory/memory.failcnt',
       'cpu/cpu.stat','cpuacct/cpuacct.usage','cpuacct/cpuacct.stat']
paths={('cgroup/'+n):Path('/sys/fs/cgroup')/n for n in names}
paths.update({('host_pressure/'+n):Path('/proc/pressure')/n for n in ['cpu','io','memory']})
paths.update({('process/'+n):Path(f'/proc/{pid}')/n for n in ['stat','io','status']})
if json.loads((folder/'start.json').read_text())['job'].get('network_diagnostic'):
    paths.update({('network_namespace/'+n):Path('/proc/net')/n for n in ['snmp','netstat']})
paths={n:p for n,p in paths.items() if p.is_file()}
(folder/'sampler-protocol.json').write_text(json.dumps(dict(pid=pid,starttime=starttime,interval=interval,paths={k:str(v) for k,v in paths.items()},host_psi_is_not_container_psi=True)))
with (folder/'resources.jsonl').open('x') as output:
    while not (folder/'stop-sampler').exists():
        try:
            if identity()!=starttime:break
            begin=time.monotonic()
            row=dict(utc_ns=time.time_ns(),monotonic_ns=time.monotonic_ns(),values={n:p.read_text() for n,p in paths.items()})
            row['sample_seconds']=time.monotonic()-begin
            output.write(json.dumps(row)+'\n');output.flush()
        except FileNotFoundError:
            break
        time.sleep(max(0,interval-(time.monotonic()-begin)))
(folder/'sampler-closed.json').write_text(json.dumps(dict(ended=time.time(),pid=pid,starttime=starttime)))
