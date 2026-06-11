import { describe, expect, it } from 'vitest';
import { formatMode, joinBrowserPath, normalizeBrowserPath, parentBrowserPath } from './browserPath';

describe('browser path helpers', () => {
  it('normalizes empty, relative, and parent segments', () => {
    expect(normalizeBrowserPath('')).toBe('/');
    expect(normalizeBrowserPath('projects')).toBe('/projects');
    expect(normalizeBrowserPath('/projects/../logs/./today')).toBe('/logs/today');
    expect(normalizeBrowserPath('/../../')).toBe('/');
  });

  it('joins child paths and computes parent paths', () => {
    expect(joinBrowserPath('/', 'docs')).toBe('/docs');
    expect(joinBrowserPath('/docs', 'readme')).toBe('/docs/readme');
    expect(parentBrowserPath('/docs/readme')).toBe('/docs');
    expect(parentBrowserPath('/')).toBe('/');
  });

  it('formats numeric modes as octal text', () => {
    expect(formatMode(0o644)).toBe('0644');
    expect(formatMode(0o755)).toBe('0755');
  });
});
