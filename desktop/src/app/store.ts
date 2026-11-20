import { Injectable, computed, signal } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  ActivityEntry,
  Config,
  DeviceLive,
  DiscoveredDevice,
  Meta,
  QualityState,
  QueueItem,
  QueueState,
  Stats,
  PairedScreen,
  ServiceEvent,
  VolumeState,
} from './models';

const MAX_ACTIVITY = 200;

function errorText(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

/** Global app state + typed bridge to the Tauri backend. */
@Injectable({ providedIn: 'root' })
export class Store {
  readonly meta = signal<Meta | null>(null);
  readonly draft = signal<Config | null>(null);
  private readonly savedJson = signal<string>('');
  readonly dirty = computed(() => {
    const d = this.draft();
    return d !== null && JSON.stringify(d) !== this.savedJson();
  });

  readonly running = signal(false);
  readonly busy = signal(false);
  readonly devices = signal<Record<string, DeviceLive>>({});
  readonly activity = signal<ActivityEntry[]>([]);
  readonly adsMuted = signal(0);
  readonly adsSkipped = signal(0);
  readonly toast = signal<{ text: string; error: boolean } | null>(null);

  readonly discovering = signal(false);
  readonly discovered = signal<DiscoveredDevice[]>([]);

  /** Configured devices merged with their live status. */
  readonly deviceList = computed<DeviceLive[]>(() => {
    const live = this.devices();
    const saved: Config | null = this.savedJson() ? JSON.parse(this.savedJson()) : null;
    return (saved?.devices ?? []).map(
      (d) =>
        live[d.screen_id] ?? {
          screen_id: d.screen_id,
          name: d.name,
          status: { kind: 'disconnected' },
          ads: 0,
        },
    );
  });

  private toastTimer?: ReturnType<typeof setTimeout>;

  async init(): Promise<void> {
    await listen<ServiceEvent>('service-event', (e) => this.onServiceEvent(e.payload));
    await listen<DiscoveredDevice>('discovery-device', (e) =>
      this.discovered.update((list) => [...list, e.payload]),
    );
    await listen('discovery-done', () => this.discovering.set(false));
    await listen<Stats>('stats', (e) => this.applyStats(e.payload));

    try {
      this.meta.set(await invoke<Meta>('get_meta'));
      this.applyStats(await invoke<Stats>('get_stats'));
      await this.reload();
    } catch (e) {
      this.notify(`Failed to load configuration: ${errorText(e)}`, true);
      return;
    }
    // Started only now, after the listeners above, so no status event is lost.
    // In tray mode the service stays stopped until the user presses Start,
    // so nothing connects or polls while disconnected.
    if (this.meta()?.tray_mode) {
      this.running.set(false);
      return;
    }
    try {
      this.running.set(await invoke<boolean>('autostart_service'));
    } catch (e) {
      this.notify(`Auto-start failed: ${errorText(e)}`, true);
    }
  }

  // ---------------------------------------------------------------- config

  async reload(): Promise<void> {
    const cfg = await invoke<Config>('get_config');
    this.savedJson.set(JSON.stringify(cfg));
    this.draft.set(cfg);
  }

  /** Applies an edit to the draft config (immutably). */
  patch(edit: (c: Config) => void): void {
    const current = this.draft();
    if (!current) return;
    const next = structuredClone(current);
    edit(next);
    this.draft.set(next);
  }

  /** Sets a single top-level config field. */
  set<K extends keyof Config>(key: K, value: Config[K]): void {
    this.patch((c) => (c[key] = value));
  }

  discard(): void {
    const saved = this.savedJson();
    if (saved) this.draft.set(JSON.parse(saved));
  }

  async save(): Promise<void> {
    const cfg = this.draft();
    if (!cfg) return;
    this.busy.set(true);
    try {
      await invoke('save_config', { config: cfg });
      this.savedJson.set(JSON.stringify(cfg));
      const warnings = await invoke<string[]>('validate_config', { config: cfg }).catch(() => []);
      warnings.forEach((w) => this.log('warn', w));
      if (this.running()) {
        this.devices.set({});
        await invoke('start_service'); // restart with the new config
        this.notify('Saved – service restarted with the new settings');
      } else {
        this.notify('Settings saved');
      }
    } catch (e) {
      this.notify(errorText(e), true);
    } finally {
      this.busy.set(false);
    }
  }

  // ---------------------------------------------------------------- service

  async start(): Promise<void> {
    if (this.dirty()) await this.save();
    this.busy.set(true);
    try {
      this.devices.set({});
      await invoke('start_service');
      this.running.set(true);
    } catch (e) {
      this.notify(errorText(e), true);
    } finally {
      this.busy.set(false);
    }
  }

  async stop(): Promise<void> {
    this.busy.set(true);
    try {
      await invoke('stop_service');
      this.running.set(false);
    } catch (e) {
      this.notify(errorText(e), true);
    } finally {
      this.busy.set(false);
    }
  }

  // ---------------------------------------------------------------- devices

  async pair(code: string): Promise<PairedScreen | null> {
    try {
      const paired = await invoke<PairedScreen>('pair_device', {
        code,
        useProxy: this.draft()?.use_proxy ?? false,
      });
      this.addDevice(paired.screen_id, paired.name ?? 'YouTube on TV');
      return paired;
    } catch (e) {
      this.notify(errorText(e), true);
      return null;
    }
  }

  addDevice(screenId: string, name: string): void {
    this.patch((c) => {
      if (!c.devices.some((d) => d.screen_id === screenId)) {
        c.devices.push({ screen_id: screenId, name });
      }
    });
    this.notify(`Added “${name}” – don't forget to save`);
  }

  async discover(): Promise<void> {
    this.discovered.set([]);
    this.discovering.set(true);
    try {
      await invoke('start_discovery', { active: true, useProxy: this.draft()?.use_proxy ?? false });
    } catch (e) {
      this.discovering.set(false);
      this.notify(errorText(e), true);
    }
  }

  // ---------------------------------------------------------------- remote

  private async remote(cmd: string, args: Record<string, unknown>): Promise<void> {
    try {
      await invoke(cmd, args);
    } catch (e) {
      this.notify(errorText(e), true);
    }
  }

  remotePlay(screenId: string): Promise<void> {
    return this.remote('remote_play', { screenId });
  }

  remotePause(screenId: string): Promise<void> {
    return this.remote('remote_pause', { screenId });
  }

  remoteSeek(screenId: string, secs: number): Promise<void> {
    return this.remote('remote_seek', { screenId, secs });
  }

  remoteNext(screenId: string): Promise<void> {
    return this.remote('remote_next', { screenId });
  }

  remotePrevious(screenId: string): Promise<void> {
    return this.remote('remote_previous', { screenId });
  }

  remoteSpeed(screenId: string, speed: number): Promise<void> {
    return this.remote('remote_speed', { screenId, speed });
  }

  remoteDpad(screenId: string, key: string): Promise<void> {
    return this.remote('remote_dpad', { screenId, key });
  }

  remotePlayVideo(screenId: string, videoId: string): Promise<void> {
    return this.remote('remote_play_video', { screenId, videoId });
  }

  remoteSetPlaylist(screenId: string, videoId: string, queue?: string): Promise<void> {
    return this.remote('remote_set_playlist', { screenId, videoId, queue });
  }

  remoteAddVideo(screenId: string, videoId: string): Promise<void> {
    return this.remote('remote_add_video', { screenId, videoId });
  }

  remoteAddVideos(screenId: string, videoIds: string): Promise<void> {
    return this.remote('remote_add_videos', { screenId, videoIds });
  }

  remoteInsertVideo(screenId: string, videoId: string): Promise<void> {
    return this.remote('remote_insert_video', { screenId, videoId });
  }

  remoteRemoveVideo(screenId: string, videoId: string): Promise<void> {
    return this.remote('remote_remove_video', { screenId, videoId });
  }

  remoteClearPlaylist(screenId: string): Promise<void> {
    return this.remote('remote_clear_playlist', { screenId });
  }

  remoteQueue(screenId: string): Promise<QueueState> {
    return invoke('remote_queue', { screenId });
  }

  remoteQueueItems(screenId: string): Promise<QueueItem[]> {
    return invoke('remote_queue_items', { screenId });
  }

  remoteVolume(screenId: string): Promise<VolumeState> {
    return invoke('remote_volume', { screenId });
  }

  remoteQuality(screenId: string): Promise<QualityState> {
    return invoke('remote_quality', { screenId });
  }

  // ---------------------------------------------------------------- misc

  notify(text: string, error = false): void {
    this.toast.set({ text, error });
    clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => this.toast.set(null), error ? 7000 : 3500);
  }

  private log(level: ActivityEntry['level'], text: string, device?: string): void {
    this.activity.update((list) => [{ at: new Date(), level, text, device }, ...list].slice(0, MAX_ACTIVITY));
  }

  private upsertDevice(screenId: string, name: string, edit: (d: DeviceLive) => void): void {
    this.devices.update((all) => {
      const current: DeviceLive = all[screenId]
        ? { ...all[screenId] }
        : { screen_id: screenId, name, status: { kind: 'connecting' }, ads: 0 };
      current.name = name;
      edit(current);
      return { ...all, [screenId]: current };
    });
  }

  private applyStats(s: Stats): void {
    this.adsMuted.set(s.ads_muted);
    this.adsSkipped.set(s.ads_skipped);
  }

  async resetStats(): Promise<void> {
    this.applyStats(await invoke<Stats>('reset_stats'));
  }

  private onServiceEvent(ev: ServiceEvent): void {
    switch (ev.type) {
      case 'started':
        this.running.set(true);
        this.log('info', `Service started for ${ev.devices} device(s)`);
        break;
      case 'stopped':
        this.running.set(false);
        this.log('info', 'Service stopped');
        break;
      case 'warning':
        this.log('warn', ev.message);
        break;
      case 'device_status':
        this.upsertDevice(ev.screen_id, ev.name, (d) => (d.status = ev.status));
        if (ev.status.kind === 'connected') {
          this.log('info', `Connected${ev.status.screen_name ? ` to ${ev.status.screen_name}` : ''}`, ev.name);
        } else if (ev.status.kind === 'error') {
          this.log('warn', ev.status.message, ev.name);
        }
        break;
      case 'playback':
        this.upsertDevice(ev.screen_id, ev.name, (d) => (d.playback = ev.playback));
        break;
      case 'queue':
        this.upsertDevice(ev.screen_id, ev.name, (d) => (d.queue = ev.queue));
        break;
      case 'volume':
        this.upsertDevice(ev.screen_id, ev.name, (d) => (d.volume = ev.volume));
        break;
      case 'quality':
        this.upsertDevice(ev.screen_id, ev.name, (d) => (d.quality = ev.quality));
        break;
      case 'ad':
        // Totals come from the backend via the `stats` event.
        if (ev.action === 'muted') {
          this.upsertDevice(ev.screen_id, ev.name, (d) => d.ads++);
          this.log('ad', 'Ad started – muted', ev.name);
        } else if (ev.action === 'skipped') {
          this.upsertDevice(ev.screen_id, ev.name, (d) => d.ads++);
          this.log('ad', 'Ad skipped', ev.name);
        } else {
          this.log('info', 'Ad ended – unmuted', ev.name);
        }
        break;
    }
  }
}

export function fmt(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
}
