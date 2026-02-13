import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  Stack,
  TextInput,
  PasswordInput,
  Button,
  Paper,
  ScrollArea,
  Text,
  Group,
  Badge,
  SegmentedControl,
  ActionIcon,
} from '@mantine/core';
import { useLocalStorage } from '@mantine/hooks';

interface LogEntry {
  level: 'info' | 'warn' | 'error';
  message: string;
  timestamp: Date;
}

type BackendKind = 'git' | 'wiki';

const MAX_LOG_ENTRIES = 500;

function appendLog(prev: LogEntry[], entry: LogEntry): LogEntry[] {
  const next = [...prev, entry];
  return next.length > MAX_LOG_ENTRIES ? next.slice(-MAX_LOG_ENTRIES) : next;
}

type LogLevel = 'all' | 'info' | 'warn' | 'error';

interface FieldErrors {
  gitSource?: string;
  gitMountPoint?: string;
  wikiUrl?: string;
  wikiSlug?: string;
  wikiToken?: string;
  wikiMountPoint?: string;
}

function App() {
  const [backend, setBackend] = useLocalStorage<BackendKind>({
    key: 'omnifuse-backend',
    defaultValue: 'git',
  });
  const [mounted, setMounted] = useState(false);
  const [loading, setLoading] = useState(false);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [fuseAvailable, setFuseAvailable] = useState<boolean | null>(null);
  const [logFilter, setLogFilter] = useState<LogLevel>('all');
  const [errors, setErrors] = useState<FieldErrors>({});

  // Git (persisted)
  const [gitSource, setGitSource] = useLocalStorage({ key: 'omnifuse-git-source', defaultValue: '' });
  const [gitBranch, setGitBranch] = useLocalStorage({ key: 'omnifuse-git-branch', defaultValue: 'main' });
  const [gitMountPoint, setGitMountPoint] = useLocalStorage({ key: 'omnifuse-git-mount', defaultValue: '' });

  // Wiki (persisted, except token)
  const [wikiUrl, setWikiUrl] = useLocalStorage({ key: 'omnifuse-wiki-url', defaultValue: '' });
  const [wikiSlug, setWikiSlug] = useLocalStorage({ key: 'omnifuse-wiki-slug', defaultValue: '' });
  const [wikiToken, setWikiToken] = useState('');
  const [wikiMountPoint, setWikiMountPoint] = useLocalStorage({ key: 'omnifuse-wiki-mount', defaultValue: '' });

  const filteredLogs = useMemo(
    () => logFilter === 'all' ? logs : logs.filter((l) => l.level === logFilter),
    [logs, logFilter],
  );

  // Check FUSE on startup
  useEffect(() => {
    const checkFuse = async () => {
      try {
        const available = await invoke<boolean>('check_fuse');
        setFuseAvailable(available);
        if (!available) {
          setLogs([{
            level: 'warn',
            message: 'FUSE not found. Install macFUSE (macOS) or libfuse3 (Linux).',
            timestamp: new Date(),
          }]);
        }
      } catch {
        setFuseAvailable(false);
      }
    };
    checkFuse();
  }, []);

  // Subscribe to VFS events
  useEffect(() => {
    const unlisteners: Promise<UnlistenFn>[] = [
      listen('vfs:mounted', () => {
        setMounted(true);
        setLoading(false);
      }),
      listen('vfs:unmounted', () => {
        setMounted(false);
        setLoading(false);
      }),
      listen<{ level: string; message: string }>('vfs:log', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: e.payload.level as 'info' | 'warn' | 'error',
            message: e.payload.message,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ message: string }>('vfs:error', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'error',
            message: e.payload.message,
            timestamp: new Date(),
          }),
        );
        setLoading(false);
      }),
      listen<{ result: string }>('vfs:sync', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `sync: ${e.payload.result}`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ path: string; bytes: number }>('vfs:file-written', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `written: ${e.payload.path} (${e.payload.bytes} bytes)`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ path: string }>('vfs:file-dirty', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `modified: ${e.payload.path}`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ path: string }>('vfs:file-created', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `created: ${e.payload.path}`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ path: string }>('vfs:file-deleted', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'warn',
            message: `deleted: ${e.payload.path}`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ oldPath: string; newPath: string }>('vfs:file-renamed', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `renamed: ${e.payload.oldPath} -> ${e.payload.newPath}`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ hash: string; filesCount: number; message: string }>('vfs:commit', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `commit ${e.payload.hash.slice(0, 7)}: ${e.payload.message} (${e.payload.filesCount} files)`,
            timestamp: new Date(),
          }),
        );
      }),
      listen<{ commitsCount: number }>('vfs:push', (e) => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'info',
            message: `push: ${e.payload.commitsCount} commit(s)`,
            timestamp: new Date(),
          }),
        );
      }),
      listen('vfs:push-rejected', () => {
        setLogs((prev) =>
          appendLog(prev, {
            level: 'warn',
            message: 'push rejected — will retry after pull',
            timestamp: new Date(),
          }),
        );
      }),
    ];

    return () => {
      unlisteners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);

  const validate = (): boolean => {
    const e: FieldErrors = {};
    if (backend === 'git') {
      if (!gitSource.trim()) e.gitSource = 'Required';
      if (!gitMountPoint.trim()) e.gitMountPoint = 'Required';
    } else {
      if (!wikiUrl.trim()) {
        e.wikiUrl = 'Required';
      } else {
        try { new URL(wikiUrl); } catch { e.wikiUrl = 'Invalid URL'; }
      }
      if (!wikiSlug.trim()) e.wikiSlug = 'Required';
      if (!wikiToken.trim()) e.wikiToken = 'Required';
      if (!wikiMountPoint.trim()) e.wikiMountPoint = 'Required';
    }
    setErrors(e);
    return Object.keys(e).length === 0;
  };

  const handleMount = async () => {
    if (!validate()) return;
    setLoading(true);
    try {
      if (backend === 'git') {
        await invoke('mount_git', {
          source: gitSource.trim(),
          mountPoint: gitMountPoint.trim(),
          branch: gitBranch.trim() || null,
        });
      } else {
        await invoke('mount_wiki', {
          baseUrl: wikiUrl.trim(),
          rootSlug: wikiSlug.trim(),
          authToken: wikiToken,
          mountPoint: wikiMountPoint.trim(),
        });
      }
    } catch (e) {
      setLogs((prev) =>
        appendLog(prev, { level: 'error', message: String(e), timestamp: new Date() }),
      );
      setLoading(false);
    }
  };

  const handleUnmount = async () => {
    setLoading(true);
    try {
      await invoke('unmount');
    } catch (e) {
      setLogs((prev) =>
        appendLog(prev, { level: 'error', message: String(e), timestamp: new Date() }),
      );
      setLoading(false);
    }
  };

  const handlePickFolder = async (setter: (v: string) => void) => {
    try {
      const path = await invoke<string | null>('pick_folder');
      if (path) setter(path);
    } catch (e) {
      console.error(e);
    }
  };

  const getLevelColor = (level: string) => {
    switch (level) {
      case 'error':
        return 'red';
      case 'warn':
        return 'yellow';
      default:
        return 'dimmed';
    }
  };

  const canMount =
    backend === 'git'
      ? gitSource && gitMountPoint
      : wikiUrl && wikiSlug && wikiToken && wikiMountPoint;

  return (
    <Stack p="md" gap="sm">
      <Group>
        <Text fw={600} size="lg">
          OmniFuse
        </Text>
        <Badge color={mounted ? 'green' : 'gray'} variant="filled">
          {mounted ? 'Mounted' : 'Not mounted'}
        </Badge>
        {fuseAvailable === false && (
          <Badge color="red" variant="light">
            FUSE not found
          </Badge>
        )}
      </Group>

      <SegmentedControl
        value={backend}
        onChange={(v) => setBackend(v as BackendKind)}
        data={[
          { label: 'Git', value: 'git' },
          { label: 'Wiki', value: 'wiki' },
        ]}
        disabled={mounted || loading}
      />

      {backend === 'git' ? (
        <>
          <TextInput
            label="Repository"
            placeholder="URL or local path"
            value={gitSource}
            onChange={(e) => { setGitSource(e.target.value); setErrors((p) => ({ ...p, gitSource: undefined })); }}
            disabled={mounted || loading}
            error={errors.gitSource}
          />
          <TextInput
            label="Branch"
            placeholder="main"
            value={gitBranch}
            onChange={(e) => setGitBranch(e.target.value)}
            disabled={mounted || loading}
          />
          <Group align="end" gap="xs">
            <TextInput
              label="Mount point"
              placeholder="/path/to/mount"
              value={gitMountPoint}
              onChange={(e) => { setGitMountPoint(e.target.value); setErrors((p) => ({ ...p, gitMountPoint: undefined })); }}
              disabled={mounted || loading}
              style={{ flex: 1 }}
              error={errors.gitMountPoint}
            />
            <Button
              onClick={() => handlePickFolder(setGitMountPoint)}
              disabled={mounted || loading}
              variant="default"
            >
              Browse
            </Button>
          </Group>
        </>
      ) : (
        <>
          <TextInput
            label="Wiki API URL"
            placeholder="https://wiki.example.com"
            value={wikiUrl}
            onChange={(e) => { setWikiUrl(e.target.value); setErrors((p) => ({ ...p, wikiUrl: undefined })); }}
            disabled={mounted || loading}
            error={errors.wikiUrl}
          />
          <TextInput
            label="Root slug"
            placeholder="my/project"
            value={wikiSlug}
            onChange={(e) => { setWikiSlug(e.target.value); setErrors((p) => ({ ...p, wikiSlug: undefined })); }}
            disabled={mounted || loading}
            error={errors.wikiSlug}
          />
          <PasswordInput
            label="Token"
            placeholder="auth token"
            value={wikiToken}
            onChange={(e) => { setWikiToken(e.target.value); setErrors((p) => ({ ...p, wikiToken: undefined })); }}
            disabled={mounted || loading}
            error={errors.wikiToken}
          />
          <Group align="end" gap="xs">
            <TextInput
              label="Mount point"
              placeholder="/path/to/mount"
              value={wikiMountPoint}
              onChange={(e) => { setWikiMountPoint(e.target.value); setErrors((p) => ({ ...p, wikiMountPoint: undefined })); }}
              disabled={mounted || loading}
              style={{ flex: 1 }}
              error={errors.wikiMountPoint}
            />
            <Button
              onClick={() => handlePickFolder(setWikiMountPoint)}
              disabled={mounted || loading}
              variant="default"
            >
              Browse
            </Button>
          </Group>
        </>
      )}

      <Button
        onClick={mounted ? handleUnmount : handleMount}
        color={mounted ? 'red' : 'blue'}
        loading={loading}
        disabled={!mounted && !canMount}
      >
        {mounted ? 'Unmount' : 'Mount'}
      </Button>

      <Paper withBorder p="xs">
        <Group justify="space-between" mb="xs">
          <Text size="sm" fw={500}>
            Log
          </Text>
          <Group gap="xs">
            <SegmentedControl
              size="xs"
              value={logFilter}
              onChange={(v) => setLogFilter(v as LogLevel)}
              data={[
                { label: 'All', value: 'all' },
                { label: 'Info', value: 'info' },
                { label: 'Warn', value: 'warn' },
                { label: 'Error', value: 'error' },
              ]}
            />
            <ActionIcon
              variant="subtle"
              size="sm"
              onClick={() => setLogs([])}
              title="Clear log"
            >
              &times;
            </ActionIcon>
          </Group>
        </Group>
        <ScrollArea h={150}>
          {filteredLogs.length === 0 ? (
            <Text size="xs" c="dimmed">
              No events
            </Text>
          ) : (
            filteredLogs.map((log, i) => (
              <Text key={i} size="xs" c={getLevelColor(log.level)}>
                [{log.timestamp.toLocaleTimeString()}] {log.message}
              </Text>
            ))
          )}
        </ScrollArea>
      </Paper>
    </Stack>
  );
}

export default App;
