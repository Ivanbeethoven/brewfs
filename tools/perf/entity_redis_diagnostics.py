#!/usr/bin/env python3
"""Capture and explain Redis traffic for diagnostic entity mount runs only.

MONITOR perturbs latency. External commands are not network round trips: one
pipeline may contain several commands. Lua commands never represent client RPCs.
Commandstats EVAL/EVALSHA time includes nested Lua commands; do not sum both.
"""

import argparse
import collections
import hashlib
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import time


# redis-cli strips the RESP simple-string '+'; direct socket capture preserves it.
LINE = re.compile(rb'^\+?(\d+\.\d+) \[(\d+) ([^\]]+)\] (.*)$')
QUOTED = re.compile(rb'"((?:[^"\\]|\\.)*)"')
RAW_CONSTANT = re.compile(
    r'\b(?:const|static)\s+(\w+)\s*:\s*&(?:\s*\x27static\s+)?str\s*=\s*'
    r'r(?P<hashes>\#*)"(?P<body>.*?)"(?P=hashes)\s*;', re.S
)


def write_json(path, value):
    pathlib.Path(path).write_text(json.dumps(value, indent=2) + "\n")


def unquote(value):
    """Decode Redis sdscatrepr escapes, including binary \x5cxHH arguments."""
    result = bytearray()
    index = 0
    escapes = {ord('n'): 10, ord('r'): 13, ord('t'): 9, ord('a'): 7, ord('b'): 8}
    while index < len(value):
        byte = value[index]
        index += 1
        if byte == 92:
            if index == len(value):
                raise ValueError("truncated MONITOR escape")
            byte = value[index]
            index += 1
            if byte == ord('x'):
                if index + 2 > len(value):
                    raise ValueError("truncated MONITOR hex escape")
                byte = int(value[index:index + 2], 16)
                index += 2
            else:
                byte = escapes.get(byte, byte)
        result.append(byte)
    return bytes(result)


def monitor_row(line):
    match = LINE.fullmatch(line.rstrip(b'\r\n'))
    if match is None:
        raise ValueError("invalid MONITOR line")
    encoded = match[4]
    args = []
    previous = 0
    for argument in QUOTED.finditer(encoded):
        if encoded[previous:argument.start()].strip():
            raise ValueError("invalid MONITOR argument boundary")
        args.append(unquote(argument[1]))
        previous = argument.end()
    if not args or encoded[previous:].strip():
        raise ValueError("invalid MONITOR argument list")
    command = args[0].decode('ascii').lower()
    if command in ('script', 'client', 'config', 'command') and len(args) > 1:
        command += '|' + args[1].decode('ascii').lower()
    return {'time': float(match[1]), 'db': int(match[2]),
            'source': match[3].decode('ascii'), 'command': command, 'args': args}


def inventory(args):
    root = pathlib.Path(args.root)
    names = subprocess.check_output([
        'git', '-C', str(root), 'ls-files', '-z', '--cached', '--others',
        '--exclude-standard', '--', 'src/**/*.rs', 'src/**/*.lua',
    ]).decode().split('\0')
    scripts = collections.defaultdict(list)
    for name in sorted(set(names) - {''}):
        path = root / name
        if not path.is_file():
            continue
        source = path.read_bytes().decode('utf-8')
        if path.suffix == '.lua':
            scripts[hashlib.sha1(source.encode()).hexdigest()].append({'path': name})
            continue
        for match in RAW_CONSTANT.finditer(source):
            body = match['body']
            if not ('LUA' in match[1] or 'redis.call' in body or 'redis.pcall' in body):
                continue
            sha = hashlib.sha1(body.encode()).hexdigest()
            scripts[sha].append({'path': name, 'constant': match[1],
                                 'line': source.count('\n', 0, match.start()) + 1})
    write_json(args.output, {'schema': 'brewfs-redis-script-inventory-v1',
                            'scope': 'Current source raw-string Lua constants and .lua files; unknown hashes remain explicit.',
                            'scripts': dict(sorted(scripts.items()))})


def capture(args):
    stopped = False

    def stop(_signum, _frame):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    end_seen = False
    lines = 0
    pending = bytearray()
    with socket.create_connection((args.host, args.port), timeout=10) as connection:
        connection.sendall(b'*1\r\n$7\r\nMONITOR\r\n')
        while b'\r\n' not in pending:
            chunk = connection.recv(65536)
            if not chunk:
                raise RuntimeError('Redis closed before MONITOR acknowledgement')
            pending.extend(chunk)
        response, _, rest = pending.partition(b'\r\n')
        if response != b'+OK':
            raise RuntimeError('MONITOR rejected: ' + repr(response))
        pending = bytearray(rest)
        connection.settimeout(0.25)
        with open(args.output, 'wb') as output:
            write_json(args.ready, {'ready': True, 'pid': os.getpid()})
            while not stopped and not end_seen:
                while b'\n' in pending:
                    line, _, rest = pending.partition(b'\n')
                    pending = bytearray(rest)
                    output.write(line + b'\n')
                    lines += 1
                    row = monitor_row(line)
                    if (row['command'] == 'echo' and len(row['args']) > 1
                            and row['args'][1] == args.end_marker.encode()):
                        end_seen = True
                        break
                output.flush()
                if end_seen:
                    break
                try:
                    chunk = connection.recv(65536)
                except socket.timeout:
                    continue
                if not chunk:
                    break
                pending.extend(chunk)
            output.flush()
    write_json(args.status, {'complete': end_seen, 'end_marker_seen': end_seen,
                             'lines': lines, 'signal_stop': stopped})
    return 0 if end_seen else 1


def log_snapshots(path):
    snapshots = []
    for line in pathlib.Path(path).read_text(errors='replace').splitlines():
        if 'entity_diagnostics=' not in line:
            continue
        payload = line.split('entity_diagnostics=', 1)[1].lstrip()
        try:
            snapshots.append(json.JSONDecoder().raw_decode(payload)[0])
        except json.JSONDecodeError:
            continue
    return snapshots


def snapshot(args):
    before = len(log_snapshots(args.log))
    os.kill(args.pid, signal.SIGUSR1)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        snapshots = log_snapshots(args.log)
        if len(snapshots) > before:
            write_json(args.output, snapshots[before])
            return
        os.kill(args.pid, 0)
        time.sleep(0.05)
    raise RuntimeError('No new entity_diagnostics JSON snapshot after SIGUSR1')


def process_snapshot(args):
    # comm can contain spaces or parentheses; fields after its final ')' begin
    # with field 3 (state). utime/stime are fields 14/15, starttime is field 22.
    fields = pathlib.Path(f'/proc/{args.pid}/stat').read_text().rsplit(')', 1)[1].split()
    write_json(args.output, {'pid': args.pid, 'starttime_ticks': int(fields[19]),
                            'clock_ticks_per_second': os.sysconf('SC_CLK_TCK'),
                            'utime_ticks': int(fields[11]), 'stime_ticks': int(fields[12]),
                            'threads': int(fields[17]), 'monotonic_ns': time.monotonic_ns()})


def redis_snapshot(args):
    # Bracket INFO on the client. Redis takes its counter sample between these
    # timestamps; midpoint intervals therefore have bounded request uncertainty.
    with socket.create_connection((args.host, args.port), timeout=10) as connection:
        section = args.section.encode('ascii')
        start = time.monotonic_ns()
        connection.sendall(b'*2\r\n$4\r\nINFO\r\n$' + str(len(section)).encode()
                           + b'\r\n' + section + b'\r\n')
        response = connection.makefile('rb')
        header = response.readline(128)
        if not header.startswith(b'$') or not header.endswith(b'\r\n'):
            raise RuntimeError('INFO returned an unexpected RESP header')
        length = int(header[1:-2])
        if not 0 <= length <= 1024 * 1024:
            raise RuntimeError('INFO returned an invalid response length')
        payload = response.read(length)
        if len(payload) != length or response.read(2) != b'\r\n':
            raise RuntimeError('INFO returned a truncated response')
        end = time.monotonic_ns()
    pathlib.Path(args.output).write_bytes(payload)
    write_json(args.timing_output, {
        'request_start_monotonic_ns': start, 'reply_end_monotonic_ns': end,
        'midpoint_monotonic_ns': (start + end) // 2,
        'request_wall_seconds': (end - start) / 1e9,
    })


def sampling_window(phase, prefix):
    before_path, after_path = (phase / f'{prefix}-{part}.timing.json'
                              for part in ('before', 'after'))
    if not before_path.exists() or not after_path.exists():
        return None
    before, after = (json.loads(path.read_text()) for path in (before_path, after_path))
    wall = (after['midpoint_monotonic_ns'] - before['midpoint_monotonic_ns']) / 1e9
    lower = (after['request_start_monotonic_ns'] - before['reply_end_monotonic_ns']) / 1e9
    upper = (after['reply_end_monotonic_ns'] - before['request_start_monotonic_ns']) / 1e9
    if lower <= 0 or not lower <= wall <= upper:
        raise ValueError('Redis sampling timestamps overlap or moved backwards')
    return {'snapshot_wall_seconds': wall, 'snapshot_wall_lower_bound_seconds': lower,
            'snapshot_wall_upper_bound_seconds': upper}


def cpu_deltas(phase, files, workload_seconds):
    result = {}
    before_path, after_path = phase / 'process-cpu-before.json', phase / 'process-cpu-after.json'
    if before_path.exists() and after_path.exists():
        before, after = (json.loads(path.read_text()) for path in (before_path, after_path))
        for key in ('pid', 'starttime_ticks', 'clock_ticks_per_second'):
            if before[key] != after[key]:
                raise ValueError('mount process identity changed during CPU sampling')
        hz = after['clock_ticks_per_second']
        user = (after['utime_ticks'] - before['utime_ticks']) / hz
        system = (after['stime_ticks'] - before['stime_ticks']) / hz
        wall = (after['monotonic_ns'] - before['monotonic_ns']) / 1e9
        if wall <= 0 or min(user, system) < 0:
            raise ValueError('mount CPU sampling counters or clock moved backwards')
        result['mount_process'] = {
            'user_seconds': user, 'system_seconds': system, 'total_seconds': user + system,
            'cpu_ms_per_file': (user + system) * 1000 / files,
            'cpu_seconds_per_workload_wall_second': (user + system) / workload_seconds,
            'snapshot_wall_seconds': wall,
            'cpu_seconds_per_snapshot_wall_second': (user + system) / wall,
            'resolution_seconds': 1 / hz,
        }
    before_path, after_path = phase / 'redis-cpu-before.txt', phase / 'redis-cpu-after.txt'
    if before_path.exists() and after_path.exists():
        def parse(path):
            return {name: float(value) for line in path.read_text().splitlines()
                    if line.startswith('used_cpu_') for name, value in [line.split(':', 1)]}
        before, after = parse(before_path), parse(after_path)
        delta = {key: value - before.get(key, 0) for key, value in after.items()}
        if min(delta.values()) < 0:
            raise ValueError('Redis CPU sampling counters moved backwards')
        total = delta['used_cpu_user'] + delta['used_cpu_sys']
        result['redis_server'] = {'counters_seconds': delta, 'total_seconds': total,
                                  'cpu_ms_per_file': total * 1000 / files,
                                  'cpu_seconds_per_workload_wall_second': total / workload_seconds}
        window = sampling_window(phase, 'redis-cpu')
        if window:
            result['redis_server'].update(
                **window,
                cpu_seconds_per_snapshot_wall_second=total / window['snapshot_wall_seconds'],
                cpu_seconds_per_snapshot_wall_lower_bound=total / window['snapshot_wall_upper_bound_seconds'],
                cpu_seconds_per_snapshot_wall_upper_bound=total / window['snapshot_wall_lower_bound_seconds'],
            )
    return result


def commandstats(path):
    result = {}
    for line in pathlib.Path(path).read_text().splitlines():
        if not line.startswith('cmdstat_'):
            continue
        name, fields = line[len('cmdstat_'):].split(':', 1)
        values = dict(field.split('=', 1) for field in fields.split(','))
        result[name] = {key: int(values.get(key, 0))
                       for key in ('calls', 'usec', 'rejected_calls', 'failed_calls')}
    return result


def entity_delta(phase, files):
    before_path, after_path = phase / 'entity-before.json', phase / 'entity-after.json'
    if not before_path.exists() and not after_path.exists():
        return None
    if not before_path.exists() or not after_path.exists():
        raise ValueError('incomplete entity timer snapshot pair')
    before, after = (json.loads(path.read_text()) for path in (before_path, after_path))
    if (before.get('schema') != 'brewfs-entity-diagnostics-v1'
            or after.get('schema') != before.get('schema')
            or not before.get('enabled') or not after.get('enabled')):
        raise ValueError('unexpected or disabled entity timer snapshots')
    previous = {(row['name'], row['operation']): row for row in before['samples']}
    samples = []
    for row in after['samples']:
        prior = previous.get((row['name'], row['operation']), {})
        delta = {key: row[key] - prior.get(key, 0)
                 for key in ('count', 'elapsed_ns', 'round_trips', 'rows')}
        if min(delta.values()) < 0:
            raise ValueError('entity cumulative timer moved backwards')
        if not any(delta.values()):
            continue
        delta.update(name=row['name'], operation=row['operation'],
                     count_per_file=delta['count'] / files,
                     elapsed_ms_per_file=delta['elapsed_ns'] / 1e6 / files,
                     round_trips_per_file=delta['round_trips'] / files,
                     rows_per_file=delta['rows'] / files,
                     elapsed_ms_per_sample=delta['elapsed_ns'] / 1e6 / delta['count'] if delta['count'] else None)
        samples.append(delta)
    return {'aggregate': [row for row in samples if row['operation'] is None],
            'by_operation': [row for row in samples if row['operation'] is not None],
            'dropped_metric_registrations_before': before['dropped_metric_registrations'],
            'dropped_metric_registrations_after': after['dropped_metric_registrations'],
            'notes': after['notes']}


def analyze(args):
    phase = pathlib.Path(args.phase)
    summary = json.loads((phase / 'summary.json').read_text())
    expected = summary['expected_files']
    if expected <= 0:
        raise ValueError('positive expected file count required')
    scripts = json.loads(pathlib.Path(args.inventory).read_text())['scripts']
    counts = {'external_client': collections.Counter(), 'lua_nested': collections.Counter()}
    clients = collections.Counter()
    hashes = collections.Counter()
    begin = end = None
    inside = False
    malformed = 0
    for line in (phase / 'redis-monitor.txt').read_bytes().splitlines():
        try:
            row = monitor_row(line)
        except (ValueError, UnicodeError):
            malformed += 1
            continue
        if row['command'] == 'echo' and row['args'][1:] == [args.begin_marker.encode()]:
            begin = row['time']
            inside = True
            continue
        if row['command'] == 'echo' and row['args'][1:] == [args.end_marker.encode()]:
            end = row['time']
            inside = False
            break
        if not inside:
            continue
        kind = 'lua_nested' if row['source'] == 'lua' else 'external_client'
        counts[kind][row['command']] += 1
        if kind == 'external_client':
            clients[row['source']] += 1
            if row['command'] in ('evalsha', 'evalsha_ro') and len(row['args']) > 1:
                hashes[row['args'][1].decode('ascii').lower()] += 1
            elif row['command'] in ('eval', 'eval_ro') and len(row['args']) > 1:
                hashes[hashlib.sha1(row['args'][1]).hexdigest()] += 1
    before = commandstats(phase / 'redis-before.txt')
    after = commandstats(phase / 'redis-after.txt')
    deltas = {}
    for command in sorted(before.keys() | after.keys()):
        delta = {key: after.get(command, {}).get(key, 0) - before.get(command, {}).get(key, 0)
                 for key in ('calls', 'usec', 'rejected_calls', 'failed_calls')}
        if not any(delta.values()):
            continue
        delta.update(calls_per_file=delta['calls'] / expected,
                     usec_per_file=delta['usec'] / expected,
                     usec_per_call=delta['usec'] / delta['calls'] if delta['calls'] else None,
                     monitor_external_calls=counts['external_client'][command],
                     monitor_lua_nested_calls=counts['lua_nested'][command])
        deltas[command] = delta
    inventory_rows = [{'sha1': sha, 'external_calls': count, 'calls_per_file': count / expected,
                       'source_matches': scripts.get(sha, [])}
                      for sha, count in hashes.most_common()]
    script_seconds = sum(deltas.get(command, {}).get('usec', 0)
                         for command in ('evalsha', 'eval', 'evalsha_ro', 'eval_ro')) / 1e6
    stats_window = sampling_window(phase, 'redis-commandstats')
    if stats_window:
        stats_window.update(
            inclusive_script_seconds=script_seconds,
            script_seconds_per_snapshot_wall_second=script_seconds / stats_window['snapshot_wall_seconds'],
            script_seconds_per_snapshot_wall_lower_bound=script_seconds / stats_window['snapshot_wall_upper_bound_seconds'],
            script_seconds_per_snapshot_wall_upper_bound=script_seconds / stats_window['snapshot_wall_lower_bound_seconds'],
        )
    complete = begin is not None and end is not None and malformed == 0
    output = {
        'schema': 'brewfs-entity-redis-diagnostic-v1', 'status': 'diagnostic' if complete else 'incomplete',
        'expected_files': expected, 'successful_files': summary['files'], 'workload_errors': summary['errors'],
        'workload_seconds': summary['seconds'],
        'monitor_window_seconds': end - begin if complete else None,
        'begin_marker_seen': begin is not None, 'end_marker_seen': end is not None,
        'malformed_monitor_lines': malformed,
        'commands': {kind: {'total': sum(counter.values()), 'per_file': sum(counter.values()) / expected,
                            'by_command': dict(counter.most_common())} for kind, counter in counts.items()},
        'external_clients': dict(clients.most_common()), 'evalsha_inventory': inventory_rows,
        'health_ping_external_calls': counts['external_client']['ping'],
        'commandstats_delta': deltas,
        'inclusive_script_usec_per_file': script_seconds * 1e6 / expected,
        'commandstats_sampling_window': stats_window,
        'entity_snapshots': {'before': 'entity-before.json' if (phase / 'entity-before.json').exists() else None,
                             'after': 'entity-after.json' if (phase / 'entity-after.json').exists() else None},
        'entity_timer_delta': entity_delta(phase, expected),
        'cpu_delta': cpu_deltas(phase, expected, summary['seconds']),
        'limitations': [
            'MONITOR and timers perturb the workload; throughput is diagnostic, never acceptance.',
            'External command counts are not round trips: pipelined requests can contain several commands.',
            'Lua nested commands are server work, not client/backend RPCs.',
            'EVAL/EVALSHA commandstats usec includes nested commands; do not add their timings together.',
            'Commandstats spans a slightly wider window including marker ECHOs and INFO; it is server elapsed command time, not client wait or CPU time.',
            'MONITOR includes scanner directory setup plus concurrent lease/health traffic; per-file denominators use expected files.',
            'SHA source matches describe the captured working source; binary hashes and source snapshots remain necessary for build provenance.',
            'Entity elapsed spans overlap across nesting/concurrency; aggregate and per-operation rows describe the same samples. Neither is exclusive occupancy or CPU time.',
            'CPU is separately sampled from mount /proc utime+stime and Redis INFO CPU. Windows include diagnostic boundaries/background work; Redis CPU includes MONITOR overhead. CPU/wall may exceed one for multithreaded work.',
            'CPU/workload-wall and EVAL-service/workload-wall use mismatched windows: scanner seconds exclude mkdir/setup. They are indicative ratios, never occupancy. Prefer CPU/snapshot-wall and EVAL-service/commandstats-snapshot-wall; Redis midpoint intervals have request-window bounds.',
        ],
    }
    write_json(phase / 'redis-diagnostic.json', output)
    print(json.dumps({'phase': str(phase), 'status': output['status'],
                      'external_commands_per_file': output['commands']['external_client']['per_file'],
                      'lua_commands_per_file': output['commands']['lua_nested']['per_file'],
                      'inclusive_script_usec_per_file': output['inclusive_script_usec_per_file']}))
    return 0 if complete else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    command = commands.add_parser('inventory')
    command.add_argument('--root', required=True)
    command.add_argument('--output', required=True)
    command.set_defaults(run=inventory)
    command = commands.add_parser('capture')
    command.add_argument('--host', default='127.0.0.1')
    command.add_argument('--port', type=int, required=True)
    for option in ('output', 'ready', 'status', 'end-marker'):
        command.add_argument('--' + option, required=True)
    command.set_defaults(run=capture)
    command = commands.add_parser('snapshot')
    command.add_argument('--pid', type=int, required=True)
    command.add_argument('--log', required=True)
    command.add_argument('--output', required=True)
    command.set_defaults(run=snapshot)
    command = commands.add_parser('process-snapshot')
    command.add_argument('--pid', type=int, required=True)
    command.add_argument('--output', required=True)
    command.set_defaults(run=process_snapshot)
    command = commands.add_parser('redis-snapshot')
    command.add_argument('--host', default='127.0.0.1')
    command.add_argument('--port', type=int, required=True)
    command.add_argument('--section', choices=('CPU', 'commandstats'), required=True)
    command.add_argument('--output', required=True)
    command.add_argument('--timing-output', required=True)
    command.set_defaults(run=redis_snapshot)
    command = commands.add_parser('analyze')
    for option in ('phase', 'inventory', 'begin-marker', 'end-marker'):
        command.add_argument('--' + option, required=True)
    command.set_defaults(run=analyze)
    args = parser.parse_args()
    return args.run(args) or 0


if __name__ == '__main__':
    sys.exit(main())
