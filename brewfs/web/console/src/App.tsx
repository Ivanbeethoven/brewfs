import {
  Activity,
  Database,
  FolderTree,
  Gauge,
  HardDrive,
  Settings,
  type LucideIcon,
} from 'lucide-react';
import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { fetchHealth, type HealthResponse } from './api';

type PageKey = 'overview' | 'filesystems' | 'jobs' | 'csi' | 'settings';

const navItems: Array<{ key: PageKey; label: string; icon: LucideIcon }> = [
  { key: 'overview', label: 'Overview', icon: Gauge },
  { key: 'filesystems', label: 'Filesystems', icon: HardDrive },
  { key: 'jobs', label: 'Jobs', icon: Activity },
  { key: 'csi', label: 'CSI', icon: Database },
  { key: 'settings', label: 'Settings', icon: Settings },
];

function pageTitle(page: PageKey): string {
  return navItems.find((item) => item.key === page)?.label ?? 'Overview';
}

export function App() {
  const [page, setPage] = useState<PageKey>('overview');
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetchHealth()
      .then((result) => {
        if (!cancelled) {
          setHealth(result);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : 'health request failed');
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const status = useMemo(() => {
    if (error) return { label: 'API unavailable', tone: 'bad' };
    if (!health) return { label: 'Connecting', tone: 'warn' };
    return { label: `BrewFS ${health.version}`, tone: 'good' };
  }, [error, health]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <FolderTree size={24} aria-hidden="true" />
          <div>
            <strong>BrewFS</strong>
            <span>Console</span>
          </div>
        </div>
        <nav aria-label="Primary navigation">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.key}
                className={page === item.key ? 'nav-item active' : 'nav-item'}
                type="button"
                onClick={() => setPage(item.key)}
              >
                <Icon size={18} aria-hidden="true" />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>
      </aside>

      <main className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Phase 1A scaffold</p>
            <h1>{pageTitle(page)}</h1>
          </div>
          <div className={`status-pill ${status.tone}`}>{status.label}</div>
        </header>

        <section className="content-grid">{renderPage(page, health, error)}</section>
      </main>
    </div>
  );
}

function renderPage(page: PageKey, health: HealthResponse | null, error: string | null) {
  if (page === 'overview') {
    return (
      <>
        <Panel title="Runtime">
          <Metric label="Service" value={health?.service ?? 'waiting'} />
          <Metric label="Commit" value={health?.commit_short ?? 'unknown'} />
          <Metric label="Auth" value={health?.auth_mode ?? 'unknown'} />
        </Panel>
        <Panel title="Scaffold status">
          <p className="muted">
            The console shell is connected to the health API. Volume registry, jobs, file browsing,
            trash, ACL, and CSI data are intentionally empty in this phase.
          </p>
          {error ? <p className="error-text">{error}</p> : null}
        </Panel>
      </>
    );
  }

  if (page === 'filesystems') {
    return (
      <EmptyPanel title="No registered filesystems" detail="Volume registry arrives in Phase 1B." />
    );
  }

  if (page === 'jobs') {
    return (
      <EmptyPanel
        title="No jobs"
        detail="Runtime job discovery arrives with control-plane integration."
      />
    );
  }

  if (page === 'csi') {
    return (
      <EmptyPanel
        title="CSI dashboard disabled"
        detail="Kubernetes resource discovery is a subsequent read-only integration."
      />
    );
  }

  return (
    <EmptyPanel
      title="Settings unavailable"
      detail="Token auth and registry settings arrive in Phase 1B."
    />
  );
}

function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <article className="panel">
      <h2>{title}</h2>
      {children}
    </article>
  );
}

function EmptyPanel({ title, detail }: { title: string; detail: string }) {
  return (
    <article className="panel empty-panel">
      <h2>{title}</h2>
      <p className="muted">{detail}</p>
    </article>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}
