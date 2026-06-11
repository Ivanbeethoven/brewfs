export type AuthMode = 'disabled' | 'token';

export interface HealthResponse {
  service: 'brewfs-console';
  version: string;
  commit_short: string;
  auth_mode: AuthMode;
  integrations: {
    csi_dashboard: boolean;
  };
  static_assets_available: boolean;
}

export async function fetchHealth(): Promise<HealthResponse> {
  const response = await fetch('/api/health', {
    headers: { Accept: 'application/json' },
  });

  if (!response.ok) {
    throw new Error(`health request failed: ${response.status}`);
  }

  return (await response.json()) as HealthResponse;
}
