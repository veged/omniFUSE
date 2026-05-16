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

type BackendKind = 'git' | 'wiki' | 's3';
type EventLevel = 'info' | 'warn' | 'error';

interface OmniEvent {
  version: number;
  seq: number;
  time: number;
  mountId: string;
  opId?: number;
  kind: string;
  level: EventLevel;
  source: string;
  data?: Record<string, unknown>;
  error?: {
    code: string;
    message: string;
    action: string;
  };
}

const MAX_LOG_ENTRIES = 500;

function appendLog(prev: LogEntry[], entry: LogEntry): LogEntry[] {
  const next = [...prev, entry];
  return next.length > MAX_LOG_ENTRIES ? next.slice(-MAX_LOG_ENTRIES) : next;
}

function dataString(event: OmniEvent, key: string): string | undefined {
  const value = event.data?.[key];
  return typeof value === 'string' ? value : undefined;
}

function dataNumber(event: OmniEvent, key: string): number | undefined {
  const value = event.data?.[key];
  return typeof value === 'number' ? value : undefined;
}

function eventMessage(event: OmniEvent): string {
  if (event.error) return event.error.message;

  switch (event.kind) {
    case 'mount.start':
      return `mounting ${dataString(event, 'backend') ?? 'backend'}`;
    case 'mount.ready':
      return `mounted ${dataString(event, 'mountPoint') ?? 'filesystem'}`;
    case 'mount.stop':
      return 'unmounted';
    case 'file.change': {
      const path = dataString(event, 'path') ?? 'file';
      const bytes = dataNumber(event, 'bytes');
      return bytes === undefined ? `changed ${path}` : `written ${path} (${bytes} bytes)`;
    }
    case 'file.flush':
      return `flushed ${dataString(event, 'path') ?? 'file'}`;
    case 'file.create':
      return `created ${dataString(event, 'path') ?? 'file'}`;
    case 'file.delete':
      return `deleted ${dataString(event, 'path') ?? 'file'}`;
    case 'file.rename':
      return `renamed ${dataString(event, 'oldPath') ?? 'file'} -> ${dataString(event, 'newPath') ?? 'file'}`;
    case 'sync.start':
      return `sync started (${dataNumber(event, 'dirty') ?? 0} dirty)`;
    case 'sync.done':
      return `sync done (${dataNumber(event, 'synced') ?? 0} synced)`;
    case 'sync.defer':
      return 'sync deferred';
    case 'remote.poll':
      return 'remote poll';
    case 'remote.change':
      return `remote changed (${dataNumber(event, 'changed') ?? 0} changed, ${dataNumber(event, 'deleted') ?? 0} deleted)`;
    case 'remote.defer':
      return 'remote deferred';
    case 'conflict':
      return 'conflict detected';
    case 'queue.drop':
      return 'event queue dropped work';
    default:
      return event.kind;
  }
}

type LogLevel = 'all' | 'info' | 'warn' | 'error';

interface FieldErrors {
  gitSource?: string;
  gitMountPoint?: string;
  wikiUrl?: string;
  wikiSlug?: string;
  wikiToken?: string;
  wikiMountPoint?: string;
  s3Bucket?: string;
  s3SecretAccessKey?: string;
  s3MountPoint?: string;
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

  // S3 (persisted, except secret)
  const [s3Bucket, setS3Bucket] = useLocalStorage({ key: 'omnifuse-s3-bucket', defaultValue: '' });
  const [s3Prefix, setS3Prefix] = useLocalStorage({ key: 'omnifuse-s3-prefix', defaultValue: '' });
  const [s3Endpoint, setS3Endpoint] = useLocalStorage({ key: 'omnifuse-s3-endpoint', defaultValue: '' });
  const [s3Region, setS3Region] = useLocalStorage({ key: 'omnifuse-s3-region', defaultValue: '' });
  const [s3AccessKeyId, setS3AccessKeyId] = useLocalStorage({ key: 'omnifuse-s3-access-key-id', defaultValue: '' });
  const [s3SecretAccessKey, setS3SecretAccessKey] = useState('');
  const [s3MountPoint, setS3MountPoint] = useLocalStorage({ key: 'omnifuse-s3-mount', defaultValue: '' });

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

  // Subscribe to product events
  useEffect(() => {
    const unlisteners: Promise<UnlistenFn>[] = [
      listen<OmniEvent>('omnifuse:event', (e) => {
        if (e.payload.kind === 'mount.ready') {
          setMounted(true);
          setLoading(false);
        } else if (e.payload.kind === 'mount.stop' || e.payload.kind === 'mount.fail') {
          setMounted(false);
          setLoading(false);
        }

        setLogs((prev) =>
          appendLog(prev, {
            level: e.payload.level,
            message: eventMessage(e.payload),
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
    } else if (backend === 'wiki') {
      if (!wikiUrl.trim()) {
        e.wikiUrl = 'Required';
      } else {
        try { new URL(wikiUrl); } catch { e.wikiUrl = 'Invalid URL'; }
      }
      if (!wikiSlug.trim()) e.wikiSlug = 'Required';
      if (!wikiToken.trim()) e.wikiToken = 'Required';
      if (!wikiMountPoint.trim()) e.wikiMountPoint = 'Required';
    } else {
      if (!s3Bucket.trim()) e.s3Bucket = 'Required';
      if (!s3SecretAccessKey.trim()) e.s3SecretAccessKey = 'Required';
      if (!s3MountPoint.trim()) e.s3MountPoint = 'Required';
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
      } else if (backend === 'wiki') {
        await invoke('mount_wiki', {
          baseUrl: wikiUrl.trim(),
          rootSlug: wikiSlug.trim(),
          authToken: wikiToken,
          mountPoint: wikiMountPoint.trim(),
        });
      } else {
        await invoke('mount_s3', {
          bucket: s3Bucket.trim(),
          prefix: s3Prefix.trim() || null,
          endpoint: s3Endpoint.trim() || null,
          region: s3Region.trim() || null,
          accessKeyId: s3AccessKeyId.trim() || null,
          secretAccessKey: s3SecretAccessKey,
          mountPoint: s3MountPoint.trim(),
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
      setMounted(false);
      setLoading(false);
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
      : backend === 'wiki'
        ? wikiUrl && wikiSlug && wikiToken && wikiMountPoint
        : s3Bucket && s3SecretAccessKey && s3MountPoint;

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
          { label: 'S3', value: 's3' },
        ]}
        disabled={mounted || loading}
      />

      {backend === 'git' && (
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
      )}

      {backend === 'wiki' && (
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

      {backend === 's3' && (
        <>
          <TextInput
            label="Bucket"
            placeholder="my-bucket"
            value={s3Bucket}
            onChange={(e) => { setS3Bucket(e.target.value); setErrors((p) => ({ ...p, s3Bucket: undefined })); }}
            disabled={mounted || loading}
            error={errors.s3Bucket}
          />
          <TextInput
            label="Prefix"
            placeholder="optional/key/prefix"
            value={s3Prefix}
            onChange={(e) => setS3Prefix(e.target.value)}
            disabled={mounted || loading}
          />
          <TextInput
            label="Endpoint"
            placeholder="https://s3.amazonaws.com"
            value={s3Endpoint}
            onChange={(e) => setS3Endpoint(e.target.value)}
            disabled={mounted || loading}
          />
          <TextInput
            label="Region"
            placeholder="us-east-1 / auto"
            value={s3Region}
            onChange={(e) => setS3Region(e.target.value)}
            disabled={mounted || loading}
          />
          <TextInput
            label="Access key ID"
            placeholder="AKIA..."
            value={s3AccessKeyId}
            onChange={(e) => setS3AccessKeyId(e.target.value)}
            disabled={mounted || loading}
          />
          <PasswordInput
            label="Secret access key"
            placeholder="secret"
            value={s3SecretAccessKey}
            onChange={(e) => { setS3SecretAccessKey(e.target.value); setErrors((p) => ({ ...p, s3SecretAccessKey: undefined })); }}
            disabled={mounted || loading}
            error={errors.s3SecretAccessKey}
          />
          <Group align="end" gap="xs">
            <TextInput
              label="Mount point"
              placeholder="/path/to/mount"
              value={s3MountPoint}
              onChange={(e) => { setS3MountPoint(e.target.value); setErrors((p) => ({ ...p, s3MountPoint: undefined })); }}
              disabled={mounted || loading}
              style={{ flex: 1 }}
              error={errors.s3MountPoint}
            />
            <Button
              onClick={() => handlePickFolder(setS3MountPoint)}
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
