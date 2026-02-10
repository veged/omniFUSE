import { useState, useEffect } from 'react';
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
} from '@mantine/core';

interface LogEntry {
  level: 'info' | 'warn' | 'error';
  message: string;
  timestamp: Date;
}

type BackendKind = 'git' | 'wiki';

function App() {
  const [backend, setBackend] = useState<BackendKind>('git');
  const [mounted, setMounted] = useState(false);
  const [loading, setLoading] = useState(false);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [fuseAvailable, setFuseAvailable] = useState<boolean | null>(null);

  // Git
  const [gitSource, setGitSource] = useState('');
  const [gitBranch, setGitBranch] = useState('main');
  const [gitMountPoint, setGitMountPoint] = useState('');

  // Wiki
  const [wikiUrl, setWikiUrl] = useState('');
  const [wikiSlug, setWikiSlug] = useState('');
  const [wikiToken, setWikiToken] = useState('');
  const [wikiMountPoint, setWikiMountPoint] = useState('');

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
      } catch (e) {
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
        setLogs((prev) => [
          ...prev,
          {
            level: e.payload.level as 'info' | 'warn' | 'error',
            message: e.payload.message,
            timestamp: new Date(),
          },
        ]);
      }),
      listen<{ message: string }>('vfs:error', (e) => {
        setLogs((prev) => [
          ...prev,
          {
            level: 'error',
            message: e.payload.message,
            timestamp: new Date(),
          },
        ]);
        setLoading(false);
      }),
      listen<{ result: string }>('vfs:sync', (e) => {
        setLogs((prev) => [
          ...prev,
          {
            level: 'info',
            message: `sync: ${e.payload.result}`,
            timestamp: new Date(),
          },
        ]);
      }),
    ];

    return () => {
      unlisteners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);

  const handleMount = async () => {
    setLoading(true);
    try {
      if (backend === 'git') {
        await invoke('mount_git', {
          source: gitSource,
          mountPoint: gitMountPoint,
          branch: gitBranch || null,
        });
      } else {
        await invoke('mount_wiki', {
          baseUrl: wikiUrl,
          rootSlug: wikiSlug,
          authToken: wikiToken,
          mountPoint: wikiMountPoint,
        });
      }
    } catch (e) {
      setLogs((prev) => [
        ...prev,
        { level: 'error', message: String(e), timestamp: new Date() },
      ]);
      setLoading(false);
    }
  };

  const handleUnmount = async () => {
    setLoading(true);
    try {
      await invoke('unmount');
    } catch (e) {
      setLogs((prev) => [
        ...prev,
        { level: 'error', message: String(e), timestamp: new Date() },
      ]);
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
            onChange={(e) => setGitSource(e.target.value)}
            disabled={mounted || loading}
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
              onChange={(e) => setGitMountPoint(e.target.value)}
              disabled={mounted || loading}
              style={{ flex: 1 }}
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
            onChange={(e) => setWikiUrl(e.target.value)}
            disabled={mounted || loading}
          />
          <TextInput
            label="Root slug"
            placeholder="my/project"
            value={wikiSlug}
            onChange={(e) => setWikiSlug(e.target.value)}
            disabled={mounted || loading}
          />
          <PasswordInput
            label="Token"
            placeholder="auth token"
            value={wikiToken}
            onChange={(e) => setWikiToken(e.target.value)}
            disabled={mounted || loading}
          />
          <Group align="end" gap="xs">
            <TextInput
              label="Mount point"
              placeholder="/path/to/mount"
              value={wikiMountPoint}
              onChange={(e) => setWikiMountPoint(e.target.value)}
              disabled={mounted || loading}
              style={{ flex: 1 }}
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
        <Text size="sm" fw={500} mb="xs">
          Log
        </Text>
        <ScrollArea h={150}>
          {logs.length === 0 ? (
            <Text size="xs" c="dimmed">
              No events
            </Text>
          ) : (
            logs.map((log, i) => (
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
