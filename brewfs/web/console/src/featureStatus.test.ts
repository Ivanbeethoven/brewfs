import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadFeatureStatus } from './featureStatus';
import type { VolumeResponse } from './api';

const volumes: VolumeResponse[] = [
  {
    id: 'vol-1',
    name: 'dev-local',
    description: null,
    labels: {},
    created_at: '2026-06-11T00:00:00Z',
    updated_at: '2026-06-11T00:00:00Z',
    mount_config: {
      mount_point: '/mnt/brewfs',
      data_backend: 'local-fs',
      data_dir: '/var/lib/brewfs/data',
      meta_backend: 'sqlx',
      meta_url_redacted: null,
      chunk_size: null,
      block_size: null,
    },
    runtime: {
      mounted: false,
      pid: null,
      mount_point: null,
      started_at: null,
    },
  },
];

describe('loadFeatureStatus', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('returns unavailable for volume-scoped features when no volumes are registered', async () => {
    const status = await loadFeatureStatus('browser', [], 'secret-token');

    expect(status.state).toBe('unavailable');
    expect(status.volumeName).toBeUndefined();
  });

  it('maps unsupported file browser responses to an unsupported page state', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: { code: 'unsupported' } }), {
        status: 501,
        headers: { 'content-type': 'application/json' },
      }),
    );

    const status = await loadFeatureStatus('browser', volumes, 'secret-token');

    expect(status.state).toBe('unsupported');
    expect(status.volumeName).toBe('dev-local');
  });

  it('maps unavailable volume-scoped responses to an unavailable page state', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: { code: 'unavailable' } }), {
        status: 409,
        headers: { 'content-type': 'application/json' },
      }),
    );

    const status = await loadFeatureStatus('trash', volumes, 'secret-token');

    expect(status.state).toBe('unavailable');
    expect(status.volumeName).toBe('dev-local');
  });

  it('loads CSI summary without requiring a registered volume', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(
        JSON.stringify({
          storageclasses: 1,
          persistentvolumes: 2,
          persistentvolumeclaims: 3,
          pods: 4,
          unhealthy_mounts: 0,
        }),
        {
          status: 200,
          headers: { 'content-type': 'application/json' },
        },
      ),
    );

    const status = await loadFeatureStatus('csi', [], 'secret-token');

    expect(status.state).toBe('ready');
    expect(status.message).toContain('4 pods');
  });
});
