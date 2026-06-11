import {
  ApiError,
  fetchCsiPersistentVolumeClaims,
  fetchCsiPersistentVolumes,
  fetchCsiPods,
  fetchCsiStorageClasses,
  fetchCsiSummary,
  type CsiResourceListResponse,
} from './api';

export type CsiDashboardState = 'ready' | 'unsupported' | 'unavailable';
export type CsiResourceKey =
  | 'storageclasses'
  | 'persistentvolumes'
  | 'persistentvolumeclaims'
  | 'pods';

export interface CsiDashboardMetric {
  label: string;
  value: string;
}

export interface CsiDashboardFilters {
  namespace?: string;
  volume?: string;
}

export interface CsiResourceStatus {
  key: CsiResourceKey;
  title: string;
  state: CsiDashboardState;
  count: number | null;
  message: string;
  items: unknown[];
}

export interface CsiDashboardResult {
  state: CsiDashboardState;
  title: string;
  message: string;
  summaryMetrics: CsiDashboardMetric[];
  resources: CsiResourceStatus[];
}

type CsiResourceDescriptor = {
  key: CsiResourceKey;
  title: string;
  load: (
    filters: CsiDashboardFilters,
    token?: string | null,
  ) => Promise<CsiResourceListResponse>;
};

const resourceDescriptors: CsiResourceDescriptor[] = [
  {
    key: 'storageclasses',
    title: 'StorageClasses',
    load: (_filters, token) => fetchCsiStorageClasses(token),
  },
  {
    key: 'persistentvolumes',
    title: 'PersistentVolumes',
    load: (_filters, token) => fetchCsiPersistentVolumes(token),
  },
  {
    key: 'persistentvolumeclaims',
    title: 'PersistentVolumeClaims',
    load: (filters, token) => fetchCsiPersistentVolumeClaims(filters.namespace, token),
  },
  {
    key: 'pods',
    title: 'Pods',
    load: (filters, token) => fetchCsiPods(filters, token),
  },
];

export async function loadCsiDashboard(
  token?: string | null,
  filters: CsiDashboardFilters = {},
): Promise<CsiDashboardResult> {
  try {
    const summary = await fetchCsiSummary(token);
    const resources = await Promise.all(
      resourceDescriptors.map((descriptor) => loadResourceStatus(descriptor, filters, token)),
    );

    return {
      state: 'ready',
      title: 'CSI dashboard',
      message: `${summary.pods} pods reference BrewFS volumes; ${summary.unhealthy_mounts} mounts need attention.`,
      summaryMetrics: [
        { label: 'StorageClasses', value: String(summary.storageclasses) },
        { label: 'PersistentVolumes', value: String(summary.persistentvolumes) },
        { label: 'PersistentVolumeClaims', value: String(summary.persistentvolumeclaims) },
        { label: 'Pods', value: String(summary.pods) },
        { label: 'Unhealthy mounts', value: String(summary.unhealthy_mounts) },
      ],
      resources,
    };
  } catch (err: unknown) {
    return dashboardErrorOrThrow(err);
  }
}

async function loadResourceStatus(
  descriptor: CsiResourceDescriptor,
  filters: CsiDashboardFilters,
  token?: string | null,
): Promise<CsiResourceStatus> {
  try {
    const response = await descriptor.load(filters, token);
    return {
      key: descriptor.key,
      title: descriptor.title,
      state: 'ready',
      count: response.items.length,
      message: `${response.items.length} items`,
      items: response.items,
    };
  } catch (err: unknown) {
    return resourceErrorOrThrow(descriptor, err);
  }
}

function dashboardErrorOrThrow(err: unknown): CsiDashboardResult {
  if (err instanceof ApiError && err.status === 409) {
    return {
      state: 'unavailable',
      title: 'CSI dashboard unavailable',
      message: 'Enable the CSI dashboard integration before loading Kubernetes resources.',
      summaryMetrics: [],
      resources: [],
    };
  }
  if (err instanceof ApiError && err.status === 501) {
    return {
      state: 'unsupported',
      title: 'CSI dashboard unsupported',
      message: 'The server exposes CSI endpoints but no Kubernetes adapter is connected yet.',
      summaryMetrics: [],
      resources: [],
    };
  }
  throw err;
}

function resourceErrorOrThrow(descriptor: CsiResourceDescriptor, err: unknown): CsiResourceStatus {
  if (err instanceof ApiError && err.status === 409) {
    return {
      key: descriptor.key,
      title: descriptor.title,
      state: 'unavailable',
      count: null,
      message: 'CSI dashboard integration is disabled.',
      items: [],
    };
  }
  if (err instanceof ApiError && err.status === 501) {
    return {
      key: descriptor.key,
      title: descriptor.title,
      state: 'unsupported',
      count: null,
      message: 'Kubernetes adapter support is not implemented for this resource yet.',
      items: [],
    };
  }
  throw err;
}
