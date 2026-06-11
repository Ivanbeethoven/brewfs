import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadCsiDashboard } from './csiDashboard';

describe('loadCsiDashboard', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('summarizes ready CSI counts and resource tables', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch');
    fetch
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            storageclasses: 1,
            persistentvolumes: 2,
            persistentvolumeclaims: 3,
            pods: 4,
            unhealthy_mounts: 0,
          }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ items: [{ name: 'fast' }] }), { status: 200 }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ items: [{ name: 'pv-a' }, { name: 'pv-b' }] }), {
          status: 200,
        }),
      )
      .mockResolvedValueOnce(new Response(JSON.stringify({ items: [] }), { status: 200 }))
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ items: [{ name: 'pod-a' }] }), { status: 200 }),
      );

    const result = await loadCsiDashboard('secret-token', {
      namespace: 'prod',
      volume: 'data',
    });

    expect(result.state).toBe('ready');
    expect(fetch).toHaveBeenNthCalledWith(4, '/api/csi/persistentvolumeclaims?namespace=prod', {
      headers: {
        Accept: 'application/json',
        Authorization: 'Bearer secret-token',
      },
    });
    expect(fetch).toHaveBeenNthCalledWith(5, '/api/csi/pods?namespace=prod&volume=data', {
      headers: {
        Accept: 'application/json',
        Authorization: 'Bearer secret-token',
      },
    });
    expect(result.summaryMetrics).toContainEqual({ label: 'Pods', value: '4' });
    expect(result.resources.map((resource) => [resource.key, resource.state, resource.count])).toEqual([
      ['storageclasses', 'ready', 1],
      ['persistentvolumes', 'ready', 2],
      ['persistentvolumeclaims', 'ready', 0],
      ['pods', 'ready', 1],
    ]);
  });

  it('returns unavailable when the CSI dashboard is disabled', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: { code: 'unavailable' } }), {
        status: 409,
        headers: { 'content-type': 'application/json' },
      }),
    );

    const result = await loadCsiDashboard('secret-token');

    expect(result.state).toBe('unavailable');
    expect(result.title).toBe('CSI dashboard unavailable');
    expect(result.resources).toEqual([]);
  });

  it('keeps resource-level unsupported states visible after summary loads', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch');
    fetch
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            storageclasses: 0,
            persistentvolumes: 0,
            persistentvolumeclaims: 0,
            pods: 0,
            unhealthy_mounts: 0,
          }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        ),
      )
      .mockResolvedValue(
        new Response(JSON.stringify({ error: { code: 'unsupported' } }), {
          status: 501,
          headers: { 'content-type': 'application/json' },
        }),
      );

    const result = await loadCsiDashboard('secret-token');

    expect(result.state).toBe('ready');
    expect(result.resources).toHaveLength(4);
    expect(result.resources.every((resource) => resource.state === 'unsupported')).toBe(true);
  });
});
