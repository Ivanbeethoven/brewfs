"""Keep the normal uninterrupted whole-file workload; observe phase TCP counters."""
from pathlib import Path
import runpy
import run_client as base

original_phase=base.core.io_phase

def network_snapshot():
    return {name:(Path('/proc/net')/name).read_text() for name in ['snmp','netstat']}

def phase(*args,**kwargs):
    before=network_snapshot();cgroup_before=base.cgroups()
    result,hashes,raw_before,raw_after=original_phase(*args,**kwargs)
    result['network_before']=before;result['network_after']=network_snapshot()
    result['diagnostic_cgroup_before']=cgroup_before;result['diagnostic_cgroup_after']=base.cgroups()
    result['network_diagnostic_only']=True
    return result,hashes,raw_before,raw_after

base.core.io_phase=phase
runpy.run_path('/work/run_size_budget_io.py',run_name='__main__')
