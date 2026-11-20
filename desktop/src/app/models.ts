// Mirrors the serde types of skipjack-core.

export interface Device {
  screen_id: string;
  name: string;
  /** Unknown keys (e.g. legacy `offset`) are preserved by the backend. */
  [extra: string]: unknown;
}

export interface Config {
  devices: Device[];
  mute_ads: boolean;
  skip_ads: boolean;
  auto_play: boolean;
  join_name: string;
  use_proxy: boolean;
  experimental_reload_ads: boolean;
  /** Unknown keys are preserved by the backend. */
  [extra: string]: unknown;
}

export interface Meta {
  version: string;
  data_dir: string;
  config_path: string;
  log_path: string | null;
  tray_mode: boolean;
}

export interface PairedScreen {
  screen_id: string;
  name: string | null;
  lounge_token: string | null;
}

export interface DiscoveredDevice {
  screen_id: string;
  name: string;
  source: 'dial' | 'chromecast';
}

export type PlayerState =
  | 'stopped'
  | 'buffering'
  | 'playing'
  | 'paused'
  | 'starting'
  | 'cued'
  | 'advertisement'
  | 'ad_skipped';

export interface PlaybackState {
  video_id: string;
  current_time: number;
  duration: number;
  state: PlayerState;
}

export interface QueueState {
  list_id: string | null;
  video_ids: string[];
}

export interface QueueItem {
  video_id: string;
  title: string | null;
}

export interface VolumeState {
  volume: number;
  muted: boolean;
}

export interface QualityState {
  level: string | null;
  available: string[];
}

export type AdAction = 'muted' | 'skipped' | 'unmuted';

export type DeviceStatus =
  | { kind: 'authenticating' }
  | { kind: 'waiting_for_device' }
  | { kind: 'connecting' }
  | { kind: 'connected'; screen_name: string | null }
  | { kind: 'disconnected' }
  | { kind: 'error'; message: string };

export type ServiceEvent =
  | { type: 'started'; devices: number }
  | { type: 'warning'; message: string }
  | { type: 'device_status'; screen_id: string; name: string; status: DeviceStatus }
  | { type: 'playback'; screen_id: string; name: string; playback: PlaybackState }
  | { type: 'queue'; screen_id: string; name: string; queue: QueueState }
  | { type: 'volume'; screen_id: string; name: string; volume: VolumeState }
  | { type: 'quality'; screen_id: string; name: string; quality: QualityState }
  | { type: 'ad'; screen_id: string; name: string; action: AdAction }
  | { type: 'stopped' };

export interface DeviceLive {
  screen_id: string;
  name: string;
  status: DeviceStatus;
  playback?: PlaybackState;
  queue?: QueueState;
  volume?: VolumeState;
  quality?: QualityState;
  ads: number;
}

export interface ActivityEntry {
  at: Date;
  level: 'info' | 'ad' | 'warn';
  device?: string;
  text: string;
}

export interface Stats {
  ads_muted: number;
  ads_skipped: number;
}
