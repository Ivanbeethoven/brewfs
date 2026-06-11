import type { InstanceInfoResponse, VolumeResponse } from './api';

export type VolumeCapabilityState = 'ready' | 'offline' | 'unknown';

export interface VolumeCapabilitySummary {
  state: VolumeCapabilityState;
  label: string;
  enabled: string[];
  disabled: string[];
}

const CAPABILITY_LABELS: Array<[string, string]> = [
  ['namespace', 'Namespace'],
  ['file_data', 'File data'],
  ['batch_stat', 'Batch stat'],
  ['hardlinks', 'Hardlinks'],
  ['symlinks', 'Symlinks'],
  ['rename_exchange', 'Rename exchange'],
  ['open_close_tracking', 'Open/close tracking'],
  ['stat_fs', 'StatFS'],
  ['sessions', 'Sessions'],
  ['global_locks', 'Global locks'],
  ['plocks', 'POSIX locks'],
  ['flocks', 'Flocks'],
  ['xattr', 'Xattr'],
  ['acl', 'ACL'],
  ['quota', 'Quota'],
  ['dump_load', 'Dump/load'],
  ['compaction', 'Compaction'],
  ['watch_invalidation', 'Watch invalidation'],
];

export function summarizeVolumeCapabilities(
  volume: VolumeResponse,
  instanceDetails: Record<number, InstanceInfoResponse>,
): VolumeCapabilitySummary {
  if (!volume.runtime.mounted) {
    return emptySummary('offline');
  }

  if (volume.runtime.pid === null) {
    return emptySummary('unknown');
  }

  const details = instanceDetails[volume.runtime.pid];
  if (!details) {
    return emptySummary('unknown');
  }

  const entries = orderedCapabilityEntries(details.capabilities);
  const enabled = entries.filter((entry) => entry.enabled).map((entry) => entry.label);
  const disabled = entries.filter((entry) => !entry.enabled).map((entry) => entry.label);

  return {
    state: 'ready',
    label: `${enabled.length}/${entries.length} enabled`,
    enabled,
    disabled,
  };
}

export function enabledCapabilityLabels(capabilities: Record<string, boolean>): string[] {
  return orderedCapabilityEntries(capabilities)
    .filter((entry) => entry.enabled)
    .map((entry) => entry.label);
}

function emptySummary(state: Exclude<VolumeCapabilityState, 'ready'>): VolumeCapabilitySummary {
  return {
    state,
    label: state,
    enabled: [],
    disabled: [],
  };
}

function orderedCapabilityEntries(capabilities: Record<string, boolean>) {
  const known = CAPABILITY_LABELS.filter(([key]) => key in capabilities).map(([key, label]) => ({
    key,
    label,
    enabled: capabilities[key],
  }));
  const knownKeys = new Set(CAPABILITY_LABELS.map(([key]) => key));
  const custom = Object.entries(capabilities)
    .filter(([key]) => !knownKeys.has(key))
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, enabled]) => ({
      key,
      label: key,
      enabled,
    }));

  return [...known, ...custom];
}
